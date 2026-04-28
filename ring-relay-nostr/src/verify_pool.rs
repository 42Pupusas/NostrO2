//! Schnorr-verify offload pool.
//!
//! Profiling showed >80% of CPU on the fan-out hot path was spent inside
//! k256's schnorr verify, called inline from the shard I/O thread. That
//! pinned each shard to a single core's worth of crypto throughput. This
//! module moves verify to dedicated worker threads so the shard becomes
//! pure I/O + match + dispatch.
//!
//! ## Topology (one global pool of W workers)
//!
//! - **One global MPMC jobs ring** (N shard producers, W worker
//!   consumers). Whichever shard is busiest pulls more verify capacity
//!   automatically — no static partition. Same MPMC primitive the
//!   storage REQ ring uses, so `push_block`/`pop_block` futex-style
//!   wakeups handle backpressure without us hand-rolling unpark
//!   logic.
//! - **N MPSC results rings**, one per shard. Workers stamp the source
//!   shard onto the verdict and push into that shard's MPSC. The shard
//!   only drains its own ring.
//!
//! ### Why global over per-shard
//!
//! The previous design gave each shard its own SPMC + M workers per
//! shard. Two failure modes:
//!
//! 1. **Static partition.** With shards=4 and cpus=8, each shard got
//!    exactly 2 workers. A burst on one shard couldn't pull workers
//!    from idle peers — verify capacity was capped at 2× regardless of
//!    free CPU.
//! 2. **Auto-mode collapse.** `verify_threads_per_shard = 0` divided
//!    `cpus / shards`, so at high N the per-shard count went to 1,
//!    defeating the offload entirely.
//!
//! A single global pool sidesteps both: total capacity = W workers
//! regardless of N, and any shard can saturate the whole pool. The
//! `write_ring_topology` bench already validated MPSC/MPMC for high
//! producer count beats N×SPSC; the same shape applies to N shards
//! feeding one verify pool.
//!
//! ## Wakeup discipline
//!
//! `push_block` / `pop_block` on `quetzalcoatl::mpmc` use a futex-style
//! wake bitmap, so we don't need explicit `unpark` on push or pop. The
//! results-side wake (worker → shard) still needs an explicit unpark
//! because the shard parks on its `ReaderCore` epoll, not on the
//! results ring. Workers unpark the shard's thread (filled in via
//! `shard_waker: OnceLock`) after each result push.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use nostro2::NostrNoteView;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpmc::{
    Consumer as MpmcConsumer, Producer as MpmcProducer, RingBuffer as MpmcRingBuffer,
};
use quetzalcoatl::mpsc::{
    Consumer as MpscConsumer, Producer as MpscProducer, RingBuffer as MpscRingBuffer,
};

/// Capacity for each per-shard MPSC results ring (workers → shard).
/// 1024 verdicts at ~96 B each = ~96 KiB per ring — cheap.
const RESULTS_RING_CAPACITY: usize = 1024;

/// Capacity for the global MPMC jobs ring (shards → workers). Sized to
/// absorb cross-shard bursts: enough head-room that a burst on one
/// shard doesn't block other shards' pushes if the workers happen to
/// be saturated. The previous per-shard SPMC was 12K slots × N shards
/// in aggregate; one global ring sized to a flat 16K achieves the same
/// headroom for the typical N ≤ 16.
const JOBS_RING_CAPACITY: usize = 16 * 1024;

