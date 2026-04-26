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

mod filter;
mod info;
mod protocol;
mod shard;
mod storage;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ring_relay_server::{ServerComponents, ServerConfig, ShardConfig};

pub use filter::{matches, matches_view};
pub use info::{Limitation, RelayInfo};
pub use protocol::{
    ClientMessage, ClientMessageView, ParseError, event_from_serialized, parse, parse_view,
    serialize_note, serialize_note_view,
};
pub use shard::ShardDispatcher;
pub use storage::StorageConfig;

/// Configuration for the Nostr relay layer.
#[derive(Debug, Clone)]
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
    /// kTLS config. When set, the kernel terminates TLS on every connection
    /// and the io_uring data path sees plaintext. The rustls `ServerConfig`
    /// must have `enable_secret_extraction = true`.
    #[cfg(feature = "ktls")]
    pub tls: Option<Arc<ring_relay_server::rustls::ServerConfig>>,
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

        let info = RelayInfo::minimal().with_limits(Limitation {
            max_message_length: max_message_length.map(|n| n as u32),
            max_subscriptions: Some(max_subs_per_conn as u32),
            max_filters: Some(max_filters_per_sub as u32),
            max_subid_length: max_subid_length.map(|n| n as u32),
            max_event_tags: max_event_tags.map(|n| n as u32),
            max_content_length: max_content_length.map(|n| n as u32),
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
            #[cfg(feature = "ktls")]
            tls: None,
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
        let config = Arc::new(config);

        // Storage + reader pool setup (if persistence is enabled).
        //
        // One per-shard SPSC write ring feeds the single storage thread.
        // Reader pool shares one MPSC job queue + per-thread wakers.
        let mut per_shard_write_tx: Vec<Option<storage::handle::WriteTx>> = Vec::with_capacity(n);
        let mut per_shard_write_rx: Vec<storage::handle::WriteRx> = Vec::with_capacity(n);
        let (
            storage_engine,
            storage_shutdown_opt,
            storage_thread_handle,
            reader_pool_shutdown_opt,
            reader_thread_handles,
        ) = if let Some(sc) = storage_config.as_ref() {
            for _ in 0..n {
                let (tx, rx) = quetzalcoatl::spsc::RingBuffer::new(
                    quetzalcoatl::capacity::Capacity::at_least(sc.write_ring_capacity),
                )
                .split();
                per_shard_write_tx.push(Some(storage::handle::WriteTx(tx)));
                per_shard_write_rx.push(storage::handle::WriteRx(rx));
            }

            // Index-update broadcast ring: storage thread (single producer)
            // pushes one IndexUpdate per committed write; each reader thread
            // consumes its own copy. This is what makes the storage layer
            // lock-free — readers never share state with the writer, only
            // a snapshot stream.
            //
            // Capacity: must be > peak in-flight writes between reader
            // drains. Reader drains at REQ start, so worst case is the
            // batch size (1024) × a few batches. Round up generously.
            let n_readers = sc.reader_threads.max(1);
            let index_cap =
                quetzalcoatl::capacity::Capacity::at_least((sc.write_ring_capacity * n).max(8192));
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
                    seed_opt.clone().expect("seed live")
                } else {
                    seed_opt.take().expect("seed live for last reader")
                };
                index_consumers.push(c);
            }

            let (engine, sd, thread_handle) = storage::engine::StorageEngine::spawn(
                sc,
                n_readers,
                std::mem::take(&mut per_shard_write_rx),
                index_producer,
            )?;
            let req_queue = Arc::new(storage::handle::ReqQueue::new());
            let (pool_sd, reader_threads) = storage::reader::ReaderPool::spawn(
                engine.shared(),
                Arc::clone(&req_queue),
                builder.sender(),
                storage::reader::ReaderPoolConfig {
                    data_dir: sc.data_dir.clone(),
                    ephemeral_slots: sc.ephemeral_slots,
                    replaceable_slots: sc.replaceable_slots,
                    parameterized_slots: sc.parameterized_slots,
                    max_payload: sc.max_payload,
                },
                index_consumers,
            )?;
            (
                Some((engine, req_queue)),
                Some(sd),
                Some(thread_handle),
                Some(pool_sd),
                reader_threads,
            )
        } else {
            (None, None, None, None, Vec::new())
        };
        let _ = &storage_engine;

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
                (config.max_clients * config.max_subs_per_conn).max(4096),
            );
            let (p, c) =
                quetzalcoatl::broadcast::arc::ArcRingBuffer::<shard::SubRepl>::new(cap, n).split();
            (Some(p), Some(c))
        } else {
            (None, None)
        };

        // Spawn one reader thread per shard, each running a ShardDispatcher
        // inline on the I/O thread.
        let mut reader_threads = Vec::with_capacity(n);
        let reader_wakers_arc: Arc<[std::thread::Thread]> = Arc::from(reader_thread_handles);
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
                per_shard_write_tx.get_mut(i).and_then(Option::take),
            ) {
                (Some((_engine, req_queue)), Some(storage_thread), Some(write_tx)) => {
                    Some(shard::ShardStorage {
                        write_tx,
                        req_queue: Arc::clone(req_queue),
                        storage_waker: storage_thread.clone(),
                        reader_wakers: Arc::clone(&reader_wakers_arc),
                    })
                }
                _ => None,
            };

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
            std::thread::park_timeout(std::time::Duration::from_millis(100));
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
