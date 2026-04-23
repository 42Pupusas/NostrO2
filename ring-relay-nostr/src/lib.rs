//! Ephemeral NIP-01 Nostr relay on top of [`ring_relay_server`].
//!
//! No persistence: accepted events are fanned out to matching live subscribers
//! and then dropped. `REQ` responds with an immediate `EOSE` since the relay
//! keeps no history.
//!
//! FIFO eviction: when the per-connection subscription cap is reached the
//! oldest subscription is dropped; when the global client cap is reached the
//! oldest connection is closed.

mod filter;
mod info;
mod protocol;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nostro2::{NostrNote, NostrSubscription};
use ring_relay_server::{
    ClientMessage as WsClientMessage, ServerConfig, ServerSender, ShardConfig, WsServer,
};

pub use filter::matches;
pub use info::{Limitation, RelayInfo};
pub use protocol::{ClientMessage, ParseError};

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

/// Subscriptions owned by a single connected client.
#[derive(Default)]
struct ClientState {
    /// Insertion-ordered map of sub_id → filters.
    /// We keep insertion order in a separate VecDeque for O(1) FIFO pop.
    subs: HashMap<String, Vec<NostrSubscription>>,
    order: VecDeque<String>,
}

impl ClientState {
    fn insert_sub(
        &mut self,
        sub_id: String,
        filters: Vec<NostrSubscription>,
        cap: usize,
    ) -> Option<String> {
        // If replacing an existing sub with the same id, remove it from order first.
        if self.subs.remove(&sub_id).is_some()
            && let Some(pos) = self.order.iter().position(|s| s == &sub_id)
        {
            self.order.remove(pos);
        }

        let evicted = if self.subs.len() >= cap {
            self.order.pop_front().inspect(|old| {
                self.subs.remove(old);
            })
        } else {
            None
        };

        self.order.push_back(sub_id.clone());
        self.subs.insert(sub_id, filters);
        evicted
    }

    fn remove_sub(&mut self, sub_id: &str) {
        if self.subs.remove(sub_id).is_some()
            && let Some(pos) = self.order.iter().position(|s| s == sub_id)
        {
            self.order.remove(pos);
        }
    }
}

/// An ephemeral NIP-01 relay.
///
/// Construct via [`NostrRelay::bind`], then call [`NostrRelay::run`] to drive
/// the dispatch loop on the calling thread. Drop to shut down.
pub struct NostrRelay {
    server: WsServer,
    sender: ServerSender,
    config: RelayConfig,
    clients: HashMap<i32, ClientState>,
    /// Connection arrival order for global FIFO eviction.
    client_order: VecDeque<i32>,
    shutdown: Arc<AtomicBool>,
}

