//! Extension seam for ring-relay-nostr.
//!
//! Inspired by `nostr-relay 0.4.8`'s `Extension` trait, adapted to our
//! single-threaded-per-shard model: synchronous, borrowed messages, no
//! actors, no boxing per call. Future NIPs (NIP-42 AUTH, NIP-70 protected
//! events, NIP-09 deletion routing) and ops features (rate limit, allow /
//! block lists, paid-relay gating) plug in here without touching the shard
//! hot path.
//!
//! ## Lifetime model
//!
//! Every hook is invoked from the reader I/O thread that owns the
//! [`Session`]. Extensions therefore receive `&mut Session` directly,
//! without a lock. They are stored as `Arc<dyn Extension>` on the relay
//! and shared across shards: each shard reads its own slice of the
//! extension table, never mutates the trait object. If an extension needs
//! cross-connection state, it owns its own interior mutability.
//!
//! ## Action model
//!
//! - [`ExtensionAction::Continue`] hands the message to the next extension,
//!   or to the dispatcher if all return Continue.
//! - [`ExtensionAction::Stop`] short-circuits: the carried frame (if any)
//!   is sent to the client and the dispatcher does no further processing.
//! - [`ExtensionAction::Drop`] short-circuits silently — the dispatcher
//!   neither processes the message nor sends anything.
//!
//! On outbound, [`OutboundDecision::Forward`] passes through and
//! [`OutboundDecision::Drop`] suppresses the frame. There is no rewrite
//! variant in v1 — extensions that need to alter outbound frames can
//! `Drop` and queue their own via the connection's session state.

use std::any::{Any, TypeId};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use nostro2::{NostrNoteView, NostrSubscription};

