//! Bounded-bucket persistence for NIP-01.
//!
//! Three disk-backed buckets, each a fixed-size slot table + in-memory
//! inverted indexes:
//!
//! - **Ephemeral ring** — kinds outside the replaceable ranges. Circular;
//!   oldest slot is overwritten when full. Kind-1 traffic rotates out fast.
//! - **Replaceable LRU** — kinds 10000..20000. Keyed on `pubkey`; newer
//!   events for a pubkey overwrite older ones. Oldest pubkey evicted when
//!   the slot table is full.
//! - **Parameterized LRU** — kinds 30000..40000. Keyed on
//!   `(pubkey, kind, d-tag)`. Otherwise identical to replaceable.
//!
//! ## Threading
//!
//! - **1 storage thread** owns all three buckets, their log files, and all
//!   index mutation. Shard threads hand events to it via SPSC. Writes are
//!   batched, fsync group-committed every ~10ms.
//! - **N reader threads** (fixed at startup) service historical REQ scans.
//!   Each owns an `AtomicU64` gen slot so the storage thread can compute
//!   `g_floor = min(reader_active_gens)` and refuse to overwrite any slot
//!   still visible to an active reader.
//!
//! ## Generational CoW
//!
//! `current_gen: AtomicU64` is bumped after every write batch. Each slot
//! stores the gen at which it was written. A REQ scan snapshots the current
//! gen at start (`g_req`) and writes it into its reader-slot; it ignores
//! any slot whose `slot.gen > g_req`. The storage thread stages writes whose
//! target slot still has `slot.gen >= g_floor`.
//!
//! ## Why not io_uring for disk yet
//!
//! v1 uses `pwrite` + explicit `fsync` on the storage thread. The code is
//! structured so `log.rs` can swap to io_uring writes + async fsync SQEs
//! later without touching the bucket or reader layers. Nostr events are
//! <1 KiB on average; a single thread doing buffered `pwrite` + batched
//! fsync easily pushes 100K+ events/s, which exceeds the network ingest
//! the reader shards can feed it.

pub(crate) mod bucket;
pub(crate) mod engine;
pub(crate) mod handle;
pub(crate) mod index;
pub(crate) mod log;
pub(crate) mod reader;
pub(crate) mod slot;

use std::path::PathBuf;

/// Configuration for the persistence layer.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Directory holding the three bucket log files. Created if missing.
    pub data_dir: PathBuf,
    /// Slot count for the ephemeral ring.
    pub ephemeral_slots: usize,
    /// Slot count for the replaceable LRU.
    pub replaceable_slots: usize,
    /// Slot count for the parameterized LRU.
    pub parameterized_slots: usize,
    /// Max payload bytes per slot (raw event JSON). Events larger than this
    /// are rejected with OK=false; slots themselves are sized to this value.
    pub max_payload: usize,
    /// Number of reader threads for historical REQ scans.
    pub reader_threads: usize,
    /// Capacity of the shard→storage write ring (per shard).
    pub write_ring_capacity: usize,
    /// Capacity of the shard→reader-pool REQ ring (per reader).
    pub req_ring_capacity: usize,
    /// Group-commit fsync interval. `None` disables fsync (faster, less
    /// durable — tail can be lost on crash).
    pub fsync_interval_ms: Option<u64>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./nostr-relay-data"),
            ephemeral_slots: 100_000,
            replaceable_slots: 10_000,
            parameterized_slots: 10_000,
            max_payload: 64 * 1024,
            reader_threads: 2,
            write_ring_capacity: 4096,
            req_ring_capacity: 1024,
            fsync_interval_ms: Some(10),
        }
    }
}
