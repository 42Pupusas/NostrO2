//! NIP-104 — Double Ratchet end-to-end encrypted direct messages.
//!
//! This is a native, dependency-light port of the 1:1 ratchet session used by
//! [`mmalmi/nostr-double-ratchet`](https://github.com/mmalmi/nostr-double-ratchet)
//! (the implementation `chat.iris.to` runs). It is built **entirely** on this
//! crate's spec-compliant NIP-44 v2 primitives ([`crate::Nip44`]) plus
//! HKDF-SHA256, so it pulls in no third-party `nostr` stack.
//!
//! Scope: the core symmetric/DH double ratchet — `init`, `plan_send`,
//! `plan_receive`, chain stepping, and skipped-message-key handling. The
//! multi-device `AppKeys` / invite / session-manager layers are intentionally
//! out of scope here.
//!
//! ## Crypto equivalence with the reference
//!
//! | Reference (`nostr-double-ratchet`)            | Here                                        |
//! |-----------------------------------------------|---------------------------------------------|
//! | `kdf(ikm, salt, n)` (HKDF-SHA256, info=`[i]`) | [`Nip104Crypto::kdf`]                        |
//! | `ConversationKey::derive(sk, pk)`             | `conversation_key_v2(ecdh_x)`               |
//! | `ConversationKey::new(message_key)`           | `message_key` used directly as conv-key     |
//! | `encrypt_to_bytes` + base64                   | [`Nip44::encrypt_v2`] (identical layout)    |
//! | `nip44::encrypt(sk, pk, json)`                | [`Nip44::nip_44_encrypt`]                   |
//!
//! Because every primitive is byte-identical, sessions established here
//! interoperate with Iris's ratchet.

mod group;
mod invite;
mod manager;
mod sender_key;

pub use group::*;
pub use invite::*;
pub use manager::*;
pub use sender_key::*;

use crate::Nip44;
use base64::engine::{general_purpose, Engine as _};
use nostro2_traits::{hex::Hexable as _, NostrKeypair, SignerError};
use std::collections::BTreeMap;

/// Maximum number of skipped message keys retained per chain. Matches the
/// reference implementation's `MAX_SKIP`.
pub const MAX_SKIP: usize = 1000;

/// Nostr event kind carrying a double-ratchet message.
///
/// Mirrors the reference `MESSAGE_EVENT_KIND`. The event is signed by the
/// sender's *current ephemeral* key (not their identity key), its `content`
/// is the ratchet ciphertext, and the encrypted header rides in a
/// `["header", …]` tag.
pub const MESSAGE_EVENT_KIND: u32 = 1060;

/// Tag name under which the NIP-44-encrypted ratchet header is carried.
const HEADER_TAG: &str = "header";

/// Errors raised by the double-ratchet session.
#[derive(Debug)]
pub enum Nip104Error {
    /// The session is not yet in a state that permits sending.
    CannotSendYet,
    /// Required key material is missing from the session state.
    SessionNotReady,
    /// The envelope's sender does not match any known chain.
    UnexpectedSender,
    /// More than [`MAX_SKIP`] messages would have to be skipped.
    TooManySkippedMessages,
    /// The encrypted header could not be decrypted with any of our keys.
    InvalidHeader,
    /// An invite or invite-response was malformed, unsigned, or failed to
    /// decrypt at one of its layers.
    InvalidInvite(String),
    /// A send was requested for a peer the manager has no session with.
    UnknownPeer(String),
    /// Underlying signer / key error.
    Signer(SignerError),
    /// NIP-44 layer error.
    Nip44(crate::Nip44Error),
    /// JSON (de)serialization error.
    Json(String),
    /// Base64 decoding error.
    Base64(base64::DecodeError),
}

impl std::fmt::Display for Nip104Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CannotSendYet => f.write_str("session cannot send yet"),
            Self::SessionNotReady => f.write_str("session not ready: missing key material"),
            Self::UnexpectedSender => f.write_str("envelope sender matches no known chain"),
            Self::TooManySkippedMessages => f.write_str("too many skipped messages"),
            Self::InvalidHeader => f.write_str("could not decrypt message header"),
            Self::InvalidInvite(e) => write!(f, "invalid invite: {e}"),
            Self::UnknownPeer(p) => write!(f, "no session with peer {p}"),
            Self::Signer(e) => write!(f, "signer error: {e}"),
            Self::Nip44(e) => write!(f, "nip-44 error: {e}"),
            Self::Json(e) => write!(f, "json error: {e}"),
            Self::Base64(e) => write!(f, "base64 error: {e}"),
        }
    }
}

impl std::error::Error for Nip104Error {}

impl From<SignerError> for Nip104Error {
    fn from(e: SignerError) -> Self {
        Self::Signer(e)
    }
}
impl From<crate::Nip44Error> for Nip104Error {
    fn from(e: crate::Nip44Error) -> Self {
        Self::Nip44(e)
    }
}
impl From<bourne::Error> for Nip104Error {
    fn from(e: bourne::Error) -> Self {
        Self::Json(format!("{e:?}"))
    }
}
impl From<base64::DecodeError> for Nip104Error {
    fn from(e: base64::DecodeError) -> Self {
        Self::Base64(e)
    }
}

type Result<T> = std::result::Result<T, Nip104Error>;

/// A persisted ephemeral keypair: x-only public key plus its 32-byte secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPairBytes {
    /// x-only public key, 64-char lowercase hex.
    pub public_key: String,
    /// raw secret key, 64-char lowercase hex.
    pub private_key: String,
}

