//! NIP-104 — Group **sender keys** (one-to-many ratchet).
//!
//! 1:1 [`crate::nip_104::Session`]s give every pair of devices its own
//! double ratchet. That does not scale to groups: encrypting one message to
//! `N` members costs `N` ciphertexts. The reference
//! (`mmalmi/nostr-double-ratchet`) solves this with **sender keys**, the same
//! construction Signal uses for groups:
//!
//! * Each sending device owns a per-group **sender-key chain** — a symmetric
//!   KDF ratchet seeded by a random chain key.
//! * The chain key (plus its `key_id` and current `iteration`) is distributed
//!   **once per member** over the authenticated 1:1 sessions — a
//!   [`SenderKeyDistribution`].
//! * Thereafter every group message is published **once** as a single
//!   one-to-many event, encrypted with the next message key pulled off the
//!   chain. Members who hold the distribution ratchet their copy forward and
//!   decrypt.
//!
//! This module is the pure state machine for that chain — the interop-critical
//! core. It is byte-compatible with the reference `SenderKeyState`:
//!
//! * KDF salt `ndr-sender-key-v1`,
//! * `kdf(chain_key, salt, 2)` → `[next_chain_key, message_key]` per step
//!   (reusing [`crate::nip_104::kdf`], shared with the 1:1 ratchet),
//! * message keys feed NIP-44 v2 as conversation keys,
//! * out-of-order delivery handled by storing skipped message keys by index,
//! * bounded skip (`SENDER_KEY_MAX_SKIP`) and stored-key pruning
//!   (`SENDER_KEY_MAX_STORED_SKIPPED_KEYS`).
//!
//! Like the rest of the crate it follows the **plan / apply** transaction
//! model: [`plan_encrypt`](SenderKeyState::plan_encrypt) /
//! [`plan_decrypt`](SenderKeyState::plan_decrypt) are pure and return a
//! `next_state`; nothing mutates until you `apply`.

use std::collections::BTreeMap;

use nostro2_traits::hex::Hexable;
use nostro2_traits::NostrKeypair;

use super::{Nip104Crypto, Nip104Error};

type Result<T> = std::result::Result<T, Nip104Error>;

/// Maximum number of message keys we will skip ahead to decrypt an
/// out-of-order message. Matches the reference.
pub const SENDER_KEY_MAX_SKIP: u32 = 10_000;

/// Maximum number of skipped message keys retained at once (oldest pruned).
pub const SENDER_KEY_MAX_STORED_SKIPPED_KEYS: usize = 2_000;

/// HKDF salt domain-separating the sender-key chain from the 1:1 ratchet.
const SENDER_KEY_KDF_SALT: &[u8] = b"ndr-sender-key-v1";

/// One sender-key chain: a symmetric KDF ratchet identified by `key_id`.
///
/// Both the sender and every receiver hold a copy seeded from the same
/// distribution; the sender advances it on encrypt, receivers on decrypt.
/// Chain keys are stored as 64-char hex to match the crate's `SessionState`
/// convention (the reference stores raw bytes; the wire `SenderKeyDistribution`
/// is where the hex/byte boundary lives).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderKeyState {
    /// Identifies which chain this is; rotates on membership/key change.
    pub key_id: u32,
    /// Current chain key (64-char hex).
    chain_key: String,
    /// Next message index this chain will produce/expect.
    iteration: u32,
    /// Skipped message keys (index → key hex) for out-of-order delivery.
    skipped_message_keys: BTreeMap<u32, String>,
}

/// Result of [`SenderKeyState::plan_encrypt`] — apply to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderKeyEncryptPlan {
    /// The state after this encryption.
    pub next_state: SenderKeyState,
    /// Chain id the message was encrypted under.
    pub key_id: u32,
    /// Index of this message on the chain.
    pub message_number: u32,
    /// Base64 NIP-44 v2 ciphertext.
    pub ciphertext: String,
}

/// Result of [`SenderKeyState::plan_decrypt`] — apply to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderKeyDecryptPlan {
    /// The state after this decryption.
    pub next_state: SenderKeyState,
    /// Recovered plaintext.
    pub plaintext: Vec<u8>,
}

