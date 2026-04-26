//! Schnorr-verify offload pool.
//!
//! Profiling showed >80% of CPU on the fan-out hot path was spent inside
//! k256's schnorr verify, called inline from the shard I/O thread. That
//! pinned each shard to a single core's worth of crypto throughput. This
//! module moves verify to dedicated worker threads so the shard becomes
//! pure I/O + match + dispatch.
//!
//! ## Topology
//!
//! One worker thread per shard, paired 1:1 via two `quetzalcoatl::spsc`
//! rings:
//!
//! - shard → worker: `VerifyJob` (raw_json bytes + client/event metadata).
//! - worker → shard: `VerifyResult` (the same payload plus `verified: bool`).
//!
//! The shard pushes a job and continues reading frames; the worker
//! re-parses the JSON into a borrowed `NostrNoteView`, runs `verify()`,
//! and sends the answer back. The shard drains results at the top of its
//! loop and runs the post-verify path (OK, fan-out, storage push).
//!
//! Why 1:1 instead of N:M? `quetzalcoatl` only ships SPSC, MPSC, SPMC,
//! and broadcast — there is no MPMC ring to support N workers stealing
//! jobs from M shards without an additional dependency. 1:1 keeps every
//! ring SPSC and gives each shard its own dedicated verify core, which
//! is sufficient to lift the per-shard verify ceiling that the inline
//! design imposed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nostro2::NostrNoteView;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::spsc::{Consumer, Producer, RingBuffer};

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

/// Per-shard handle. The shard owns the job producer (push) and the
/// result consumer (drain). It also keeps the worker thread's `Thread`
/// handle so it can `unpark` after pushing a job.
pub struct VerifyHandle {
    pub jobs_tx: Producer<VerifyJob>,
    pub results_rx: Consumer<VerifyResult>,
    pub worker_thread: std::thread::Thread,
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

/// Spin up one verify worker per shard. Returns one [`VerifyHandle`]
/// per shard plus a shutdown handle the relay holds for the lifetime
/// of the pool.
pub fn spawn(num_shards: usize) -> (Vec<VerifyHandle>, VerifyPoolShutdown) {
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(num_shards);
    let mut threads = Vec::with_capacity(num_shards);

    for shard_idx in 0..num_shards {
        let (jobs_tx, jobs_rx) =
            RingBuffer::<VerifyJob>::new(Capacity::at_least(DEFAULT_RING_CAPACITY)).split();
        let (results_tx, results_rx) =
            RingBuffer::<VerifyResult>::new(Capacity::at_least(DEFAULT_RING_CAPACITY)).split();

        let shutdown_for_thread = Arc::clone(&shutdown);
        let thread = std::thread::Builder::new()
            .name(format!("nostr-verify-{shard_idx}"))
            .spawn(move || {
                worker_loop(shutdown_for_thread, jobs_rx, results_tx);
            })
            .expect("spawn verify worker");
        let worker_thread = thread.thread().clone();
        threads.push(thread);

        handles.push(VerifyHandle {
            jobs_tx,
            results_rx,
            worker_thread,
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
/// onto the results ring. Re-parsing the JSON each pass costs a few
/// microseconds — dwarfed by the ~50µs schnorr verify it enables to
/// run off the I/O thread.
fn worker_loop(
    shutdown: Arc<AtomicBool>,
    mut jobs_rx: Consumer<VerifyJob>,
    mut results_tx: Producer<VerifyResult>,
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
        push_result(&mut results_tx, result);
    }
}

fn push_result(tx: &mut Producer<VerifyResult>, mut msg: VerifyResult) {
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
