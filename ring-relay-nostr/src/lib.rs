//! Ephemeral NIP-01 Nostr relay on top of [`ring_relay_server`].
//!
//! No persistence: accepted events are fanned out to matching live subscribers
//! and then dropped. `REQ` responds with an immediate `EOSE` since the relay
//! keeps no history.
//!
//! FIFO eviction: when the per-connection subscription cap is reached the
//! oldest subscription is dropped; when the per-shard client cap is reached
//! the oldest connection on that shard is closed. The underlying WS server
//! also caps total clients globally via `max_clients`.
//!
//! ## Architecture
//!
//! Each reader shard runs an inline [`ShardDispatcher`] on the I/O thread:
//! parse, verify, match subs, and emit `WriteCmd`s happen on the same thread
//! that read the bytes. No central dispatcher. This eliminates the
//! ~11K events/sec single-thread bottleneck that the old central-loop design
//! had.

mod backoff;
mod extension;
mod filter;
mod info;
mod protocol;
mod shard;
mod storage;
mod verify_pool;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ring_relay_server::{ServerComponents, ServerConfig, ShardConfig};

pub use extension::{
    Extension, ExtensionAction, MessageRef, OutboundDecision, OutboundFrame, OutboundKind,
    Session, extract_ip,
};
pub use filter::{
    DeletionRef, deletion_refs_from_view, expiration_from_note, expiration_from_view,
    leading_zero_bits, matches, matches_match_view, matches_view,
};
pub use info::{Limitation, RelayInfo};
pub use protocol::{
    ClientMessage, ClientMessageView, ParseError, event_from_serialized, parse, parse_view,
    serialize_note, serialize_note_view,
};
pub use shard::ShardDispatcher;
pub use storage::StorageConfig;
pub use verify_pool::MatchView;