impl SenderKeyState {
    /// Create a fresh chain from a 32-byte `chain_key`, starting at
    /// `iteration`.
    #[must_use]
    pub fn new(key_id: u32, chain_key: &[u8; 32], iteration: u32) -> Self {
        Self {
            key_id,
            chain_key: chain_key.to_hex(),
            iteration,
            skipped_message_keys: BTreeMap::new(),
        }
    }

    /// This chain's id.
    #[must_use]
    pub const fn key_id(&self) -> u32 {
        self.key_id
    }

    /// The next message index this chain will produce/expect.
    #[must_use]
    pub const fn iteration(&self) -> u32 {
        self.iteration
    }

    /// The current chain key as 64-char hex (used to re-derive a
    /// [`crate::nip_104::SenderKeyDistribution`] for late joiners).
    #[must_use]
    pub fn chain_key_hex(&self) -> String {
        self.chain_key.clone()
    }

    /// Number of skipped message keys currently stored.
    #[must_use]
    pub fn skipped_len(&self) -> usize {
        self.skipped_message_keys.len()
    }

    /// Plan encryption of `plaintext` as the next message on the chain. Pure:
    /// the returned plan carries the advanced `next_state`; `self` is
    /// unchanged until [`apply_encrypt`](Self::apply_encrypt).
    ///
    /// # Errors
    /// [`Nip104Error`] on KDF/cipher failure or iteration overflow.
    pub fn plan_encrypt<K: NostrKeypair>(&self, plaintext: &[u8]) -> Result<SenderKeyEncryptPlan> {
        let mut next_state = self.clone();
        let message_number = next_state.iteration;
        let (next_chain_key, message_key) =
            Self::derive_message_key::<K>(&K::decode_hex_32(&next_state.chain_key)?);
        next_state.chain_key = next_chain_key.to_hex();
        next_state.iteration = next_state
            .iteration
            .checked_add(1)
            .ok_or(Nip104Error::SessionNotReady)?;

        let ciphertext = K::encrypt_with_message_key(&message_key, plaintext)?;
        Ok(SenderKeyEncryptPlan {
            next_state,
            key_id: self.key_id,
            message_number,
            ciphertext,
        })
    }

    /// Commit an [`SenderKeyEncryptPlan`].
    pub fn apply_encrypt(&mut self, plan: SenderKeyEncryptPlan) {
        *self = plan.next_state;
    }

    /// Encrypt `plaintext`, returning `(message_number, base64 ciphertext)` and
    /// advancing the chain. Convenience over plan/apply.
    ///
    /// # Errors
    /// As [`plan_encrypt`](Self::plan_encrypt).
    pub fn encrypt<K: NostrKeypair>(&mut self, plaintext: &[u8]) -> Result<(u32, String)> {
        let plan = self.plan_encrypt::<K>(plaintext)?;
        let out = (plan.message_number, plan.ciphertext.clone());
        self.apply_encrypt(plan);
        Ok(out)
    }

    /// Plan decryption of a message at `message_number` carrying `key_id`.
    /// Handles in-order, future (skip-ahead) and past (skipped-key) messages.
    /// Pure: `self` is unchanged until [`apply_decrypt`](Self::apply_decrypt).
    ///
    /// # Errors
    /// [`Nip104Error`] on key-id mismatch, too many skipped messages, a
    /// duplicate/missing past message, or cipher failure.
    pub fn plan_decrypt<K: NostrKeypair>(
        &self,
        key_id: u32,
        message_number: u32,
        ciphertext_b64: &str,
    ) -> Result<SenderKeyDecryptPlan> {
        if key_id != self.key_id {
            return Err(Nip104Error::InvalidHeader);
        }
        let mut next_state = self.clone();
        let plaintext = next_state.decrypt_in_place::<K>(message_number, ciphertext_b64)?;
        Ok(SenderKeyDecryptPlan {
            next_state,
            plaintext,
        })
    }

    /// Commit a [`SenderKeyDecryptPlan`].
    pub fn apply_decrypt(&mut self, plan: SenderKeyDecryptPlan) -> Vec<u8> {
        *self = plan.next_state;
        plan.plaintext
    }

    /// Decrypt a message at `message_number`, advancing the chain. Convenience
    /// over plan/apply.
    ///
    /// # Errors
    /// As [`plan_decrypt`](Self::plan_decrypt).
    pub fn decrypt<K: NostrKeypair>(
        &mut self,
        message_number: u32,
        ciphertext_b64: &str,
    ) -> Result<Vec<u8>> {
        let plan = self.plan_decrypt::<K>(self.key_id, message_number, ciphertext_b64)?;
        Ok(self.apply_decrypt(plan))
    }

