//! Reader pool: N threads that service historical REQ scans against
//! thread-local bucket indexes.
//!
//! ## Lock-free architecture
//!
//! Each reader thread owns its own private `BucketIndex` for each of the
//! three buckets, plus a read-only `ReadOnlyLog` handle to fetch payloads
//! on demand. Readers never share state with the storage thread; instead
//! they consume `IndexUpdate` messages off a `quetzalcoatl` broadcast
//! ring and apply them to their local indexes.
//!
//! Bootstrap: each reader independently calls `BucketIndex::rebuild_*`
//! against the on-disk logs at startup. After that, all updates flow via
//! the broadcast ring. There is no shared `RwLock` and no contention
//! between readers or with the storage thread.
//!
//! ## Generational CoW
//!
//! Each reader owns its own `AtomicU64` generation slot inside
//! `SharedState::reader_active_gens`. At REQ start it loads
//! `current_gen` into its slot; at REQ end it writes `u64::MAX` back.
//! The storage thread computes `g_floor = min(active_gens)` for eviction
//! and refuses to overwrite slots a reader cares about.
//!
//! Per-REQ timeout: ~500ms wall-clock. If a scan takes longer we abort
//! it, release the gen slot, and send `CLOSED` with a reason.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use quetzalcoatl::broadcast::arc::ArcConsumer;
use ring_relay_server::ServerSender;

use super::engine::SharedState;
use super::handle::{IndexUpdate, ReqJob, ReqRx};
use super::index::{BucketIndex, SlotMeta};
use super::log::ReadOnlyLog;
use super::slot::BucketKind;
use crate::protocol;

const REQ_TIMEOUT: Duration = Duration::from_millis(500);

pub struct ReaderPoolShutdown {
    pub flag: Arc<AtomicBool>,
    pub threads: Vec<std::thread::JoinHandle<()>>,
}