/// Header value scan helper used by the dispatcher when
/// [`crate::RelayConfig::trusted_ip_header`] is set.
///
/// The first comma-separated entry of the matching header is parsed as an
/// `IpAddr`. Returns `None` if the header is absent or the value is not a
/// valid IP. Behind a trusted proxy, the first entry is the real client.
#[must_use]
pub fn extract_ip(headers: &[(String, String)], header_name: &str) -> Option<IpAddr> {
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(header_name) {
            let first = v.split(',').next().unwrap_or("").trim();
            if let Ok(ip) = first.parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    None
}

/// Per-connection state owned by the shard. Replaces the older internal
/// `ClientState`. All access is single-threaded on the I/O thread that
/// accepted the client.
pub struct Session {
    pub fd: i32,
    pub remote_ip: Option<IpAddr>,
    pub connected_at: Instant,
    /// Populated by a future NIP-42 extension; `None` means unauthed.
    pub authed_pubkey: Option<[u8; 32]>,
    /// Populated by a future NIP-42 extension when issuing AUTH challenges.
    pub auth_challenge: Option<Box<str>>,
    pub(crate) subs: HashMap<Arc<str>, Arc<[NostrSubscription]>>,
    pub(crate) sub_order: VecDeque<Arc<str>>,
    /// Extension scratch storage, keyed by `TypeId`. Mirrors the pattern
    /// `nostr-relay 0.4.8` uses on `Setting`. Each extension keeps its
    /// per-connection state under its own type without us pre-declaring it.
    ext: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Session {
    pub(crate) fn new(fd: i32, remote_ip: Option<IpAddr>) -> Self {
        Self {
            fd,
            remote_ip,
            connected_at: Instant::now(),
            authed_pubkey: None,
            auth_challenge: None,
            subs: HashMap::new(),
            sub_order: VecDeque::new(),
            ext: HashMap::new(),
        }
    }

    /// Insert the extension's per-connection state. Replaces any prior
    /// value of the same type.
    pub fn set_ext<T: Send + Sync + 'static>(&mut self, value: T) {
        self.ext.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Borrow the extension's per-connection state, if set.
    #[must_use]
    pub fn ext<T: 'static>(&self) -> Option<&T> {
        self.ext
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Mutably borrow the extension's per-connection state, if set.
    pub fn ext_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.ext
            .get_mut(&TypeId::of::<T>())
            .and_then(|b| b.downcast_mut::<T>())
    }

    pub(crate) fn insert_sub(
        &mut self,
        sub_id: Arc<str>,
        filters: Arc<[NostrSubscription]>,
        cap: usize,
    ) -> Option<Arc<str>> {
        if self.subs.remove(&sub_id).is_some()
            && let Some(pos) = self.sub_order.iter().position(|s| s == &sub_id)
        {
            self.sub_order.remove(pos);
        }

        let evicted = if self.subs.len() >= cap {
            self.sub_order.pop_front().inspect(|old| {
                self.subs.remove(old);
            })
        } else {
            None
        };

        self.sub_order.push_back(Arc::clone(&sub_id));
        self.subs.insert(sub_id, filters);
        evicted
    }

    pub(crate) fn remove_sub(&mut self, sub_id: &str) -> bool {
        if self.subs.remove(sub_id).is_some() {
            if let Some(pos) = self.sub_order.iter().position(|s| s.as_ref() == sub_id) {
                self.sub_order.remove(pos);
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn subs(&self) -> &HashMap<Arc<str>, Arc<[NostrSubscription]>> {
        &self.subs
    }
}

/// Borrowed view of a parsed NIP-01 client message handed to extensions.
/// Mirrors [`crate::ClientMessageView`] but exposes only what extensions
/// need to make admit / deny decisions, never internal raw-JSON pointers.
pub enum MessageRef<'a> {
    Event(&'a NostrNoteView<'a>),
    Req {
        sub_id: &'a str,
        filters: &'a [NostrSubscription],
    },
    Close {
        sub_id: &'a str,
    },
    /// Any verb that's not part of NIP-01. AUTH and COUNT will land here
    /// until a future extension claims them.
    Unknown(&'a str),
}

/// Borrowed view of an outbound frame the dispatcher is about to send.
/// Extensions inspect-but-don't-rewrite in v1; return [`OutboundDecision::Drop`]
/// to suppress.
pub struct OutboundFrame<'a> {
    pub fd: i32,
    pub kind: OutboundKind<'a>,
}

/// What kind of outbound frame is being dispatched. `Text` covers all
/// JSON-array control frames (OK / EOSE / CLOSED / NOTICE); `Event` is
/// the verbatim-splice fan-out path.
pub enum OutboundKind<'a> {
    Text(&'a str),
    Event { sub_id: &'a str, note_json: &'a [u8] },
}

/// What an extension wants the dispatcher to do with an inbound message.
pub enum ExtensionAction {
    /// Hand off to the next extension (or to the dispatcher if last).
    Continue,
    /// Short-circuit. Send the carried frame to the client (if `Some`),
    /// then drop the message.
    Stop(Option<String>),
    /// Short-circuit silently — neither process nor reply.
    Drop,
}

/// What an extension wants the dispatcher to do with an outbound frame.
#[derive(Debug, Clone, Copy)]
pub enum OutboundDecision {
    Forward,
    Drop,
}

/// Extension hook trait. All hooks are sync; default impls do nothing,
/// so concrete extensions only override the points they care about.
pub trait Extension: Send + Sync {
    /// Stable identifier used in tracing events. Should be a short
    /// kebab-case name.
    fn name(&self) -> &'static str;

    /// Called once after a fresh client is registered with the shard.
    fn on_connect(&self, _session: &mut Session) {}

    /// Called once when the client disconnects, before [`Session`] is
    /// dropped.
    fn on_disconnect(&self, _session: &mut Session) {}

    /// Called after a frame parses successfully but before any
    /// validate / verify / fan-out / storage step. Returning anything
    /// other than [`ExtensionAction::Continue`] short-circuits.
    fn on_message(&self, _msg: &MessageRef<'_>, _session: &mut Session) -> ExtensionAction {
        ExtensionAction::Continue
    }

    /// Called for every outbound frame. Returning
    /// [`OutboundDecision::Drop`] suppresses the frame entirely.
    fn on_outbound(&self, _frame: &OutboundFrame<'_>, _session: &mut Session) -> OutboundDecision {
        OutboundDecision::Forward
    }
}

/// Helper used by the shard dispatcher: walk the extension list against an
/// inbound message and collapse the result. Stops on the first non-Continue.
///
/// Returns an `Option<String>`:
/// - `None` means the dispatcher should keep processing the message.
/// - `Some(None.into())` style is never produced — the helper folds Drop
///   and Stop into the dispatcher's "did the extension consume the
///   message?" signal via the [`AdmitOutcome`] enum.
pub(crate) enum AdmitOutcome {
    /// Continue with full processing.
    Continue,
    /// Extension wants the dispatcher to send this frame and stop.
    Reply(String),
    /// Extension dropped the message; send nothing.
    Drop,
}

pub(crate) fn run_admission(
    extensions: &[Arc<dyn Extension>],
    msg: &MessageRef<'_>,
    session: &mut Session,
) -> AdmitOutcome {
    for ext in extensions {
        match ext.on_message(msg, session) {
            ExtensionAction::Continue => {}
            ExtensionAction::Stop(Some(frame)) => return AdmitOutcome::Reply(frame),
            ExtensionAction::Stop(None) | ExtensionAction::Drop => return AdmitOutcome::Drop,
        }
    }
    AdmitOutcome::Continue
}

// `run_outbound` was inlined into `ShardDispatcher::dispatch_text` /
// `dispatch_event_frame` because those need to walk the extension list
// while holding `&mut session` from a `clients.get_mut(&fd)` lookup —
// putting it behind a free function would force an extra mutable borrow
// chain. Kept as a comment so the intent stays visible.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Recorder {
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Extension for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn on_connect(&self, _s: &mut Session) {
            self.calls.lock().unwrap().push("connect");
        }
        fn on_disconnect(&self, _s: &mut Session) {
            self.calls.lock().unwrap().push("disconnect");
        }
        fn on_message(&self, _m: &MessageRef<'_>, _s: &mut Session) -> ExtensionAction {
            self.calls.lock().unwrap().push("message");
            ExtensionAction::Continue
        }
        fn on_outbound(&self, _f: &OutboundFrame<'_>, _s: &mut Session) -> OutboundDecision {
            self.calls.lock().unwrap().push("outbound");
            OutboundDecision::Forward
        }
    }

    struct Stopper(String);
    impl Extension for Stopper {
        fn name(&self) -> &'static str {
            "stopper"
        }
        fn on_message(&self, _m: &MessageRef<'_>, _s: &mut Session) -> ExtensionAction {
            ExtensionAction::Stop(Some(self.0.clone()))
        }
    }

    #[test]
    fn admission_continues_when_all_continue() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let exts: Vec<Arc<dyn Extension>> = vec![
            Arc::new(Recorder {
                calls: Arc::clone(&calls),
            }),
            Arc::new(Recorder {
                calls: Arc::clone(&calls),
            }),
        ];
        let mut session = Session::new(7, None);
        let outcome = run_admission(&exts, &MessageRef::Close { sub_id: "x" }, &mut session);
        assert!(matches!(outcome, AdmitOutcome::Continue));
        assert_eq!(*calls.lock().unwrap(), vec!["message", "message"]);
    }

    #[test]
    fn admission_short_circuits_on_stop() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let exts: Vec<Arc<dyn Extension>> = vec![
            Arc::new(Stopper("denied".into())),
            Arc::new(Recorder {
                calls: Arc::clone(&calls),
            }),
        ];
        let mut session = Session::new(7, None);
        let outcome = run_admission(&exts, &MessageRef::Close { sub_id: "x" }, &mut session);
        match outcome {
            AdmitOutcome::Reply(s) => assert_eq!(s, "denied"),
            _ => panic!("expected Reply"),
        }
        // second extension never invoked
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn extract_ip_picks_first_csv_entry() {
        let headers = vec![
            ("X-Forwarded-For".into(), "10.0.0.1, 10.0.0.2".into()),
            ("Host".into(), "example.com".into()),
        ];
        assert_eq!(
            extract_ip(&headers, "x-forwarded-for"),
            Some("10.0.0.1".parse().unwrap())
        );
    }

    #[test]
    fn extract_ip_returns_none_when_unparseable() {
        let headers = vec![("X-Real-IP".into(), "not-an-ip".into())];
        assert!(extract_ip(&headers, "x-real-ip").is_none());
    }

    #[test]
    fn session_ext_round_trip() {
        struct Tag(u32);
        let mut s = Session::new(1, None);
        assert!(s.ext::<Tag>().is_none());
        s.set_ext(Tag(42));
        assert_eq!(s.ext::<Tag>().map(|t| t.0), Some(42));
        if let Some(t) = s.ext_mut::<Tag>() {
            t.0 = 99;
        }
        assert_eq!(s.ext::<Tag>().map(|t| t.0), Some(99));
    }
}