impl KeyPairBytes {
    fn secret_bytes(&self) -> Result<[u8; 32]> {
        let mut buf = [0_u8; 32];
        nostro2_traits::hex::FromHex::decode_hex_to_slice(self.private_key.as_str(), &mut buf)
            .map_err(|_| Nip104Error::Signer(SignerError::InvalidPublicKey))?;
        Ok(buf)
    }
    fn from_secret<K: NostrKeypair>(secret: &[u8; 32]) -> Result<Self> {
        let kp = K::from_secret_bytes(secret)?;
        Ok(Self {
            public_key: kp.public_key(),
            private_key: secret.to_hex(),
        })
    }
}

bourne::json! {
    /// The plaintext ratchet header, transmitted NIP-44-encrypted in each
    /// message. Wire field names are camelCase to match the reference
    /// implementation's JSON exactly (this is the interop-critical type).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Header {
        number: u32,
        #[bourne(rename = "previousChainLength")]
        previous_chain_length: u32,
        #[bourne(rename = "nextPublicKey")]
        next_public_key: String,
    }
}

/// Per-sender map of skipped message keys (index → 32-byte key, hex).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkippedKeysEntry {
    /// message index → message key (hex).
    pub message_keys: BTreeMap<u32, String>,
}

/// The full double-ratchet session state for one 1:1 channel.
///
/// Held in memory and cloned for the plan/apply transaction model. JSON
/// persistence is a separate concern layered on later; the wire-critical
/// [`Header`] is the only type with a fixed external encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    /// Root chain key (hex).
    pub root_key: String,
    /// Peer's current DH public key (x-only hex), if known.
    pub their_current_nostr_public_key: Option<String>,
    /// Peer's next DH public key (x-only hex), if known.
    pub their_next_nostr_public_key: Option<String>,
    /// Our previous DH keypair (kept to decrypt late-arriving messages).
    pub our_previous_nostr_key: Option<KeyPairBytes>,
    /// Our current DH keypair.
    pub our_current_nostr_key: Option<KeyPairBytes>,
    /// Our next DH keypair (advertised in outgoing headers).
    pub our_next_nostr_key: KeyPairBytes,
    /// Receiving chain key (hex), if a receiving chain exists.
    pub receiving_chain_key: Option<String>,
    /// Sending chain key (hex), if a sending chain exists.
    pub sending_chain_key: Option<String>,
    /// Next index to use on the sending chain.
    pub sending_chain_message_number: u32,
    /// Next index expected on the receiving chain.
    pub receiving_chain_message_number: u32,
    /// Number of messages sent on the previous sending chain.
    pub previous_sending_chain_message_count: u32,
    /// Skipped message keys, keyed by sender DH pubkey (hex).
    pub skipped_keys: BTreeMap<String, SkippedKeysEntry>,
}

/// A ready-to-publish encrypted message: the NIP-44-encrypted header and the
/// double-ratcheted ciphertext, plus the sender DH pubkey the recipient uses
/// to locate the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageEnvelope {
    /// Sender's current DH public key (x-only hex).
    pub sender: String,
    /// NIP-44 v2 encrypted, base64 `Header` JSON.
    pub encrypted_header: String,
    /// Base64 NIP-44 v2 ciphertext of the message payload.
    pub ciphertext: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::redundant_pub_crate)]
pub(crate) enum HeaderTarget {
    Current,
    Next,
    Previous,
}

/// A 1:1 double-ratchet session. Generic over the in-process keypair type `K`
/// so it works with the production `K256Keypair` and any test signer alike.
#[derive(Debug, Clone)]
pub struct Session<K: NostrKeypair> {
    /// The serializable ratchet state.
    pub state: SessionState,
    _marker: std::marker::PhantomData<fn() -> K>,
}

impl<K: NostrKeypair> Session<K> {
    /// Wrap an existing [`SessionState`] (e.g. loaded from storage).
    #[must_use]
    pub fn from_state(state: SessionState) -> Self {
        Self {
            state,
            _marker: std::marker::PhantomData,
        }
    }

    /// Initiator-side session bootstrap.
    ///
    /// `their_ephemeral_pubkey` is the peer's ephemeral x-only key, `our_secret`
    /// our ephemeral secret, and `shared_secret` the X3DH output.
    ///
    /// # Errors
    /// Propagates key-construction and NIP-44 derivation failures.
    pub fn new_initiator(
        their_ephemeral_pubkey: &[u8; 32],
        our_secret: &[u8; 32],
        shared_secret: &[u8; 32],
    ) -> Result<Self> {
        Self::init(their_ephemeral_pubkey, our_secret, true, shared_secret)
    }

    /// Responder-side session bootstrap. See [`Session::new_initiator`].
    ///
    /// # Errors
    /// Propagates key-construction and NIP-44 derivation failures.
    pub fn new_responder(
        their_ephemeral_pubkey: &[u8; 32],
        our_secret: &[u8; 32],
        shared_secret: &[u8; 32],
    ) -> Result<Self> {
        Self::init(their_ephemeral_pubkey, our_secret, false, shared_secret)
    }

    fn init(
        their_ephemeral_pubkey: &[u8; 32],
        our_secret: &[u8; 32],
        is_initiator: bool,
        shared_secret: &[u8; 32],
    ) -> Result<Self> {
        let our_current = KeyPairBytes::from_secret::<K>(our_secret)?;
        let their_next_hex = their_ephemeral_pubkey.to_hex();

        let state = if is_initiator {
            let our_next_secret = K::generate().secret_bytes();
            let our_next = KeyPairBytes::from_secret::<K>(&our_next_secret)?;
            // root/sending chains seeded from a DH between our *next* key and
            // the peer's ephemeral key, mixed into the shared secret.
            let conv = K::derive_conv_key(&our_next_secret, their_ephemeral_pubkey)?;
            let outs = K::kdf(shared_secret, &conv, 2);
            SessionState {
                root_key: outs[0].to_hex(),
                their_current_nostr_public_key: None,
                their_next_nostr_public_key: Some(their_next_hex),
                our_previous_nostr_key: None,
                our_current_nostr_key: Some(our_current),
                our_next_nostr_key: our_next,
                receiving_chain_key: None,
                sending_chain_key: Some(outs[1].to_hex()),
                sending_chain_message_number: 0,
                receiving_chain_message_number: 0,
                previous_sending_chain_message_count: 0,
                skipped_keys: BTreeMap::new(),
            }
        } else {
            SessionState {
                root_key: shared_secret.to_hex(),
                their_current_nostr_public_key: None,
                their_next_nostr_public_key: Some(their_next_hex),
                our_previous_nostr_key: None,
                our_current_nostr_key: None,
                our_next_nostr_key: our_current,
                receiving_chain_key: None,
                sending_chain_key: None,
                sending_chain_message_number: 0,
                receiving_chain_message_number: 0,
                previous_sending_chain_message_count: 0,
                skipped_keys: BTreeMap::new(),
            }
        };

        Ok(Self::from_state(state))
    }

