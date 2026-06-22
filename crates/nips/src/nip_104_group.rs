//! NIP-104 — Group transport: distributions, outer events, and a manager.
//!
//! Builds on [`crate::nip_104_sender_key::SenderKeyState`] (the symmetric
//! chain) to give the hybrid group model from `mmalmi/nostr-double-ratchet`:
//!
//! 1. A sending device mints a per-group sender-key chain and an associated
//!    **sender-event keypair** (the pubkey that signs its one-to-many events).
//! 2. The chain seed travels to each member as a [`SenderKeyDistribution`],
//!    carried inside the authenticated 1:1 ratchet sessions (so only members
//!    learn it).
//! 3. Group messages are published **once** as a [`GroupSenderKeyMessage`]
//!    outer event, encrypted with the next key off the chain.
//! 4. Members decrypt with the chain state learned from the distribution.
//!
//! [`GroupManager`] is the pure, in-memory state machine tying these together:
//! it owns our sending chain per group, tracks received chains keyed by sender,
//! and is side-effect free — methods return the wire objects to publish or the
//! plaintext decrypted, leaving transport to the caller (exactly like
//! [`crate::nip_104_manager::SessionManager`]).

use std::collections::BTreeMap;

use base64::engine::{general_purpose, Engine as _};
use nostro2_traits::hex::Hexable;
use nostro2_traits::NostrKeypair;

use crate::nip_104::{decode_hex_32, Nip104Error, MESSAGE_EVENT_KIND};
use crate::nip_104_sender_key::SenderKeyState;

type Result<T> = std::result::Result<T, Nip104Error>;

/// Outer Nostr event kind for group messages.
///
/// Same kind as 1:1 ratchet messages (the reference's `MESSAGE_EVENT_KIND` =
/// 1060); members tell the two apart by whether the event `pubkey` maps to a
/// sender-key chain.
pub const GROUP_MESSAGE_KIND: u32 = MESSAGE_EVENT_KIND;

/// Inner-rumor kind for a sender-key distribution, delivered pairwise over
/// each member's 1:1 Double Ratchet session (reference
/// `GROUP_SENDER_KEY_DISTRIBUTION_KIND`).
pub const GROUP_SENDER_KEY_DISTRIBUTION_KIND: u32 = 10446;

/// Inner-rumor kind for a group chat message (reference `CHAT_MESSAGE_KIND`,
/// the NIP-17 private-DM kind). The plaintext inside an outer group event is a
/// JSON rumor of this kind.
pub const GROUP_CHAT_MESSAGE_KIND: u32 = 14;

bourne::json! {
    /// Seed for one sender-key chain, distributed to members over their 1:1
    /// sessions. Field names match the reference `SenderKeyDistribution`
    /// (snake_case); `chain_key` is 64-char hex.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SenderKeyDistribution {
        pub group_id: String,
        pub key_id: u32,
        pub sender_event_pubkey: String,
        pub chain_key: String,
        pub iteration: u32,
        pub created_at: i64,
    }
}

bourne::json! {
    /// A published one-to-many group message. The `sender_event_pubkey`
    /// locates the receiving chain; `key_id` + `message_number` index it.
    /// `ciphertext` is the base64 NIP-44 v2 payload from the chain.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GroupSenderKeyMessage {
        pub group_id: String,
        pub sender_event_pubkey: String,
        pub key_id: u32,
        pub message_number: u32,
        pub created_at: i64,
        pub ciphertext: String,
    }
}

/// Our own sending side for one group: the chain plus the sender-event keypair
/// it is published/signed under.
#[derive(Debug, Clone)]
struct SendingChain {
    sender_event_pubkey: String,
    sender_event_secret: [u8; 32],
    state: SenderKeyState,
}

/// All state for a single group.
#[derive(Debug, Clone, Default)]
struct GroupRecord {
    /// Our sending chain, once minted.
    sending: Option<SendingChain>,
    /// Received chains, keyed by their `sender_event_pubkey`.
    receiving: BTreeMap<String, SenderKeyState>,
}

