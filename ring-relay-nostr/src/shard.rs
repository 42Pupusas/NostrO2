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
use tracing::{debug, error, trace, warn};

use crate::extension::{
    self, AdmitOutcome, Extension, MessageRef, OutboundDecision, OutboundFrame, OutboundKind,
    Session,
};
use crate::protocol::ClientMessageView;
use crate::storage::handle::{ReqJob, ReqTx, WriteReq, WriteTx};
use crate::storage::slot::decode_hex32;
use crate::verify_pool::{VerifyHandle, VerifyJob, VerifyResult, snapshot_match_view};
use crate::{AuthGate, RelayConfig, filter, protocol};

/// Generate a fresh 16-byte challenge string for NIP-42 AUTH. Hex
/// encoded so it's safe to include verbatim in a JSON array. We pull
/// from `OsRng` rather than a deterministic source because cheap and
/// unpredictable beats clever and deterministic — the schnorr
/// signature is what actually binds the AUTH; the challenge just
/// proves the AUTH was minted *for this connection* rather than
/// replayed.
fn generate_auth_challenge() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    let mut out = String::with_capacity(32);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in &buf {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Compare two relay URLs per NIP-42's "must match" rule.
///
/// Loose equality: case-insensitive scheme + host, case-sensitive
/// path. Trailing slashes are normalized so `wss://r.example` and
/// `wss://r.example/` compare equal. We don't do full URL
/// normalization (port elision, etc.) — operators should set
/// `relay_url` to exactly the URL clients connect to.
fn relay_url_matches(expected: &str, got: &str) -> bool {
    fn normalize(s: &str) -> String {
        let s = s.trim_end_matches('/');
        s.to_ascii_lowercase()
    }
    normalize(expected) == normalize(got)
}

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

/// Replicated subs from one peer-owned client (owner, client_id) → sub_id → filters.
type ReplicaClient = HashMap<Arc<str>, Arc<[NostrSubscription]>>;

/// Per-shard storage plumbing. When `None`, the shard runs ephemeral;
/// when `Some`, EVENTs are pushed to the storage thread and REQs dispatch
/// to the reader pool.
///
/// `WriteTx` is an MPSC producer so it is `Send + !Sync`; the shard owns
/// it outright (not behind `Arc`). The REQ producer is per-shard too
/// (cloned from the relay-level seed) so each shard gets a fresh batch
/// reservation without contending on a single shared producer cell.
pub(crate) struct ShardStorage {
    pub write_tx: WriteTx,
    pub req_tx: ReqTx,
    pub storage_waker: std::thread::Thread,
    /// Per-shard MPSC consumer for storage commit / drop verdicts.
    /// Drained at the top of every shard loop iteration; each ack
    /// becomes an `OK` frame to the original publisher.
    pub ack_rx: crate::storage::handle::AckRx,
    /// Set once the shard's main loop starts. Storage thread reads
    /// this to unpark us when an ack lands; without the unpark, the
    /// shard would only see the verdict on the next inbound frame.
    pub ack_waker: Arc<std::sync::OnceLock<std::thread::Thread>>,
}

/// Per-shard Nostr dispatcher. One instance per reader thread.
pub struct ShardDispatcher {
    config: Arc<RelayConfig>,
    sender: ServerSender,
    /// This shard's index, stamped into every outgoing SubRepl message so
    /// peers can skip our own broadcasts on readback.
    owner_id: u32,
    clients: HashMap<i32, Session>,
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
    /// Schnorr-verify offload. `None` → verify inline on the I/O thread
    /// (legacy path, only used in tests). `Some` → push the parsed event
    /// to a dedicated worker and continue reading frames; the post-verify
    /// path runs from `drain_verify_results` on the next loop iteration.
    verify: Option<VerifyHandle>,
    /// Snapshot of the relay's extension list. `Arc<dyn Extension>` is
    /// cloned once per shard at construction; the slice is read-only on
    /// the hot path, so an empty list reduces to a single length check.
    extensions: Arc<[Arc<dyn Extension>]>,
    /// Outbound frames produced inside `on_connect` (e.g. NIP-42 AUTH
    /// challenge) that must be sent *after* `ServerSender::register`
    /// runs in the accept loop — otherwise the writer doesn't yet
    /// know about the fd and silently drops the frame. Drained per
    /// fd by `take_post_register_frame`.
    post_register_frames: HashMap<i32, String>,
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
        let extensions: Arc<[Arc<dyn Extension>]> = Arc::from(config.extensions.clone());
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
            verify,
            extensions,
            post_register_frames: HashMap::new(),
        }
    }

    /// Pull any frame `on_connect` enqueued for `fd`. Called by
    /// `run_shard` immediately after `ServerSender::register(fd, ...)`
    /// so the writer is ready to receive.
    pub(crate) fn take_post_register_frame(&mut self, fd: i32) -> Option<String> {
        self.post_register_frames.remove(&fd)
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
                    spins = crate::backoff::step(spins);
                }
            }
        }
    }

    /// Handle one frame-level event from the reader core.
    pub fn handle(&mut self, event: ReaderEvent<'_>) {
        match event {
            ReaderEvent::Connected { fd, headers, .. } => self.on_connect(fd, &headers),
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

    fn on_connect(&mut self, client_id: i32, headers: &[(String, String)]) {
        // Per-shard FIFO eviction. The server layer already caps total fds
        // at `max_clients`, so this is defensive.
        while self.client_order.len() >= self.config.max_clients {
            if let Some(old) = self.client_order.pop_front() {
                self.clients.remove(&old);
                warn!(fd = old, "evicting client: shard at capacity");
                self.dispatch_text(old, protocol::notice("evicted: relay at capacity"));
                self.sender
                    .close_client(old, ring_relay_server::CloseCode::PolicyViolation);
            } else {
                break;
            }
        }

        // Resolve real client IP from configured trust header, if any.
        let remote_ip = self
            .config
            .trusted_ip_header
            .as_deref()
            .and_then(|h| extension::extract_ip(headers, h));

        let mut session = Session::new(client_id, remote_ip);
        // NIP-42: if AUTH is configured, mint a per-connection challenge
        // and stash it on the session before extensions run. Extensions
        // can read the challenge but shouldn't rewrite it.
        if self.config.auth.is_some() {
            let challenge = generate_auth_challenge();
            let frame = protocol::auth_challenge(&challenge);
            session.auth_challenge = Some(challenge.into_boxed_str());
            // Defer the actual send: `core.accept` fires `Connected`
            // before `sender.register(fd, ...)` runs, so writing now
            // would land on an unregistered fd and silently drop. The
            // accept loop drains this map right after `register`.
            self.post_register_frames.insert(client_id, frame);
        }
        for ext in self.extensions.iter() {
            ext.on_connect(&mut session);
        }
        debug!(fd = client_id, ?remote_ip, "client connected");
        self.clients.insert(client_id, session);
        self.client_order.push_back(client_id);
    }

    fn on_disconnect(&mut self, client_id: i32) {
        let removed = self.clients.remove(&client_id);
        if let Some(pos) = self.client_order.iter().position(|&c| c == client_id) {
            self.client_order.remove(pos);
        }
        if let Some(mut session) = removed {
            for ext in self.extensions.iter() {
                ext.on_disconnect(&mut session);
            }
            debug!(fd = client_id, "client disconnected");
            // Tell peers to drop this client's replica entry.
            self.push_repl(SubRepl::ClientGone {
                owner: self.owner_id,
                client_id,
            });
        }
    }

    /// Funnel for outbound text frames. Walks the extension list (no-op when
    /// empty) and forwards to the sender unless an extension dropped the
    /// frame. Centralizing here means new control verbs don't have to
    /// thread the hook through manually.
    fn dispatch_text(&mut self, fd: i32, frame: String) {
        if self.extensions.is_empty() {
            self.sender.send_text(fd, frame);
            return;
        }
        let Some(session) = self.clients.get_mut(&fd) else {
            // No session (e.g. eviction sequence above): bypass extensions
            // — they observe by-session, and there's nothing to observe.
            self.sender.send_text(fd, frame);
            return;
        };
        let outbound = OutboundFrame {
            fd,
            kind: OutboundKind::Text(frame.as_str()),
        };
        let mut decision = OutboundDecision::Forward;
        for ext in self.extensions.iter() {
            if let OutboundDecision::Drop = ext.on_outbound(&outbound, session) {
                decision = OutboundDecision::Drop;
                break;
            }
        }
        if matches!(decision, OutboundDecision::Forward) {
            self.sender.send_text(fd, frame);
        }
    }

    /// Outbound funnel for the verbatim-splice EVENT fan-out path.
    fn dispatch_event_frame(&mut self, fd: i32, sub_id: Arc<str>, note_bytes: Arc<[u8]>) {
        if self.extensions.is_empty() {
            self.sender.send_event_frame(fd, sub_id, note_bytes);
            return;
        }
        let Some(session) = self.clients.get_mut(&fd) else {
            self.sender.send_event_frame(fd, sub_id, note_bytes);
            return;
        };
        let outbound = OutboundFrame {
            fd,
            kind: OutboundKind::Event {
                sub_id: sub_id.as_ref(),
                note_json: note_bytes.as_ref(),
            },
        };
        let mut decision = OutboundDecision::Forward;
        for ext in self.extensions.iter() {
            if let OutboundDecision::Drop = ext.on_outbound(&outbound, session) {
                decision = OutboundDecision::Drop;
                break;
            }
        }
        if matches!(decision, OutboundDecision::Forward) {
            self.sender.send_event_frame(fd, sub_id, note_bytes);
        }
    }

    fn on_text(&mut self, client_id: i32, text: &str) {
        if let Some(max) = self.config.max_message_length
            && text.len() > max
        {
            warn!(fd = client_id, len = text.len(), max, "frame exceeds max_message_length");
            self.dispatch_text(
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
                warn!(fd = client_id, error = %e, "frame parse failed");
                self.dispatch_text(client_id, protocol::notice(&format!("invalid: {e}")));
                return;
            }
        };

        // Run extension admission against a borrowed view of the parsed
        // message, before any validate / verify / fan-out / storage step.
        // Extensions can short-circuit with a reply or silently drop.
        if !self.extensions.is_empty() {
            let msg_ref = match &parsed {
                ClientMessageView::Event { note, .. } => MessageRef::Event(note),
                ClientMessageView::Req { sub_id, filters } => MessageRef::Req {
                    sub_id,
                    filters: filters.as_slice(),
                },
                ClientMessageView::Close { sub_id } => MessageRef::Close { sub_id },
                ClientMessageView::Auth { note, .. } => MessageRef::Auth(note),
                ClientMessageView::Unknown(verb) => MessageRef::Unknown(verb),
            };
            let outcome = if let Some(session) = self.clients.get_mut(&client_id) {
                extension::run_admission(&self.extensions, &msg_ref, session)
            } else {
                AdmitOutcome::Continue
            };
            match outcome {
                AdmitOutcome::Continue => {}
                AdmitOutcome::Reply(frame) => {
                    self.dispatch_text(client_id, frame);
                    return;
                }
                AdmitOutcome::Drop => return,
            }
        }

        match parsed {
            ClientMessageView::Event { note, raw } => {
                trace!(fd = client_id, kind = note.kind, "event accepted for processing");
                self.on_event(client_id, &note, raw.get());
            }
            ClientMessageView::Req { sub_id, filters } => {
                debug!(fd = client_id, %sub_id, n_filters = filters.len(), "req received");
                self.on_req(client_id, sub_id, filters);
            }
            ClientMessageView::Close { sub_id } => {
                debug!(fd = client_id, %sub_id, "close received");
                self.on_close_sub(client_id, sub_id);
            }
            ClientMessageView::Auth { note, .. } => {
                debug!(fd = client_id, "auth attempt");
                self.on_auth(client_id, &note);
            }
            ClientMessageView::Unknown(verb) => {
                debug!(fd = client_id, %verb, "unsupported verb");
                self.dispatch_text(
                    client_id,
                    protocol::notice(&format!("unsupported verb: {verb}")),
                );
            }
        }
    }

    fn on_event(&mut self, client_id: i32, note: &NostrNoteView<'_>, note_json: &str) {
        let id = note.id.as_deref().unwrap_or("");

        // NIP-42: write-side auth gate. Refuse before any decode work so
        // unauthed publishers don't even pay for the hex check.
        if matches!(
            self.config.auth.as_ref().and_then(|a| a.gate),
            Some(AuthGate::Write | AuthGate::All)
        ) && self
            .clients
            .get(&client_id)
            .is_none_or(|s| s.authed_pubkey.is_none())
        {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: this relay requires AUTH for EVENT"),
            );
            return;
        }

        // Decode hex up front. Both the verify-pool and storage paths
        // need raw bytes, and we refuse to carry zero-byte fallbacks
        // anywhere downstream — a malformed id/pubkey would otherwise
        // index events under all-zero keys and let separate clients
        // collide on a shared "broken" slot in the replaceable bucket.
        let Some(id_bytes) = note.id.as_deref().and_then(decode_hex32) else {
            self.dispatch_text(client_id, protocol::ok(id, false, "invalid: malformed id"));
            return;
        };
        let Some(pk_bytes) = decode_hex32(note.pubkey.as_ref()) else {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "invalid: malformed pubkey"),
            );
            return;
        };

        // Cheap pre-checks (length, tag count, drift). The expensive
        // schnorr verify is *not* done here; it gets offloaded if we have
        // a verify pool, or done inline as a fallback in the legacy path.
        if let Err(reason) = self.pre_validate_event(note, &id_bytes) {
            self.dispatch_text(client_id, protocol::ok(id, false, reason));
            return;
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
                event_id: id_bytes,
                pubkey: pk_bytes,
                kind,
                source_shard: self.owner_id as u16,
            };
            self.push_verify_job(job);
        } else {
            // Legacy inline path: verify on the I/O thread. Used by
            // tests and any caller that constructs a ShardDispatcher
            // without spawning a verify pool.
            let verified = note.verify();
            // Mirror the verify-pool worker: only build the matcher
            // snapshot for events we'll actually fan out.
            let view = if verified {
                Some(Arc::new(snapshot_match_view(note, &event_id_hex)))
            } else {
                None
            };
            self.complete_event(VerifyResult {
                raw_json,
                client_id,
                event_id_hex,
                event_id: id_bytes,
                pubkey: pk_bytes,
                kind,
                verified,
                view,
            });
        }
    }

    /// Push a verify job onto the global MPMC jobs ring. The ring's
    /// futex-style wake bitmap handles worker wakeups; we don't need
    /// explicit `unpark`. `push_block` parks the shard if the ring is
    /// full — sustained backpressure means verify is the bottleneck,
    /// and slowing the shard's frame parser is exactly the desired
    /// shape of backpressure. Returns Err only when every worker has
    /// dropped (relay shutdown), in which case we discard quietly.
    fn push_verify_job(&mut self, job: VerifyJob) {
        let Some(handle) = self.verify.as_mut() else {
            return;
        };
        let _ = handle.jobs_tx.push_block(job);
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

    /// Drain commit / drop verdicts the storage thread has emitted
    /// since the last loop iteration; turn each into an `OK` frame to
    /// the publisher. This is what makes `OK=true` truthful in storage
    /// mode — the shard no longer fires the OK at verify time but
    /// waits for the verdict to land here.
    ///
    /// `Stored` and `Duplicate` both produce `OK=true`. `Duplicate`
    /// carries a non-empty message so a curious client can distinguish
    /// the two; NIP-01 says relays SHOULD treat duplicates as success.
    pub fn drain_storage_acks(&mut self) {
        // Take the storage handle out so we can borrow self mutably
        // for `dispatch_text` while we're inside the drain loop.
        let Some(mut storage) = self.storage.take() else {
            return;
        };
        while let Some(ack) = storage.ack_rx.0.pop() {
            let id = ack.event_id_hex.as_ref();
            match ack.outcome {
                crate::storage::handle::AckOutcome::Stored => {
                    self.dispatch_text(ack.client_id, protocol::ok(id, true, ""));
                }
                crate::storage::handle::AckOutcome::Duplicate => {
                    self.dispatch_text(
                        ack.client_id,
                        protocol::ok(id, true, "duplicate: already have this event"),
                    );
                }
                crate::storage::handle::AckOutcome::Rejected(reason) => {
                    warn!(fd = ack.client_id, %id, reason, "storage rejected event");
                    self.dispatch_text(ack.client_id, protocol::ok(id, false, reason));
                }
            }
        }
        self.storage = Some(storage);
    }

    /// Post-verify path: queue persistence (ack flows back via the
    /// storage results ring), fan out live to subscribers. The publisher's
    /// `OK` is sent only after the storage thread reports back via
    /// [`Self::drain_storage_acks`] — that way `OK=true` is truthful
    /// about whether the write actually committed, and `OK=false` carries
    /// the real reason (deleted id, address-deleted, oversized, ring
    /// overflow). Verify failure still goes out immediately because no
    /// storage step is involved.
    fn complete_event(&mut self, result: VerifyResult) {
        let id = result.event_id_hex.as_ref();
        if !result.verified {
            warn!(fd = result.client_id, %id, "event verify failed");
            self.dispatch_text(
                result.client_id,
                protocol::ok(id, false, "invalid: bad signature or id"),
            );
            return;
        }

        let note_bytes = result.raw_json;

        // Storage: parsing + indexing happens on the storage thread.
        // The ack (commit / duplicate / reject) flows back via the
        // storage results ring; the shard sends `OK` only once it
        // hears the verdict — see `drain_storage_acks`.
        if let Some(storage) = &self.storage {
            let req = WriteReq {
                raw_json: Arc::clone(&note_bytes),
                event_id: result.event_id,
                event_id_hex: Arc::clone(&result.event_id_hex),
                pubkey: result.pubkey,
                kind: result.kind,
                client_id: result.client_id,
                source_shard: self.owner_id as u16,
            };
            if storage.write_tx.try_push(req).is_ok() {
                storage.storage_waker.unpark();
            } else {
                // Ring overflow: storage thread couldn't keep up. Tell
                // the publisher honestly that we dropped it. This path
                // is rare in practice — write_ring_capacity * shards
                // gives plenty of headroom — but a sustained burst can
                // hit it.
                warn!(fd = result.client_id, %id, "storage write ring full; dropping event");
                self.dispatch_text(
                    result.client_id,
                    protocol::ok(id, false, "error: storage backlogged"),
                );
            }
        } else {
            // Ephemeral mode: no storage step, OK truthfully reflects
            // the verify result. Same behavior the relay had before
            // the truthful-OK change.
            self.dispatch_text(result.client_id, protocol::ok(id, true, ""));
        }

        // The verify worker's pre-built `MatchView` is required for
        // the fan-out match loop. A verified event always carries
        // one (the worker only elides it on `verified == false`, and
        // we returned early above) — defensive bail-out otherwise.
        let Some(match_view) = result.view.as_deref() else {
            return;
        };

        // Two paths:
        // - Empty extension list (default, hot path): walk the table
        //   in place and call `self.sender.send_event_frame` directly.
        //   Zero allocations beyond the existing Arc clones.
        // - Non-empty extensions: collect matching (fd, sub_id) pairs
        //   first so we can drop the immutable borrow on `self.clients`
        //   and route through `dispatch_event_frame` (which needs
        //   `&mut self` to look up the session for the outbound hook).
        if self.extensions.is_empty() {
            for (&other_id, session) in &self.clients {
                for (sub_id, filters) in session.subs() {
                    if filters
                        .iter()
                        .any(|f| filter::matches_match_view(match_view, f))
                    {
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
                    if filters
                        .iter()
                        .any(|f| filter::matches_match_view(match_view, f))
                    {
                        self.sender.send_event_frame(
                            client_id,
                            Arc::clone(sub_id),
                            Arc::clone(&note_bytes),
                        );
                        break;
                    }
                }
            }
        } else {
            let mut targets: Vec<(i32, Arc<str>)> = Vec::new();
            for (&other_id, session) in &self.clients {
                for (sub_id, filters) in session.subs() {
                    if filters
                        .iter()
                        .any(|f| filter::matches_match_view(match_view, f))
                    {
                        targets.push((other_id, Arc::clone(sub_id)));
                        break;
                    }
                }
            }
            for (&(_owner, client_id), subs) in &self.replica {
                for (sub_id, filters) in subs {
                    if filters
                        .iter()
                        .any(|f| filter::matches_match_view(match_view, f))
                    {
                        targets.push((client_id, Arc::clone(sub_id)));
                        break;
                    }
                }
            }
            for (fd, sub_id) in targets {
                self.dispatch_event_frame(fd, sub_id, Arc::clone(&note_bytes));
            }
        }
    }

    /// Cheap, allocation-free validation of an EVENT before the
    /// expensive schnorr verify. Splits out so the verify-pool path can
    /// run this on the I/O thread (rejecting obvious junk early without
    /// burdening the worker), then defer the schnorr math.
    fn pre_validate_event(
        &self,
        note: &NostrNoteView<'_>,
        id_bytes: &[u8; 32],
    ) -> Result<(), &'static str> {
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
        // NIP-40: reject events whose `expiration` tag is already in the
        // past. Cheap to check here before the verify pool sees the event,
        // since most expired-on-arrival events come from clock skew or
        // replays and signatures verify is the expensive step.
        if let Some(exp) = filter::expiration_from_view(note)
            && exp <= now
        {
            return Err("invalid: event expired");
        }
        // NIP-13: minimum proof-of-work. Counts leading zero bits on
        // the raw 32-byte event id. Runs before schnorr verify because
        // the bit count is essentially free and ids that don't meet the
        // bar can't be cheaply forged anyway (the id is the sha256 of
        // the canonical serialization). 0 disables.
        let min_pow = self.config.min_pow_difficulty;
        if min_pow > 0 && filter::leading_zero_bits(id_bytes) < min_pow {
            return Err("pow: insufficient difficulty");
        }
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn validate_event(
        &self,
        note: &NostrNoteView<'_>,
        id_bytes: &[u8; 32],
    ) -> Result<(), &'static str> {
        self.pre_validate_event(note, id_bytes)?;
        if !note.verify() {
            return Err("invalid: bad signature or id");
        }
        Ok(())
    }

    fn on_req(&mut self, client_id: i32, sub_id: &str, filters: Vec<NostrSubscription>) {
        if let Some(max) = self.config.max_subid_length
            && sub_id.len() > max
        {
            self.dispatch_text(
                client_id,
                protocol::closed(sub_id, "invalid: sub_id exceeds max_subid_length"),
            );
            return;
        }
        if filters.len() > self.config.max_filters_per_sub {
            self.dispatch_text(
                client_id,
                protocol::closed(sub_id, "invalid: too many filters"),
            );
            return;
        }

        // NIP-42: read-side auth gate. If `auth.gate` requires auth for
        // reads and this connection hasn't authed yet, refuse the REQ
        // with `auth-required:` per the spec. The client should reply
        // by signing the AUTH challenge and re-issuing the REQ.
        if matches!(
            self.config.auth.as_ref().and_then(|a| a.gate),
            Some(AuthGate::Read | AuthGate::All)
        ) && self
            .clients
            .get(&client_id)
            .is_none_or(|s| s.authed_pubkey.is_none())
        {
            self.dispatch_text(
                client_id,
                protocol::closed(sub_id, "auth-required: this relay requires AUTH for REQ"),
            );
            return;
        }

        let sub_id_arc: Arc<str> = Arc::from(sub_id);
        let filters_arc: Arc<[NostrSubscription]> = Arc::from(filters.into_boxed_slice());

        let evicted = {
            let Some(state) = self.clients.get_mut(&client_id) else {
                return;
            };
            state.insert_sub(
                Arc::clone(&sub_id_arc),
                Arc::clone(&filters_arc),
                self.config.max_subs_per_conn,
            )
        };

        if let Some(old_sub) = evicted {
            warn!(fd = client_id, evicted = %old_sub, "subscription evicted (fifo)");
            self.dispatch_text(
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
        // immediate EOSE (ephemeral behavior). In storage mode the reader
        // pool emits EOSE once its scan completes. Live fan-out (including
        // cross-shard) is unaffected — `push_repl(SubRepl::Add)` above
        // already replicated this sub to peer shards regardless of mode.
        if let Some(storage) = &self.storage {
            let job = ReqJob {
                client_fd: client_id,
                sub_id: Arc::clone(&sub_id_arc),
                filters: Arc::clone(&filters_arc),
            };
            // Backpressure via mpmc push_block: blocks the shard if the
            // reader pool is saturated rather than dropping the REQ.
            // Returns Err(_) only when every reader has dropped, which
            // means the relay is shutting down — discard quietly.
            let _ = storage.req_tx.submit(job);
        } else {
            self.dispatch_text(client_id, protocol::eose(&sub_id_arc));
        }
    }

    fn on_close_sub(&mut self, client_id: i32, sub_id: &str) {
        if let Some(max) = self.config.max_subid_length
            && sub_id.len() > max
        {
            self.dispatch_text(
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

    /// Validate a NIP-42 AUTH event sent by the client.
    ///
    /// Per NIP-42 the event must be:
    ///   - kind 22242
    ///   - signed (id matches sha256 of canonical form, schnorr valid)
    ///   - signed by the pubkey it claims (implicit from the verify)
    ///   - carrying a `relay` tag whose value matches our `relay_url`
    ///   - carrying a `challenge` tag matching the one we issued
    ///   - `created_at` within `max_clock_skew_secs` of `now`
    ///
    /// On success we stash the hex-decoded pubkey in
    /// [`Session::authed_pubkey`] and reply `OK=true`. On any failure
    /// we reply `OK=false "auth-required: <reason>"` (NIP-42 mandates
    /// the prefix). The connection stays open either way.
    fn on_auth(&mut self, client_id: i32, note: &NostrNoteView<'_>) {
        let id = note.id.as_deref().unwrap_or("");

        let Some(auth_cfg) = self.config.auth.as_ref() else {
            // AUTH wasn't enabled — should not happen because we'd
            // never have sent a challenge, but be defensive.
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: AUTH not enabled"),
            );
            return;
        };

        if note.kind != 22242 {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: AUTH event must be kind 22242"),
            );
            return;
        }

        // Schnorr verify happens here inline; AUTH is rare so we don't
        // bother offloading to the verify pool.
        if !note.verify() {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: bad signature or id"),
            );
            return;
        }

        let now = NostrNote::now();
        if (note.created_at - now).abs() > auth_cfg.max_clock_skew_secs {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: created_at outside skew window"),
            );
            return;
        }

        // Walk tags exactly once. We need the relay URL and the
        // challenge — first match wins for each.
        let mut got_relay: Option<&str> = None;
        let mut got_challenge: Option<&str> = None;
        for row in note.tags.iter() {
            let Some(name) = row.first().map(AsRef::as_ref) else {
                continue;
            };
            let Some(value) = row.get(1).map(AsRef::as_ref) else {
                continue;
            };
            match name {
                "relay" if got_relay.is_none() => got_relay = Some(value),
                "challenge" if got_challenge.is_none() => got_challenge = Some(value),
                _ => {}
            }
        }

        let Some(relay_tag) = got_relay else {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: missing relay tag"),
            );
            return;
        };
        if !relay_url_matches(&auth_cfg.relay_url, relay_tag) {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: relay tag does not match"),
            );
            return;
        }

        let Some(challenge_tag) = got_challenge else {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: missing challenge tag"),
            );
            return;
        };
        let session_challenge = self
            .clients
            .get(&client_id)
            .and_then(|s| s.auth_challenge.as_deref());
        let Some(expected) = session_challenge else {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: no challenge issued"),
            );
            return;
        };
        if challenge_tag != expected {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: challenge mismatch"),
            );
            return;
        }

        let Some(pk_bytes) = decode_hex32(note.pubkey.as_ref()) else {
            self.dispatch_text(
                client_id,
                protocol::ok(id, false, "auth-required: malformed pubkey"),
            );
            return;
        };

        if let Some(session) = self.clients.get_mut(&client_id) {
            session.authed_pubkey = Some(pk_bytes);
            // The challenge can't be reused — a NIP-42 challenge is
            // one-shot. Clearing it forces any future re-AUTH to use a
            // fresh challenge (which we'd issue on the next connect).
            session.auth_challenge = None;
        }
        debug!(fd = client_id, pubkey = %note.pubkey.as_ref(), "auth accepted");
        self.dispatch_text(client_id, protocol::ok(id, true, ""));
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
            error!(error = %e, owner_id, "shard fatal: ReaderCore::new");
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
    // Same wakeup discipline for the storage ack ring: storage thread
    // emits OK / OK=false verdicts asynchronously, and we want them
    // delivered to publishers without waiting for the next inbound
    // frame to wake the epoll.
    if let Some(s) = storage.as_ref() {
        let _ = s.ack_waker.set(std::thread::current());
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
        // 1b. Drain storage acks so a publisher whose write committed
        //     (or got dropped) on the previous loop turn gets its
        //     OK / OK=false before we read more frames. This is what
        //     makes OK truthful: storage submits asynchronously and
        //     reports back here, not at verify time.
        dispatcher.drain_storage_acks();

        // 2. Accept any new clients on our SPSC ring.
        while let Some(client) = accept_rx.pop() {
            let fd = client.fd;
            let deflate = client.deflate.clone();
            core.accept(client, |event| dispatcher.handle(event));
            sender.register(fd, deflate);
            // NIP-42 challenge (and any future on_connect outbound) goes
            // out *after* register so the writer knows the fd.
            if let Some(frame) = dispatcher.take_post_register_frame(fd) {
                sender.send_text(fd, frame);
            }
        }

        if core.is_idle() {
            core.park_timeout();
            continue;
        }

        // 3. Drive the I/O: parse + verify + fan-out all happen inline via
        //    ShardDispatcher::handle inside poll_once.
        if let Err(e) = core.poll_once(|event| dispatcher.handle(event)) {
            error!(error = %e, owner_id, "shard fatal: poll_once");
            return;
        }
    }

    core.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_fifo_eviction() {
        let mut state = Session::new(1, None);
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
        assert!(!state.subs().contains_key("a"));
        assert!(state.subs().contains_key("b"));
        assert!(state.subs().contains_key("c"));
    }

    #[test]
    fn session_replace_same_id() {
        let mut state = Session::new(1, None);
        let f: Arc<[NostrSubscription]> =
            Arc::from(vec![NostrSubscription::default()].into_boxed_slice());
        state.insert_sub(Arc::from("a"), Arc::clone(&f), 2);
        state.insert_sub(Arc::from("b"), Arc::clone(&f), 2);
        let evicted = state.insert_sub(Arc::from("a"), f, 2);
        assert!(evicted.is_none());
        assert_eq!(state.subs().len(), 2);
    }
}