/// Configuration for the Nostr relay layer.
///
/// `RelayConfig` is `Clone` but not `Debug`: the `extensions` field holds
/// `Arc<dyn Extension>`, which has no `Debug` requirement (forcing one
/// would burden every extension impl). Operators that want to log config
/// should walk the structured fields directly.
#[derive(Clone)]
pub struct RelayConfig {
    /// Max concurrent connections. The oldest connection is evicted on overflow.
    pub max_clients: usize,
    /// Max subscriptions per connection. The oldest sub is dropped on overflow.
    pub max_subs_per_conn: usize,
    /// Max filters per REQ. Over-limit REQs are rejected with CLOSED.
    pub max_filters_per_sub: usize,
    /// Max size of an inbound client frame, in bytes. Over-limit frames are
    /// rejected with NOTICE before parsing. `None` disables the check.
    pub max_message_length: Option<usize>,
    /// Max length of an EVENT's `content` field, in bytes. Over-limit events
    /// are rejected with OK=false. `None` disables the check.
    pub max_content_length: Option<usize>,
    /// Max number of tags on an EVENT. Over-limit events are rejected with
    /// OK=false. `None` disables the check.
    pub max_event_tags: Option<usize>,
    /// Max length of a subscription id on REQ / CLOSE, in bytes. Over-limit
    /// REQs are rejected with CLOSED; over-limit CLOSEs with NOTICE. `None`
    /// disables the check.
    pub max_subid_length: Option<usize>,
    /// Reader/writer sharding for the underlying WS transport.
    pub shards: ShardConfig,
    /// Reject EVENTs with `created_at` further in the past than this, in seconds.
    /// `None` disables the check.
    pub max_past_drift: Option<u64>,
    /// Reject EVENTs with `created_at` further in the future than this, in seconds.
    /// `None` disables the check.
    pub max_future_drift: Option<u64>,
    /// NIP-11 relay information document served on `GET /`. When `None`,
    /// plain HTTP requests get a 400.
    pub info: Option<RelayInfo>,
    /// Persistence configuration. When `None`, the relay is ephemeral
    /// (original behavior): REQs return immediate EOSE, events aren't
    /// stored. When `Some`, events are persisted to the bounded buckets
    /// and REQs stream historical matches before EOSE.
    pub storage: Option<StorageConfig>,
    /// Total number of schnorr-verify worker threads. All reader
    /// shards push into one shared MPMC jobs ring, so any shard can
    /// pull from the full pool when busy — there's no per-shard
    /// partition.
    ///
    /// `0` (the default) means "auto": match the host's available
    /// parallelism so total verify capacity ≈ CPU count. Set
    /// explicitly to override.
    pub verify_threads: usize,
    /// kTLS config. When set, the kernel terminates TLS on every connection
    /// and the io_uring data path sees plaintext. The rustls `ServerConfig`
    /// must have `enable_secret_extraction = true`.
    #[cfg(feature = "ktls")]
    pub tls: Option<Arc<ring_relay_server::rustls::ServerConfig>>,
    /// Extensions invoked at shard hook points (connect / disconnect /
    /// inbound message / outbound frame). The same `Arc<dyn Extension>`
    /// is shared across all shards. Default is empty — preserving the
    /// vanilla NIP-01 wire behavior. See [`Extension`].
    pub extensions: Vec<Arc<dyn Extension>>,
    /// Header to consult for the real client IP (e.g. `"x-forwarded-for"`).
    /// `None` disables IP extraction. The first comma-separated entry of
    /// the matching header is parsed; behind a trusted proxy this is the
    /// real client. Untrusted deployments should leave this `None`.
    pub trusted_ip_header: Option<String>,
    /// NIP-13 minimum proof-of-work difficulty. Events whose id has
    /// fewer leading zero bits than this are rejected with `OK=false
    /// "pow: insufficient difficulty"`. `0` (the default) disables the
    /// check. The value is also surfaced in the NIP-11 limitation
    /// document so well-behaved clients can pre-mine.
    pub min_pow_difficulty: u32,
    /// NIP-42 AUTH configuration. `None` (the default) disables AUTH
    /// entirely — the relay never sends the `["AUTH", challenge]`
    /// frame and ignores any inbound `AUTH` verb. `Some` activates
    /// AUTH per [`AuthConfig`].
    pub auth: Option<AuthConfig>,
    /// Per-shard `ReaderCore` io_uring submission/completion ring
    /// capacity (entries). Default 4096.
    pub read_buffer_capacity: u32,
    /// Capacity of each per-shard MPSC verify-results ring (workers →
    /// shard). Default 1024 — verdicts are ~96 B so a full ring is
    /// ~96 KiB.
    pub verify_results_ring_capacity: usize,
    /// Capacity of the global MPMC verify-jobs ring (shards → workers).
    /// Sized to absorb cross-shard bursts without producers parking.
    /// Default 16384.
    pub verify_jobs_ring_capacity: usize,
    /// Upper bound used by the `verify_threads = 0` (auto) policy. The
    /// auto-pick is `clamp(cpus / 2, 1, verify_auto_thread_cap)`. The
    /// global MPMC jobs ring shows a measured contention regression
    /// past ~8 consumers, so the default is 8. Setting `verify_threads`
    /// explicitly bypasses this cap entirely.
    pub verify_auto_thread_cap: usize,
    /// Floor for the index-update broadcast ring capacity in storage
    /// mode. Effective capacity is
    /// `max(write_ring_capacity * shards, index_broadcast_capacity_floor)`.
    /// Default 8192.
    pub index_broadcast_capacity_floor: usize,
    /// Floor for the cross-shard sub-replication broadcast ring
    /// capacity. Effective capacity is
    /// `max(max_clients * max_subs_per_conn, repl_broadcast_capacity_floor)`.
    /// Default 4096.
    pub repl_broadcast_capacity_floor: usize,
    /// Park timeout used by [`NostrRelay::run`] while waiting on the
    /// shutdown flag, in milliseconds. Lower values shorten shutdown
    /// latency at the cost of a bit more wakeup traffic. Default 100.
    pub shutdown_poll_interval_ms: u64,
}