impl ReaderPoolShutdown {
    pub fn stop(&mut self) {
        self.flag.store(true, Ordering::Release);
        for h in &self.threads {
            h.thread().unpark();
        }
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}

/// Bucket sizing handed to the reader pool so each reader can construct
/// its own private bucket indexes during bootstrap.
pub struct ReaderPoolConfig {
    pub data_dir: std::path::PathBuf,
    pub ephemeral_slots: usize,
    pub replaceable_slots: usize,
    pub parameterized_slots: usize,
    pub max_payload: usize,
}

pub struct ReaderPool;

impl ReaderPool {
    /// Spawn the reader pool.
    ///
    /// `req_consumers` and `index_consumers` must each have one entry per
    /// reader thread; the count of `req_consumers` determines pool size.
    pub fn spawn(
        shared: Arc<SharedState>,
        req_consumers: Vec<ReqRx>,
        sender: ServerSender,
        cfg: ReaderPoolConfig,
        index_consumers: Vec<ArcConsumer<IndexUpdate>>,
    ) -> std::io::Result<(ReaderPoolShutdown, Vec<std::thread::Thread>)> {
        assert_eq!(
            req_consumers.len(),
            index_consumers.len(),
            "req and index consumer counts must match",
        );
        let reader_threads = req_consumers.len();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(reader_threads);
        let mut thread_handles: Vec<std::thread::Thread> = Vec::with_capacity(reader_threads);
        for (i, (req_rx, index_rx)) in req_consumers
            .into_iter()
            .zip(index_consumers.into_iter())
            .enumerate()
        {
            let shared_c = Arc::clone(&shared);
            let sender_c = sender.clone();
            let sd_c = Arc::clone(&shutdown);
            let dir_c = cfg.data_dir.clone();
            let eph_slots = cfg.ephemeral_slots;
            let rep_slots = cfg.replaceable_slots;
            let par_slots = cfg.parameterized_slots;
            let max_payload = cfg.max_payload;
            let handle = std::thread::Builder::new()
                .name(format!("nostr-reader-{i}"))
                .spawn(move || {
                    reader_loop(
                        i,
                        shared_c,
                        req_rx,
                        sender_c,
                        sd_c,
                        dir_c,
                        eph_slots,
                        rep_slots,
                        par_slots,
                        max_payload,
                        index_rx,
                    );
                })?;
            thread_handles.push(handle.thread().clone());
            threads.push(handle);
        }
        Ok((
            ReaderPoolShutdown {
                flag: shutdown,
                threads,
            },
            thread_handles,
        ))
    }
}

/// Per-reader thread state: one private `BucketIndex` and one
/// `ReadOnlyLog` per bucket, plus the broadcast consumer.
struct ReaderState {
    eph_idx: BucketIndex,
    rep_idx: BucketIndex,
    par_idx: BucketIndex,
    eph_log: ReadOnlyLog,
    rep_log: ReadOnlyLog,
    par_log: ReadOnlyLog,
    index_rx: ArcConsumer<IndexUpdate>,
}

impl ReaderState {
    /// Drain any pending `IndexUpdate` messages from the broadcast ring
    /// and apply them to the local indexes. Call at the top of each REQ
    /// so the snapshot is as fresh as possible.
    fn drain_updates(&mut self) {
        while let Some(update) = self.index_rx.pop() {
            let upd: &IndexUpdate = &update;
            let idx = match upd.bucket {
                BucketKind::Ephemeral => &mut self.eph_idx,
                BucketKind::Replaceable => &mut self.rep_idx,
                BucketKind::Parameterized => &mut self.par_idx,
            };
            idx.remove_slot(upd.slot_idx);
            if let Some(meta) = upd.meta.clone() {
                idx.insert_slot(upd.slot_idx, meta);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reader_loop(
    reader_idx: usize,
    shared: Arc<SharedState>,
    req_rx: ReqRx,
    sender: ServerSender,
    shutdown: Arc<AtomicBool>,
    data_dir: std::path::PathBuf,
    ephemeral_slots: usize,
    replaceable_slots: usize,
    parameterized_slots: usize,
    max_payload: usize,
    index_rx: ArcConsumer<IndexUpdate>,
) {
    // Bootstrap: open one read-only handle per bucket and seed our local
    // BucketIndex by replaying the on-disk log. After this point the
    // storage thread may already be writing, but every committed write
    // produces an IndexUpdate that we'll apply via `drain_updates`.
    let eph_path = data_dir.join("ephemeral.log");
    let rep_path = data_dir.join("replaceable.log");
    let par_path = data_dir.join("parameterized.log");

    let state = match build_reader_state(
        &eph_path,
        &rep_path,
        &par_path,
        ephemeral_slots,
        replaceable_slots,
        parameterized_slots,
        max_payload,
        index_rx,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("reader {reader_idx}: bootstrap failed: {e}");
            return;
        }
    };
    let mut state = state;

    let gen_slot: &AtomicU64 = &shared.reader_active_gens[reader_idx];

    while !shutdown.load(Ordering::Acquire) {
        // Drain the broadcast ring before potentially parking on the
        // REQ ring. The storage thread blocks (push_with_backoff) once
        // the index ring fills, so an idle reader that hasn't drained
        // becomes a hard throughput cap on ingest. We also drain again
        // right after wake-up below, so this branch handles the
        // common case of a reader that has been awake servicing
        // REQs and now needs to catch up before the next pop_block.
        state.drain_updates();

        // Block until a REQ arrives or every shard's ReqTx has dropped.
        // pop_block uses the ring's futex-style wake bitmap, so the
        // shard side wakes us via push_block without needing the
        // explicit `reader_wakers[..].unpark()` call we used to make.
        // Shutdown is signalled by unparking the thread; pop_block's
        // timed park reacts within ~200µs.
        let Some(job) = req_rx.next() else {
            // All shards have dropped — we're shutting down.
            break;
        };

        // Drain index updates again now that we've been woken — the
        // REQ producer may have raced storage publishes that we want
        // visible before we serve the request.
        state.drain_updates();

        let g_req = shared.current_gen.load(Ordering::Acquire);
        gen_slot.store(g_req, Ordering::Release);

        let deadline = Instant::now() + REQ_TIMEOUT;
        let _timed_out = serve_req(&job, g_req, deadline, &state, &sender);

        // Release gen slot so the storage thread can advance g_floor.
        gen_slot.store(u64::MAX, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_reader_state(
    eph_path: &std::path::Path,
    rep_path: &std::path::Path,
    par_path: &std::path::Path,
    ephemeral_slots: usize,
    replaceable_slots: usize,
    parameterized_slots: usize,
    max_payload: usize,
    index_rx: ArcConsumer<IndexUpdate>,
) -> std::io::Result<ReaderState> {
    use super::bucket::{Bucket, EphemeralBucket, ParameterizedBucket, ReplaceableBucket};

    // Each reader rebuilds its own BucketIndex from disk. We open each
    // log via the read-only constructor used by the storage path so we
    // share file-handle semantics, then immediately reopen a separate
    // read-only fd for the actual scan path. The temporary `Bucket` is
    // discarded after rebuild — we keep its index, not its write-side
    // state.
    let mut eph_tmp = EphemeralBucket::open(eph_path, ephemeral_slots, max_payload)?;
    eph_tmp.rebuild()?;
    let eph_log = eph_tmp.log().reopen_readonly(eph_path)?;
    let eph_idx = std::mem::replace(
        eph_tmp.index_mut_for_handoff(),
        BucketIndex::new(ephemeral_slots),
    );

    let mut rep_tmp = ReplaceableBucket::open(rep_path, replaceable_slots, max_payload)?;
    rep_tmp.rebuild()?;
    let rep_log = rep_tmp.log().reopen_readonly(rep_path)?;
    let rep_idx = std::mem::replace(
        rep_tmp.index_mut_for_handoff(),
        BucketIndex::new(replaceable_slots),
    );

    let mut par_tmp = ParameterizedBucket::open(par_path, parameterized_slots, max_payload)?;
    par_tmp.rebuild()?;
    let par_log = par_tmp.log().reopen_readonly(par_path)?;
    let par_idx = std::mem::replace(
        par_tmp.index_mut_for_handoff(),
        BucketIndex::new(parameterized_slots),
    );

    Ok(ReaderState {
        eph_idx,
        rep_idx,
        par_idx,
        eph_log,
        rep_log,
        par_log,
        index_rx,
    })
}

fn serve_req(
    job: &ReqJob,
    g_req: u64,
    deadline: Instant,
    state: &ReaderState,
    sender: &ServerSender,
) -> bool {
    // Aggregate cumulative limit across all filters.
    let mut total_limit: Option<usize> = None;
    let mut per_filter_limit: Vec<Option<usize>> = Vec::with_capacity(job.filters.len());
    for f in job.filters.iter() {
        per_filter_limit.push(f.limit.map(|n| n as usize));
        if let Some(n) = f.limit {
            total_limit = Some(total_limit.unwrap_or(0) + n as usize);
        }
    }

    let mut emitted: usize = 0;
    let mut seen_ids: std::collections::HashSet<[u8; 32]> =
        std::collections::HashSet::with_capacity(64);

    let mut matches: Vec<EmitEntry> = Vec::new();

    for (filter_idx, filter) in job.filters.iter().enumerate() {
        if Instant::now() >= deadline {
            sender.send_text(job.client_fd, protocol::closed(&job.sub_id, "timeout"));
            return true;
        }
        let limit = per_filter_limit[filter_idx];

        scan_bucket(
            BucketRef::Ephemeral,
            &state.eph_idx,
            filter,
            g_req,
            limit,
            &mut matches,
            &mut seen_ids,
        );
        scan_bucket(
            BucketRef::Replaceable,
            &state.rep_idx,
            filter,
            g_req,
            limit,
            &mut matches,
            &mut seen_ids,
        );
        scan_bucket(
            BucketRef::Parameterized,
            &state.par_idx,
            filter,
            g_req,
            limit,
            &mut matches,
            &mut seen_ids,
        );
    }

    // Sort oldest-first (lowest created_at first, ties broken by event_id).
    matches.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });

    for entry in matches {
        if Instant::now() >= deadline {
            sender.send_text(job.client_fd, protocol::closed(&job.sub_id, "timeout"));
            return true;
        }
        if let Some(limit) = total_limit
            && emitted >= limit
        {
            break;
        }
        let log = match entry.bucket {
            BucketRef::Ephemeral => &state.eph_log,
            BucketRef::Replaceable => &state.rep_log,
            BucketRef::Parameterized => &state.par_log,
        };
        let payload = log.read_payload(entry.slot_idx as usize, entry.payload_len);
        let Ok(bytes) = payload else { continue };

        // Validate seq hasn't changed mid-scan (storage rewrote the slot
        // despite the gen gate — shouldn't happen, but defensive).
        if let Some(slot_head) = log.read_slot(entry.slot_idx as usize).ok().flatten()
            && slot_head.0.seq != entry.seq
        {
            continue; // raced; the slot was overwritten
        }

        sender.send_event_frame(
            job.client_fd,
            Arc::clone(&job.sub_id),
            Arc::from(bytes.into_boxed_slice()),
        );
        emitted += 1;
    }

    sender.send_text(job.client_fd, protocol::eose(&job.sub_id));
    false
}

#[derive(Clone, Copy)]
enum BucketRef {
    Ephemeral,
    Replaceable,
    Parameterized,
}

struct EmitEntry {
    bucket: BucketRef,
    slot_idx: u32,
    seq: std::num::NonZeroU64,
    payload_len: u32,
    created_at: i64,
    event_id: [u8; 32],
}

fn scan_bucket(
    which: BucketRef,
    index: &BucketIndex,
    filter: &nostro2::NostrSubscription,
    g_req: u64,
    filter_limit: Option<usize>,
    out: &mut Vec<EmitEntry>,
    seen_ids: &mut std::collections::HashSet<[u8; 32]>,
) {
    let candidates: Vec<(u32, SlotMeta)> = index
        .candidates(filter)
        .into_iter()
        .filter_map(|i| index.meta[i as usize].as_ref().map(|m| (i, m.clone())))
        .collect();

    let mut filtered: Vec<(u32, SlotMeta)> = candidates
        .into_iter()
        .filter(|(_, m)| m.generation <= g_req && m.matches(filter))
        .collect();
    filtered.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));
    if let Some(limit) = filter_limit {
        filtered.truncate(limit);
    }

    for (slot_idx, meta) in filtered {
        if !seen_ids.insert(meta.event_id) {
            continue;
        }
        out.push(EmitEntry {
            bucket: which,
            slot_idx,
            seq: meta.seq,
            payload_len: meta.payload_len,
            created_at: meta.created_at,
            event_id: meta.event_id,
        });
    }
}