/// One EVENT queued for verify. Owned because it must cross threads;
/// the shard parses the inbound frame as a borrowed view, then promotes
/// the JSON substring to an `Arc<[u8]>` so the worker can re-parse.
pub struct VerifyJob {
    /// Raw JSON bytes of the EVENT object (the substring captured via
    /// serde_json::RawValue at parse time).
    pub raw_json: Arc<[u8]>,
    /// fd of the client that sent this EVENT — passed back so the shard
    /// knows where to send OK / fan-out.
    pub client_id: i32,
    /// Event id, hex-decoded from the parsed view. The shard already
    /// produced this; no point reparsing on the worker.
    pub event_id_hex: Arc<str>,
    /// Pre-decoded event id bytes. Shared with the post-verify path so
    /// the storage write request doesn't have to redo hex decoding.
    pub event_id: [u8; 32],
    /// Pre-decoded pubkey bytes, same rationale.
    pub pubkey: [u8; 32],
    /// Event kind. Used by the storage path to pick the bucket.
    pub kind: u32,
    /// Source shard index. The verify worker uses this to route the
    /// result back to the originating shard's MPSC results ring.
    pub source_shard: u16,
}

/// Owned, send-able snapshot of the EVENT fields the live fan-out
/// matcher actually reads. Built once on the verify worker (which
/// already owns a parsed `NostrNoteView`) and shared via `Arc` to every
/// fan-out match so the shard never re-parses the JSON.
///
/// Tag rows are stored as a single flat `Box<[Box<str>]>` plus an
/// offsets table — same layout as `nostro2::TagsView` but owned, so
/// `iter_tags` walks contiguous memory without per-row indirection.
pub struct MatchView {
    /// Hex-encoded event id. Shares ownership with
    /// `VerifyResult::event_id_hex`.
    pub id: Arc<str>,
    /// Hex-encoded pubkey. Owned because filter matching needs it as
    /// a `&str` and the parsed view's pubkey was borrowed from the
    /// raw JSON.
    pub pubkey: Arc<str>,
    pub kind: u32,
    pub created_at: i64,
    /// Flattened tag cells, row-major.
    pub tag_cells: Box<[Box<str>]>,
    /// Row offsets: `tag_offsets[i]..tag_offsets[i+1]` covers row `i`.
    /// Always has `rows + 1` entries (closing sentinel).
    pub tag_offsets: Box<[u32]>,
}

impl MatchView {
    /// Iterate tag rows as `&[Box<str>]` slices. Walks the offsets
    /// table directly so there is no per-row bounds check.
    pub fn iter_tags(&self) -> impl Iterator<Item = &[Box<str>]> {
        self.tag_offsets
            .windows(2)
            .map(|w| &self.tag_cells[w[0] as usize..w[1] as usize])
    }
}

/// Verdict returned to the shard. Carries the original payload back so
/// the shard's post-verify path doesn't have to look anything up.
pub struct VerifyResult {
    pub raw_json: Arc<[u8]>,
    pub client_id: i32,
    pub event_id_hex: Arc<str>,
    pub event_id: [u8; 32],
    pub pubkey: [u8; 32],
    pub kind: u32,
    /// `true` iff `note.verify()` succeeded: id matches sha256 of the
    /// canonical serialization AND the schnorr signature is valid.
    pub verified: bool,
    /// Owned matcher snapshot built on the worker. `None` only when
    /// `verified == false` (no point spending allocation on a rejected
    /// event).
    pub view: Option<Arc<MatchView>>,
}

/// Per-shard handle. Each shard gets its own clone of the global MPMC
/// jobs producer plus its own MPSC results consumer. With MPMC
/// `push_block` the handle no longer carries worker-thread Vecs or
/// per-push wakeup hints — the futex-style wake bitmap inside the
/// MPMC handles wakeups for us.
pub struct VerifyHandle {
    /// Shared global MPMC jobs producer. Each shard holds a clone;
    /// every clone has its own batch reservation cell.
    pub jobs_tx: MpmcProducer<VerifyJob>,
    pub results_rx: MpscConsumer<VerifyResult>,
    /// Shared handle workers use to wake the shard thread when a
    /// verify result lands. Filled in by the shard's `run_shard`
    /// before its main loop starts. Without this, a shard parked on
    /// its `ReaderCore` epoll wait would not see verify results
    /// until the next inbound I/O event arrived — the verdict could
    /// sit on the ring for tens of ms in low-traffic conditions,
    /// dragging end-to-end latency.
    pub shard_waker: Arc<OnceLock<std::thread::Thread>>,
}