/// NIP-42 AUTH configuration. Activate by setting [`RelayConfig::auth`].
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// The `wss://` (or `ws://`) URL the relay identifies as. Inbound
    /// AUTH events MUST carry a `relay` tag whose value matches this
    /// URL exactly — otherwise it could be a replay from a different
    /// relay's challenge. The comparison is case-insensitive on host
    /// and case-sensitive on path (per RFC 3986).
    pub relay_url: String,
    /// Tolerance window around the AUTH event's `created_at`, in
    /// seconds. NIP-42 mandates rejecting events whose `created_at`
    /// is more than 10 minutes from `now`. Default: 600.
    pub max_clock_skew_secs: i64,
    /// Gate REQ / EVENT on auth status. `None` = advisory mode (relay
    /// issues the challenge but doesn't refuse unauthed clients).
    /// See [`AuthGate`].
    pub gate: Option<AuthGate>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            relay_url: String::new(),
            max_clock_skew_secs: 600,
            gate: None,
        }
    }
}

/// What unauthed clients are not allowed to do. NIP-42 mandates the
/// relay reply with `auth-required:` prefixed messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthGate {
    /// REQ from unauthed clients → `CLOSED "auth-required: ..."`.
    /// EVENT still flows.
    Read,
    /// EVENT from unauthed clients → `OK=false "auth-required: ..."`.
    /// REQ still flows.
    Write,
    /// Both gated.
    All,
}

impl Default for RelayConfig {
    fn default() -> Self {
        let max_clients = 1024;
        let max_subs_per_conn = 32;
        let max_filters_per_sub = 16;
        let max_message_length = Some(2 * 1024 * 1024);
        let max_content_length = Some(512 * 1024);
        let max_event_tags = Some(500);
        let max_subid_length = Some(64);

        let min_pow_difficulty: u32 = 0;
        let info = RelayInfo::minimal().with_limits(Limitation {
            max_message_length: max_message_length.map(|n| n as u32),
            max_subscriptions: Some(max_subs_per_conn as u32),
            max_filters: Some(max_filters_per_sub as u32),
            max_subid_length: max_subid_length.map(|n| n as u32),
            max_event_tags: max_event_tags.map(|n| n as u32),
            max_content_length: max_content_length.map(|n| n as u32),
            min_pow_difficulty: if min_pow_difficulty == 0 {
                None
            } else {
                Some(min_pow_difficulty)
            },
            ..Limitation::default()
        });

        Self {
            max_clients,
            max_subs_per_conn,
            max_filters_per_sub,
            max_message_length,
            max_content_length,
            max_event_tags,
            max_subid_length,
            shards: ShardConfig::default(),
            max_past_drift: None,
            max_future_drift: Some(900), // 15 minutes
            info: Some(info),
            storage: None,
            verify_threads: 0,
            #[cfg(feature = "ktls")]
            tls: None,
            extensions: Vec::new(),
            trusted_ip_header: None,
            min_pow_difficulty,
            auth: None,
            read_buffer_capacity: 4096,
            verify_results_ring_capacity: 1024,
            verify_jobs_ring_capacity: 16 * 1024,
            verify_auto_thread_cap: 8,
            index_broadcast_capacity_floor: 8192,
            repl_broadcast_capacity_floor: 4096,
            shutdown_poll_interval_ms: 100,
        }
    }
}

