//! Per-shard NIP-01 dispatcher running inline on a reader I/O thread.
//!
//! Each reader shard owns its own [`ShardDispatcher`]: the client table,
//! subscription table, and FIFO order for the clients accepted on that
//! shard. Parse + verify + filter-match + writer-ring push all happen on
//! the reader thread, so no cross-thread hop per message.
//!
//! ## Cross-shard fan-out
//!
//! Each shard is authoritative for the clients it accepted, but keeps a
//! replica of every *other* shard's subscriptions so an EVENT arriving on
//! shard A can match against subs owned by shard B and deliver directly to
//! B's clients. Replication flows over a single broadcast ring of
//! [`SubRepl`] messages:
//!
//! - `REQ` on shard A → update local state → push `SubRepl::Add` to broadcast.
//! - `CLOSE`/FIFO-eviction on A → local remove → push `SubRepl::Remove`.
//! - Client disconnect on A → local drop → push `SubRepl::ClientGone`.
//! - Every shard drains the ring each iteration, applying peer messages to
//!   its local replica and skipping its own (by comparing `owner`).
//!
//! Fan-out on EVENT walks both the local client table and the replica. The
//! writer-ring routing (`fd % writer_shards`) already handles cross-shard
//! delivery; each shard just pushes `WriteCmd::SendText { fd, ... }` for
//! any matching subscriber, and the correct writer thread picks it up.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nostro2::{NostrNote, NostrNoteView, NostrSubscription};
use quetzalcoatl::broadcast::arc::{ArcConsumer, ArcProducer};
use ring_relay_server::{AcceptStream, ReaderCore, ReaderEvent, ServerSender};

use crate::protocol::ClientMessageView;
use crate::storage::handle::{ReqJob, ReqQueue, WriteReq, WriteTx};
use crate::storage::slot::decode_hex32;
use crate::verify_pool::{VerifyHandle, VerifyJob, VerifyResult};
use crate::{RelayConfig, filter, protocol};

/// A sub-replication message broadcast from one shard to all others.
pub(crate) enum SubRepl {
    Add {
        owner: u32,
        client_id: i32,
        sub_id: Arc<str>,
        filters: Arc<[NostrSubscription]>,
    },
    Remove {
        owner: u32,
        client_id: i32,
        sub_id: Arc<str>,
    },
    /// A client disconnected from its owning shard; drop all its replica subs.
    ClientGone { owner: u32, client_id: i32 },
}

/// Subscriptions owned by a single connected client (local, authoritative view).
#[derive(Default)]
struct ClientState {
    /// Insertion-ordered map of sub_id → filters. The `order` VecDeque gives
    /// O(1) FIFO pop for eviction.
    subs: HashMap<Arc<str>, Arc<[NostrSubscription]>>,
    order: VecDeque<Arc<str>>,
}

impl ClientState {
    fn insert_sub(
        &mut self,
        sub_id: Arc<str>,
        filters: Arc<[NostrSubscription]>,
        cap: usize,
    ) -> Option<Arc<str>> {
        // Replacing an existing sub with the same id: remove it from order
        // first so re-insertion puts it at the back.
        if self.subs.remove(&sub_id).is_some()
            && let Some(pos) = self
                .order
                .iter()
                .position(|s| s.as_ref() == sub_id.as_ref())
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

        self.order.push_back(Arc::clone(&sub_id));
        self.subs.insert(sub_id, filters);
        evicted
    }

    fn remove_sub(&mut self, sub_id: &str) -> bool {
        if self.subs.remove(sub_id).is_some() {
            if let Some(pos) = self.order.iter().position(|s| s.as_ref() == sub_id) {
                self.order.remove(pos);
            }
            true
        } else {
            false
        }
    }
}

/// Replicated subs from one peer-owned client (owner, client_id) → sub_id → filters.
type ReplicaClient = HashMap<Arc<str>, Arc<[NostrSubscription]>>;