    /// The in-place ratchet: pull a past skipped key, or step forward (storing
    /// skipped keys) until `message_number`, then decrypt.
    fn decrypt_in_place<K: NostrKeypair>(
        &mut self,
        message_number: u32,
        ciphertext_b64: &str,
    ) -> Result<Vec<u8>> {
        // Past message: must have a stored skipped key.
        if message_number < self.iteration {
            let key = self
                .skipped_message_keys
                .remove(&message_number)
                .ok_or(Nip104Error::InvalidHeader)?;
            return K::decrypt_with_message_key(&K::decode_hex_32(&key)?, ciphertext_b64);
        }

        // Future message: bounded skip.
        let delta = message_number - self.iteration;
        if delta > SENDER_KEY_MAX_SKIP {
            return Err(Nip104Error::TooManySkippedMessages);
        }

        // Step forward, banking skipped keys.
        while self.iteration < message_number {
            let (next_chain_key, message_key) =
                Self::derive_message_key::<K>(&K::decode_hex_32(&self.chain_key)?);
            self.chain_key = next_chain_key.to_hex();
            self.skipped_message_keys
                .insert(self.iteration, message_key.to_hex());
            self.iteration = self
                .iteration
                .checked_add(1)
                .ok_or(Nip104Error::SessionNotReady)?;
        }

        // Now at message_number: derive its key and advance once more.
        let (next_chain_key, message_key) =
            Self::derive_message_key::<K>(&K::decode_hex_32(&self.chain_key)?);
        self.chain_key = next_chain_key.to_hex();
        self.iteration = self
            .iteration
            .checked_add(1)
            .ok_or(Nip104Error::SessionNotReady)?;
        Self::prune_skipped(&mut self.skipped_message_keys);

        K::decrypt_with_message_key(&message_key, ciphertext_b64)
    }

    /// `kdf(chain_key, "ndr-sender-key-v1", 2)` → `(next_chain_key, message_key)`.
    fn derive_message_key<K: NostrKeypair>(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
        let outs = K::kdf(chain_key, SENDER_KEY_KDF_SALT, 2);
        (outs[0], outs[1])
    }

