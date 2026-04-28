//! Storage thread: sole owner of all three buckets and their log files.
//!
//! ## Lock-free architecture
//!
//! The storage thread owns the buckets *outright* — they are not behind a
//! lock. Reader threads do not share access to them. Instead, after each
//! committed write the storage thread pushes an [`IndexUpdate`] message
//! onto a `quetzalcoatl` broadcast ring. Reader threads each maintain
//! their own thread-local `BucketIndex` snapshots and apply updates from
//! the ring on demand.
//!
//! This is the classic disruptor / event-sourcing pattern: there is no
//! shared mutable state on the read path. Readers may lag the writer by a
//! handful of updates (whatever they haven't drained yet) but are never
//! blocked by the writer or by each other.
//!
//! Responsibilities of the storage thread:
//! - Drain all per-shard write rings round-robin.
//! - Parse each `WriteReq` into a payload the bucket can ingest.
//! - Dispatch to the correct bucket (by `BucketKind`).
//! - Stage writes refused due to g_floor, retry them on subsequent passes.
//! - Bump `current_gen` at batch boundaries; fsync on a timer.
//! - Push `IndexUpdate` per committed write so readers stay current.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nostro2::NostrNoteView;
use quetzalcoatl::broadcast::arc::ArcProducer;
use quetzalcoatl::mpsc::Producer as MpscProducer;

use super::StorageConfig;
use super::bucket::{
    Bucket, EphemeralBucket, EventPayload, ParameterizedBucket, ReplaceableBucket, WriteOutcome,
};
use super::handle::{AckOutcome, IndexUpdate, StorageAck, WriteReq, WriteRx};

/// Push a verdict for `req` onto its source shard's MPSC ack ring,
/// then unpark the shard so it drains promptly. The shard parks on
/// its `ReaderCore` epoll, not on the ack ring, so an unpark is
/// required — same shape as verify_pool's worker → shard wake.
fn send_ack(
    ack_txs: &[MpscProducer<StorageAck>],
    shard_wakers: &[Arc<OnceLock<std::thread::Thread>>],
    req: &WriteReq,
    outcome: AckOutcome,
) {
    let idx = req.source_shard as usize;
    let Some(tx) = ack_txs.get(idx) else {
        return;
    };
    let ack = StorageAck {
        client_id: req.client_id,
        event_id_hex: Arc::clone(&req.event_id_hex),
        outcome,
    };
    // Spin-push: an OK frame is small and the shard drains on every
    // loop iteration, so this is rarely contended. If the ring is full
    // (a pathological backpressure case), drop the ack — the publisher
    // gets no OK, which is still better than blocking the storage
    // thread on a single slow shard.
    if tx.push(ack).is_err() {
        return;
    }
    if let Some(waker) = shard_wakers.get(idx)
        && let Some(thread) = waker.get()
    {
        thread.unpark();
    }
}
use super::slot::BucketKind;
use crate::filter::{DeletionRef, deletion_refs_from_view};

/// NIP-09 deletion state owned by the storage thread.
///
/// `deleted_ids` maps `event_id → deleter_pubkey`. Future re-publishes
/// of a deleted id are dropped only if the publisher's pubkey matches
/// the deleter's — without this check, anyone could pre-emptively
/// "delete" an arbitrary 32-byte hex string and silently suppress a
/// legitimate publisher who happens to produce that id later.
///
/// `deleted_addresses` carries replaceable / parameterized addresses
/// `(kind, pubkey, d_tag) → deletion.created_at`. New events at the
/// address are dropped only if their `created_at` is older than the
/// deletion; newer events revive the address. The pubkey is part of
/// the key, and the same-pubkey ownership check at apply time means
/// only the rightful owner can write here.
///
/// State is process-local — it lives on disk only as long as the kind-5
/// events that produced it. On reopen, those events are still in the log
/// and are reapplied during bucket rebuild.
#[derive(Default)]
struct DeletionState {
    deleted_ids: std::collections::HashMap<[u8; 32], [u8; 32]>,
    deleted_addresses: std::collections::HashMap<(u32, [u8; 32], Box<str>), i64>,
}

