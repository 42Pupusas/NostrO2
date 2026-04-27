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
use std::time::{Duration, Instant};

use nostro2::NostrNoteView;
use quetzalcoatl::broadcast::arc::ArcProducer;

use super::StorageConfig;
use super::bucket::{
    Bucket, EphemeralBucket, EventPayload, ParameterizedBucket, ReplaceableBucket, WriteOutcome,
};
use super::handle::{IndexUpdate, WriteReq, WriteRx};
use super::slot::BucketKind;

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
    pub fn spawn(
        config: &StorageConfig,
        reader_threads: usize,
        write_rxs: Vec<WriteRx>,
        index_tx: ArcProducer<IndexUpdate>,
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
                    write_rxs,
                    index_tx,
                    shutdown_for_thread,
                    eph,
                    rep,
                    par,
                    next_seq,
                    fsync_interval,
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
    mut write_rxs: Vec<WriteRx>,
    index_tx: ArcProducer<IndexUpdate>,
    shutdown: Arc<AtomicBool>,
    mut eph: EphemeralBucket,
    mut rep: ReplaceableBucket,
    mut par: ParameterizedBucket,
    mut next_seq: u64,
    fsync_interval: Option<Duration>,
) {
    let mut last_fsync = Instant::now();
    let mut staged: Vec<StagedWrite> = Vec::new();
    let mut batch: Vec<WriteReq> = Vec::with_capacity(1024);

    while !shutdown.load(Ordering::Acquire) {
        batch.clear();

        // Drain each shard's write ring. quetzalcoatl SPSC doesn't expose a
        // `drain` directly on consumer, so pop until empty.
        for rx in &mut write_rxs {
            while let Some(req) = rx.0.pop() {
                batch.push(req);
                if batch.len() >= 1024 {
                    break;
                }
            }
            if batch.len() >= 1024 {
                break;
            }
        }

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
/// pushes an `IndexUpdate` so reader threads see the new slot.
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
) -> bool {
    let req = &sw.req;
    let note: NostrNoteView<'_> = match serde_json::from_slice(&req.raw_json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let payload = EventPayload {
        note: &note,
        raw_json: req.raw_json.as_ref(),
        event_id: req.event_id,
        pubkey: req.pubkey,
    };

    let bucket_kind = BucketKind::classify(req.kind);
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
            false
        }
        WriteOutcome::Stalled => true,
        WriteOutcome::Duplicate | WriteOutcome::TooBig => false,
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