/// Per-shard storage plumbing. When `None`, the shard runs ephemeral;
/// when `Some`, EVENTs are pushed to the storage thread and REQs dispatch
/// to the reader pool.
///
/// `WriteTx` is an SPSC producer so it is `Send + !Sync`; the shard owns
/// it outright (not behind `Arc`). The req queue is shared across shards.
pub(crate) struct ShardStorage {
    pub write_tx: WriteTx,
    pub req_queue: Arc<ReqQueue>,
    pub storage_waker: std::thread::Thread,
    pub reader_wakers: Arc<[std::thread::Thread]>,
}

/// Per-shard Nostr dispatcher. One instance per reader thread.
pub struct ShardDispatcher {
    config: Arc<RelayConfig>,
    sender: ServerSender,
    /// This shard's index, stamped into every outgoing SubRepl message so
    /// peers can skip our own broadcasts on readback.
    owner_id: u32,
    clients: HashMap<i32, ClientState>,
    /// Arrival order on this shard, for FIFO eviction at capacity.
    client_order: VecDeque<i32>,
    /// Replicated peer subs: (owner, client_id) → sub_id → filters.
    replica: HashMap<(u32, i32), ReplicaClient>,
    /// Outbound replication channel; None when reader_shards == 1 (no peers).
    repl_tx: Option<ArcProducer<SubRepl>>,
    /// Inbound replication channel for this shard's replica view. None on
    /// single-shard configs.
    repl_rx: Option<ArcConsumer<SubRepl>>,
    /// Storage plumbing; `None` → ephemeral mode.
    storage: Option<ShardStorage>,
    /// Reader-pool round-robin cursor for REQ dispatch.
    next_reader: usize,
    /// Schnorr-verify offload. `None` → verify inline on the I/O thread
    /// (legacy path, only used in tests). `Some` → push the parsed event
    /// to a dedicated worker and continue reading frames; the post-verify
    /// path runs from `drain_verify_results` on the next loop iteration.
    verify: Option<VerifyHandle>,
}

impl ShardDispatcher {
    pub(crate) fn new(
        config: Arc<RelayConfig>,
        sender: ServerSender,
        owner_id: u32,
        repl_tx: Option<ArcProducer<SubRepl>>,
        repl_rx: Option<ArcConsumer<SubRepl>>,
        storage: Option<ShardStorage>,
        verify: Option<VerifyHandle>,
    ) -> Self {
        Self {
            config,
            sender,
            owner_id,
            clients: HashMap::new(),
            client_order: VecDeque::new(),
            replica: HashMap::new(),
            repl_tx,
            repl_rx,
            storage,
            next_reader: 0,
            verify,
        }
    }

    /// Drain any pending replication messages from peers, applying them to
    /// the local replica. Call at the top of each loop iteration.
    pub fn drain_replication(&mut self) {
        let Some(rx) = self.repl_rx.as_mut() else {
            return;
        };
        while let Some(msg) = rx.pop() {
            match &*msg {
                SubRepl::Add {
                    owner,
                    client_id,
                    sub_id,
                    filters,
                } => {
                    let owner = *owner;
                    let client_id = *client_id;
                    if owner == self.owner_id {
                        continue; // skip our own echoes
                    }
                    self.replica
                        .entry((owner, client_id))
                        .or_default()
                        .insert(Arc::clone(sub_id), Arc::clone(filters));
                }
                SubRepl::Remove {
                    owner,
                    client_id,
                    sub_id,
                } => {
                    let owner = *owner;
                    let client_id = *client_id;
                    if owner == self.owner_id {
                        continue;
                    }
                    if let Some(subs) = self.replica.get_mut(&(owner, client_id)) {
                        subs.remove(sub_id.as_ref());
                        if subs.is_empty() {
                            self.replica.remove(&(owner, client_id));
                        }
                    }
                }
                SubRepl::ClientGone { owner, client_id } => {
                    let owner = *owner;
                    let client_id = *client_id;
                    if owner == self.owner_id {
                        continue;
                    }
                    self.replica.remove(&(owner, client_id));
                }
            }
        }
    }