    /// Bound the stored skipped-key map, dropping the oldest indices first.
    fn prune_skipped(map: &mut BTreeMap<u32, String>) {
        while map.len() > SENDER_KEY_MAX_STORED_SKIPPED_KEYS {
            let Some(first) = map.keys().next().copied() else {
                break;
            };
            map.remove(&first);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type K = crate::tests::NipTester;

    #[test]
    fn roundtrip_single_message() {
        let ck = [7_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);

        let (n, ct) = sender.encrypt::<K>(b"hello").unwrap();
        assert_eq!(n, 0);
        assert_eq!(receiver.decrypt::<K>(n, &ct).unwrap(), b"hello");
        assert_eq!(sender.iteration(), receiver.iteration());
    }

    #[test]
    fn decrypt_out_of_order() {
        let ck = [9_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);

        let (n0, c0) = sender.encrypt::<K>(b"m0").unwrap();
        let (n1, c1) = sender.encrypt::<K>(b"m1").unwrap();

        assert_eq!(receiver.decrypt::<K>(n1, &c1).unwrap(), b"m1");
        assert_eq!(receiver.decrypt::<K>(n0, &c0).unwrap(), b"m0");
    }

    #[test]
    fn rejects_duplicate_message() {
        let ck = [11_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);
        let (n, c) = sender.encrypt::<K>(b"once").unwrap();

        assert_eq!(receiver.decrypt::<K>(n, &c).unwrap(), b"once");
        assert!(receiver.decrypt::<K>(n, &c).is_err());
    }

    #[test]
    fn wrong_key_id_does_not_mutate_receiver() {
        let ck = [13_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let receiver = SenderKeyState::new(1, &ck, 0);
        let (n, c) = sender.encrypt::<K>(b"x").unwrap();
        let before = receiver.clone();

        assert!(receiver.plan_decrypt::<K>(2, n, &c).is_err());
        assert_eq!(receiver, before);
    }

    #[test]
    fn plan_encrypt_is_pure_until_apply() {
        let ck = [4_u8; 32];
        let mut sender = SenderKeyState::new(7, &ck, 0);
        let before = sender.clone();

        let plan = sender.plan_encrypt::<K>(b"deferred").unwrap();
        assert_eq!(sender, before);
        assert_eq!(plan.key_id, 7);
        assert_eq!(plan.message_number, 0);

        sender.apply_encrypt(plan);
        assert_eq!(sender.iteration(), 1);
        assert_ne!(sender, before);
    }

    #[test]
    fn skip_ahead_then_backfill() {
        let ck = [19_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);
        let (n0, c0) = sender.encrypt::<K>(b"m0").unwrap();
        let (n1, c1) = sender.encrypt::<K>(b"m1").unwrap();
        let (n2, c2) = sender.encrypt::<K>(b"m2").unwrap();

        // Receive the latest first → banks two skipped keys.
        assert_eq!(receiver.decrypt::<K>(n2, &c2).unwrap(), b"m2");
        assert_eq!(receiver.skipped_len(), 2);
        // Backfill the earlier two from stored keys.
        assert_eq!(receiver.decrypt::<K>(n0, &c0).unwrap(), b"m0");
        assert_eq!(receiver.decrypt::<K>(n1, &c1).unwrap(), b"m1");
        assert_eq!(receiver.skipped_len(), 0);
    }

    #[test]
    fn rejects_too_many_skipped() {
        let ck = [3_u8; 32];
        let receiver = SenderKeyState::new(1, &ck, 0);
        let err = receiver.plan_decrypt::<K>(1, SENDER_KEY_MAX_SKIP + 1, "AA");
        assert!(matches!(err, Err(Nip104Error::TooManySkippedMessages)));
    }

    // ── Adversarial / scale ──────────────────────────────────────

    /// Skipping ahead past the stored-key cap evicts the *oldest* skipped keys
    /// (bounded memory): the banked map never exceeds the cap, the recent tail
    /// still backfills, but the evicted head is gone forever.
    #[test]
    #[allow(clippy::cast_possible_truncation)] // cap is 2000, fits u32
    fn skip_ahead_prunes_oldest_skipped_keys() {
        // Produce a long run; remember the first and a recent ciphertext.
        const AHEAD: u32 = SENDER_KEY_MAX_STORED_SKIPPED_KEYS as u32 + 500;
        let ck = [21_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);
        let mut first = None;
        let mut recent = None;
        for i in 0..=AHEAD {
            let (n, c) = sender.encrypt::<K>(format!("m{i}").as_bytes()).unwrap();
            if i == 0 {
                first = Some((n, c.clone()));
            }
            if i == AHEAD - 1 {
                recent = Some((n, c.clone()));
            }
            if i == AHEAD {
                // Receive the very latest first → banks AHEAD skipped keys,
                // then prunes down to the cap.
                assert_eq!(receiver.decrypt::<K>(n, &c).unwrap(), format!("m{i}").as_bytes());
            }
        }
        assert_eq!(
            receiver.skipped_len(),
            SENDER_KEY_MAX_STORED_SKIPPED_KEYS,
            "stored skipped keys must be capped"
        );

        // A recent message (index AHEAD-1) still backfills from a stored key…
        let (rn, rc) = recent.unwrap();
        assert_eq!(receiver.decrypt::<K>(rn, &rc).unwrap(), format!("m{}", AHEAD - 1).as_bytes());

        // …but the long-evicted message 0 is unrecoverable (its key was pruned).
        let (fn0, fc0) = first.unwrap();
        assert!(matches!(
            receiver.plan_decrypt::<K>(1, fn0, &fc0),
            Err(Nip104Error::InvalidHeader)
        ));
    }

    /// A future message exactly `SENDER_KEY_MAX_SKIP` ahead is accepted; one
    /// more is refused. Guards the off-by-one at the skip ceiling.
    #[test]
    fn skip_ceiling_is_inclusive() {
        let ck = [23_u8; 32];
        // Receiver at iteration 0; a message at exactly MAX_SKIP has delta ==
        // MAX_SKIP, which is allowed. We don't have its ciphertext, but the
        // bound is checked before any key derivation, so a cipher failure (not
        // TooManySkippedMessages) proves we passed the gate.
        let receiver = SenderKeyState::new(1, &ck, 0);
        let at_ceiling = receiver.plan_decrypt::<K>(1, SENDER_KEY_MAX_SKIP, "not-base64!!");
        assert!(
            !matches!(at_ceiling, Err(Nip104Error::TooManySkippedMessages)),
            "delta == MAX_SKIP must pass the skip gate"
        );
        let over = receiver.plan_decrypt::<K>(1, SENDER_KEY_MAX_SKIP + 1, "not-base64!!");
        assert!(matches!(over, Err(Nip104Error::TooManySkippedMessages)));
    }

    /// Encrypting on a chain already at `u32::MAX` overflows the iteration
    /// counter — the `checked_add` must surface an error, never wrap to 0
    /// (which would silently reuse a message key).
    #[test]
    fn iteration_overflow_on_encrypt_is_rejected() {
        let ck = [25_u8; 32];
        let sender = SenderKeyState::new(1, &ck, u32::MAX);
        assert!(matches!(
            sender.plan_encrypt::<K>(b"boom"),
            Err(Nip104Error::SessionNotReady)
        ));
    }

    /// The chain inherits NIP-44 v2's plaintext bounds: a single byte and the
    /// 65 535-byte maximum both round-trip, but empty and over-max are refused
    /// (the cipher rejects them before the chain advances).
    #[test]
    fn payload_size_bounds_match_nip44() {
        let ck = [27_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);

        // One byte: the smallest legal payload.
        let (n0, c0) = sender.encrypt::<K>(b"x").unwrap();
        assert_eq!(receiver.decrypt::<K>(n0, &c0).unwrap(), b"x");

        // Exactly the NIP-44 v2 maximum (65 535 bytes).
        let max = vec![0x5A_u8; 65_535];
        let (n1, c1) = sender.encrypt::<K>(&max).unwrap();
        assert_eq!(receiver.decrypt::<K>(n1, &c1).unwrap(), max);

        // Empty plaintext and one-over-max are both rejected by the cipher,
        // and a rejected encrypt must not advance the (pure) sender plan.
        assert!(sender.plan_encrypt::<K>(b"").is_err());
        let over = vec![0_u8; 65_536];
        assert!(sender.plan_encrypt::<K>(&over).is_err());
        // Sender still at iteration 2 after the two good messages.
        assert_eq!(sender.iteration(), 2);
    }

    /// A tampered ciphertext (valid base64, broken MAC) fails to decrypt and —
    /// crucially — leaves the receiver chain untouched, so the legitimate
    /// message at that index still decrypts afterwards.
    #[test]
    fn tampered_ciphertext_fails_without_advancing() {
        use base64::engine::{general_purpose, Engine as _};
        let ck = [29_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let receiver = SenderKeyState::new(1, &ck, 0);
        let (n, c) = sender.encrypt::<K>(b"authentic").unwrap();

        // Flip a byte in the middle of the base64-decoded payload.
        let mut raw = general_purpose::STANDARD.decode(&c).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xFF;
        let tampered = general_purpose::STANDARD.encode(&raw);

        let before = receiver.clone();
        assert!(receiver.plan_decrypt::<K>(1, n, &tampered).is_err());
        assert_eq!(receiver, before, "failed decrypt must not mutate state");

        // The genuine ciphertext at the same index still works.
        let mut receiver = receiver;
        assert_eq!(receiver.decrypt::<K>(n, &c).unwrap(), b"authentic");
    }

    /// A sustained chain of many in-order messages keeps both ends in lockstep
    /// with zero skipped-key residue.
    #[test]
    fn sustained_in_order_volume() {
        const N: u32 = 5_000;
        let ck = [31_u8; 32];
        let mut sender = SenderKeyState::new(1, &ck, 0);
        let mut receiver = SenderKeyState::new(1, &ck, 0);
        for i in 0..N {
            let (n, c) = sender.encrypt::<K>(format!("msg-{i}").as_bytes()).unwrap();
            assert_eq!(n, i);
            assert_eq!(receiver.decrypt::<K>(n, &c).unwrap(), format!("msg-{i}").as_bytes());
        }
        assert_eq!(sender.iteration(), N);
        assert_eq!(receiver.iteration(), N);
        assert_eq!(receiver.skipped_len(), 0);
    }
}
