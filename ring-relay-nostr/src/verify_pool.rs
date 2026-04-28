//! Schnorr-verify offload pool.
//!
//! Profiling showed >80% of CPU on the fan-out hot path was spent inside
//! k256's schnorr verify, called inline from the shard I/O thread. That
//! pinned each shard to a single core's worth of crypto throughput. This
//! module moves verify to dedicated worker threads so the shard becomes
//! pure I/O + match + dispatch.
//!
//! ## Topology (M workers per shard)
//!
//! Per shard, with `verify_threads_per_shard = M`:
//!
//! - **One jobs SPMC** (single producer = the shard, M consumers = the
//!   workers). Workers compete via CAS on the consumer head; whichever
//!   worker is least busy pops next. Self-balancing — a slow worker
//!   simply pulls less, no jobs get stuck behind it.
//! - **One results MPSC** (M producers cloned to workers, single
//!   consumer on the shard). Workers push verdicts back; the shard
//!   drains the ring at the top of every loop iteration.
//!
//! ## Why SPMC over per-worker SPSCs
//!
//! The earlier design used M SPSCs (one per worker) plus a shard-side
//! round-robin cursor with a full-sweep retry on push. That suffered
//! from head-of-line blocking: a worker stalled in a page fault or a
//! GC-induced pause left its queued jobs stranded until it woke, while
//! the round-robin push had to walk all M producer head pointers on
//! every backoff iteration. SPMC collapses this to a single ring with
//! consumer-side CAS distribution — natural load-balancing, less code,
//! one cache line for the head pointer instead of M.
//!
//! ## Wakeup discipline
//!
//! On push, the shard unparks **one** worker (round-robin hint). The
//! woken worker pops; if more jobs remain it unparks the next worker
//! before processing. This "torch-passing" keeps at most one extra
//! worker awake when load is low, while still parallelising under
//! backlog. Workers fall back to `park_timeout(10ms)` as a safety net
//! against missed unparks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nostro2::NostrNoteView;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{
    Consumer as MpscConsumer, Producer as MpscProducer, RingBuffer as MpscRingBuffer,
};
use quetzalcoatl::spmc::{
    Consumer as SpmcConsumer, Producer as SpmcProducer, RingBuffer as SpmcRingBuffer,
};

/// Capacity for the per-shard MPSC results ring (workers → shard).
/// 1024 verdicts at ~96 B each = ~96 KiB per ring — cheap.
const RESULTS_RING_CAPACITY: usize = 1024;

/// Capacity for the per-shard SPMC jobs ring (shard → workers). Sized
/// to match the aggregate buffering the previous M-SPSC design provided
/// (M workers × 1024 per-worker rings = 12288 jobs). With a single
/// shared SPMC ring we lose the per-consumer headroom, so we size up
/// here to keep the shard's `push_verify_job` from spinning into
/// `std::thread::sleep` under burst — which would prevent the shard
/// from draining the results ring and livelock the pipeline.
const JOBS_RING_CAPACITY: usize = 12 * 1024;

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
}

