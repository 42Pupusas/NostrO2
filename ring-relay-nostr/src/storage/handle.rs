//! Handles the shard threads use to talk to the storage engine and reader
//! pool.
//!
//! Each shard gets its own SPSC producer into a per-shard write ring
//! drained by the storage thread. REQ jobs go to the reader pool via a
//! shared MPSC (we use a simple Mutex<VecDeque> for v1 since REQ jobs are
//! coarse-grained; the reader pool replaces this with something lock-free
//! once it matters in profiles).

use std::sync::Arc;

use nostro2::NostrSubscription;
use quetzalcoatl::spsc::{Consumer as SpscConsumer, Producer as SpscProducer};

/// One EVENT queued for persistence.
pub struct WriteReq {
    /// Raw JSON bytes of the event object (the exact substring of the
    /// inbound frame). The storage thread parses a view from this.
    pub raw_json: Arc<[u8]>,
    /// Hex-decoded event id.
    pub event_id: [u8; 32],
    /// Hex-decoded pubkey.
    pub pubkey: [u8; 32],
    /// The event's `kind`. Used to pick the bucket.
    pub kind: u32,
}

/// One REQ queued for historical replay.
///
/// The reader thread is fully responsible for delivery: stream matching
/// EVENT frames, then send EOSE. Live events that arrive during the scan
/// are emitted by the shard's normal fan-out path on top — NIP-01 does
/// not require historical and live to be sequenced relative to EOSE
/// (clients dedupe by event id), so we don't buffer live events.
pub struct ReqJob {
    /// The connected client fd so the reader can deliver via ServerSender.
    pub client_fd: i32,
    pub sub_id: Arc<str>,
    pub filters: Arc<[NostrSubscription]>,
}

/// Per-shard producer side of the writes ring.
pub struct WriteTx(pub SpscProducer<WriteReq>);

impl WriteTx {
    /// Best-effort push; returns `Err` if the ring is full. The storage
    /// thread is expected to keep up; if it can't, callers should wake it
    /// and retry (see `push_with_wake`).
    pub fn try_push(&self, req: WriteReq) -> Result<(), WriteReq> {
        self.0.push(req)
    }
}

/// Storage-thread side of the writes ring.
pub struct WriteRx(pub SpscConsumer<WriteReq>);

/// Simple lock-protected FIFO for REQ jobs. REQs are coarse-grained
/// (one per subscribe) so contention here is tiny compared to EVENT
/// ingest.
pub struct ReqQueue {
    inner: std::sync::Mutex<std::collections::VecDeque<ReqJob>>,
}

impl ReqQueue {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }
    pub fn push(&self, job: ReqJob) {
        self.inner.lock().unwrap().push_back(job);
    }
    pub fn pop(&self) -> Option<ReqJob> {
        self.inner.lock().unwrap().pop_front()
    }
}