/// A group message decrypted by [`GroupManager::decrypt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReceivedMessage {
    /// The group it belongs to.
    pub group_id: String,
    /// The sender-event pubkey that produced it.
    pub sender_event_pubkey: String,
    /// The recovered plaintext.
    pub plaintext: Vec<u8>,
}

/// Pure, in-memory group transport state machine.
///
/// Generic over the in-process keypair `K`, matching [`SenderKeyState`] and
/// [`crate::nip_104_manager::SessionManager`].
#[derive(Debug, Clone)]
pub struct GroupManager<K: NostrKeypair> {
    our_pubkey: String,
    groups: BTreeMap<String, GroupRecord>,
    /// Reverse index: a sender-event pubkey → the `group_id` its chain belongs
    /// to. Lets us route a bare outer event (which carries no `group_id`) to
    /// the right chain by its author pubkey alone.
    sender_to_group: BTreeMap<String, String>,
    _marker: std::marker::PhantomData<fn() -> K>,
}

impl<K: NostrKeypair> GroupManager<K> {
    /// Create a manager owned by `our_pubkey` (our owner/device identity hex).
    #[must_use]
    pub fn new(our_pubkey: impl Into<String>) -> Self {
        Self {
            our_pubkey: our_pubkey.into(),
            groups: BTreeMap::new(),
            sender_to_group: BTreeMap::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Our identity pubkey.
    #[must_use]
    pub fn our_pubkey(&self) -> &str {
        &self.our_pubkey
    }

    /// Whether we hold a sending chain for `group_id`.
    #[must_use]
    pub fn has_sending_chain(&self, group_id: &str) -> bool {
        self.groups
            .get(group_id)
            .is_some_and(|g| g.sending.is_some())
    }

    /// The sender-event pubkeys whose chains we can currently decrypt for
    /// `group_id`, sorted.
    #[must_use]
    pub fn known_senders(&self, group_id: &str) -> Vec<String> {
        self.groups
            .get(group_id)
            .map(|g| g.receiving.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// **Mint** our sending chain for `group_id`. Generates a fresh sender-event
    /// keypair and a random chain key, then returns the
    /// [`SenderKeyDistribution`] to hand to every member over their 1:1
    /// session. Replaces any prior sending chain (a key rotation).
    ///
    /// # Errors
    /// Propagates key/derivation failures.
    pub fn rotate_sending_chain(
        &mut self,
        group_id: &str,
        key_id: u32,
        created_at: i64,
    ) -> Result<SenderKeyDistribution> {
        let sender_event = K::generate();
        let sender_event_pubkey = sender_event.public_key();
        let sender_event_secret = sender_event.secret_bytes();
        let chain_key = K::generate().secret_bytes();
        let state = SenderKeyState::new(key_id, &chain_key, 0);

        let dist = SenderKeyDistribution {
            group_id: group_id.to_owned(),
            key_id,
            sender_event_pubkey: sender_event_pubkey.clone(),
            chain_key: chain_key.to_hex(),
            iteration: 0,
            created_at,
        };

        self.sender_to_group
            .insert(sender_event_pubkey.clone(), group_id.to_owned());
        self.groups.entry(group_id.to_owned()).or_default().sending = Some(SendingChain {
            sender_event_pubkey,
            sender_event_secret,
            state,
        });
        Ok(dist)
    }

    /// Re-derive the current [`SenderKeyDistribution`] for our sending chain —
    /// e.g. to hand the chain to a member who joined after we minted it. Note
    /// the `iteration` reflects the chain's *current* position, so a late
    /// joiner only decrypts messages from here forward.
    ///
    /// # Errors
    /// [`Nip104Error::SessionNotReady`] if we have no sending chain yet.
    pub fn current_distribution(
        &self,
        group_id: &str,
        created_at: i64,
    ) -> Result<SenderKeyDistribution> {
        let send = self
            .groups
            .get(group_id)
            .and_then(|g| g.sending.as_ref())
            .ok_or(Nip104Error::SessionNotReady)?;
        Ok(SenderKeyDistribution {
            group_id: group_id.to_owned(),
            key_id: send.state.key_id(),
            sender_event_pubkey: send.sender_event_pubkey.clone(),
            chain_key: send.state.chain_key_hex(),
            iteration: send.state.iteration(),
            created_at,
        })
    }

    /// **Install** a [`SenderKeyDistribution`] received from a member — the
    /// receiving chain we use to decrypt that sender's group messages. A newer
    /// distribution for the same sender (same `sender_event_pubkey`) replaces
    /// the old chain (handles rotation).
    ///
    /// # Errors
    /// [`Nip104Error`] if the chain key is malformed hex.
    pub fn apply_distribution(&mut self, dist: &SenderKeyDistribution) -> Result<()> {
        let chain_key = decode_hex_32(&dist.chain_key)?;
        let state = SenderKeyState::new(dist.key_id, &chain_key, dist.iteration);
        self.sender_to_group
            .insert(dist.sender_event_pubkey.clone(), dist.group_id.clone());
        self.groups
            .entry(dist.group_id.clone())
            .or_default()
            .receiving
            .insert(dist.sender_event_pubkey.clone(), state);
        Ok(())
    }

    /// Frame a distribution as the unsigned kind-10446 session rumor to hand to
    /// the `SessionManager` for every member; its serialized JSON becomes the
    /// inner ratchet plaintext. Tags match the reference: l/key/ms; pubkey is
    /// our device identity; id is the rumor hash.
    ///
    /// # Errors
    /// JSON serialization failure of the distribution payload.
    pub fn distribution_to_rumor(
        &self,
        dist: &SenderKeyDistribution,
        created_at: i64,
        now_ms: i64,
    ) -> Result<nostro2::NostrNote> {
        let mut tags = nostro2::NostrTags::new();
        tags.add_custom_tag("l", &dist.group_id);
        tags.add_custom_tag("key", &dist.key_id.to_string());
        tags.add_custom_tag("ms", &now_ms.to_string());
        let mut rumor = nostro2::NostrNote {
            pubkey: self.our_pubkey.clone(),
            kind: GROUP_SENDER_KEY_DISTRIBUTION_KIND,
            content: bourne::to_string(dist)?,
            created_at,
            tags,
            ..Default::default()
        };
        rumor
            .serialize_id()
            .map_err(|e| Nip104Error::Json(e.to_string()))?;
        Ok(rumor)
    }

    /// Consume an inbound session rumor. If it is a kind-10446 distribution,
    /// parse and install it, returning the applied distribution. Returns
    /// Ok(None) for any other rumor kind, so it composes with a 1:1 loop.
    ///
    /// # Errors
    /// Malformed payload or bad chain-key hex.
    pub fn apply_distribution_rumor(
        &mut self,
        rumor: &nostro2::NostrNote,
    ) -> Result<Option<SenderKeyDistribution>> {
        if rumor.kind != GROUP_SENDER_KEY_DISTRIBUTION_KIND {
            return Ok(None);
        }
        let dist: SenderKeyDistribution = bourne::parse_str(&rumor.content)?;
        self.apply_distribution(&dist)?;
        Ok(Some(dist))
    }

    /// **Encrypt** `plaintext` as the next message on our sending chain for
    /// `group_id`, returning the [`GroupSenderKeyMessage`] to publish once.
    ///
    /// # Errors
    /// [`Nip104Error::SessionNotReady`] if we have no sending chain; plus
    /// cipher failures.
    pub fn encrypt(
        &mut self,
        group_id: &str,
        plaintext: &[u8],
        created_at: i64,
    ) -> Result<GroupSenderKeyMessage> {
        let send = self
            .groups
            .get_mut(group_id)
            .and_then(|g| g.sending.as_mut())
            .ok_or(Nip104Error::SessionNotReady)?;
        let key_id = send.state.key_id();
        let (message_number, ciphertext) = send.state.encrypt::<K>(plaintext)?;
        Ok(GroupSenderKeyMessage {
            group_id: group_id.to_owned(),
            sender_event_pubkey: send.sender_event_pubkey.clone(),
            key_id,
            message_number,
            created_at,
            ciphertext,
        })
    }

    /// **Decrypt** an inbound [`GroupSenderKeyMessage`], routing it to the
    /// receiving chain for its `sender_event_pubkey`.
    ///
    /// # Errors
    /// [`Nip104Error::SessionNotReady`] if we hold no chain for that sender
    /// (we are missing their distribution); plus key-id/ordering/cipher
    /// failures from the chain.
    pub fn decrypt(&mut self, msg: &GroupSenderKeyMessage) -> Result<GroupReceivedMessage> {
        let chain = self
            .groups
            .get_mut(&msg.group_id)
            .and_then(|g| g.receiving.get_mut(&msg.sender_event_pubkey))
            .ok_or(Nip104Error::SessionNotReady)?;
        let plan = chain.plan_decrypt::<K>(msg.key_id, msg.message_number, &msg.ciphertext)?;
        let plaintext = chain.apply_decrypt(plan);
        Ok(GroupReceivedMessage {
            group_id: msg.group_id.clone(),
            sender_event_pubkey: msg.sender_event_pubkey.clone(),
            plaintext,
        })
    }

    /// **Encrypt and build the publishable outer event** for `group_id`. The
    /// returned [`nostro2::NostrNote`] is a signed kind-[`GROUP_MESSAGE_KIND`]
    /// event authored by our per-group sender-event key, with
    /// `content = base64(key_id_be32 || message_number_be32 || nip44_bytes)`
    /// and empty tags — byte-compatible with the reference `OneToManyChannel`.
    /// Publish it **once**; every member decrypts it.
    ///
    /// # Errors
    /// [`Nip104Error::SessionNotReady`] if we have no sending chain; plus
    /// cipher/signing failures.
    pub fn encrypt_to_event(
        &mut self,
        group_id: &str,
        plaintext: &[u8],
        created_at: i64,
    ) -> Result<nostro2::NostrNote> {
        let send = self
            .groups
            .get_mut(group_id)
            .and_then(|g| g.sending.as_mut())
            .ok_or(Nip104Error::SessionNotReady)?;
        let key_id = send.state.key_id();
        let (message_number, ciphertext_b64) = send.state.encrypt::<K>(plaintext)?;
        let content = encode_outer_content(key_id, message_number, &ciphertext_b64)?;

        let signer = K::from_secret_bytes(&send.sender_event_secret)
            .map_err(Nip104Error::Signer)?;
        let mut note = nostro2::NostrNote {
            kind: GROUP_MESSAGE_KIND,
            content,
            created_at,
            tags: nostro2::NostrTags::new(),
            ..Default::default()
        };
        note.sign_with(&signer)
            .map_err(|_| Nip104Error::Signer(nostro2_traits::SignerError::InvalidSignature))?;
        Ok(note)
    }

    /// **Decrypt an inbound outer event.** Verifies the event, routes by its
    /// `pubkey` (the sender-event key) to the matching receiving chain, parses
    /// the compact payload, and decrypts. Returns `Ok(None)` if the author is
    /// not a sender-key chain we know (e.g. a 1:1 message, or a member whose
    /// distribution we have not yet received).
    ///
    /// # Errors
    /// [`Nip104Error`] on a bad signature, malformed payload, or chain
    /// decrypt failure.
    pub fn decrypt_event(
        &mut self,
        event: &nostro2::NostrNote,
    ) -> Result<Option<GroupReceivedMessage>> {
        use nostro2::NostrEvent as _;
        if event.kind != GROUP_MESSAGE_KIND {
            return Ok(None);
        }
        // Route by author: do we hold a receiving chain for this sender?
        let Some(group_id) = self.sender_to_group.get(&event.pubkey).cloned() else {
            return Ok(None);
        };
        let known = self
            .groups
            .get(&group_id)
            .is_some_and(|g| g.receiving.contains_key(&event.pubkey));
        if !known {
            return Ok(None);
        }
        if !event.verify() {
            return Err(Nip104Error::InvalidHeader);
        }
        let (key_id, message_number, ciphertext_b64) = decode_outer_content(&event.content)?;
        let msg = GroupSenderKeyMessage {
            group_id,
            sender_event_pubkey: event.pubkey.clone(),
            key_id,
            message_number,
            created_at: event.created_at,
            ciphertext: ciphertext_b64,
        };
        Ok(Some(self.decrypt(&msg)?))
    }
}

/// Build the reference's compact outer payload:
/// `base64(key_id_be32 || message_number_be32 || raw_nip44_bytes)`.
///
/// Our [`SenderKeyState`] ciphertext is the **base64** NIP-44 payload; we
/// decode it back to the raw bytes the reference frames.
fn encode_outer_content(key_id: u32, message_number: u32, ciphertext_b64: &str) -> Result<String> {
    let nip44_bytes = general_purpose::STANDARD
        .decode(ciphertext_b64)
        .map_err(|_| Nip104Error::InvalidHeader)?;
    let mut payload = Vec::with_capacity(8 + nip44_bytes.len());
    payload.extend_from_slice(&key_id.to_be_bytes());
    payload.extend_from_slice(&message_number.to_be_bytes());
    payload.extend_from_slice(&nip44_bytes);
    Ok(general_purpose::STANDARD.encode(&payload))
}

/// Inverse of [`encode_outer_content`], returning
/// `(key_id, message_number, base64 nip44 ciphertext)` ready for the chain.
fn decode_outer_content(content: &str) -> Result<(u32, u32, String)> {
    let bytes = general_purpose::STANDARD
        .decode(content)
        .map_err(|_| Nip104Error::InvalidHeader)?;
    if bytes.len() < 8 {
        return Err(Nip104Error::InvalidHeader);
    }
    let key_id = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let message_number = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let ciphertext_b64 = general_purpose::STANDARD.encode(&bytes[8..]);
    Ok((key_id, message_number, ciphertext_b64))
}

#[cfg(test)]
mod tests {
    use super::*;

    type K = crate::tests::NipTester;

    fn mgr(id: &str) -> GroupManager<K> {
        GroupManager::<K>::new(id.to_owned())
    }

    #[test]
    fn one_to_many_two_members() {
        // Alice mints a chain and distributes it to Bob and Carol.
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        let mut carol = mgr("carol");

        let dist = alice.rotate_sending_chain("g1", 1, 1000).unwrap();
        bob.apply_distribution(&dist).unwrap();
        carol.apply_distribution(&dist).unwrap();

        // One published message decrypts for every member.
        let msg = alice.encrypt("g1", b"gm everyone", 1001).unwrap();
        assert_eq!(bob.decrypt(&msg).unwrap().plaintext, b"gm everyone");
        assert_eq!(carol.decrypt(&msg).unwrap().plaintext, b"gm everyone");
    }

    #[test]
    fn sequential_messages_advance_all() {
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        let dist = alice.rotate_sending_chain("g", 1, 0).unwrap();
        bob.apply_distribution(&dist).unwrap();

        for i in 0..5 {
            let m = alice.encrypt("g", format!("m{i}").as_bytes(), i).unwrap();
            assert_eq!(bob.decrypt(&m).unwrap().plaintext, format!("m{i}").as_bytes());
        }
    }

    #[test]
    fn late_joiner_gets_current_distribution() {
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        let mut dave = mgr("dave");

        let dist0 = alice.rotate_sending_chain("g", 1, 0).unwrap();
        bob.apply_distribution(&dist0).unwrap();
        let _m0 = alice.encrypt("g", b"before dave", 1).unwrap();

        // Dave joins now: he gets the chain at its current iteration.
        let dist_now = alice.current_distribution("g", 2).unwrap();
        assert_eq!(dist_now.iteration, 1);
        dave.apply_distribution(&dist_now).unwrap();

        // New message: both Bob (iter 1) and Dave (iter 1) decrypt.
        let m1 = alice.encrypt("g", b"after dave", 3).unwrap();
        assert_eq!(bob.decrypt(&m1).unwrap().plaintext, b"after dave");
        assert_eq!(dave.decrypt(&m1).unwrap().plaintext, b"after dave");
    }

    #[test]
    fn two_senders_route_independently() {
        // Alice and Bob each have their own chain; Carol decrypts both.
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        let mut carol = mgr("carol");

        let da = alice.rotate_sending_chain("g", 1, 0).unwrap();
        let db = bob.rotate_sending_chain("g", 1, 0).unwrap();
        assert_ne!(da.sender_event_pubkey, db.sender_event_pubkey);
        carol.apply_distribution(&da).unwrap();
        carol.apply_distribution(&db).unwrap();

        let ma = alice.encrypt("g", b"from alice", 1).unwrap();
        let mb = bob.encrypt("g", b"from bob", 1).unwrap();
        assert_eq!(carol.decrypt(&mb).unwrap().plaintext, b"from bob");
        assert_eq!(carol.decrypt(&ma).unwrap().plaintext, b"from alice");
        assert_eq!(carol.known_senders("g").len(), 2);
    }

    #[test]
    fn decrypt_without_distribution_fails() {
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        alice.rotate_sending_chain("g", 1, 0).unwrap();
        let m = alice.encrypt("g", b"secret", 1).unwrap();
        // Bob never got the distribution.
        assert!(matches!(bob.decrypt(&m), Err(Nip104Error::SessionNotReady)));
    }

    #[test]
    fn rotation_replaces_sending_chain() {
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        let d1 = alice.rotate_sending_chain("g", 1, 0).unwrap();
        bob.apply_distribution(&d1).unwrap();
        let _ = alice.encrypt("g", b"old", 1).unwrap();

        // Rotate to a new key id → fresh sender-event pubkey + chain.
        let d2 = alice.rotate_sending_chain("g", 2, 2).unwrap();
        assert_ne!(d1.sender_event_pubkey, d2.sender_event_pubkey);
        bob.apply_distribution(&d2).unwrap();
        let m = alice.encrypt("g", b"new", 3).unwrap();
        assert_eq!(m.key_id, 2);
        assert_eq!(bob.decrypt(&m).unwrap().plaintext, b"new");
    }

    #[test]
    fn distribution_json_roundtrips() {
        let mut alice = mgr("alice");
        let dist = alice.rotate_sending_chain("g", 1, 1234).unwrap();
        let json = bourne::to_string(&dist).unwrap();
        let back: SenderKeyDistribution = bourne::parse_str(&json).unwrap();
        assert_eq!(dist, back);
    }

    #[test]
    fn message_json_roundtrips() {
        let mut alice = mgr("alice");
        alice.rotate_sending_chain("g", 1, 0).unwrap();
        let msg = alice.encrypt("g", b"hi", 7).unwrap();
        let json = bourne::to_string(&msg).unwrap();
        let back: GroupSenderKeyMessage = bourne::parse_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn distribution_rumor_roundtrips_over_session() {
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");

        let dist = alice.rotate_sending_chain("g7", 1, 1000).unwrap();
        // Alice frames the kind-10446 rumor she'd send Bob over their 1:1 session.
        let rumor = alice.distribution_to_rumor(&dist, 1000, 1_000_000).unwrap();
        assert_eq!(rumor.kind, GROUP_SENDER_KEY_DISTRIBUTION_KIND);
        assert_eq!(rumor.pubkey, "alice");
        assert!(rumor.id.is_some());
        assert_eq!(rumor.tags.find_tags("l"), vec!["g7".to_owned()]);
        assert_eq!(rumor.tags.find_tags("key"), vec!["1".to_owned()]);
        assert_eq!(rumor.tags.find_tags("ms"), vec!["1000000".to_owned()]);

        // Bob receives that rumor (out of his session) and installs the chain.
        let applied = bob.apply_distribution_rumor(&rumor).unwrap().unwrap();
        assert_eq!(applied, dist);

        // Now the one-to-many outer event decrypts for Bob.
        let ev = alice.encrypt_to_event("g7", b"after distro", 1001).unwrap();
        let got = bob.decrypt_event(&ev).unwrap().unwrap();
        assert_eq!(got.plaintext, b"after distro");
    }

    #[test]
    fn apply_distribution_rumor_ignores_other_kinds() {
        let mut bob = mgr("bob");
        let mut other = nostro2::NostrNote {
            kind: GROUP_CHAT_MESSAGE_KIND,
            content: "hi".to_owned(),
            ..Default::default()
        };
        let _ = other.serialize_id();
        assert!(bob.apply_distribution_rumor(&other).unwrap().is_none());
    }

    #[test]
    fn outer_event_roundtrip_one_to_many() {
        use nostro2::NostrEvent as _;
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        let mut carol = mgr("carol");

        let dist = alice.rotate_sending_chain("g1", 1, 1000).unwrap();
        bob.apply_distribution(&dist).unwrap();
        carol.apply_distribution(&dist).unwrap();

        // One signed outer event, published once.
        let ev = alice.encrypt_to_event("g1", b"gm group", 1001).unwrap();
        assert_eq!(ev.kind, GROUP_MESSAGE_KIND);
        assert_eq!(ev.pubkey, dist.sender_event_pubkey);
        assert!(ev.verify());
        assert_eq!(ev.tags.iter().count(), 0);

        // Every member decrypts the same wire event.
        let b = bob.decrypt_event(&ev).unwrap().unwrap();
        let c = carol.decrypt_event(&ev).unwrap().unwrap();
        assert_eq!(b.plaintext, b"gm group");
        assert_eq!(c.plaintext, b"gm group");
        assert_eq!(b.group_id, "g1");
    }

    #[test]
    fn decrypt_event_ignores_unknown_author() {
        let mut alice = mgr("alice");
        let mut bob = mgr("bob");
        alice.rotate_sending_chain("g", 1, 0).unwrap();
        // Bob has no distribution → unknown author → Ok(None), not an error.
        let ev = alice.encrypt_to_event("g", b"secret", 1).unwrap();
        assert!(bob.decrypt_event(&ev).unwrap().is_none());
    }

    #[test]
    fn outer_content_frames_match_reference_layout() {
        // key_id and message_number are big-endian u32 prefixes.
        let content = encode_outer_content(0x0102_0304, 0x0506_0708, &{
            // a minimal valid base64 of some bytes
            general_purpose::STANDARD.encode([0xAA_u8; 40])
        })
        .unwrap();
        let raw = general_purpose::STANDARD.decode(&content).unwrap();
        assert_eq!(&raw[..4], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&raw[4..8], &[0x05, 0x06, 0x07, 0x08]);
        assert_eq!(&raw[8..], &[0xAA_u8; 40]);

        let (k, n, ct) = decode_outer_content(&content).unwrap();
        assert_eq!(k, 0x0102_0304);
        assert_eq!(n, 0x0506_0708);
        assert_eq!(general_purpose::STANDARD.decode(ct).unwrap(), [0xAA_u8; 40]);
    }
}
