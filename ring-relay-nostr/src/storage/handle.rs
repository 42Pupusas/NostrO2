//! Handles the shard threads use to talk to the storage engine and reader
//! pool.
//!
//! Each shard gets its own MPSC producer handle into the shared write ring
//! drained by the storage thread. REQ jobs flow through a shared MPMC ring
//! (many shard producers, many reader consumers) so reader threads can
//! `pop_block` directly instead of polling a Mutex<VecDeque> on a 1ms
//! timer.

use std::sync::Arc;

use nostro2::NostrSubscription;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpmc::{
    Consumer as MpmcConsumer, Producer as MpmcProducer, RingBuffer as MpmcRingBuffer,
};
use quetzalcoatl::mpsc::{Consumer as MpscConsumer, Producer as MpscProducer};

use super::index::SlotMeta;
use super::slot::BucketKind;

/// One slot mutation, broadcast from the storage thread to every reader
/// thread. Readers replay these in order against their thread-local
/// `BucketIndex` snapshots; this is the disruptor / event-sourcing
/// pattern that makes the index lock-free without a per-batch full
/// clone.
///
/// `meta = None` means the slot was cleared (e.g. an LRU eviction with
/// no replacement, which currently can't happen — eviction always
/// installs a new entry — but the variant is kept for forward
/// compatibility with NIP-09 deletion).
#[derive(Clone)]
pub struct IndexUpdate {
    pub bucket: BucketKind,
    pub slot_idx: u32,
    pub meta: Option<SlotMeta>,
}

/// One EVENT queued for persistence.
pub struct WriteReq {
    /// Raw JSON bytes of the event object (the exact substring of the
    /// inbound frame). The storage thread parses a view from this.
    pub raw_json: Arc<[u8]>,
    /// Hex-decoded event id.
    pub event_id: [u8; 32],
    /// Hex-encoded event id, kept here so the storage thread can build
    /// `OK` ack frames without re-encoding 32 bytes back to hex per
    /// commit. Shared `Arc<str>` from the verify worker.
    pub event_id_hex: Arc<str>,
    /// Hex-decoded pubkey.
    pub pubkey: [u8; 32],
    /// The event's `kind`. Used to pick the bucket.
    pub kind: u32,
    /// fd of the client that sent this EVENT — used to route the
    /// commit / reject ack back to the right shard's session.
    pub client_id: i32,
    /// Source shard index. Storage uses this to route the ack into
    /// that shard's MPSC results ring (mirrors `VerifyJob`).
    pub source_shard: u16,
}

/// One commit / drop verdict the storage thread sends back to a shard
/// after attempting to ingest a [`WriteReq`].
///
/// The shard turns these into NIP-01 `OK` frames for the publisher.
/// Carries `event_id_hex` and `client_id` so the shard can dispatch
/// without an event_id-keyed lookup.
pub struct StorageAck {
    pub client_id: i32,
    pub event_id_hex: Arc<str>,
    pub outcome: AckOutcome,
}

/// What the storage thread decided about a write.
///
/// Note: this is *commit-truthful*, not *fsync-truthful*. `Stored` fires
/// once the in-memory bucket index has been updated and the slot bytes
/// have been pwritten; the fsync may still be pending up to
/// `fsync_interval_ms`. A crash between commit and fsync can lose the
/// tail. Documented limitation; fsync-truthful OK requires batching the
/// ack against fsync boundaries and is a follow-up.
pub enum AckOutcome {
    /// Slot committed. Send `OK=true`.
    Stored,
    /// Storage already had this event id (NIP-01 dedupe). Send
    /// `OK=true` with a "duplicate" message — common relay behavior.
    Duplicate,
    /// Storage refused the write for the carried reason. Send
    /// `OK=false` with the reason as the message field (e.g.
    /// `"blocked: event was deleted"`, `"invalid: payload too large"`).
    Rejected(&'static str),
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

/// Shard-side consumer of the ack ring. Drained at the top of every
/// shard loop iteration before frame I/O so committed events get their
/// `OK` before the next inbound frame triggers more work.
///
/// The producer side is held by the storage thread as a plain
/// `MpscProducer<StorageAck>` — there's one per shard, indexed by
/// `WriteReq::source_shard`, mirroring `verify_pool`'s per-shard
/// results-ring topology.
pub struct AckRx(pub MpscConsumer<StorageAck>);

/// Per-shard producer side of the writes ring. `Clone` because every
/// shard gets its own handle into the same shared MPSC ring.
#[derive(Clone)]
pub struct WriteTx(pub MpscProducer<WriteReq>);

impl WriteTx {
    /// Best-effort push; returns `Err` if the ring is full. The storage
    /// thread is expected to keep up; if it can't, callers should wake it
    /// and retry (see `push_with_wake`).
    pub fn try_push(&self, req: WriteReq) -> Result<(), WriteReq> {
        self.0.push(req)
    }
}

/// Storage-thread side of the writes ring.
pub struct WriteRx(pub MpscConsumer<WriteReq>);

/// MPMC REQ-job queue. Shards push, reader threads pop_block, capacity
/// is sized to the relay's max in-flight REQ count. Cloning a [`ReqTx`]
/// is cheap (Arc + tiny per-handle batch state); cloning a [`ReqRx`]
/// stages a fresh starting scan offset so multiple readers don't
/// thunder on slot 0.
///
/// Capacity must be `>= 4` (mpmc invariant); callers should pass at
/// least `max_clients * max_subs_per_conn`.
pub struct ReqQueue {
    tx: MpmcProducer<ReqJob>,
    rx_seed: MpmcConsumer<ReqJob>,
}

/// Shard-side handle. `Clone` so each shard gets its own producer
/// without sharing the per-handle batch reservation cell.
#[derive(Clone)]
pub struct ReqTx(pub MpmcProducer<ReqJob>);

/// Reader-side handle. `Clone` so each reader thread gets its own
/// consumer with a staggered scan cursor.
#[derive(Clone)]
pub struct ReqRx(pub MpmcConsumer<ReqJob>);

impl ReqQueue {
    pub fn new(capacity: usize) -> Self {
        let (tx, rx_seed) =
            MpmcRingBuffer::<ReqJob>::new(Capacity::at_least(capacity.max(4))).split();
        Self { tx, rx_seed }
    }

    /// Distribute one [`ReqRx`] handle per reader thread, returning the
    /// seed producer for the relay to clone per shard. Consumes the
    /// consumer-seed on the last reader so producer/consumer counts
    /// match the population exactly.
    pub fn into_consumers(self, n: usize) -> (Vec<ReqRx>, MpmcProducer<ReqJob>) {
        assert!(n >= 1, "ReqQueue requires at least one reader");
        let mut out = Vec::with_capacity(n);
        let mut seed = Some(self.rx_seed);
        for i in 0..n {
            let c = if i + 1 < n {
                seed.as_ref().expect("seed live").clone()
            } else {
                seed.take().expect("seed live for last reader")
            };
            out.push(ReqRx(c));
        }
        (out, self.tx)
    }
}

impl ReqTx {
    /// Submit a REQ. Blocks until a slot frees if the ring is full;
    /// returns `Err(job)` only when every reader has dropped (no one
    /// left to drain).
    pub fn submit(&self, job: ReqJob) -> Result<(), ReqJob> {
        self.0.push_block(job)
    }
}

impl ReqRx {
    /// Block until a job arrives. Returns `None` only when every
    /// shard has dropped its [`ReqTx`] AND the ring is empty.
    pub fn next(&self) -> Option<ReqJob> {
        self.0.pop_block()
    }
}