/// Per-shard handle. The shard owns the single SPMC jobs producer, the
/// MPSC results consumer, and the worker thread handles for targeted
/// `unpark`.
pub struct VerifyHandle {
    /// Single SPMC jobs producer. All workers for this shard share the
    /// matching consumer (cloned once per worker at spawn time).
    pub jobs_tx: SpmcProducer<VerifyJob>,
    pub results_rx: MpscConsumer<VerifyResult>,
    pub worker_threads: Vec<std::thread::Thread>,
    /// Round-robin hint for which worker to unpark on push. Any worker
    /// can pop any job, so this is purely a wakeup-fairness knob; if
    /// the chosen worker is busy, the next push (or the worker's own
    /// torch-pass on its way out) will wake another.
    pub next_wake_hint: usize,
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

/// Spin up `num_shards * verify_threads_per_shard` worker threads.
/// Returns one [`VerifyHandle`] per shard plus a shutdown handle.
///
/// `verify_threads_per_shard` is clamped to ≥ 1 so callers that pass 0
/// don't end up with shards that have no workers.
pub fn spawn(
    num_shards: usize,
    verify_threads_per_shard: usize,
) -> (Vec<VerifyHandle>, VerifyPoolShutdown) {
    let workers_per = verify_threads_per_shard.max(1);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(num_shards);
    let mut threads = Vec::with_capacity(num_shards * workers_per);

    for shard_idx in 0..num_shards {
        // One results ring per shard, MPSC: every worker for this shard
        // gets a clone of the producer; the shard owns the consumer.
        let (results_seed_tx, results_rx) =
            MpscRingBuffer::<VerifyResult>::new(Capacity::at_least(RESULTS_RING_CAPACITY)).split();

        // One jobs ring per shard, SPMC: shard owns the (non-cloneable)
        // producer; each worker gets a clone of the consumer and competes
        // for slots via CAS. Self-balancing across workers.
        let (jobs_tx, jobs_rx_seed) =
            SpmcRingBuffer::<VerifyJob>::new(Capacity::at_least(JOBS_RING_CAPACITY)).split();

        // Shared handle the shard fills in once it starts running.
        // Workers read from it to unpark the shard after pushing a
        // result, so a shard that's epoll-parked between TCP frames
        // still wakes promptly when a verdict lands.
        let shard_waker: Arc<OnceLock<std::thread::Thread>> = Arc::new(OnceLock::new());

        let mut worker_threads = Vec::with_capacity(workers_per);
        let mut spawned: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(workers_per);

        for worker_idx in 0..workers_per {
            // Clone the SPMC consumer once per worker (the seed handle
            // was returned from split()).
            let jobs_rx = jobs_rx_seed.clone();
            let results_tx = results_seed_tx.clone();
            let waker_for_worker = Arc::clone(&shard_waker);

            let shutdown_for_thread = Arc::clone(&shutdown);
            let thread = std::thread::Builder::new()
                .name(format!("nostr-verify-{shard_idx}-{worker_idx}"))
                .spawn(move || {
                    worker_loop(shutdown_for_thread, jobs_rx, results_tx, waker_for_worker);
                })
                .expect("spawn verify worker");
            worker_threads.push(thread.thread().clone());
            spawned.push(thread);
        }

        for h in spawned {
            threads.push(h);
        }

        // Drop the seed handles so producer/consumer counts match the
        // intended populations: producer = 1 (the shard, captured below);
        // consumers = workers_per; mpsc producers = workers_per.
        drop(jobs_rx_seed);
        drop(results_seed_tx);

        handles.push(VerifyHandle {
            jobs_tx,
            results_rx,
            worker_threads,
            next_wake_hint: 0,
            shard_waker,
        });
    }

    (
        handles,
        VerifyPoolShutdown {
            flag: shutdown,
            threads,
        },
    )
}

/// Verify worker. Drains the jobs ring; for each job, re-parses the
/// JSON, runs the full id+schnorr `verify()`, and pushes the verdict
/// onto the shard's shared MPSC results ring. Re-parsing the JSON each
/// pass costs a few microseconds — dwarfed by the ~50µs schnorr verify
/// it enables to run off the I/O thread.
fn worker_loop(
    shutdown: Arc<AtomicBool>,
    jobs_rx: SpmcConsumer<VerifyJob>,
    results_tx: MpscProducer<VerifyResult>,
    shard_waker: Arc<OnceLock<std::thread::Thread>>,
) {
    while !shutdown.load(Ordering::Acquire) {
        // SPMC has no pop_block in 0.8 — keep the park-poll fallback,
        // but check shutdown between pops so a shutdown-unpark wakes
        // us promptly.
        let Some(job) = jobs_rx.pop() else {
            std::thread::park_timeout(Duration::from_millis(10));
            continue;
        };

        // No torch-pass: with the batched-CAS SPMC consumer, `is_empty()`
        // can read false negatives because each consumer claims up to 32
        // slots into a thread-local cursor before re-touching `head`. A
        // peer-wake based on `is_empty()` would silently skip when the
        // claiming worker still has 31 queued items, leaving peers
        // parked. Wakeups instead come solely from the shard's per-push
        // round-robin unpark, which is sufficient because the batched
        // claim already amortizes contention.

        let verified = match serde_json::from_slice::<NostrNoteView<'_>>(&job.raw_json) {
            Ok(view) => view.verify(),
            // If parse fails on the worker side, the shard already
            // accepted the parse (we got here from a successful
            // parse_view). Treat as verify-failure to be safe.
            Err(_) => false,
        };

        let result = VerifyResult {
            raw_json: job.raw_json,
            client_id: job.client_id,
            event_id_hex: job.event_id_hex,
            event_id: job.event_id,
            pubkey: job.pubkey,
            kind: job.kind,
            verified,
        };

        // Backpressure: park on the results ring if the shard hasn't
        // drained yet. push_block uses the mpsc futex-style wake bitmap
        // and bails out (Err) only when the shard's consumer drops —
        // which means we're shutting down. Discard quietly in that case.
        let _ = results_tx.push_block(result);

        // Wake the shard so it drains the result promptly. Without
        // this an epoll-parked shard would only see the verdict on
        // the next inbound TCP frame, dragging end-to-end latency.
        if let Some(t) = shard_waker.get() {
            t.unpark();
        }
    }
}
