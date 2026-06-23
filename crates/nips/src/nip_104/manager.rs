//! NIP-104 — Session manager (multi-device fan-out).
//!
//! [`crate::nip_104`] gives a single 1:1 [`Session`]; [`crate::nip_104::Invite`]
//! bootstraps one from a QR/URL/event. Real chat clients juggle *many*
//! sessions at once: a peer may run several devices, and each device is a
//! separate ratchet. This module is the routing layer that sits on top —
//! a dependency-light, synchronous distillation of the reference
//! `SessionManager` from `mmalmi/nostr-double-ratchet`.
//!
//! The reference class also owns storage adapters, async pub/sub, message
//! queues, receipts and expiration policy — all *runtime* concerns an
//! application supplies. What is portable (and interop-critical) is the pure
//! state machine:
//!
//! * **track** sessions keyed by `(peer, device)`,
//! * **route** an inbound kind-1060 event to whichever session can decrypt it,
//! * **fan out** an outbound message to *every* device a peer has, and
//! * **bootstrap** new sessions from invites.
//!
//! Everything here is in-memory and side-effect free: methods return the
//! events to publish (or the message decrypted), leaving transport, storage
//! and scheduling to the caller. That keeps it `no_std`-friendly in spirit and
//! trivially testable without a relay.
//!
//! ```ignore
//! let mut alice = SessionManager::new(alice_identity);
//! let response = alice.accept_invite(&bobs_invite, None, now)?; // publish it
//! // …bob calls receive_invite_response with the same event…
//! for event in alice.send(&bob_pubkey, b"hi", now)? { publish(event); }
//! if let Some(msg) = alice.process_event(&incoming) { /* msg.plaintext */ }
//! ```

use std::collections::BTreeMap;

use super::{Invite, Nip104Error, Session, MESSAGE_EVENT_KIND};
use nostro2_traits::NostrKeypair;

type Result<T> = std::result::Result<T, Nip104Error>;

/// A message successfully decrypted by [`SessionManager::process_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// The peer (owner) public key the session belongs to.
    pub peer: String,
    /// The specific device id (device identity pubkey) that sent it.
    pub device_id: String,
    /// The decrypted plaintext.
    pub plaintext: Vec<u8>,
}

/// All sessions we hold for a single peer, one per device.
#[derive(Debug, Clone)]
struct PeerRecord<K: NostrKeypair> {
    devices: BTreeMap<String, Session<K>>,
}

impl<K: NostrKeypair> Default for PeerRecord<K> {
    fn default() -> Self {
        Self {
            devices: BTreeMap::new(),
        }
    }
}

/// Routes double-ratchet sessions across many peers and devices.
///
/// Generic over the in-process keypair `K` exactly like [`Session`], so it
/// works with the production `K256Keypair` and any test signer alike.
#[derive(Debug, Clone)]
pub struct SessionManager<K: NostrKeypair> {
    identity: K,
    our_pubkey: String,
    peers: BTreeMap<String, PeerRecord<K>>,
}

impl<K: NostrKeypair> SessionManager<K> {
    /// Create a manager owning our long-term `identity` keypair (used to
    /// authenticate invite handshakes).
    #[must_use]
    pub fn new(identity: K) -> Self {
        let our_pubkey = identity.public_key();
        Self {
            identity,
            our_pubkey,
            peers: BTreeMap::new(),
        }
    }

    /// Our identity public key (x-only hex).
    #[must_use]
    pub fn our_pubkey(&self) -> &str {
        &self.our_pubkey
    }

    /// Whether we currently hold at least one session with `peer`.
    #[must_use]
    pub fn has_session(&self, peer: &str) -> bool {
        self.peers.get(peer).is_some_and(|p| !p.devices.is_empty())
    }

    /// The peers (owner pubkeys) we hold sessions with, sorted.
    pub fn peers(&self) -> impl Iterator<Item = &String> {
        self.peers.keys()
    }