/// An ephemeral NIP-01 relay.
///
/// Owns the underlying WS server components and one reader thread per
/// configured shard, each running a [`ShardDispatcher`] inline. Drop to
/// shut down.
pub struct NostrRelay {
    /// Held for shutdown on drop. Must drop after reader_threads have joined.
    components: Option<ServerComponents>,
    reader_threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    shutdown: Arc<AtomicBool>,
    storage_shutdown: Option<storage::engine::StorageShutdown>,
    reader_pool_shutdown: Option<storage::reader::ReaderPoolShutdown>,
    /// One schnorr-verify worker per shard. Held so its threads are
    /// joined on relay shutdown.
    verify_pool_shutdown: Option<verify_pool::VerifyPoolShutdown>,
    /// Park interval for `run()`'s shutdown poll. Mirrors
    /// `RelayConfig::shutdown_poll_interval_ms` so we don't have to
    /// keep the full config around.
    shutdown_poll_interval: std::time::Duration,
}

impl NostrRelay {
    /// Start a relay on `addr:port`. Pass port `0` for an OS-assigned port.
    ///
    /// # Errors
    /// Propagates any failure from [`ServerComponents::prepare`] or reader
    /// thread spawn.
    pub fn bind(
        addr: [u8; 4],
        port: u16,
        config: RelayConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let http_handler = config.info.clone().map(|info| {
            let handler: Arc<ring_relay_server::HttpHandler> =
                Arc::new(move |req: ring_relay_server::HttpRequest<'_>| handle_http(&info, req));
            handler
        });

        let server_config = ServerConfig {
            shards: ShardConfig {
                reader_shards: config.shards.reader_shards,
                writer_shards: config.shards.writer_shards,
            },
            subprotocols: Vec::new(),
            deflate: None,
            http_handler,
            #[cfg(feature = "ktls")]
            tls: config.tls.clone(),
        };

        let (builder, accept_rxs) =
            ServerComponents::prepare(addr, port, config.max_clients, server_config)?;
        let port = builder.port();
        let shutdown = builder.shutdown_flag();
        let n = accept_rxs.len();
        let storage_config = config.storage.clone();
        let shutdown_poll_interval =
            std::time::Duration::from_millis(config.shutdown_poll_interval_ms);
        let config = Arc::new(config);

        // Storage + reader pool setup (if persistence is enabled).
        //
        // EXPERIMENT (mpsc-write-ring branch): one shared MPSC write ring
        // for all shards, replacing the previous N SPSC rings. Capacity is
        // sized to `write_ring_capacity * n` so total in-flight buffering
        // matches the SPSC layout — only the contention shape changes.
        let mut shard_write_tx: Vec<Option<storage::handle::WriteTx>> = Vec::with_capacity(n);
        // Per-shard ack rings (storage → one shard). Mirrors the verify
        // pool's results-ring shape: one MPSC per shard plus a `OnceLock<Thread>`
        // each shard publishes its own JoinHandle's Thread into. The
        // storage thread holds the producers + wakers Vec, indexed by
        // `WriteReq::source_shard`.
        let mut shard_ack_rx: Vec<Option<storage::handle::AckRx>> = Vec::with_capacity(n);
        let mut shard_ack_waker: Vec<Option<Arc<std::sync::OnceLock<std::thread::Thread>>>> =
            Vec::with_capacity(n);
        let (
            storage_engine,
            storage_shutdown_opt,
            storage_thread_handle,
            reader_pool_shutdown_opt,
            reader_thread_handles,
        ) = if let Some(sc) = storage_config.as_ref() {
            let total_cap = sc.write_ring_capacity.saturating_mul(n).max(sc.write_ring_capacity);
            let (tx_seed, rx) = quetzalcoatl::mpsc::RingBuffer::new(
                quetzalcoatl::capacity::Capacity::at_least(total_cap),
            )
            .split();
            for _ in 0..n {
                shard_write_tx.push(Some(storage::handle::WriteTx(tx_seed.clone())));
            }
            // Drop the seed producer so producer count == n exactly.
            drop(tx_seed);
            let storage_write_rx = storage::handle::WriteRx(rx);

            // One ack ring per shard. Capacity = write_ring_capacity is
            // generous: even at full ingest saturation the shard drains
            // every loop pass, so the ring rarely fills past a few slots.
            let mut ack_producers: Vec<quetzalcoatl::mpsc::Producer<storage::handle::StorageAck>> =
                Vec::with_capacity(n);
            let mut ack_wakers: Vec<Arc<std::sync::OnceLock<std::thread::Thread>>> =
                Vec::with_capacity(n);
            for _ in 0..n {
                let (ack_tx, ack_rx) = quetzalcoatl::mpsc::RingBuffer::<
                    storage::handle::StorageAck,
                >::new(quetzalcoatl::capacity::Capacity::at_least(
                    sc.write_ring_capacity,
                ))
                .split();
                ack_producers.push(ack_tx);
                shard_ack_rx.push(Some(storage::handle::AckRx(ack_rx)));
                let waker = Arc::new(std::sync::OnceLock::new());
                ack_wakers.push(Arc::clone(&waker));
                shard_ack_waker.push(Some(waker));
            }

            // Index-update broadcast ring: storage thread (single producer)
            // pushes one IndexUpdate per committed write; each reader thread
            // consumes its own copy. This is what makes the storage layer
            // lock-free — readers never share state with the writer, only
            // a snapshot stream.
            //
            // Capacity: must be > peak in-flight writes between reader
            // drains. Reader drains at REQ start, so worst case is the
            // batch size × a few batches. Floor configurable via
            // `RelayConfig::index_broadcast_capacity_floor`.
            let n_readers = sc.reader_threads.max(1);
            let index_cap = quetzalcoatl::capacity::Capacity::at_least(
                (sc.write_ring_capacity * n).max(config.index_broadcast_capacity_floor),
            );
            let (index_producer, index_consumer_seed) =
                quetzalcoatl::broadcast::arc::ArcRingBuffer::<storage::handle::IndexUpdate>::new(
                    index_cap, n_readers,
                )
                .split();

            // Distribute one consumer slot per reader thread.
            let mut index_consumers: Vec<_> = Vec::with_capacity(n_readers);
            let mut seed_opt = Some(index_consumer_seed);
            for i in 0..n_readers {
                let c = if i + 1 < n_readers {
                    seed_opt.as_ref().expect("seed live").clone()
                } else {
                    seed_opt.take().expect("seed live for last reader")
                };
                index_consumers.push(c);
            }

            let (engine, sd, thread_handle) = storage::engine::StorageEngine::spawn(
                sc,
                n_readers,
                storage_write_rx,
                index_producer,
                ack_producers,
                ack_wakers,
            )?;
            // REQ jobs flow shards → readers via an MPMC ring. Capacity
            // tracks the worst-case in-flight REQ population so producers
            // park rather than fail.
            let req_capacity = (config.max_clients * config.max_subs_per_conn).max(4);
            let req_queue = storage::handle::ReqQueue::new(req_capacity);
            let (req_consumers, req_producer_seed) = req_queue.into_consumers(n_readers);
            // The reader pool keeps its consumers alive for the lifetime
            // of the relay. We hold a single shard-side ReqTx behind an
            // Arc so each shard can clone its own producer handle without
            // needing the seed (and so the producer count stays accurate
            // even on a zero-shard test config).
            let req_tx = storage::handle::ReqTx(req_producer_seed);
            let (pool_sd, reader_threads) = storage::reader::ReaderPool::spawn(
                engine.shared(),
                req_consumers,
                builder.sender(),
                storage::reader::ReaderPoolConfig {
                    data_dir: sc.data_dir.clone(),
                    ephemeral_slots: sc.ephemeral_slots,
                    replaceable_slots: sc.replaceable_slots,
                    parameterized_slots: sc.parameterized_slots,
                    max_payload: sc.max_payload,
                    req_timeout: std::time::Duration::from_millis(sc.req_timeout_ms),
                },
                index_consumers,
            )?;
            (
                Some((engine, req_tx)),
                Some(sd),
                Some(thread_handle),
                Some(pool_sd),
                reader_threads,
            )
        } else {
            (None, None, None, None, Vec::new())
        };

        // Cross-shard sub replication via one broadcast ring. Only allocate
        // when there's more than one shard — a single-shard relay has no
        // peers to replicate to.
        //
        // The broadcast ring caps its consumer count at creation (max_consumers
        // = N), so we must distribute exactly N consumer handles across the
        // shards — the seed consumer to shard 0, N-1 clones to shards 1..N.
        // Producer is MP so we can freely clone.
        let (repl_producer, mut repl_consumer_opt) = if n > 1 {
            let cap = quetzalcoatl::capacity::Capacity::at_least(
                (config.max_clients * config.max_subs_per_conn)
                    .max(config.repl_broadcast_capacity_floor),
            );
            let (p, c) =
                quetzalcoatl::broadcast::arc::ArcRingBuffer::<shard::SubRepl>::new(cap, n).split();
            (Some(p), Some(c))
        } else {
            (None, None)
        };

        // Schnorr-verify offload: one global pool of W workers, one
        // shared MPMC jobs ring fed by all reader shards. Profiling
        // showed verify pinned the shard's I/O thread at >80% CPU, so
        // we hand each event to the pool and continue reading frames
        // immediately. Whichever shard is busiest pulls more verify
        // capacity automatically — no static partition.
        //
        // verify_threads == 0 means "auto": pick a default that's
        // roughly half the host's parallelism, clamped to
        // `[1, verify_auto_thread_cap]`. The default cap is 8; it
        // reflects a measured contention regression on the global
        // MPMC jobs ring above ~8 consumers — past that, the CAS
        // traffic on the head pointer eats more than the extra worker
        // brings in. Schnorr verify is ~50µs; 8 workers is already
        // 160K verifies/sec, comfortably above what one io_uring
        // reader shard can feed even at saturated throughput. Users
        // who want more can set `verify_threads` explicitly (which
        // bypasses the cap) or raise `verify_auto_thread_cap`.
        let total_verify_threads = if config.verify_threads == 0 {
            let cpus = std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1);
            (cpus / 2).clamp(1, config.verify_auto_thread_cap.max(1))
        } else {
            config.verify_threads
        };
        let (mut verify_handles, verify_pool_shutdown) = verify_pool::spawn(
            n,
            total_verify_threads,
            config.verify_jobs_ring_capacity,
            config.verify_results_ring_capacity,
        );
        verify_handles.reverse(); // pop yields shard 0 first

        // Spawn one reader thread per shard, each running a ShardDispatcher
        // inline on the I/O thread.
        let mut reader_threads = Vec::with_capacity(n);
        // The mpmc REQ ring's wake bitmap handles reader wakeups on push,
        // so we no longer need to hand each shard the full reader-thread
        // handle list. We still keep the Vec around so the relay can
        // unpark readers on shutdown via the pool's StopFlag.
        let _ = reader_thread_handles;
        for (i, accept_rx) in accept_rxs.into_iter().enumerate() {
            let sender = builder.sender();
            let cfg = Arc::clone(&config);
            let shard_shutdown = Arc::clone(&shutdown);
            let owner_id = i as u32;

            // Producer is cloneable (MP); clone one per shard.
            let repl_tx = repl_producer.clone();
            // Consumer is cloneable, but each clone claims a fresh slot. Hand
            // the seed to shard 0, clone for shards 1..N-1, then give the
            // seed to the last shard to avoid needing an extra slot.
            let repl_rx = if i + 1 < n {
                repl_consumer_opt.clone()
            } else {
                repl_consumer_opt.take()
            };

            let shard_storage = match (
                storage_engine.as_ref(),
                storage_thread_handle.as_ref(),
                shard_write_tx.get_mut(i).and_then(Option::take),
                shard_ack_rx.get_mut(i).and_then(Option::take),
                shard_ack_waker.get_mut(i).and_then(Option::take),
            ) {
                (
                    Some((_engine, req_tx_seed)),
                    Some(storage_thread),
                    Some(write_tx),
                    Some(ack_rx),
                    Some(ack_waker),
                ) => Some(shard::ShardStorage {
                    write_tx,
                    // Per-shard producer clone — each clone gets its
                    // own batch reservation cell, so shards don't
                    // serialize on a shared producer state.
                    req_tx: req_tx_seed.clone(),
                    storage_waker: storage_thread.clone(),
                    ack_rx,
                    ack_waker,
                }),
                _ => None,
            };

            let verify_handle = verify_handles.pop();

            let handle = std::thread::Builder::new()
                .name(format!("nostr-shard-{i}"))
                .spawn(move || {
                    shard::run_shard(
                        accept_rx,
                        sender,
                        cfg,
                        shard_shutdown,
                        owner_id,
                        repl_tx,
                        repl_rx,
                        shard_storage,
                        verify_handle,
                    );
                })?;
            reader_threads.push(handle);
        }

        // Seed producer is still alive here; drop it so only shard-owned
        // producer clones remain. Consumer seed was handed to the last shard
        // (or was None in single-shard mode).
        drop(repl_producer);

        let components = builder.start_listener()?;

        Ok(Self {
            components: Some(components),
            reader_threads,
            port,
            shutdown,
            storage_shutdown: storage_shutdown_opt,
            reader_pool_shutdown: reader_pool_shutdown_opt,
            verify_pool_shutdown: Some(verify_pool_shutdown),
            shutdown_poll_interval,
        })
    }