    /// Whether the session currently holds enough state to encrypt a message.
    #[must_use]
    pub const fn can_send(&self) -> bool {
        self.state.their_next_nostr_public_key.is_some()
            && self.state.our_current_nostr_key.is_some()
    }

    fn matches_sender(&self, sender: &str) -> bool {
        self.state.their_current_nostr_public_key.as_deref() == Some(sender)
            || self.state.their_next_nostr_public_key.as_deref() == Some(sender)
            || self.state.skipped_keys.contains_key(sender)
    }

    /// The peer DH public keys (x-only hex) this session will currently accept
    /// as a message `sender` — exactly the set [`matches_sender`](Self::matches_sender)
    /// tests: the peer's current and next ratchet keys plus every banked
    /// skipped-chain key.
    ///
    /// This set changes **only when a message is received** (the DH ratchet
    /// turns or a skipped key is banked); sending never alters it. A router can
    /// therefore index sessions by these keys and refresh the index after each
    /// [`plan_receive`](Self::plan_receive)/[`apply`](Self::apply), giving O(1)
    /// inbound routing instead of trial-decrypting every session.
    #[must_use]
    pub fn accepted_senders(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(2 + self.state.skipped_keys.len());
        if let Some(cur) = self.state.their_current_nostr_public_key.as_deref() {
            out.push(cur.to_owned());
        }
        if let Some(next) = self.state.their_next_nostr_public_key.as_deref() {
            if Some(next) != self.state.their_current_nostr_public_key.as_deref() {
                out.push(next.to_owned());
            }
        }
        for k in self.state.skipped_keys.keys() {
            if Some(k.as_str()) != self.state.their_current_nostr_public_key.as_deref()
                && Some(k.as_str()) != self.state.their_next_nostr_public_key.as_deref()
            {
                out.push(k.clone());
            }
        }
        out
    }

    /// Encrypt `payload`, returning the envelope and the post-send state.
    ///
    /// The session is **not** mutated; call [`Session::apply`] with the returned
    /// state to commit. This separation lets callers persist atomically.
    ///
    /// # Errors
    /// [`Nip104Error::CannotSendYet`] if no sending chain exists, plus any
    /// crypto failure.
    pub fn plan_send(&self, payload: &[u8]) -> Result<(SessionState, MessageEnvelope)> {
        if !self.can_send() {
            return Err(Nip104Error::CannotSendYet);
        }
        let mut next = self.state.clone();
        let (header, ciphertext) = K::ratchet_encrypt(&mut next, payload)?;

        let our_current = self
            .state
            .our_current_nostr_key
            .as_ref()
            .ok_or(Nip104Error::SessionNotReady)?;
        let their_next = self
            .state
            .their_next_nostr_public_key
            .as_deref()
            .ok_or(Nip104Error::SessionNotReady)?;

        let our_kp = K::from_secret_bytes(&our_current.secret_bytes()?)?;
        let header_json = bourne::to_string(&header)?;
        let encrypted_header = our_kp.nip_44_encrypt(&header_json, their_next)?.into_owned();

        Ok((
            next,
            MessageEnvelope {
                sender: our_current.public_key.clone(),
                encrypted_header,
                ciphertext,
            },
        ))
    }

    /// Decrypt `envelope`, returning the plaintext and the post-receive state.
    /// The session is not mutated; commit with [`Session::apply`].
    ///
    /// # Errors
    /// [`Nip104Error::UnexpectedSender`] if the sender is unknown, plus any
    /// crypto or ratchet failure.
    pub fn plan_receive(&self, envelope: &MessageEnvelope) -> Result<(SessionState, Vec<u8>)> {
        if !self.matches_sender(&envelope.sender) {
            return Err(Nip104Error::UnexpectedSender);
        }
        let mut next = self.state.clone();
        let previous_chain_sender = next
            .their_current_nostr_public_key
            .clone()
            .or_else(|| next.their_next_nostr_public_key.clone());

        let (header, target) = K::decrypt_header(&next, &envelope.encrypted_header, &envelope.sender)?;
        let should_ratchet = target == HeaderTarget::Next;

        if should_ratchet && next.their_next_nostr_public_key.as_ref() != Some(&header.next_public_key) {
            next.their_current_nostr_public_key = next.their_next_nostr_public_key.take();
            next.their_next_nostr_public_key = Some(header.next_public_key.clone());
        }

        if should_ratchet {
            if next.receiving_chain_key.is_some() {
                let skipped_sender = previous_chain_sender.ok_or(Nip104Error::SessionNotReady)?;
                K::skip_message_keys(&mut next, header.previous_chain_length, &skipped_sender)?;
            }
            K::ratchet_step(&mut next)?;
        }

        let payload = K::ratchet_decrypt(&mut next, &header, &envelope.ciphertext, &envelope.sender)?;
        Ok((next, payload))
    }