    /// The device ids we hold sessions with for `peer`, sorted.
    #[must_use]
    pub fn devices(&self, peer: &str) -> Vec<String> {
        self.peers
            .get(peer)
            .map(|p| p.devices.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Total number of sessions across all peers and devices.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.peers.values().map(|p| p.devices.len()).sum()
    }

    /// Install (or replace) a session for `(peer, device_id)` directly — for
    /// restoring persisted state or wiring an externally-bootstrapped session.
    pub fn install_session(&mut self, peer: &str, device_id: &str, session: Session<K>) {
        self.peers
            .entry(peer.to_owned())
            .or_default()
            .devices
            .insert(device_id.to_owned(), session);
    }

    /// **Invitee side.** Accept `invite`, install the resulting initiator
    /// session (keyed under the inviter), and return the signed
    /// invite-response event to publish back.
    ///
    /// `owner_pubkey` is our optional multi-device owner key.
    ///
    /// # Errors
    /// Propagates [`Invite::accept`] crypto/signing failures.
    pub fn accept_invite(
        &mut self,
        invite: &Invite,
        owner_pubkey: Option<&str>,
        created_at: i64,
    ) -> Result<nostro2::NostrNote> {
        let (session, response) = invite.accept::<K>(&self.identity, owner_pubkey, created_at)?;
        let device_id = invite
            .device_id
            .clone()
            .unwrap_or_else(|| invite.inviter.clone());
        self.install_session(&invite.inviter, &device_id, session);
        Ok(response)
    }

    /// **Inviter side.** Consume an invite-response `event`, install the mirror
    /// responder session, and return the peer (owner) pubkey we now have a
    /// session with.
    ///
    /// The session is keyed under the invitee's owner pubkey when supplied
    /// (so all of a multi-device peer's devices group together), else under
    /// their identity; the device id is always the invitee's identity pubkey.
    ///
    /// # Errors
    /// Propagates [`Invite::receive`] failures (bad signature, wrong kind,
    /// missing ephemeral secret, crypto).
    pub fn receive_invite_response(
        &mut self,
        invite: &Invite,
        event: &nostro2::NostrNote,
    ) -> Result<String> {
        let (session, recovered) = invite.receive::<K>(event, &self.identity)?;
        let peer = recovered
            .owner_public_key
            .clone()
            .unwrap_or_else(|| recovered.invitee_identity.clone());
        self.install_session(&peer, &recovered.invitee_identity, session);
        Ok(peer)
    }

    /// Route an inbound kind-[`MESSAGE_EVENT_KIND`] event to whichever session
    /// can decrypt it, commit that session's ratchet advance, and return the
    /// decrypted message.
    ///
    /// Returns `None` if the event is not a message event or no held session
    /// accepts it (wrong peer, replay, or tampered — the underlying codec
    /// verifies the signature).
    pub fn process_event(&mut self, event: &nostro2::NostrNote) -> Option<ReceivedMessage> {
        if event.kind != MESSAGE_EVENT_KIND {
            return None;
        }
        // Trial-decrypt against every session, mutating in place on a hit. A
        // session's own codec verifies the signature, so a wrong/forged event
        // simply fails to decrypt and we move on.
        for (peer, record) in &mut self.peers {
            for (device_id, session) in &mut record.devices {
                if let Ok((next, plaintext)) = session.plan_receive_event(event) {
                    session.apply(next);
                    return Some(ReceivedMessage {
                        peer: peer.clone(),
                        device_id: device_id.clone(),
                        plaintext,
                    });
                }
            }
        }
        None
    }

    /// Fan out: encrypt `payload` to **every** device session held for `peer`,
    /// committing each ratchet advance, and return one signed kind-1060 event
    /// per device ready to publish.
    ///
    /// Devices that cannot send yet (their first inbound message hasn't
    /// arrived) are skipped. The returned order follows sorted device id.
    ///
    /// # Errors
    /// [`Nip104Error::UnknownPeer`] if we hold no sessions for `peer`; plus any
    /// per-session send failure.
    pub fn send(
        &mut self,
        peer: &str,
        payload: &[u8],
        created_at: i64,
    ) -> Result<Vec<nostro2::NostrNote>> {
        let record = self
            .peers
            .get_mut(peer)
            .filter(|p| !p.devices.is_empty())
            .ok_or_else(|| Nip104Error::UnknownPeer(peer.to_owned()))?;

        let mut events = Vec::with_capacity(record.devices.len());
        for session in record.devices.values_mut() {
            if !session.can_send() {
                continue;
            }
            let (next, event) = session.plan_send_event(payload, created_at)?;
            session.apply(next);
            events.push(event);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Concrete-typed `public_key()` calls need the trait in scope; library code
    // reaches it through the `NostrKeypair: NostrSigner` bound.
    use nostro2_traits::NostrSigner as _;

    type K = crate::tests::NipTester;

    fn ident(seed: u8) -> K {
        K::from_secret_bytes(&[seed; 32]).unwrap()
    }

    const NOW: i64 = 1_700_000_000;

    /// Two managers bootstrap via an invite, then chat *through the manager* —
    /// routing and fan-out both exercised end to end.
    #[test]
    fn two_managers_handshake_and_chat() {
        let mut alice = SessionManager::new(ident(0x01));
        let mut bob = SessionManager::new(ident(0x02));

        // Alice mints an invite and keeps it (holds the ephemeral secret).
        let invite = Invite::create_new::<K>(alice.our_pubkey(), None).unwrap();

        // Bob accepts → his manager installs an initiator session with Alice,
        // and emits a response Alice must consume.
        let response = bob.accept_invite(&invite, None, NOW).unwrap();
        assert!(bob.has_session(alice.our_pubkey()));

        let peer = alice.receive_invite_response(&invite, &response).unwrap();
        assert_eq!(peer, bob.our_pubkey());
        assert!(alice.has_session(bob.our_pubkey()));

        // Bob (initiator) speaks first; Alice routes it home.
        let outbound = bob.send(alice.our_pubkey(), b"hello alice", NOW).unwrap();
        assert_eq!(outbound.len(), 1);
        let got = alice.process_event(&outbound[0]).expect("alice decrypts");
        assert_eq!(got.peer, bob.our_pubkey());
        assert_eq!(got.plaintext, b"hello alice");

        // And the reply direction.
        let reply = alice.send(bob.our_pubkey(), b"hi bob", NOW).unwrap();
        assert_eq!(reply.len(), 1);
        let got = bob.process_event(&reply[0]).expect("bob decrypts");
        assert_eq!(got.peer, alice.our_pubkey());
        assert_eq!(got.plaintext, b"hi bob");
    }

    /// One peer, two devices, one owner: a single `send` fans out to both, and
    /// each device's manager decrypts exactly its own copy.
    #[test]
    fn send_fans_out_to_every_device() {
        let mut alice = SessionManager::new(ident(0x10));

        // Bob runs two devices under one owner key.
        let bob_owner = ident(0x20).public_key();
        let mut bob_dev1 = SessionManager::new(ident(0x21));
        let mut bob_dev2 = SessionManager::new(ident(0x22));

        // One public invite; both devices accept it, each claiming the owner.
        let invite = Invite::create_new::<K>(alice.our_pubkey(), None).unwrap();
        let r1 = bob_dev1.accept_invite(&invite, Some(&bob_owner), NOW).unwrap();
        let r2 = bob_dev2.accept_invite(&invite, Some(&bob_owner), NOW).unwrap();

        // Alice receives both → one peer (the owner) with two device sessions.
        let p1 = alice.receive_invite_response(&invite, &r1).unwrap();
        let p2 = alice.receive_invite_response(&invite, &r2).unwrap();
        assert_eq!(p1, bob_owner);
        assert_eq!(p2, bob_owner);
        assert_eq!(alice.devices(&bob_owner).len(), 2);

        // Initiator devices must speak first to open their send chains.
        let m1 = bob_dev1.send(alice.our_pubkey(), b"d1 up", NOW).unwrap();
        let m2 = bob_dev2.send(alice.our_pubkey(), b"d2 up", NOW).unwrap();
        assert_eq!(alice.process_event(&m1[0]).unwrap().plaintext, b"d1 up");
        assert_eq!(alice.process_event(&m2[0]).unwrap().plaintext, b"d2 up");

        // One send → two events, one per device.
        let fanned = alice.send(&bob_owner, b"broadcast", NOW).unwrap();
        assert_eq!(fanned.len(), 2);

        // Each device decrypts exactly one of the two; neither takes both.
        let to_dev1 = fanned
            .iter()
            .filter_map(|e| bob_dev1.process_event(e))
            .collect::<Vec<_>>();
        let to_dev2 = fanned
            .iter()
            .filter_map(|e| bob_dev2.process_event(e))
            .collect::<Vec<_>>();
        assert_eq!(to_dev1.len(), 1);
        assert_eq!(to_dev2.len(), 1);
        assert_eq!(to_dev1[0].plaintext, b"broadcast");
        assert_eq!(to_dev2[0].plaintext, b"broadcast");
    }

    #[test]
    fn send_to_unknown_peer_errors() {
        let mut alice = SessionManager::new(ident(0x30));
        let err = alice.send("deadbeef", b"hi", NOW).unwrap_err();
        assert!(matches!(err, Nip104Error::UnknownPeer(_)));
    }

    #[test]
    fn process_ignores_foreign_and_non_message_events() {
        let mut alice = SessionManager::new(ident(0x40));
        let mut bob = SessionManager::new(ident(0x41));

        let invite = Invite::create_new::<K>(alice.our_pubkey(), None).unwrap();
        let response = bob.accept_invite(&invite, None, NOW).unwrap();
        alice.receive_invite_response(&invite, &response).unwrap();
        let outbound = bob.send(alice.our_pubkey(), b"hi", NOW).unwrap();

        // A third party with no session ignores the message entirely.
        let mut stranger = SessionManager::new(ident(0x42));
        assert!(stranger.process_event(&outbound[0]).is_none());

        // Non-message kinds (e.g. the invite response itself) are ignored too.
        assert!(alice.process_event(&response).is_none());
    }

    #[test]
    fn install_and_introspection() {
        let mut alice = SessionManager::new(ident(0x50));
        let mut bob = SessionManager::new(ident(0x51));

        assert_eq!(alice.session_count(), 0);
        assert!(!alice.has_session(bob.our_pubkey()));

        let invite = Invite::create_new::<K>(alice.our_pubkey(), None).unwrap();
        let response = bob.accept_invite(&invite, None, NOW).unwrap();
        let peer = alice.receive_invite_response(&invite, &response).unwrap();

        assert_eq!(alice.session_count(), 1);
        assert!(alice.has_session(&peer));
        assert_eq!(alice.peers().count(), 1);
        assert_eq!(alice.devices(&peer), vec![bob.our_pubkey().to_owned()]);
    }
}
