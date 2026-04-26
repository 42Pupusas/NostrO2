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
//! - **M jobs SPSCs** (one per worker). Shard owns all M producers and
//!   round-robins pushes across them. Each worker owns its consumer.
//! - **One results MPSC** (M producers cloned to workers, single
//!   consumer on the shard). Workers push verdicts back; the shard
//!   drains the ring at the top of every loop iteration.
//!
//! Why this shape? `quetzalcoatl` ships SPSC, MPSC, SPMC, and broadcast
//! — but no MPMC, so we can't have a single shared jobs queue served by
//! many workers. The per-worker SPSC + shared MPSC results combo is the
//! cleanest fit and lets each shard scale its verify rate up to M cores
//! independent of the I/O shard count.
//!
//! ## Round-robin distribution
//!
//! The shard increments a `next_worker` cursor mod M for each push. If
//! a particular worker's ring is full it backs off briefly before
//! retrying — but in practice all workers drain at the same rate so
//! the rings stay nearly empty.

use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nostro2::NostrNoteView;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{
    Consumer as MpscConsumer, Producer as MpscProducer, RingBuffer as MpscRingBuffer,
};
use quetzalcoatl::spsc::{
    Consumer as SpscConsumer, Producer as SpscProducer, RingBuffer as SpscRingBuffer,
};

/// Default capacity for both the jobs and results rings, per shard.
/// Sized large enough to absorb a burst from a single publisher's
/// connection without backpressuring the shard's frame parser. 1024 jobs
/// at ~1 KiB each = ~1 MiB worst case per ring — cheap.
const DEFAULT_RING_CAPACITY: usize = 1024;

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

/// Per-shard handle. The shard owns M job producers (round-robined),
/// one result consumer, and the worker thread handles for `unpark`.
pub struct VerifyHandle {
    pub jobs_txs: Vec<SpscProducer<VerifyJob>>,
    pub results_rx: MpscConsumer<VerifyResult>,
    pub worker_threads: Vec<std::thread::Thread>,
    /// Round-robin cursor across `jobs_txs`. Bumped on every push so
    /// successive events go to different workers.
    pub next_worker: usize,
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
            MpscRingBuffer::<VerifyResult>::new(Capacity::at_least(DEFAULT_RING_CAPACITY)).split();

        // Shared handle the shard fills in once it starts running.
        // Workers read from it to unpark the shard after pushing a
        // result, so a shard that's epoll-parked between TCP frames
        // still wakes promptly when a verdict lands.
        let shard_waker: Arc<OnceLock<std::thread::Thread>> = Arc::new(OnceLock::new());

        let mut jobs_txs = Vec::with_capacity(workers_per);
        let mut worker_threads = Vec::with_capacity(workers_per);

        for worker_idx in 0..workers_per {
            // Each worker has its own SPSC jobs ring fed only by the
            // shard. SPSC is the lightest-weight option and matches the
            // single-producer reality (shard) ↔ single-consumer reality
            // (worker).
            let (jobs_tx, jobs_rx) =
                SpscRingBuffer::<VerifyJob>::new(Capacity::at_least(DEFAULT_RING_CAPACITY)).split();
            jobs_txs.push(jobs_tx);

            // Clone the MPSC producer for this worker. The seed
            // producer stays unused; we drop it after the loop.
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
            threads.push(thread);
        }

        // Drop the seed producer so total producer count == workers_per.
        // (The mpsc Producer is reference-counted internally; dropping
        // here just frees one reference.)
        drop(results_seed_tx);

        handles.push(VerifyHandle {
            jobs_txs,
            results_rx,
            worker_threads,
            next_worker: 0,
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
    mut jobs_rx: SpscConsumer<VerifyJob>,
    results_tx: MpscProducer<VerifyResult>,
    shard_waker: Arc<OnceLock<std::thread::Thread>>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let Some(job) = jobs_rx.pop() else {
            std::thread::park_timeout(Duration::from_millis(10));
            continue;
        };

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

        // Backpressure: if the shard hasn't drained results yet, spin
        // briefly. Results-ring overflow means the shard can't keep up
        // with our verify rate, which is *good* news on a perf bench.
        push_result(&results_tx, result);

        // Wake the shard so it drains the result promptly. Without
        // this an epoll-parked shard would only see the verdict on
        // the next inbound TCP frame, dragging end-to-end latency.
        if let Some(t) = shard_waker.get() {
            t.unpark();
        }
    }
}

fn push_result(tx: &MpscProducer<VerifyResult>, mut msg: VerifyResult) {
    let mut spins = 0u32;
    loop {
        match tx.push(msg) {
            Ok(()) => return,
            Err(returned) => {
                msg = returned;
                if spins < 64 {
                    std::hint::spin_loop();
                } else if spins < 256 {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(Duration::from_micros(10));
                }
                spins = spins.saturating_add(1);
            }
        }
    }
}