    /// Commit a planned state transition produced by [`Session::plan_send`] or
    /// [`Session::plan_receive`].
    pub fn apply(&mut self, next: SessionState) {
        self.state = next;
    }

    /// Like [`plan_send`](Self::plan_send), but also renders a ready-to-publish,
    /// signed kind-[`MESSAGE_EVENT_KIND`] Nostr event.
    ///
    /// The event is signed by the sender's *current ephemeral* key (the one
    /// driving the ratchet), exactly as the reference implementation does — so
    /// the published event interoperates with Iris. The session is not
    /// mutated; commit the returned state with [`apply`](Self::apply).
    ///
    /// `created_at` is the Unix timestamp to stamp on the event.
    ///
    /// # Errors
    /// Propagates [`plan_send`](Self::plan_send) failures plus any signing
    /// error.
    pub fn plan_send_event(
        &self,
        payload: &[u8],
        created_at: i64,
    ) -> Result<(SessionState, nostro2::NostrNote)> {
        let (next, envelope) = self.plan_send(payload)?;
        let our_current = self
            .state
            .our_current_nostr_key
            .as_ref()
            .ok_or(Nip104Error::SessionNotReady)?;
        let signer = K::from_secret_bytes(&our_current.secret_bytes()?)?;
        let event = envelope.to_event(&signer, created_at)?;
        Ok((next, event))
    }

    /// Like [`plan_receive`](Self::plan_receive), but takes a raw kind-1060
    /// Nostr event, verifies it, and extracts the envelope before decrypting.
    ///
    /// # Errors
    /// [`Nip104Error::InvalidHeader`] if the event is malformed or fails
    /// signature verification, plus any [`plan_receive`](Self::plan_receive)
    /// failure.
    pub fn plan_receive_event(
        &self,
        event: &nostro2::NostrNote,
    ) -> Result<(SessionState, Vec<u8>)> {
        let envelope = MessageEnvelope::from_event(event)?;
        self.plan_receive(&envelope)
    }
}

impl MessageEnvelope {
    /// Render this envelope as a signed kind-[`MESSAGE_EVENT_KIND`] Nostr event.
    ///
    /// `signer` must be the sender's current ephemeral keypair (its public key
    /// must equal [`self.sender`](Self::sender)); the reference implementation
    /// signs ratchet messages with that key, and the recipient locates the
    /// chain by the event's `pubkey`.
    ///
    /// # Errors
    /// Returns [`Nip104Error::Signer`] if signing fails.
    pub fn to_event<S: nostro2::NostrSigner>(
        &self,
        signer: &S,
        created_at: i64,
    ) -> Result<nostro2::NostrNote> {
        let mut tags = nostro2::NostrTags::new();
        tags.add_custom_tag(HEADER_TAG, &self.encrypted_header);
        let mut note = nostro2::NostrNote {
            kind: MESSAGE_EVENT_KIND,
            content: self.ciphertext.clone(),
            created_at,
            tags,
            ..Default::default()
        };
        note.sign_with(signer)
            .map_err(|_| Nip104Error::Signer(SignerError::InvalidSignature))?;
        Ok(note)
    }

    /// Parse and verify a kind-[`MESSAGE_EVENT_KIND`] Nostr event into an
    /// envelope. The event's `pubkey` becomes the [`sender`](Self::sender),
    /// its `content` the ciphertext, and the `["header", …]` tag the encrypted
    /// header.
    ///
    /// # Errors
    /// [`Nip104Error::InvalidHeader`] if the kind is wrong, the signature is
    /// invalid, or the `header` tag is missing.
    pub fn from_event(event: &nostro2::NostrNote) -> Result<Self> {
        use nostro2::NostrEvent as _;
        if event.kind != MESSAGE_EVENT_KIND {
            return Err(Nip104Error::InvalidHeader);
        }
        if !event.verify() {
            return Err(Nip104Error::InvalidHeader);
        }
        let encrypted_header = event
            .tags
            .find_tags_ref(HEADER_TAG)
            .into_iter()
            .next()
            .ok_or(Nip104Error::InvalidHeader)?
            .to_owned();
        Ok(Self {
            sender: event.pubkey.clone(),
            encrypted_header,
            ciphertext: event.content.clone(),
        })
    }
}

// ── Ratchet internals (faithful port of session.rs) ───────────────────