impl NostrRelay {
    /// Start a relay on `addr:port`. Pass port `0` for an OS-assigned port.
    ///
    /// # Errors
    /// Propagates any failure from [`WsServer::bind_with_config`].
    pub fn bind(
        addr: [u8; 4],
        port: u16,
        config: RelayConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let http_handler = config.info.clone().map(|info| {
            let handler: Arc<ring_relay_server::HttpHandler> =
                Arc::new(move |req: ring_relay_server::HttpRequest<'_>| {
                    handle_http(&info, req)
                });
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
        let server = WsServer::bind_with_config(addr, port, config.max_clients, server_config)?;
        let sender = server.sender();
        Ok(Self {
            server,
            sender,
            config,
            clients: HashMap::new(),
            client_order: VecDeque::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The port the underlying WS server is bound to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// A handle that can trigger [`NostrRelay::run`] to exit cleanly.
    #[must_use]
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            flag: Arc::clone(&self.shutdown),
        }
    }

    /// Run the dispatch loop on the calling thread. Returns when the shutdown
    /// handle is tripped or an unrecoverable error occurs on the WS layer.
    pub fn run(&mut self) {
        while !self.shutdown.load(Ordering::Acquire) {
            let msg = self.server.recv();
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: WsClientMessage) {
        match msg {
            WsClientMessage::Connected { client_id, .. } => self.on_connect(client_id),
            WsClientMessage::Disconnected { client_id, .. } => self.on_disconnect(client_id),
            WsClientMessage::Text { client_id, text } => self.on_text(client_id, &text),
            WsClientMessage::Binary { .. } => {
                // NIP-01 is text-only. Silently ignore binary frames.
            }
        }
    }

    fn on_connect(&mut self, client_id: i32) {
        // Enforce global FIFO eviction. `max_clients` at the WS layer already
        // gates total fds, so this branch is defensive — it catches the case
        // where clients persist in our state longer than the WS accepts them.
        while self.client_order.len() >= self.config.max_clients {
            if let Some(old) = self.client_order.pop_front() {
                self.clients.remove(&old);
                self.sender
                    .send_text(old, protocol::notice("evicted: relay at capacity"));
                self.sender
                    .close_client(old, ring_relay_server::CloseCode::PolicyViolation);
            } else {
                break;
            }
        }
        self.clients.insert(client_id, ClientState::default());
        self.client_order.push_back(client_id);
    }

    fn on_disconnect(&mut self, client_id: i32) {
        self.clients.remove(&client_id);
        if let Some(pos) = self.client_order.iter().position(|&c| c == client_id) {
            self.client_order.remove(pos);
        }
    }

    fn on_text(&mut self, client_id: i32, text: &str) {
        let parsed = match protocol::parse(text) {
            Ok(msg) => msg,
            Err(e) => {
                self.sender
                    .send_text(client_id, protocol::notice(&format!("invalid: {e}")));
                return;
            }
        };

        match parsed {
            ClientMessage::Event(note) => self.on_event(client_id, note),
            ClientMessage::Req { sub_id, filters } => self.on_req(client_id, sub_id, filters),
            ClientMessage::Close { sub_id } => self.on_close_sub(client_id, &sub_id),
            ClientMessage::Unknown(verb) => {
                self.sender.send_text(
                    client_id,
                    protocol::notice(&format!("unsupported verb: {verb}")),
                );
            }
        }
    }

    fn on_event(&mut self, client_id: i32, note: NostrNote) {
        let id = note.id.clone().unwrap_or_default();

        if !self.validate_event(&note) {
            self.sender
                .send_text(client_id, protocol::ok(&id, false, "invalid: bad event"));
            return;
        }

        // Ack the sender. Ephemeral relay never has duplicates worth reporting.
        self.sender
            .send_text(client_id, protocol::ok(&id, true, ""));

        // Pre-encode the outbound frame once per sub_id we match against, but
        // since each subscriber may have a different sub_id the payload text
        // differs. Cheap enough: serialize once per (client, sub_id) hit.
        for (&other_id, state) in &self.clients {
            for (sub_id, filters) in &state.subs {
                if filters.iter().any(|f| filter::matches(&note, f)) {
                    let frame = protocol::event(sub_id, &note);
                    self.sender.send_text(other_id, frame);
                    break; // one match per sub is enough
                }
            }
        }
    }

    fn validate_event(&self, note: &NostrNote) -> bool {
        // Signature + id hash check (nostro2 handles both).
        if !note.verify() {
            return false;
        }

        let now = NostrNote::now();
        if let Some(past) = self.config.max_past_drift
            && note.created_at < now.saturating_sub(past as i64)
        {
            return false;
        }
        if let Some(fut) = self.config.max_future_drift
            && note.created_at > now.saturating_add(fut as i64)
        {
            return false;
        }
        true
    }

    fn on_req(&mut self, client_id: i32, sub_id: String, filters: Vec<NostrSubscription>) {
        if filters.len() > self.config.max_filters_per_sub {
            self.sender.send_text(
                client_id,
                protocol::closed(&sub_id, "invalid: too many filters"),
            );
            return;
        }

        let Some(state) = self.clients.get_mut(&client_id) else {
            return;
        };

        let evicted = state.insert_sub(sub_id.clone(), filters, self.config.max_subs_per_conn);

        if let Some(old_sub) = evicted {
            self.sender.send_text(
                client_id,
                protocol::closed(&old_sub, "rate-limited: subscription evicted (fifo)"),
            );
        }

        // Ephemeral: no stored events to replay. Immediate EOSE per NIP-01.
        self.sender.send_text(client_id, protocol::eose(&sub_id));
    }

    fn on_close_sub(&mut self, client_id: i32, sub_id: &str) {
        if let Some(state) = self.clients.get_mut(&client_id) {
            state.remove_sub(sub_id);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_state_fifo_eviction() {
        let mut state = ClientState::default();
        let filters = vec![NostrSubscription::default()];
        assert!(state.insert_sub("a".into(), filters.clone(), 2).is_none());
        assert!(state.insert_sub("b".into(), filters.clone(), 2).is_none());
        let evicted = state.insert_sub("c".into(), filters, 2);
        assert_eq!(evicted.as_deref(), Some("a"));
        assert!(!state.subs.contains_key("a"));
        assert!(state.subs.contains_key("b"));
        assert!(state.subs.contains_key("c"));
    }

    #[test]
    fn client_state_replace_same_id() {
        let mut state = ClientState::default();
        let f = vec![NostrSubscription::default()];
        state.insert_sub("a".into(), f.clone(), 2);
        state.insert_sub("b".into(), f.clone(), 2);
        // Re-inserting "a" should not evict "b".
        let evicted = state.insert_sub("a".into(), f, 2);
        assert!(evicted.is_none());
        assert_eq!(state.order.len(), 2);
    }
}