    /// The port the underlying WS server is bound to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// A handle that can trigger [`NostrRelay::run`] to exit cleanly.
    #[must_use]
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            flag: Arc::clone(&self.shutdown),
        }
    }

    /// Park on the shutdown flag. Returns when [`ShutdownHandle::shutdown`]
    /// is called or the relay is dropped from another thread.
    ///
    /// Unlike the old central-loop design, this does no per-message work —
    /// each reader thread handles its own clients end-to-end. `run` is only
    /// a blocking wait on shutdown, useful when you want the main thread
    /// to own the relay's lifetime.
    pub fn run(&mut self) {
        while !self.shutdown.load(Ordering::Acquire) {
            std::thread::park_timeout(self.shutdown_poll_interval);
        }
    }
}

impl Drop for NostrRelay {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);

        // Wake reader threads out of their poll_once timeout so they exit
        // promptly.
        for h in &self.reader_threads {
            h.thread().unpark();
        }
        for h in self.reader_threads.drain(..) {
            let _ = h.join();
        }
        // Verify workers held shard-owned producer/consumer rings, but
        // those handles dropped with the reader threads. Tear the pool
        // down now so its threads join before the rest of the relay.
        if let Some(mut vp) = self.verify_pool_shutdown.take() {
            vp.stop();
        }
        // Reader-pool threads push writes; stop them before the writer
        // rings tear down.
        if let Some(mut rp) = self.reader_pool_shutdown.take() {
            rp.stop();
        }
        // Storage thread has no outbound writes to the WS layer; stop it
        // after the shard readers so no late writes arrive.
        if let Some(mut sd) = self.storage_shutdown.take() {
            sd.stop();
        }
        // Readers are gone — safe to tear down listener + writers.
        drop(self.components.take());
    }
}

/// NIP-11 router: serve the relay information document on `GET /` when the
/// client sends `Accept: application/nostr+json`. Everything else gets 404.
fn handle_http(info: &RelayInfo, req: ring_relay_server::HttpRequest<'_>) -> Vec<u8> {
    if req.method == "GET" && req.path == "/" {
        let wants_nostr_json = req.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("accept")
                && v.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("application/nostr+json"))
        });
        if wants_nostr_json {
            return info::http_response(info);
        }
    }
    info::not_found()
}

/// Trip [`NostrRelay::run`] to return.
#[derive(Clone)]
pub struct ShutdownHandle {
    flag: Arc<AtomicBool>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        self.flag.store(true, Ordering::Release);
    }
}
