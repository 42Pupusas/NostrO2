//! Storage thread: owns all three buckets and their log files.
//!
//! Responsibilities:
//! - Drain all per-shard write rings round-robin.
//! - Parse each `WriteReq` into a payload the bucket can ingest.
//! - Dispatch to the correct bucket (by `BucketKind`).
//! - Stage writes refused due to g_floor, retry them on subsequent passes.
//! - Bump `current_gen` at batch boundaries; fsync on a timer.
//! - Publish live updates to the shared `IndexSnapshot` so reader threads
//!   can pick them up.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nostro2::NostrNoteView;

use super::StorageConfig;
use super::bucket::{
    Bucket, EphemeralBucket, EventPayload, ParameterizedBucket, ReplaceableBucket, WriteOutcome,
};
use super::handle::{WriteReq, WriteRx};
use super::slot::BucketKind;

/// The shared, reader-accessible view of all bucket indexes. Readers hold
/// an `Arc` and scan it; the storage thread mutates through `&mut` on its
/// owned copy (see below — this is a simplification for v1: one `RwLock`
/// over the whole state, upgrade to `arc-swap` or finer-grained CoW if
/// profiles demand).
///
/// The generational CoW rule we designed is still enforced on eviction:
/// the storage thread refuses to overwrite a slot whose `gen >= g_floor`.
/// Readers therefore never observe *in-flight* mutation on a slot they
/// care about. The `RwLock` here protects the *structure* (hashmaps
/// resizing, vec grows) — the gen gate protects the *content*.
pub struct SharedState {
    pub ephemeral: std::sync::RwLock<EphemeralBucket>,
    pub replaceable: std::sync::RwLock<ReplaceableBucket>,
    pub parameterized: std::sync::RwLock<ParameterizedBucket>,
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
/// and hands out `StorageHandle` clones to shards.
pub struct StorageEngine {
    pub shared: Arc<SharedState>,
}

impl StorageEngine {
    /// Open buckets + spawn the storage thread. Returns the shutdown
    /// controller and the shared state handle.
    pub fn spawn(
        config: &StorageConfig,
        reader_threads: usize,
        write_rxs: Vec<WriteRx>,
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
        let next_seq = initial_next_seq(&eph, &rep, &par);
        // current_gen must start at > any persisted gen so reopens don't
        // render old slots "from the future."
        let initial_gen = initial_next_gen(&eph, &rep, &par);

        let reader_active_gens: Arc<[AtomicU64]> = (0..reader_threads.max(1))
            .map(|_| AtomicU64::new(u64::MAX))
            .collect();

        let shared = Arc::new(SharedState {
            ephemeral: std::sync::RwLock::new(eph),
            replaceable: std::sync::RwLock::new(rep),
            parameterized: std::sync::RwLock::new(par),
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
                    shutdown_for_thread,
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

fn initial_next_seq(
    eph: &EphemeralBucket,
    rep: &ReplaceableBucket,
    par: &ParameterizedBucket,
) -> u64 {
    let max_from = |b: &dyn Bucket| -> u64 {
        b.index()
            .meta
            .iter()
            .flatten()
            .map(|m| m.seq.get())
            .max()
            .unwrap_or(0)
    };
    max_from(eph).max(max_from(rep)).max(max_from(par))
}

fn initial_next_gen(
    eph: &EphemeralBucket,
    rep: &ReplaceableBucket,
    par: &ParameterizedBucket,
) -> u64 {
    let max_from = |b: &dyn Bucket| -> u64 {
        b.index()
            .meta
            .iter()
            .flatten()
            .map(|m| m.generation)
            .max()
            .unwrap_or(0)
    };
    max_from(eph)
        .max(max_from(rep))
        .max(max_from(par))
        .saturating_add(1)
}

fn storage_loop(
    shared: Arc<SharedState>,
    mut write_rxs: Vec<WriteRx>,
    shutdown: Arc<AtomicBool>,
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
                    do_fsync(&shared);
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
        staged.retain_mut(|sw| ingest_one(&shared, sw, generation, g_floor, &mut next_seq));

        // Then ingest the fresh batch.
        for req in batch.drain(..) {
            let mut sw = StagedWrite { req };
            if ingest_one(&shared, &mut sw, generation, g_floor, &mut next_seq) {
                staged.push(sw);
            }
        }

        // Commit this batch by bumping the generation so readers can see it.
        shared.current_gen.fetch_add(1, Ordering::AcqRel);

        // Opportunistic fsync.
        if let Some(interval) = fsync_interval
            && last_fsync.elapsed() >= interval
        {
            do_fsync(&shared);
            last_fsync = Instant::now();
        }
    }

    // Final flush on shutdown.
    do_fsync(&shared);
}

struct StagedWrite {
    req: WriteReq,
}

/// Try to ingest one write. Returns `true` if it should be re-staged (
/// stalled by g_floor), `false` if committed or dropped.
fn ingest_one(
    shared: &SharedState,
    sw: &mut StagedWrite,
    generation: u64,
    g_floor: u64,
    next_seq: &mut u64,
) -> bool {
    let req = &sw.req;
    // Parse into a view against the raw JSON.
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

    let outcome = match BucketKind::classify(req.kind) {
        BucketKind::Ephemeral => {
            let mut b = shared.ephemeral.write().unwrap();
            b.try_write(&payload, generation, g_floor, next_seq)
        }
        BucketKind::Replaceable => {
            let mut b = shared.replaceable.write().unwrap();
            b.try_write(&payload, generation, g_floor, next_seq)
        }
        BucketKind::Parameterized => {
            let mut b = shared.parameterized.write().unwrap();
            b.try_write(&payload, generation, g_floor, next_seq)
        }
    };

    matches!(outcome, WriteOutcome::Stalled)
}

fn do_fsync(shared: &SharedState) {
    if let Ok(b) = shared.ephemeral.read() {
        let _ = b.log().fsync();
    }
    if let Ok(b) = shared.replaceable.read() {
        let _ = b.log().fsync();
    }
    if let Ok(b) = shared.parameterized.read() {
        let _ = b.log().fsync();
    }
}
