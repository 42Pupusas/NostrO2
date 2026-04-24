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

/// Configuration for the Nostr relay layer.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Max concurrent connections. The oldest connection is evicted on overflow.
    pub max_clients: usize,
    /// Max subscriptions per connection. The oldest sub is dropped on overflow.
    pub max_subs_per_conn: usize,
    /// Max filters per REQ. Over-limit REQs are rejected with CLOSED.
    pub max_filters_per_sub: usize,
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
    /// kTLS config. When set, the kernel terminates TLS on every connection
    /// and the io_uring data path sees plaintext. The rustls `ServerConfig`
    /// must have `enable_secret_extraction = true`.
    #[cfg(feature = "ktls")]
    pub tls: Option<Arc<ring_relay_server::rustls::ServerConfig>>,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_clients: 1024,
            max_subs_per_conn: 32,
            max_filters_per_sub: 16,
            shards: ShardConfig::default(),
            max_past_drift: None,
            max_future_drift: Some(900), // 15 minutes
            info: Some(RelayInfo::minimal()),
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
        let config = Arc::new(config);

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
            let (p, c) = quetzalcoatl::broadcast::arc::ArcRingBuffer::<shard::SubRepl>::new(
                cap, n,
            )
            .split();
            (Some(p), Some(c))
        } else {
            (None, None)
        };

        // Spawn one reader thread per shard, each running a ShardDispatcher
        // inline on the I/O thread.
        let mut reader_threads = Vec::with_capacity(n);
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