    /// Push a replication message with backpressure. Broadcast rings block
    /// the caller until space is available; a REQ storm on one shard will
    /// slow that shard's own dispatch but can't stall peers.
    fn push_repl(&self, mut msg: SubRepl) {
        let Some(tx) = self.repl_tx.as_ref() else {
            return;
        };
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
                        std::thread::sleep(std::time::Duration::from_micros(10));
                    }
                    spins = spins.saturating_add(1);
                }
            }
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
        // at `max_clients`, so this is defensive.
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
        let had = self.clients.remove(&client_id).is_some();
        if let Some(pos) = self.client_order.iter().position(|&c| c == client_id) {
            self.client_order.remove(pos);
        }
        if had {
            // Tell peers to drop this client's replica entry.
            self.push_repl(SubRepl::ClientGone {
                owner: self.owner_id,
                client_id,
            });
        }
    }

    fn on_text(&mut self, client_id: i32, text: &str) {
        if let Some(max) = self.config.max_message_length
            && text.len() > max
        {
            self.sender.send_text(
                client_id,
                protocol::notice(&format!(
                    "invalid: message exceeds max_message_length ({max} bytes)"
                )),
            );
            return;
        }

        let parsed = match protocol::parse_view(text) {
            Ok(msg) => msg,
            Err(e) => {
                self.sender
                    .send_text(client_id, protocol::notice(&format!("invalid: {e}")));
                return;
            }
        };

        match parsed {
            ClientMessageView::Event { note, raw } => self.on_event(client_id, &note, raw.get()),
            ClientMessageView::Req { sub_id, filters } => self.on_req(client_id, sub_id, filters),
            ClientMessageView::Close { sub_id } => self.on_close_sub(client_id, sub_id),
            ClientMessageView::Unknown(verb) => {
                self.sender.send_text(
                    client_id,
                    protocol::notice(&format!("unsupported verb: {verb}")),
                );
            }
        }
    }

    fn on_event(&mut self, client_id: i32, note: &NostrNoteView<'_>, note_json: &str) {
        let id = note.id.as_deref().unwrap_or("");

        // Cheap pre-checks (length, tag count, drift). The expensive
        // schnorr verify is *not* done here; it gets offloaded if we have
        // a verify pool, or done inline as a fallback in the legacy path.
        if let Err(reason) = self.pre_validate_event(note) {
            self.sender
                .send_text(client_id, protocol::ok(id, false, reason));
            return;
        }

        // Decode hex once; both the verify-pool path and the storage
        // path want these as raw bytes, and the verify worker shouldn't
        // have to redo it.
        let mut event_id = [0u8; 32];
        let mut pubkey = [0u8; 32];
        if let Some(id_hex) = note.id.as_deref()
            && let Some(id_bytes) = decode_hex32(id_hex)
        {
            event_id = id_bytes;
        }
        if let Some(pk_bytes) = decode_hex32(note.pubkey.as_ref()) {
            pubkey = pk_bytes;
        }

        let raw_json: Arc<[u8]> = Arc::from(note_json.as_bytes());
        let event_id_hex: Arc<str> = Arc::from(id);
        let kind = note.kind;

        if self.verify.is_some() {
            // Offload: hand the parsed event to the verify worker and
            // return. The post-verify path (OK + fan-out + storage) runs
            // from `drain_verify_results` once the verdict comes back.
            let job = VerifyJob {
                raw_json,
                client_id,
                event_id_hex,
                event_id,
                pubkey,
                kind,
            };
            self.push_verify_job(job);
        } else {
            // Legacy inline path: verify on the I/O thread. Used by
            // tests and any caller that constructs a ShardDispatcher
            // without spawning a verify pool.
            let verified = note.verify();
            self.complete_event(VerifyResult {
                raw_json,
                client_id,
                event_id_hex,
                event_id,
                pubkey,
                kind,
                verified,
            });
        }
    }

    /// Push a verify job onto the shard's SPMC jobs ring and wake one
    /// worker. Any worker can pop any job, so the wake target is just a
    /// fairness hint — if it's busy, another worker will pop on its
    /// own park-timeout or via a peer's torch-pass.
    ///
    /// On full ring we back off briefly. The ring is sized to absorb
    /// bursts; sustained backpressure means verify is the bottleneck,
    /// at which point spinning is the right answer (slowing the
    /// shard's frame parser is exactly the desired backpressure).
    fn push_verify_job(&mut self, mut job: VerifyJob) {
        let Some(handle) = self.verify.as_mut() else {
            return;
        };
        let n = handle.worker_threads.len();
        if n == 0 {
            return;
        }
        let mut spins = 0u32;
        loop {
            match handle.jobs_tx.push(job) {
                Ok(()) => {
                    let idx = handle.next_wake_hint % n;
                    handle.next_wake_hint = handle.next_wake_hint.wrapping_add(1);
                    handle.worker_threads[idx].unpark();
                    return;
                }
                Err(returned) => {
                    job = returned;
                    if spins < 64 {
                        std::hint::spin_loop();
                    } else if spins < 256 {
                        std::thread::yield_now();
                    } else {
                        std::thread::sleep(std::time::Duration::from_micros(10));
                    }
                    spins = spins.saturating_add(1);
                }
            }
        }
    }

    /// Drain any verify verdicts that came back from our worker thread
    /// since the last loop iteration; run the post-verify path on each.
    pub fn drain_verify_results(&mut self) {
        // Take the consumer out so the borrow checker lets us call
        // self.complete_event in the loop body.
        let Some(mut handle) = self.verify.take() else {
            return;
        };
        while let Some(result) = handle.results_rx.pop() {
            self.complete_event(result);
        }
        self.verify = Some(handle);
    }

    /// Post-verify path: emit OK to the publisher, fan out to local /
    /// replicated subs, and queue persistence. Called either from
    /// `drain_verify_results` (offload path) or directly from `on_event`
    /// (legacy inline path).
    fn complete_event(&mut self, result: VerifyResult) {
        let id = result.event_id_hex.as_ref();
        if !result.verified {
            self.sender.send_text(
                result.client_id,
                protocol::ok(id, false, "invalid: bad signature or id"),
            );
            return;
        }

        self.sender
            .send_text(result.client_id, protocol::ok(id, true, ""));

        let note_bytes = result.raw_json;

        // Storage: fire-and-forget; parsing + indexing happens on the
        // storage thread. Drop persistence on ring overflow.
        if let Some(storage) = &self.storage {
            let req = WriteReq {
                raw_json: Arc::clone(&note_bytes),
                event_id: result.event_id,
                pubkey: result.pubkey,
                kind: result.kind,
            };
            if storage.write_tx.try_push(req).is_ok() {
                storage.storage_waker.unpark();
            }
        }

        // Re-parse the JSON for filter matching. This is cheaper than
        // shipping the parsed view across the verify-pool boundary
        // (which would force allocations to satisfy the borrow), and
        // serde_json::from_slice on a small EVENT body is microseconds.
        let view: NostrNoteView<'_> = match serde_json::from_slice(&note_bytes) {
            Ok(v) => v,
            Err(_) => return,
        };

        for (&other_id, state) in &self.clients {
            for (sub_id, filters) in &state.subs {
                if filters.iter().any(|f| filter::matches_view(&view, f)) {
                    self.sender.send_event_frame(
                        other_id,
                        Arc::clone(sub_id),
                        Arc::clone(&note_bytes),
                    );
                    break;
                }
            }
        }

        for (&(_owner, client_id), subs) in &self.replica {
            for (sub_id, filters) in subs {
                if filters.iter().any(|f| filter::matches_view(&view, f)) {
                    self.sender.send_event_frame(
                        client_id,
                        Arc::clone(sub_id),
                        Arc::clone(&note_bytes),
                    );
                    break;
                }
            }
        }
    }

    /// Cheap, allocation-free validation of an EVENT before the
    /// expensive schnorr verify. Splits out so the verify-pool path can
    /// run this on the I/O thread (rejecting obvious junk early without
    /// burdening the worker), then defer the schnorr math.
    fn pre_validate_event(&self, note: &NostrNoteView<'_>) -> Result<(), &'static str> {
        if let Some(max) = self.config.max_content_length
            && note.content.len() > max
        {
            return Err("invalid: content exceeds max_content_length");
        }
        if let Some(max) = self.config.max_event_tags
            && note.tags.len() > max
        {
            return Err("invalid: too many tags");
        }

        let now = NostrNote::now();
        if let Some(past) = self.config.max_past_drift
            && note.created_at < now.saturating_sub(past as i64)
        {
            return Err("invalid: created_at too far in the past");
        }
        if let Some(fut) = self.config.max_future_drift
            && note.created_at > now.saturating_add(fut as i64)
        {
            return Err("invalid: created_at too far in the future");
        }
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn validate_event(&self, note: &NostrNoteView<'_>) -> Result<(), &'static str> {
        self.pre_validate_event(note)?;
        if !note.verify() {
            return Err("invalid: bad signature or id");
        }
        Ok(())
    }

    fn on_req(&mut self, client_id: i32, sub_id: &str, filters: Vec<NostrSubscription>) {
        if let Some(max) = self.config.max_subid_length
            && sub_id.len() > max
        {
            self.sender.send_text(
                client_id,
                protocol::closed(sub_id, "invalid: sub_id exceeds max_subid_length"),
            );
            return;
        }
        if filters.len() > self.config.max_filters_per_sub {
            self.sender.send_text(
                client_id,
                protocol::closed(sub_id, "invalid: too many filters"),
            );
            return;
        }

        let Some(state) = self.clients.get_mut(&client_id) else {
            return;
        };

        let sub_id_arc: Arc<str> = Arc::from(sub_id);
        let filters_arc: Arc<[NostrSubscription]> = Arc::from(filters.into_boxed_slice());

        let evicted = state.insert_sub(
            Arc::clone(&sub_id_arc),
            Arc::clone(&filters_arc),
            self.config.max_subs_per_conn,
        );
        // End the mutable borrow of `state` here; subsequent code uses
        // `filters_arc` which we already own.
        let _ = state;

        if let Some(old_sub) = evicted {
            self.sender.send_text(
                client_id,
                protocol::closed(&old_sub, "rate-limited: subscription evicted (fifo)"),
            );
            self.push_repl(SubRepl::Remove {
                owner: self.owner_id,
                client_id,
                sub_id: old_sub,
            });
        }

        // Replicate the new sub to peer shards.
        self.push_repl(SubRepl::Add {
            owner: self.owner_id,
            client_id,
            sub_id: Arc::clone(&sub_id_arc),
            filters: Arc::clone(&filters_arc),
        });

        // Replay historical matches if storage is enabled; otherwise
        // immediate EOSE (ephemeral behavior). The reader thread sends
        // EOSE itself once the scan completes.
        if let Some(storage) = &self.storage {
            let job = ReqJob {
                client_fd: client_id,
                sub_id: Arc::clone(&sub_id_arc),
                filters: Arc::clone(&filters_arc),
            };
            storage.req_queue.push(job);
            let reader_idx = self.next_reader % storage.reader_wakers.len();
            self.next_reader = self.next_reader.wrapping_add(1);
            storage.reader_wakers[reader_idx].unpark();
        } else {
            // Replicate the new sub to peer shards (live-only mode).
            // NOTE: in storage mode we skip replication of subs since
            // historical replay covers both buckets AND live fan-out runs
            // locally; cross-shard sub-replication for live events still
            // works via the existing `replica` table update below in the
            // non-storage path.
            self.sender
                .send_text(client_id, protocol::eose(&sub_id_arc));
        }
    }

    fn on_close_sub(&mut self, client_id: i32, sub_id: &str) {
        if let Some(max) = self.config.max_subid_length
            && sub_id.len() > max
        {
            self.sender.send_text(
                client_id,
                protocol::notice("invalid: CLOSE sub_id exceeds max_subid_length"),
            );
            return;
        }
        let removed = if let Some(state) = self.clients.get_mut(&client_id) {
            state.remove_sub(sub_id)
        } else {
            false
        };
        if removed {
            self.push_repl(SubRepl::Remove {
                owner: self.owner_id,
                client_id,
                sub_id: Arc::from(sub_id.to_owned().into_boxed_str()),
            });
        }
    }
}

/// Reader-thread entry point: drive a [`ReaderCore`] and dispatch every
/// frame-level event through a [`ShardDispatcher`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_shard(
    mut accept_rx: AcceptStream,
    sender: ServerSender,
    config: Arc<RelayConfig>,
    shutdown: Arc<AtomicBool>,
    owner_id: u32,
    repl_tx: Option<ArcProducer<SubRepl>>,
    repl_rx: Option<ArcConsumer<SubRepl>>,
    storage: Option<ShardStorage>,
    verify: Option<VerifyHandle>,
) {
    let mut core = match ReaderCore::new(4096) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nostr shard fatal: ReaderCore::new: {e}");
            return;
        }
    };
    // Hand the verify workers our thread so they can unpark us
    // when a result lands. Without this we'd only notice verdicts
    // when an inbound TCP frame happened to wake the epoll wait —
    // a 10ms+ tail under low publisher rates.
    if let Some(handle) = verify.as_ref() {
        let _ = handle.shard_waker.set(std::thread::current());
    }

    let mut dispatcher = ShardDispatcher::new(
        config,
        sender.clone(),
        owner_id,
        repl_tx,
        repl_rx,
        storage,
        verify,
    );

    while !shutdown.load(Ordering::Acquire) {
        // 1. Apply any replication updates from peers first, so an EVENT in
        //    step 3 sees the latest sub view.
        dispatcher.drain_replication();
        // 1a. Drain verify-pool results so an EVENT verified on the
        //     previous loop turn fans out before we read more frames.
        dispatcher.drain_verify_results();

        // 2. Accept any new clients on our SPSC ring.
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

        // 3. Drive the I/O: parse + verify + fan-out all happen inline via
        //    ShardDispatcher::handle inside poll_once.
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
        let filters: Arc<[NostrSubscription]> =
            Arc::from(vec![NostrSubscription::default()].into_boxed_slice());
        assert!(
            state
                .insert_sub(Arc::from("a"), Arc::clone(&filters), 2)
                .is_none()
        );
        assert!(
            state
                .insert_sub(Arc::from("b"), Arc::clone(&filters), 2)
                .is_none()
        );
        let evicted = state.insert_sub(Arc::from("c"), filters, 2);
        assert_eq!(evicted.as_deref(), Some("a"));
        assert!(!state.subs.contains_key("a"));
        assert!(state.subs.contains_key("b"));
        assert!(state.subs.contains_key("c"));
    }

    #[test]
    fn client_state_replace_same_id() {
        let mut state = ClientState::default();
        let f: Arc<[NostrSubscription]> =
            Arc::from(vec![NostrSubscription::default()].into_boxed_slice());
        state.insert_sub(Arc::from("a"), Arc::clone(&f), 2);
        state.insert_sub(Arc::from("b"), Arc::clone(&f), 2);
        let evicted = state.insert_sub(Arc::from("a"), f, 2);
        assert!(evicted.is_none());
        assert_eq!(state.order.len(), 2);
    }
}
