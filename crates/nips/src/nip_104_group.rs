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

use nostro2_traits::hex::Hexable;
use nostro2_traits::NostrKeypair;

use crate::nip_104::{decode_hex_32, Nip104Error};
use crate::nip_104_sender_key::SenderKeyState;

type Result<T> = std::result::Result<T, Nip104Error>;

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

/// Our own sending side for one group: the chain plus the sender-event key it
/// is published under.
#[derive(Debug, Clone)]
struct SendingChain {
    sender_event_pubkey: String,
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
    _marker: std::marker::PhantomData<fn() -> K>,
}

impl<K: NostrKeypair> GroupManager<K> {
    /// Create a manager owned by `our_pubkey` (our owner/device identity hex).
    #[must_use]
    pub fn new(our_pubkey: impl Into<String>) -> Self {
        Self {
            our_pubkey: our_pubkey.into(),
            groups: BTreeMap::new(),
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

        self.groups.entry(group_id.to_owned()).or_default().sending = Some(SendingChain {
            sender_event_pubkey,
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
        self.groups
            .entry(dist.group_id.clone())
            .or_default()
            .receiving
            .insert(dist.sender_event_pubkey.clone(), state);
        Ok(())
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
}