/// Shared state between storage and reader threads. After the lock-free
/// conversion, this contains only the generational-CoW gate — no buckets,
/// no `RwLock`. Reader threads own their own `BucketIndex` instances and
/// receive mutations via the broadcast ring.
pub struct SharedState {
    /// Current generation. Bumped after each batch commit. Readers load
    /// this at REQ start and ignore any slot with `slot.gen > g_req`.
    pub current_gen: Arc<AtomicU64>,
    /// Each reader thread owns one slot here. At REQ start it stores
    /// `g_req`; at REQ end it stores `u64::MAX` (idle). `g_floor` is the
    /// min over all entries. Bounded by `reader_threads`.
    pub reader_active_gens: Arc<[AtomicU64]>,
}

impl SharedState {
    pub fn compute_g_floor(&self) -> u64 {
        self.reader_active_gens
            .iter()
            .map(|a| a.load(Ordering::Acquire))
            .min()
            .unwrap_or(u64::MAX)
    }
}

/// Handle the main thread holds to gracefully shut down the storage loop.
pub struct StorageShutdown {
    pub flag: Arc<AtomicBool>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

impl StorageShutdown {
    pub fn stop(&mut self) {
        self.flag.store(true, Ordering::Release);
        if let Some(h) = self.thread.as_ref() {
            h.thread().unpark();
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

/// The storage engine. The main thread constructs this, spawns its worker,
/// and hands the `SharedState` arc to the reader pool.
pub struct StorageEngine {
    pub shared: Arc<SharedState>,
}

impl StorageEngine {
    /// Open buckets + spawn the storage thread.
    ///
    /// `index_tx` is the producer side of the broadcast ring; the storage
    /// thread pushes `IndexUpdate` messages here for the reader pool to
    /// consume. The caller is responsible for sizing the ring.
    /// `ack_txs` carries one MPSC producer per shard; storage routes
    /// per-write verdicts via `WriteReq::source_shard` indexing into
    /// this Vec. `shard_wakers` is the parallel `Vec<Arc<OnceLock<Thread>>>`
    /// — a `Thread` handle is published into each `OnceLock` once the
    /// corresponding shard's main loop starts; before then the storage
    /// thread queues acks without waking, since a shard that hasn't
    /// finished startup will drain its ring as soon as it does.
    pub fn spawn(
        config: &StorageConfig,
        reader_threads: usize,
        write_rx: WriteRx,
        index_tx: ArcProducer<IndexUpdate>,
        ack_txs: Vec<MpscProducer<StorageAck>>,
        shard_wakers: Vec<Arc<OnceLock<std::thread::Thread>>>,
    ) -> std::io::Result<(Self, StorageShutdown, std::thread::Thread)> {
        std::fs::create_dir_all(&config.data_dir)?;
        let eph_path = config.data_dir.join("ephemeral.log");
        let rep_path = config.data_dir.join("replaceable.log");
        let par_path = config.data_dir.join("parameterized.log");

        let mut eph = EphemeralBucket::open(&eph_path, config.ephemeral_slots, config.max_payload)?;
        let mut rep =
            ReplaceableBucket::open(&rep_path, config.replaceable_slots, config.max_payload)?;
        let mut par =
            ParameterizedBucket::open(&par_path, config.parameterized_slots, config.max_payload)?;

        eph.rebuild()?;
        rep.rebuild()?;
        par.rebuild()?;

        // Next seq for newly-written slots must be > any existing slot's seq.
        // current_gen must start at > any persisted gen so reopens don't
        // render old slots "from the future."
        let (next_seq, max_gen) = highest_seq_and_gen(&eph, &rep, &par);
        let initial_gen = max_gen.saturating_add(1);

        let reader_active_gens: Arc<[AtomicU64]> = (0..reader_threads.max(1))
            .map(|_| AtomicU64::new(u64::MAX))
            .collect();

        let shared = Arc::new(SharedState {
            current_gen: Arc::new(AtomicU64::new(initial_gen)),
            reader_active_gens,
        });

        let shutdown = Arc::new(AtomicBool::new(false));
        let shared_for_thread = Arc::clone(&shared);
        let shutdown_for_thread = Arc::clone(&shutdown);
        let fsync_interval = config.fsync_interval_ms.map(Duration::from_millis);

        let handle = std::thread::Builder::new()
            .name("nostr-storage".into())
            .spawn(move || {
                storage_loop(
                    shared_for_thread,
                    write_rx,
                    index_tx,
                    shutdown_for_thread,
                    eph,
                    rep,
                    par,
                    next_seq,
                    fsync_interval,
                    ack_txs,
                    shard_wakers,
                );
            })?;
        let thread = handle.thread().clone();
        Ok((
            StorageEngine { shared },
            StorageShutdown {
                flag: shutdown,
                thread: Some(handle),
            },
            thread,
        ))
    }

    pub fn shared(&self) -> Arc<SharedState> {
        Arc::clone(&self.shared)
    }
}

/// Highest `(seq, generation)` pair seen across all three buckets'
/// rebuilt indexes. Used at startup to seed `next_seq` and `current_gen`
/// so we never reuse a value that already lives on disk.
fn highest_seq_and_gen(
    eph: &EphemeralBucket,
    rep: &ReplaceableBucket,
    par: &ParameterizedBucket,
) -> (u64, u64) {
    let mut max_seq: u64 = 0;
    let mut max_gen: u64 = 0;
    for b in [eph as &dyn Bucket, rep as &dyn Bucket, par as &dyn Bucket] {
        for meta in b.index().meta.iter().flatten() {
            max_seq = max_seq.max(meta.seq.get());
            max_gen = max_gen.max(meta.generation);
        }
    }
    (max_seq, max_gen)
}

#[allow(clippy::too_many_arguments)]
fn storage_loop(
    shared: Arc<SharedState>,
    mut write_rx: WriteRx,
    index_tx: ArcProducer<IndexUpdate>,
    shutdown: Arc<AtomicBool>,
    mut eph: EphemeralBucket,
    mut rep: ReplaceableBucket,
    mut par: ParameterizedBucket,
    mut next_seq: u64,
    fsync_interval: Option<Duration>,
    ack_txs: Vec<MpscProducer<StorageAck>>,
    shard_wakers: Vec<Arc<OnceLock<std::thread::Thread>>>,
) {
    let mut last_fsync = Instant::now();
    let mut staged: Vec<StagedWrite> = Vec::new();
    let mut batch: Vec<WriteReq> = Vec::with_capacity(1024);
    // NIP-09 deletion state. Replayed at startup from existing kind-5
    // events on disk via `seed_deletions_from_disk` below.
    let mut deletions = DeletionState::default();
    seed_deletions_from_disk(&mut eph, &mut rep, &mut par, &mut deletions, &index_tx);

    while !shutdown.load(Ordering::Acquire) {
        batch.clear();

        // Drain the shared MPSC write ring up to the batch cap.
        // `drain_up_to` amortizes the consumer's head-pointer update —
        // one Release store per drain instead of one per item — so this
        // is meaningfully cheaper than a `while let Some = pop()` loop
        // when the ring is hot.
        write_rx.0.drain_up_to(1024, |req| batch.push(req));

        if batch.is_empty() && staged.is_empty() {
            // Idle: park with timeout so we can still fsync on schedule.
            if let Some(interval) = fsync_interval {
                let since = last_fsync.elapsed();
                if since >= interval {
                    do_fsync(&eph, &rep, &par);
                    last_fsync = Instant::now();
                    std::thread::park_timeout(interval);
                } else {
                    std::thread::park_timeout(interval - since);
                }
            } else {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            continue;
        }

        let generation = shared.current_gen.load(Ordering::Acquire);
        let g_floor = shared.compute_g_floor();

        // First, retry any staged writes; they're older so they get priority.
        staged.retain_mut(|sw| {
            ingest_one(
                &mut eph,
                &mut rep,
                &mut par,
                sw,
                generation,
                g_floor,
                &mut next_seq,
                &index_tx,
                &mut deletions,
                &ack_txs,
                &shard_wakers,
            )
        });

        // Then ingest the fresh batch.
        for req in batch.drain(..) {
            let mut sw = StagedWrite { req };
            if ingest_one(
                &mut eph,
                &mut rep,
                &mut par,
                &mut sw,
                generation,
                g_floor,
                &mut next_seq,
                &index_tx,
                &mut deletions,
                &ack_txs,
                &shard_wakers,
            ) {
                staged.push(sw);
            }
        }

        // Commit this batch by bumping the generation so readers can see it.
        shared.current_gen.fetch_add(1, Ordering::AcqRel);

        // Opportunistic fsync.
        if let Some(interval) = fsync_interval
            && last_fsync.elapsed() >= interval
        {
            do_fsync(&eph, &rep, &par);
            last_fsync = Instant::now();
        }
    }

    // Final flush on shutdown.
    do_fsync(&eph, &rep, &par);
}

struct StagedWrite {
    req: WriteReq,
}

/// Try to ingest one write. Returns `true` if it should be re-staged
/// (stalled by g_floor), `false` if committed or dropped. On commit,
/// pushes an `IndexUpdate` so reader threads see the new slot. On every
/// terminal outcome (commit, dup, drop, oversize) it also pushes a
/// [`StorageAck`] back to the source shard so the publisher learns the
/// real verdict.
#[allow(clippy::too_many_arguments)]
fn ingest_one(
    eph: &mut EphemeralBucket,
    rep: &mut ReplaceableBucket,
    par: &mut ParameterizedBucket,
    sw: &mut StagedWrite,
    generation: u64,
    g_floor: u64,
    next_seq: &mut u64,
    index_tx: &ArcProducer<IndexUpdate>,
    deletions: &mut DeletionState,
    ack_txs: &[MpscProducer<StorageAck>],
    shard_wakers: &[Arc<OnceLock<std::thread::Thread>>],
) -> bool {
    let req = &sw.req;
    let note: NostrNoteView<'_> = match serde_json::from_slice(&req.raw_json) {
        Ok(v) => v,
        Err(_) => {
            // Should not happen — the shard already parsed the same
            // bytes via the verify pool. But be honest if it does.
            send_ack(
                ack_txs,
                shard_wakers,
                req,
                AckOutcome::Rejected("invalid: malformed event"),
            );
            return false;
        }
    };

    // NIP-09 id-target gating. The deleter's pubkey is recorded alongside
    // the id; we only suppress when this publish comes from that same
    // pubkey (schnorr-signature binding means Eve can't forge Alice's id,
    // so Eve's pre-emptive deletion of Alice's id never trips here).
    if req.kind != 5
        && let Some(deleter) = deletions.deleted_ids.get(&req.event_id)
        && *deleter == req.pubkey
    {
        send_ack(
            ack_txs,
            shard_wakers,
            req,
            AckOutcome::Rejected("blocked: event was deleted"),
        );
        return false;
    }

    let bucket_kind = BucketKind::classify(req.kind);

    // NIP-09 address-target enforcement. The deletion's own same-pubkey
    // ownership check ran at apply time, so any address present here was
    // deleted by its rightful owner. A *newer* event at the same address
    // is allowed; only older ones are suppressed.
    if req.kind != 5
        && let Some(deletion_ts) =
            lookup_address_deletion(deletions, &note, bucket_kind, req.kind, &req.pubkey)
        && note.created_at < deletion_ts
    {
        send_ack(
            ack_txs,
            shard_wakers,
            req,
            AckOutcome::Rejected("blocked: address was deleted"),
        );
        return false;
    }

    let payload = EventPayload {
        note: &note,
        raw_json: req.raw_json.as_ref(),
        event_id: req.event_id,
        pubkey: req.pubkey,
    };
    let outcome = match bucket_kind {
        BucketKind::Ephemeral => eph.try_write(&payload, generation, g_floor, next_seq),
        BucketKind::Replaceable => rep.try_write(&payload, generation, g_floor, next_seq),
        BucketKind::Parameterized => par.try_write(&payload, generation, g_floor, next_seq),
    };

    match outcome {
        WriteOutcome::Committed { slot_idx, meta } => {
            // Broadcast the new slot meta to reader threads.
            // Backpressure: the ring is sized per-batch * margin so
            // overflow shouldn't happen in practice; if it does, spin
            // briefly. Storage thread can afford the wait — the
            // alternative is dropping a write that already hit disk.
            push_with_backoff(
                index_tx,
                IndexUpdate {
                    bucket: bucket_kind,
                    slot_idx,
                    meta: Some(meta),
                },
            );
            // NIP-09: if this is a deletion event, apply it after the
            // commit so the kind-5 itself is replayable. Apply uses the
            // same-pubkey ownership rule.
            if req.kind == 5 {
                apply_kind5_deletion(eph, rep, par, &note, &req.pubkey, deletions, index_tx);
            }
            send_ack(ack_txs, shard_wakers, req, AckOutcome::Stored);
            false
        }
        WriteOutcome::Stalled => {
            // Re-staged for retry. No ack yet — the next ingest_one
            // pass will land on Committed (or one of the drop paths)
            // and ack then.
            true
        }
        WriteOutcome::Duplicate => {
            send_ack(ack_txs, shard_wakers, req, AckOutcome::Duplicate);
            false
        }
        WriteOutcome::TooBig => {
            send_ack(
                ack_txs,
                shard_wakers,
                req,
                AckOutcome::Rejected("invalid: payload exceeds max_payload"),
            );
            false
        }
    }
}

/// Look up an address-deletion timestamp for an incoming event.
///
/// Returns `Some(ts)` if there's a recorded deletion for this event's
/// `(kind, pubkey, d_tag)`. `d_tag` is empty for replaceable kinds and
/// extracted from the `d` tag for parameterized. Ephemeral kinds aren't
/// addressable; we return `None` to skip the lookup entirely.
fn lookup_address_deletion(
    deletions: &DeletionState,
    note: &NostrNoteView<'_>,
    bucket_kind: BucketKind,
    kind: u32,
    pubkey: &[u8; 32],
) -> Option<i64> {
    let d_tag: Box<str> = match bucket_kind {
        BucketKind::Replaceable => Box::from(""),
        BucketKind::Parameterized => extract_d_tag(note),
        BucketKind::Ephemeral => return None,
    };
    let key = (kind, *pubkey, d_tag);
    deletions.deleted_addresses.get(&key).copied()
}

/// Mirror of `ParameterizedBucket::extract_d_tag` for use during the
/// pre-bucket lookup. Inline rather than going through the bucket so
/// `ingest_one` can decide before borrowing the bucket mutably.
fn extract_d_tag(note: &NostrNoteView<'_>) -> Box<str> {
    for row in note.tags.iter() {
        if row.first().map(|s| s.as_ref()) == Some("d")
            && let Some(v) = row.get(1)
        {
            return Box::from(v.as_ref());
        }
    }
    Box::from("")
}

/// Apply a kind-5 deletion event to the in-memory indexes.
///
/// For each `e` and `a` tag on the deletion event:
///   - Verify the target's pubkey matches the deletion event's pubkey.
///     A kind-5 from Bob cannot delete Alice's events.
///   - For `e`: locate the slot by full event id across all buckets,
///     remove it, broadcast `IndexUpdate { meta: None }`, and record
///     the id in `deletions.deleted_ids` so future re-publishes drop.
///   - For `a`: parse the address `(kind, pubkey, d_tag)`, verify
///     ownership, and record in `deletions.deleted_addresses`. Locating
///     the existing slot for replaceable/parameterized address would
///     require a per-bucket address index that we don't keep yet —
///     existing slots get filtered out by the deleted_addresses set
///     on read in a follow-up. v1 records the deletion so future
///     ingests at that address are gated.
fn apply_kind5_deletion(
    eph: &mut EphemeralBucket,
    rep: &mut ReplaceableBucket,
    par: &mut ParameterizedBucket,
    note: &NostrNoteView<'_>,
    author_pubkey: &[u8; 32],
    deletions: &mut DeletionState,
    index_tx: &ArcProducer<IndexUpdate>,
) {
    let refs = deletion_refs_from_view(note);
    for r in refs {
        match r {
            DeletionRef::EventId(target_id) => {
                let removed = try_remove_id(
                    eph,
                    BucketKind::Ephemeral,
                    &target_id,
                    author_pubkey,
                    index_tx,
                ) || try_remove_id(
                    rep,
                    BucketKind::Replaceable,
                    &target_id,
                    author_pubkey,
                    index_tx,
                ) || try_remove_id(
                    par,
                    BucketKind::Parameterized,
                    &target_id,
                    author_pubkey,
                    index_tx,
                );
                // Record the (id → deleter pubkey) regardless of whether
                // the target was found:
                //   - If found, ownership was just verified by try_remove_id;
                //     a future republish from that same pubkey is suppressed.
                //   - If not found (target not yet in storage, or never),
                //     we still gate any future publish *from this same
                //     pubkey* with this id. Eve can't poison Alice's id
                //     because the suppression only fires when Eve herself
                //     republishes that id — which she can't, since the id
                //     binds the signing pubkey through the schnorr signature.
                let _ = removed;
                deletions.deleted_ids.insert(target_id, *author_pubkey);
            }
            DeletionRef::Address {
                kind,
                pubkey,
                d_tag,
            } => {
                if pubkey != *author_pubkey {
                    continue;
                }
                // Route the address removal to the bucket the kind would
                // have written to. NIP-16 replaceable: kind 0 / 3 /
                // 10000..20000 → ReplaceableBucket; NIP-33 parameterized:
                // 30000..40000 → ParameterizedBucket. Anything else
                // shouldn't carry an `a` ref (ephemeral kinds aren't
                // addressable), but the caller could still send one;
                // we silently ignore it.
                let target_bucket_kind = BucketKind::classify(kind);
                let removed_slot = match target_bucket_kind {
                    BucketKind::Replaceable => {
                        rep.try_remove_address(kind, &pubkey, note.created_at)
                    }
                    BucketKind::Parameterized => {
                        par.try_remove_address(kind, &pubkey, &d_tag, note.created_at)
                    }
                    BucketKind::Ephemeral => None,
                };
                if let Some(slot_idx) = removed_slot {
                    push_with_backoff(
                        index_tx,
                        IndexUpdate {
                            bucket: target_bucket_kind,
                            slot_idx,
                            meta: None,
                        },
                    );
                }
                // Record the address regardless of whether a slot was
                // removed: a future re-publish of an event at this
                // address with `created_at` older than this deletion
                // must still be dropped on ingest.
                let key = (kind, pubkey, d_tag);
                let existing = deletions.deleted_addresses.get(&key).copied().unwrap_or(0);
                if note.created_at > existing {
                    deletions
                        .deleted_addresses
                        .insert(key, note.created_at);
                }
            }
        }
    }
}

/// Look up `target_id` in `bucket`'s index. If found and the slot's
/// pubkey equals `author_pubkey`, remove the slot and broadcast a
/// `meta: None` update. Returns `true` if the slot was removed.
fn try_remove_id<B: Bucket>(
    bucket: &mut B,
    bucket_kind: BucketKind,
    target_id: &[u8; 32],
    author_pubkey: &[u8; 32],
    index_tx: &ArcProducer<IndexUpdate>,
) -> bool {
    let Some(slot_idx) = bucket.index().find_by_full_id(target_id) else {
        return false;
    };
    let owner_matches = bucket
        .index()
        .meta
        .get(slot_idx as usize)
        .and_then(|o| o.as_ref())
        .map(|m| m.pubkey == *author_pubkey)
        .unwrap_or(false);
    if !owner_matches {
        return false;
    }
    bucket.index_mut_for_handoff().remove_slot(slot_idx);
    push_with_backoff(
        index_tx,
        IndexUpdate {
            bucket: bucket_kind,
            slot_idx,
            meta: None,
        },
    );
    true
}

/// Walk every bucket's index for kind-5 events at startup and replay
/// them into `deletions`. The buckets' `rebuild()` already loaded the
/// slots; we just need to read each kind-5 payload back from disk and
/// apply the same `apply_kind5_deletion` logic.
///
/// Order matters: kind-5 events are sorted by `created_at` ascending so
/// that a deletion's targets are always processed against the same
/// state the live path saw at the time the deletion was first applied.
/// Without the sort, a chain of deletions could miss its targets if a
/// later deletion was processed before the earlier one wrote to the
/// `deleted_ids` set.
fn seed_deletions_from_disk(
    eph: &mut EphemeralBucket,
    rep: &mut ReplaceableBucket,
    par: &mut ParameterizedBucket,
    deletions: &mut DeletionState,
    index_tx: &ArcProducer<IndexUpdate>,
) {
    // Collect (created_at, payload bytes, author_pubkey) for every kind-5
    // slot across all three buckets. Kind 5 currently routes to the
    // ephemeral bucket per `BucketKind::classify`, but we scan all
    // three so a future routing change doesn't silently break replay.
    let mut kind5: Vec<(i64, Vec<u8>, [u8; 32])> = Vec::new();
    for (i, slot) in eph.index().meta.iter().enumerate() {
        if let Some(meta) = slot
            && meta.kind == 5
            && let Ok(payload) = eph.log().read_payload(i, meta.payload_len)
        {
            kind5.push((meta.created_at, payload, meta.pubkey));
        }
    }
    for (i, slot) in rep.index().meta.iter().enumerate() {
        if let Some(meta) = slot
            && meta.kind == 5
            && let Ok(payload) = rep.log().read_payload(i, meta.payload_len)
        {
            kind5.push((meta.created_at, payload, meta.pubkey));
        }
    }
    for (i, slot) in par.index().meta.iter().enumerate() {
        if let Some(meta) = slot
            && meta.kind == 5
            && let Ok(payload) = par.log().read_payload(i, meta.payload_len)
        {
            kind5.push((meta.created_at, payload, meta.pubkey));
        }
    }
    kind5.sort_by_key(|(ts, _, _)| *ts);

    for (_, payload, pubkey) in kind5 {
        let Ok(view) = serde_json::from_slice::<NostrNoteView<'_>>(&payload) else {
            continue;
        };
        apply_kind5_deletion(eph, rep, par, &view, &pubkey, deletions, index_tx);
    }
}

fn push_with_backoff(tx: &ArcProducer<IndexUpdate>, mut msg: IndexUpdate) {
    let mut spins = 0u32;
    loop {
        match tx.push(msg) {
            Ok(()) => return,
            Err(returned) => {
                msg = returned;
                spins = crate::backoff::step(spins);
            }
        }
    }
}

fn do_fsync(eph: &EphemeralBucket, rep: &ReplaceableBucket, par: &ParameterizedBucket) {
    let _ = eph.log().fsync();
    let _ = rep.log().fsync();
    let _ = par.log().fsync();
}