/// Crate-internal extension trait gathering the double-ratchet crypto glue
/// that is generic over the in-process keypair `K`. Mirrors the structure of
/// [`crate::Nip44`]: a blanket-implemented trait on `NostrKeypair`, so no
/// free functions leak into the module. Every method is an associated function
/// (no `self`); `K` is the implementing key type.
#[allow(clippy::redundant_pub_crate)]
pub(crate) trait Nip104Crypto: NostrKeypair + Sized {
    /// HKDF-SHA256 KDF used for all chain stepping.
    ///
    /// Mirrors the reference `kdf(input1, input2, num_outputs)`: `input2` is the
    /// salt, `input1` the IKM, and each output `i` (1-based) is
    /// `HKDF-Expand(info = [i])` truncated to 32 bytes.
    fn kdf(input1: &[u8], input2: &[u8], num_outputs: usize) -> Vec<[u8; 32]> {
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(input2), input1);
        let mut outputs = Vec::with_capacity(num_outputs);
        for i in 1..=num_outputs {
            let mut okm = [0_u8; 32];
            hk.expand(&[u8::try_from(i).unwrap_or(u8::MAX)], &mut okm)
                .expect("32 bytes is a valid HKDF length");
            outputs.push(okm);
        }
        outputs
    }

    /// Decode a 64-char lowercase-hex string into 32 raw bytes.
    fn decode_hex_32(s: &str) -> Result<[u8; 32]> {
        let mut buf = [0_u8; 32];
        nostro2_traits::hex::FromHex::decode_hex_to_slice(s, &mut buf)
            .map_err(|_| Nip104Error::Signer(SignerError::InvalidPublicKey))?;
        Ok(buf)
    }

    /// `ConversationKey::derive(sk, pk)` — ECDH x-coordinate fed through NIP-44
    /// v2 conversation-key derivation (HKDF-extract, salt `"nip44-v2"`).
    fn derive_conv_key(sk: &[u8; 32], pk: &[u8; 32]) -> Result<[u8; 32]> {
        let kp = Self::from_secret_bytes(sk)?;
        let shared = kp.ecdh_x(pk)?;
        let conv = <Self as Nip44>::conversation_key_v2(zeroize::Zeroizing::new(shared))?;
        Ok(*conv)
    }

    /// Encrypt with a raw 32-byte message key as the NIP-44 v2 conversation
    /// key, returning the standard base64 payload (`Ag…`). Equivalent to the
    /// reference's `ConversationKey::new(mk)` + `encrypt_to_bytes` + base64.
    fn encrypt_with_message_key(message_key: &[u8; 32], plaintext: &[u8]) -> Result<String> {
        let nonce = Self::generate_nonce_32();
        Ok(<Self as Nip44>::encrypt_v2(message_key, &nonce, plaintext)?)
    }

    /// Inverse of [`encrypt_with_message_key`](Self::encrypt_with_message_key),
    /// returning the raw plaintext bytes.
    fn decrypt_with_message_key(message_key: &[u8; 32], ciphertext_b64: &str) -> Result<Vec<u8>> {
        let decoded = general_purpose::STANDARD.decode(ciphertext_b64)?;
        let s = <Self as Nip44>::decrypt_v2_bytes(message_key, &decoded)?;
        Ok(s)
    }

    /// Drop a fresh message key off the sending chain, advancing it, and return
    /// the header + ciphertext for `plaintext`.
    fn ratchet_encrypt(state: &mut SessionState, plaintext: &[u8]) -> Result<(Header, String)> {
        let sending_chain_key = Self::decode_hex_32(
            state
                .sending_chain_key
                .as_deref()
                .ok_or(Nip104Error::SessionNotReady)?,
        )?;
        let outs = Self::kdf(&sending_chain_key, &[1_u8], 2);
        state.sending_chain_key = Some(outs[0].to_hex());
        let message_key = outs[1];

        let header = Header {
            number: state.sending_chain_message_number,
            next_public_key: state.our_next_nostr_key.public_key.clone(),
            previous_chain_length: state.previous_sending_chain_message_count,
        };
        state.sending_chain_message_number += 1;

        let ciphertext = Self::encrypt_with_message_key(&message_key, plaintext)?;
        Ok((header, ciphertext))
    }

    /// Pull the matching message key off the receiving chain (or the skipped
    /// store) and decrypt `ciphertext`.
    fn ratchet_decrypt(
        state: &mut SessionState,
        header: &Header,
        ciphertext: &str,
        sender: &str,
    ) -> Result<Vec<u8>> {
        if let Some(pt) = Self::try_skipped_message_keys(state, header, ciphertext, sender)? {
            return Ok(pt);
        }
        if state.receiving_chain_key.is_none() {
            return Err(Nip104Error::SessionNotReady);
        }
        Self::skip_message_keys(state, header.number, sender)?;

        let receiving_chain_key = Self::decode_hex_32(
            state
                .receiving_chain_key
                .as_deref()
                .ok_or(Nip104Error::SessionNotReady)?,
        )?;
        let outs = Self::kdf(&receiving_chain_key, &[1_u8], 2);
        state.receiving_chain_key = Some(outs[0].to_hex());
        let message_key = outs[1];
        state.receiving_chain_message_number += 1;

        Self::decrypt_with_message_key(&message_key, ciphertext)
    }

    /// Perform the DH ratchet step: derive a new receiving chain from the
    /// peer's next key, then a fresh sending chain + root from a new DH key.
    fn ratchet_step(state: &mut SessionState) -> Result<()> {
        state.previous_sending_chain_message_count = state.sending_chain_message_number;
        state.sending_chain_message_number = 0;
        state.receiving_chain_message_number = 0;

        let their_next = state
            .their_next_nostr_public_key
            .as_deref()
            .ok_or(Nip104Error::SessionNotReady)?;
        let their_next_bytes = Self::decode_hex_32(their_next)?;
        let root_key = Self::decode_hex_32(&state.root_key)?;

        // First DH: our_next × their_next → new receiving chain.
        let conv1 =
            Self::derive_conv_key(&state.our_next_nostr_key.secret_bytes()?, &their_next_bytes)?;
        let outs1 = Self::kdf(&root_key, &conv1, 2);
        state.receiving_chain_key = Some(outs1[1].to_hex());
        state.our_previous_nostr_key = state.our_current_nostr_key.take();
        state.our_current_nostr_key = Some(state.our_next_nostr_key.clone());

        // Fresh DH key, second DH → new root + sending chain.
        let our_next_secret = Self::generate().secret_bytes();
        state.our_next_nostr_key = KeyPairBytes::from_secret::<Self>(&our_next_secret)?;
        let conv2 = Self::derive_conv_key(&our_next_secret, &their_next_bytes)?;
        let outs2 = Self::kdf(&outs1[0], &conv2, 2);
        state.root_key = outs2[0].to_hex();
        state.sending_chain_key = Some(outs2[1].to_hex());
        Ok(())
    }

    /// Advance the receiving chain to `until`, banking each skipped message key
    /// under `sender` for out-of-order delivery.
    fn skip_message_keys(state: &mut SessionState, until: u32, sender: &str) -> Result<()> {
        if until <= state.receiving_chain_message_number {
            return Ok(());
        }
        if (until - state.receiving_chain_message_number) as usize > MAX_SKIP {
            return Err(Nip104Error::TooManySkippedMessages);
        }
        let entry = state.skipped_keys.entry(sender.to_owned()).or_default();
        while state.receiving_chain_message_number < until {
            let rck = Self::decode_hex_32(
                state
                    .receiving_chain_key
                    .as_deref()
                    .ok_or(Nip104Error::SessionNotReady)?,
            )?;
            let outs = Self::kdf(&rck, &[1_u8], 2);
            state.receiving_chain_key = Some(outs[0].to_hex());
            entry
                .message_keys
                .insert(state.receiving_chain_message_number, outs[1].to_hex());
            state.receiving_chain_message_number += 1;
        }
        Self::prune_skipped(&mut entry.message_keys);
        Ok(())
    }

    /// Try a banked skipped message key for `header.number`; on success consume
    /// it and return the plaintext.
    fn try_skipped_message_keys(
        state: &mut SessionState,
        header: &Header,
        ciphertext: &str,
        sender: &str,
    ) -> Result<Option<Vec<u8>>> {
        let Some(entry) = state.skipped_keys.get_mut(sender) else {
            return Ok(None);
        };
        let Some(mk_hex) = entry.message_keys.remove(&header.number) else {
            return Ok(None);
        };
        let message_key = Self::decode_hex_32(&mk_hex)?;
        let pt = Self::decrypt_with_message_key(&message_key, ciphertext)?;
        if entry.message_keys.is_empty() {
            state.skipped_keys.remove(sender);
        }
        Ok(Some(pt))
    }

    /// Decrypt the message header, trying our current/next/previous DH keys and
    /// reporting which one matched (so the caller knows whether to ratchet).
    fn decrypt_header(
        state: &SessionState,
        encrypted_header: &str,
        sender: &str,
    ) -> Result<(Header, HeaderTarget)> {
        if let Some(current) = &state.our_current_nostr_key {
            if let Ok(h) = Self::try_decrypt_header(&current.secret_bytes()?, sender, encrypted_header)
            {
                return Ok((h, HeaderTarget::Current));
            }
        }
        if let Ok(h) =
            Self::try_decrypt_header(&state.our_next_nostr_key.secret_bytes()?, sender, encrypted_header)
        {
            return Ok((h, HeaderTarget::Next));
        }
        if let Some(previous) = &state.our_previous_nostr_key {
            if let Ok(h) =
                Self::try_decrypt_header(&previous.secret_bytes()?, sender, encrypted_header)
            {
                return Ok((h, HeaderTarget::Previous));
            }
        }
        Err(Nip104Error::InvalidHeader)
    }

    /// Decrypt a header with one specific DH secret.
    fn try_decrypt_header(
        our_secret: &[u8; 32],
        sender: &str,
        encrypted_header: &str,
    ) -> Result<Header> {
        let kp = Self::from_secret_bytes(our_secret)?;
        let json = kp.nip_44_decrypt(encrypted_header, sender)?;
        Ok(bourne::parse_str(&json)?)
    }

    /// Bound the skipped-key store to [`MAX_SKIP`], evicting the oldest.
    fn prune_skipped(map: &mut BTreeMap<u32, String>) {
        while map.len() > MAX_SKIP {
            let Some(first) = map.keys().next().copied() else {
                break;
            };
            map.remove(&first);
        }
    }
}