/// Owned by the relay; joins worker threads on drop.
pub struct VerifyPoolShutdown {
    flag: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl VerifyPoolShutdown {
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

/// Spin up `total_verify_threads` worker threads sharing one global
/// MPMC jobs ring. Returns one [`VerifyHandle`] per shard plus a
/// shutdown handle.
///
/// `total_verify_threads` is clamped to ≥ 1.
pub fn spawn(
    num_shards: usize,
    total_verify_threads: usize,
) -> (Vec<VerifyHandle>, VerifyPoolShutdown) {
    let total_workers = total_verify_threads.max(1);
    let shutdown = Arc::new(AtomicBool::new(false));

    // One global MPMC jobs ring. Producer seed is cloned once per
    // shard; consumer seed is cloned once per worker. Drop both seeds
    // afterwards so producer/consumer counts match the live populations.
    let (jobs_tx_seed, jobs_rx_seed) =
        MpmcRingBuffer::<VerifyJob>::new(Capacity::at_least(JOBS_RING_CAPACITY)).split();

    // One MPSC results ring per shard. Each worker holds a producer
    // clone for every shard so it can route a result by `source_shard`.
    let mut results_consumers: Vec<MpscConsumer<VerifyResult>> = Vec::with_capacity(num_shards);
    let mut results_producer_seeds: Vec<MpscProducer<VerifyResult>> =
        Vec::with_capacity(num_shards);
    let mut shard_wakers: Vec<Arc<OnceLock<std::thread::Thread>>> = Vec::with_capacity(num_shards);
    for _ in 0..num_shards {
        let (tx_seed, rx) =
            MpscRingBuffer::<VerifyResult>::new(Capacity::at_least(RESULTS_RING_CAPACITY)).split();
        results_consumers.push(rx);
        results_producer_seeds.push(tx_seed);
        shard_wakers.push(Arc::new(OnceLock::new()));
    }

    // Per-worker state: clone the shared jobs consumer, clone every
    // shard's results producer + waker. The clones each carry their
    // own batch reservation cell so workers don't serialize on a
    // shared producer state.
    let mut worker_handles: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(total_workers);
    for worker_idx in 0..total_workers {
        let jobs_rx = jobs_rx_seed.clone();
        let results_txs: Vec<MpscProducer<VerifyResult>> = results_producer_seeds.to_vec();
        let wakers: Vec<Arc<OnceLock<std::thread::Thread>>> =
            shard_wakers.iter().map(Arc::clone).collect();
        let shutdown_for_thread = Arc::clone(&shutdown);
        let thread = std::thread::Builder::new()
            .name(format!("nostr-verify-{worker_idx}"))
            .spawn(move || {
                worker_loop(shutdown_for_thread, jobs_rx, results_txs, wakers);
            })
            .expect("spawn verify worker");
        worker_handles.push(thread);
    }

    // Drop the seed handles so producer/consumer counts match the live
    // populations exactly:
    //   - jobs producers = num_shards (one clone per shard)
    //   - jobs consumers = total_workers (one clone per worker)
    //   - results producers = total_workers per shard (one clone per worker)
    //   - results consumers = 1 per shard (held by the shard)
    drop(jobs_rx_seed);
    for tx in results_producer_seeds {
        drop(tx);
    }

    // Build the per-shard handles by zipping the consumers, wakers,
    // and one clone of the jobs producer.
    let mut handles: Vec<VerifyHandle> = Vec::with_capacity(num_shards);
    for (results_rx, shard_waker) in results_consumers.into_iter().zip(shard_wakers.into_iter()) {
        handles.push(VerifyHandle {
            jobs_tx: jobs_tx_seed.clone(),
            results_rx,
            shard_waker,
        });
    }
    drop(jobs_tx_seed);

    (
        handles,
        VerifyPoolShutdown {
            flag: shutdown,
            threads: worker_handles,
        },
    )
}

/// Verify worker. Drains the global jobs ring; for each job, parses
/// the JSON, runs the full id+schnorr `verify()`, builds a `MatchView`
/// snapshot on success, and pushes the verdict into the source shard's
/// MPSC results ring.
fn worker_loop(
    shutdown: Arc<AtomicBool>,
    jobs_rx: MpmcConsumer<VerifyJob>,
    results_txs: Vec<MpscProducer<VerifyResult>>,
    shard_wakers: Vec<Arc<OnceLock<std::thread::Thread>>>,
) {
    while !shutdown.load(Ordering::Acquire) {
        // pop_block parks on the futex-style wake bitmap; returns None
        // only when every producer has dropped, which means the relay
        // is shutting down.
        let Some(job) = jobs_rx.pop_block() else {
            break;
        };

        // Parse once. On success: verify schnorr+id, then if valid build
        // the owned MatchView so the shard's matcher loop never re-parses.
        // We elide the MatchView build for verify-failure cases — those
        // events get rejected, no fan-out happens.
        let (verified, view) = match serde_json::from_slice::<NostrNoteView<'_>>(&job.raw_json) {
            Ok(view) => {
                if view.verify() {
                    (true, Some(Arc::new(snapshot_match_view(&view, &job.event_id_hex))))
                } else {
                    (false, None)
                }
            }
            // If parse fails on the worker side, the shard already
            // accepted the parse (we got here from a successful
            // parse_view). Treat as verify-failure to be safe.
            Err(_) => (false, None),
        };

        let shard_idx = job.source_shard as usize;
        let result = VerifyResult {
            raw_json: job.raw_json,
            client_id: job.client_id,
            event_id_hex: job.event_id_hex,
            event_id: job.event_id,
            pubkey: job.pubkey,
            kind: job.kind,
            verified,
            view,
        };

        // Backpressure: park on the source shard's results ring if it
        // hasn't drained yet. push_block uses the mpsc futex-style
        // wake bitmap and bails out only when the shard's consumer
        // drops — which means the shard is gone (shutdown). Discard
        // quietly in that case.
        let _ = results_txs[shard_idx].push_block(result);

        // Wake the source shard so it drains the result promptly.
        // Without this, an epoll-parked shard would only see the
        // verdict on the next inbound TCP frame, dragging end-to-end
        // latency.
        if let Some(t) = shard_wakers[shard_idx].get() {
            t.unpark();
        }
    }
}

/// Convert a borrowed `NostrNoteView` into the owned, send-able
/// matcher snapshot. Runs on the verify worker after a successful
/// `view.verify()` (or inline on the shard for the legacy fallback
/// path), so the cost is amortized against ~50 µs of schnorr math —
/// the extra microsecond of allocation is invisible alongside it.
pub(crate) fn snapshot_match_view(view: &NostrNoteView<'_>, event_id_hex: &Arc<str>) -> MatchView {
    // Flatten tag rows into one cell vec + offsets table. Mirrors
    // `nostro2::TagsView`'s layout but owned (Box<str>) so the matcher
    // can iterate without touching the borrowed Cow source.
    let row_count = view.tags.len();
    let mut tag_cells: Vec<Box<str>> = Vec::with_capacity(row_count * 2);
    let mut tag_offsets: Vec<u32> = Vec::with_capacity(row_count + 1);
    tag_offsets.push(0);
    for row in view.tags.iter() {
        for cell in row {
            tag_cells.push(Box::<str>::from(cell.as_ref()));
        }
        tag_offsets.push(tag_cells.len() as u32);
    }

    MatchView {
        id: Arc::clone(event_id_hex),
        pubkey: Arc::<str>::from(view.pubkey.as_ref()),
        kind: view.kind,
        created_at: view.created_at,
        tag_cells: tag_cells.into_boxed_slice(),
        tag_offsets: tag_offsets.into_boxed_slice(),
    }
}
