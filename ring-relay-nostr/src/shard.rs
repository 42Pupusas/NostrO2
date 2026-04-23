//! Per-shard NIP-01 dispatcher running inline on a reader I/O thread.
//!
//! Each reader shard owns its own [`ShardDispatcher`]: the client table,
//! subscription table, and FIFO order for the clients accepted on that
//! shard. Parse + verify + filter-match + writer-ring push all happen on
//! the reader thread, so no cross-thread hop per message.
//!
//! Fan-out is currently shard-local — an EVENT only reaches subscriptions
//! owned by the shard it arrived on. Multi-shard fan-out (sub replication)
//! is added in a follow-up step.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nostro2::{NostrNote, NostrSubscription};
use ring_relay_server::{AcceptStream, ReaderCore, ReaderEvent, ServerSender};

use crate::{RelayConfig, filter, protocol};
use crate::protocol::ClientMessage;

/// Subscriptions owned by a single connected client.
#[derive(Default)]
struct ClientState {
    /// Insertion-ordered map of sub_id → filters. The `order` VecDeque gives
    /// O(1) FIFO pop for eviction.
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
        // Replacing an existing sub with the same id: remove it from order
        // first so re-insertion puts it at the back.
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

/// Per-shard Nostr dispatcher. One instance per reader thread.
pub struct ShardDispatcher {
    config: Arc<RelayConfig>,
    sender: ServerSender,
    clients: HashMap<i32, ClientState>,
    /// Arrival order on this shard, for FIFO eviction at capacity.
    client_order: VecDeque<i32>,
}

impl ShardDispatcher {
    pub fn new(config: Arc<RelayConfig>, sender: ServerSender) -> Self {
        Self {
            config,
            sender,
            clients: HashMap::new(),
            client_order: VecDeque::new(),
        }
    }

    /// Handle one frame-level event from the reader core.
    pub fn handle(&mut self, event: ReaderEvent<'_>) {
        match event {
            ReaderEvent::Connected { fd, .. } => self.on_connect(fd),
            ReaderEvent::Disconnected { fd, .. } => self.on_disconnect(fd),
            ReaderEvent::Text { fd, text } => self.on_text(fd, text),
            ReaderEvent::Binary { .. } => {
                // NIP-01 is text-only. Silently ignore binary frames.
            }
            ReaderEvent::Ping { fd } => {
                self.sender.pong(fd);
            }
        }
    }

    fn on_connect(&mut self, client_id: i32) {
        // Per-shard FIFO eviction. The server layer already caps total fds
        // at `max_clients`, so this is defensive — catches cases where the
        // Nostr state persists past the WS accept.
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

        self.sender
            .send_text(client_id, protocol::ok(&id, true, ""));

        // Shard-local fan-out. Cross-shard fan-out comes with sub replication.
        for (&other_id, state) in &self.clients {
            for (sub_id, filters) in &state.subs {
                if filters.iter().any(|f| filter::matches(&note, f)) {
                    let frame = protocol::event(sub_id, &note);
                    self.sender.send_text(other_id, frame);
                    break;
                }
            }
        }
    }

    fn validate_event(&self, note: &NostrNote) -> bool {
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

/// Reader-thread entry point: drive a [`ReaderCore`] and dispatch every
/// frame-level event through a [`ShardDispatcher`].
pub(crate) fn run_shard(
    mut accept_rx: AcceptStream,
    sender: ServerSender,
    config: Arc<RelayConfig>,
    shutdown: Arc<AtomicBool>,
) {
    let mut core = match ReaderCore::new(4096) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nostr shard fatal: ReaderCore::new: {e}");
            return;
        }
    };
    let mut dispatcher = ShardDispatcher::new(config, sender.clone());

    // For each new client: register it with the writer shard that owns its
    // fd (the built-in WsServer reader does this automatically; we own our
    // own reader loop so we handle it here).
    while !shutdown.load(Ordering::Acquire) {
        while let Some(client) = accept_rx.pop() {
            let fd = client.fd;
            let deflate = client.deflate.clone();
            core.accept(client, |event| dispatcher.handle(event));
            sender.register(fd, deflate);
        }

        if core.is_idle() {
            core.park_timeout();
            continue;
        }

        if let Err(e) = core.poll_once(|event| dispatcher.handle(event)) {
            eprintln!("nostr shard fatal: poll_once: {e}");
            return;
        }
    }

    core.shutdown();
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
        let evicted = state.insert_sub("a".into(), f, 2);
        assert!(evicted.is_none());
        assert_eq!(state.order.len(), 2);
    }
}