impl<K: NostrKeypair> Nip104Crypto for K {}

#[cfg(test)]
mod tests {
    use super::*;

    use nostro2_traits::NostrSigner as _;

    type K = crate::tests::NipTester;

    fn shared_secret() -> [u8; 32] {
        [7_u8; 32]
    }

    #[test]
    fn kdf_matches_reference_shape() {
        // Two outputs, salt and ikm distinct, deterministic.
        let a = K::kdf(&[1_u8; 32], &[2_u8; 32], 2);
        let b = K::kdf(&[1_u8; 32], &[2_u8; 32], 2);
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
        assert_ne!(a[0], a[1]);
    }

    #[test]
    fn header_json_is_camel_case_and_round_trips() {
        let h = Header {
            number: 3,
            previous_chain_length: 2,
            next_public_key: "ab".repeat(32),
        };
        let s = bourne::to_string(&h).unwrap();
        // Wire format must use camelCase keys to interoperate with Iris.
        assert!(s.contains("\"previousChainLength\":2"), "got {s}");
        assert!(s.contains("\"nextPublicKey\":"), "got {s}");
        assert!(!s.contains("previous_chain_length"), "got {s}");
        let back: Header = bourne::parse_str(&s).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn round_trip_single_message() {
        let alice_secret = [1_u8; 32];
        let bob_secret = [2_u8; 32];
        let alice_pub = K::from_secret_bytes(&alice_secret).unwrap().pubkey_bytes();
        let bob_pub = K::from_secret_bytes(&bob_secret).unwrap().pubkey_bytes();

        let alice =
            Session::<K>::new_initiator(&bob_pub, &alice_secret, &shared_secret()).unwrap();
        let mut bob =
            Session::<K>::new_responder(&alice_pub, &bob_secret, &shared_secret()).unwrap();

        let (alice_next, envelope) = alice.plan_send(b"hello bob").unwrap();
        let _ = alice_next; // single message; no need to commit

        let (bob_next, plaintext) = bob.plan_receive(&envelope).unwrap();
        bob.apply(bob_next);
        assert_eq!(plaintext, b"hello bob");
    }

    /// Drive a full back-and-forth conversation, which forces the DH ratchet
    /// to turn on every change of speaker. This is the real test of the
    /// double ratchet (vs. a single in-band message).
    #[test]
    fn bidirectional_conversation_ratchets() {
        let alice_secret = [1_u8; 32];
        let bob_secret = [2_u8; 32];
        let alice_pub = K::from_secret_bytes(&alice_secret).unwrap().pubkey_bytes();
        let bob_pub = K::from_secret_bytes(&bob_secret).unwrap().pubkey_bytes();

        let mut alice =
            Session::<K>::new_initiator(&bob_pub, &alice_secret, &shared_secret()).unwrap();
        let mut bob =
            Session::<K>::new_responder(&alice_pub, &bob_secret, &shared_secret()).unwrap();

        // Alice -> Bob (two messages on the same chain)
        for msg in [b"a1".as_slice(), b"a2".as_slice()] {
            let (an, env) = alice.plan_send(msg).unwrap();
            alice.apply(an);
            let (bn, pt) = bob.plan_receive(&env).unwrap();
            bob.apply(bn);
            assert_eq!(pt, msg);
        }

        // Bob -> Alice (sender change: DH ratchet turns)
        for msg in [b"b1".as_slice(), b"b2".as_slice(), b"b3".as_slice()] {
            let (bn, env) = bob.plan_send(msg).unwrap();
            bob.apply(bn);
            let (an, pt) = alice.plan_receive(&env).unwrap();
            alice.apply(an);
            assert_eq!(pt, msg);
        }

        // Alice -> Bob again (ratchet turns back)
        let (an, env) = alice.plan_send(b"a3").unwrap();
        alice.apply(an);
        let (bn, pt) = bob.plan_receive(&env).unwrap();
        bob.apply(bn);
        assert_eq!(pt, b"a3");
    }

    /// Messages that arrive out of order must still decrypt via skipped keys.
    #[test]
    fn out_of_order_delivery_uses_skipped_keys() {
        let alice_secret = [3_u8; 32];
        let bob_secret = [4_u8; 32];
        let alice_pub = K::from_secret_bytes(&alice_secret).unwrap().pubkey_bytes();
        let bob_pub = K::from_secret_bytes(&bob_secret).unwrap().pubkey_bytes();

        let mut alice =
            Session::<K>::new_initiator(&bob_pub, &alice_secret, &shared_secret()).unwrap();
        let mut bob =
            Session::<K>::new_responder(&alice_pub, &bob_secret, &shared_secret()).unwrap();

        let (a1, env1) = alice.plan_send(b"first").unwrap();
        alice.apply(a1);
        let (a2, env2) = alice.plan_send(b"second").unwrap();
        alice.apply(a2);
        let (a3, env3) = alice.plan_send(b"third").unwrap();
        alice.apply(a3);

        // Bob receives 1, then 3 (skips 2), then the late 2.
        let (bn, pt1) = bob.plan_receive(&env1).unwrap();
        bob.apply(bn);
        assert_eq!(pt1, b"first");
        let (bn, pt3) = bob.plan_receive(&env3).unwrap();
        bob.apply(bn);
        assert_eq!(pt3, b"third");
        let (bn, pt2) = bob.plan_receive(&env2).unwrap();
        bob.apply(bn);
        assert_eq!(pt2, b"second");
    }

    /// Cross-implementation oracle. Bob, reconstructed as the responder from
    /// the vector's fixed keys, must decrypt the **actual msg1 event produced
    /// by the reference Rust implementation** (mmalmi/nostr-double-ratchet).
    ///
    /// This is the real interop proof: a foreign implementation's ciphertext,
    /// header, and KDF chain all decrypt under our native ratchet. (Encryption
    /// is non-reproducible — random next-keys + nonces — so only decryption is
    /// a pure function of the published inputs.)
    #[test]
    fn rust_reference_vector_msg1_decrypts() {
        let vec_json = include_str!("../../test-vectors/nip104-rust-generated.json");

        // Minimal field extraction (avoids a serde dep; bourne can't do
        // arbitrary maps). The vector file is fixed, so this is safe.
        let field = |key: &str| -> String {
            let needle = format!("\"{key}\":");
            let start = vec_json.find(&needle).expect("key present") + needle.len();
            let rest = &vec_json[start..];
            let q1 = rest.find('"').unwrap() + 1;
            let q2 = rest[q1..].find('"').unwrap();
            rest[q1..q1 + q2].to_string()
        };

        let bob_sk = K::decode_hex_32(&field("bob_ephemeral_sk")).unwrap();
        let alice_pk = K::decode_hex_32(&field("alice_ephemeral_pk")).unwrap();
        let shared = K::decode_hex_32(&field("shared_secret")).unwrap();
        let plaintext = field("plaintext");
        let sender = field("pubkey");
        let header = field("header");
        let content = field("content");

        let mut bob = Session::<K>::new_responder(&alice_pk, &bob_sk, &shared).unwrap();
        let envelope = MessageEnvelope {
            sender,
            encrypted_header: header,
            ciphertext: content,
        };

        let (next, payload) = bob
            .plan_receive(&envelope)
            .expect("reference msg1 must decrypt under native ratchet");
        bob.apply(next);

        // The ratchet payload is a rumor-event JSON; its `content` carries the
        // human plaintext. Assert the foreign plaintext survived round-trip.
        let decoded = String::from_utf8(payload).expect("payload is UTF-8 JSON");
        assert!(
            decoded.contains(&plaintext),
            "decrypted rumor {decoded:?} must contain plaintext {plaintext:?}"
        );
    }

    /// Cross-implementation codec oracle. Parse the reference Rust impl's
    /// **actual signed kind-1060 event** (verifying its real Schnorr
    /// signature) and decrypt it end-to-end through our wire codec. Proves the
    /// event shape — kind, `header` tag, signing key — matches the reference,
    /// on top of the crypto.
    #[test]
    fn rust_reference_msg1_event_decrypts_via_codec() {
        let vec_json = include_str!("../../test-vectors/nip104-rust-generated.json");

        let field = |key: &str| -> String {
            let needle = format!("\"{key}\":");
            let start = vec_json.find(&needle).expect("key present") + needle.len();
            let rest = &vec_json[start..];
            let q1 = rest.find('"').unwrap() + 1;
            let q2 = rest[q1..].find('"').unwrap();
            rest[q1..q1 + q2].to_string()
        };

        let bob_sk = K::decode_hex_32(&field("bob_ephemeral_sk")).unwrap();
        let alice_pk = K::decode_hex_32(&field("alice_ephemeral_pk")).unwrap();
        let shared = K::decode_hex_32(&field("shared_secret")).unwrap();
        let plaintext = field("plaintext");

        // Extract the embedded `msg1_event` JSON object and parse it as a note.
        let ev_start = vec_json.find("\"msg1_event\":").unwrap()
            + "\"msg1_event\":".len();
        let obj_start = vec_json[ev_start..].find('{').unwrap() + ev_start;
        let obj_end = vec_json[obj_start..].find('}').unwrap() + obj_start + 1;
        let event: nostro2::NostrNote = vec_json[obj_start..obj_end].parse().unwrap();

        let mut bob = Session::<K>::new_responder(&alice_pk, &bob_sk, &shared).unwrap();
        let (next, payload) = bob
            .plan_receive_event(&event)
            .expect("reference event must decrypt via the native codec");
        bob.apply(next);

        let decoded = String::from_utf8(payload).unwrap();
        assert!(decoded.contains(&plaintext), "got {decoded:?}");
    }

    /// Full transport round-trip: Alice ratchet-encrypts, renders a *signed
    /// kind-1060 Nostr event*, and Bob decrypts straight from that event. This
    /// exercises the wire codec (kind, `header` tag, ephemeral-key signature)
    /// end-to-end, not just the in-memory envelope.
    #[test]
    fn message_event_codec_round_trips() {
        use nostro2::NostrEvent as _;

        let alice_secret = [1_u8; 32];
        let bob_secret = [2_u8; 32];
        let alice_pub = K::from_secret_bytes(&alice_secret).unwrap().pubkey_bytes();
        let bob_pub = K::from_secret_bytes(&bob_secret).unwrap().pubkey_bytes();

        let mut alice =
            Session::<K>::new_initiator(&bob_pub, &alice_secret, &shared_secret()).unwrap();
        let mut bob =
            Session::<K>::new_responder(&alice_pub, &bob_secret, &shared_secret()).unwrap();

        let (anext, event) = alice.plan_send_event(b"over the wire", 1_700_000_000).unwrap();
        alice.apply(anext);

        // The rendered event must look like a real NIP-104 message.
        assert_eq!(event.kind, MESSAGE_EVENT_KIND);
        assert!(event.verify(), "event must be self-consistently signed");
        assert_eq!(event.created_at, 1_700_000_000);
        assert_eq!(event.tags.find_tags_ref(HEADER_TAG).len(), 1);
        // Signed by Alice's *current ephemeral* key (the chain locator).
        assert_eq!(event.pubkey, alice_pub.to_hex());

        // Bob decrypts straight from the wire event.
        let (bnext, pt) = bob.plan_receive_event(&event).unwrap();
        bob.apply(bnext);
        assert_eq!(pt, b"over the wire");
    }

    /// A tampered ciphertext invalidates the signature, so the codec rejects
    /// the event before it ever reaches the ratchet.
    #[test]
    fn message_event_rejects_tampering() {
        let alice_secret = [1_u8; 32];
        let bob_secret = [2_u8; 32];
        let alice_pub = K::from_secret_bytes(&alice_secret).unwrap().pubkey_bytes();
        let bob_pub = K::from_secret_bytes(&bob_secret).unwrap().pubkey_bytes();

        let alice =
            Session::<K>::new_initiator(&bob_pub, &alice_secret, &shared_secret()).unwrap();
        let bob =
            Session::<K>::new_responder(&alice_pub, &bob_secret, &shared_secret()).unwrap();

        let (_anext, mut event) = alice.plan_send_event(b"tamper me", 1_700_000_000).unwrap();
        event.content.push('A'); // breaks the id/signature

        assert!(matches!(
            bob.plan_receive_event(&event),
            Err(Nip104Error::InvalidHeader)
        ));
    }

    #[test]
    fn unknown_sender_rejected() {
        let bob_secret = [2_u8; 32];
        let alice_pub = K::from_secret_bytes(&[1_u8; 32]).unwrap().pubkey_bytes();
        let bob = Session::<K>::new_responder(&alice_pub, &bob_secret, &shared_secret()).unwrap();

        let envelope = MessageEnvelope {
            sender: "cd".repeat(32),
            encrypted_header: "x".into(),
            ciphertext: "x".into(),
        };
        assert!(matches!(
            bob.plan_receive(&envelope),
            Err(Nip104Error::UnexpectedSender)
        ));
    }

    #[test]
    fn error_display_covers_all_variants() {
        use std::error::Error as _;

        let cases: Vec<Nip104Error> = vec![
            Nip104Error::CannotSendYet,
            Nip104Error::SessionNotReady,
            Nip104Error::UnexpectedSender,
            Nip104Error::TooManySkippedMessages,
            Nip104Error::InvalidHeader,
            Nip104Error::InvalidInvite("bad layer".into()),
            Nip104Error::UnknownPeer("npub1xyz".into()),
            Nip104Error::Signer(SignerError::InvalidSignature),
            Nip104Error::Nip44(crate::Nip44Error::MacMismatch),
            Nip104Error::Json("unexpected token".into()),
            Nip104Error::Base64(general_purpose::STANDARD.decode("!!!").unwrap_err()),
        ];
        for err in &cases {
            assert!(!format!("{err}").is_empty(), "Display empty for {err:?}");
            // The blanket `Error` impl carries no source; just exercise it.
            let _ = err.source();
        }
    }
}
