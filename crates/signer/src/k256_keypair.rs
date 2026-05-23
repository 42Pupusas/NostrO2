//! `NostrSigner` / `NostrKeypair` / `KeypairExt` impls for the `k256`
//! pure-Rust Schnorr backend.
//!
//! `K256Keypair` is a thin newtype around `k256::schnorr::SigningKey`. The
//! orphan rule forbids implementing the foreign signer/NIP traits directly
//! on the foreign `SigningKey`, so we wrap it. Users who need the raw type
//! reach it via `kp.0` or `&*kp` (Deref).

use std::ops::Deref;

use nostro2_traits::{NostrKeypair, NostrSigner, SignerError};

use crate::errors::NostrKeypairError;
use crate::ext::KeypairExt;

/// Nostr keypair backed by the `k256` pure-Rust Schnorr implementation.
#[derive(Clone)]
pub struct K256Keypair(pub k256::schnorr::SigningKey);

impl std::fmt::Debug for K256Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("K256Keypair").finish_non_exhaustive()
    }
}

impl Deref for K256Keypair {
    type Target = k256::schnorr::SigningKey;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl NostrSigner for K256Keypair {
    /// Signs with fresh OS-randomness as BIP-340 §3.2 aux input. The aux
    /// rand is *not* the nonce — the nonce is still a deterministic
    /// derivation of `(secret, aux ⊕ tagged_hash(secret), pubkey, msg)` —
    /// but mixing in fresh entropy each time defends against fault and
    /// side-channel attacks that recover the secret from glitched/peeked
    /// signatures. Mirrors the `secp256k1` backend's
    /// `sign_schnorr_with_aux_rand` so the two are interchangeable on
    /// every dimension *except* signature byte-equality (intentional).
    fn sign_prehash(&self, id: &[u8; 32]) -> Result<[u8; 64], SignerError> {
        let mut aux_rand = [0_u8; 32];
        getrandom::fill(&mut aux_rand)
            .map_err(|e| SignerError::Backend(format!("getrandom: {e}")))?;
        let sig = self
            .0
            .sign_raw(id, &aux_rand)
            .map_err(|_| SignerError::InvalidSignature)?;
        Ok(sig.to_bytes())
    }

    fn pubkey_bytes(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes().into()
    }
}

impl NostrKeypair for K256Keypair {
    fn secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    fn generate() -> Self {
        // `from_bytes` rejects the scalar zero and values ≥ curve order. Both
        // are negligible for a CSPRNG (probability ~2^-128 per attempt), but
        // we cap the loop instead of spinning forever — if `getrandom` ever
        // returned the same garbage twice in a row (broken kernel RNG), an
        // unbounded loop would hang. Three tries is far past any realistic
        // failure; if it really does fail thrice the system is too broken to
        // be silently retrying anyway.
        let mut secret = [0_u8; 32];
        for _ in 0..3 {
            getrandom::fill(&mut secret).expect("getrandom failed");
            let field_bytes = k256::FieldBytes::from(secret);
            if let Ok(sk) = k256::schnorr::SigningKey::from_bytes(&field_bytes) {
                return Self(sk);
            }
        }
        panic!("k256::SigningKey::from_bytes rejected three CSPRNG draws — RNG is broken");
    }

    fn ecdh_x(&self, peer_xonly: &[u8; 32]) -> Result<[u8; 32], SignerError> {
        // Nostr convention: x-only pubkey → reconstruct compressed SEC1 point
        // with even-parity prefix (0x02). Lossy for y-parity but matches NIP-04.
        let mut compressed = [0_u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(peer_xonly);
        let public_key = k256::PublicKey::from_sec1_bytes(&compressed)
            .map_err(|_| SignerError::InvalidPublicKey)?;
        // Round-trip through SecretKey because `schnorr::SigningKey` doesn't
        // expose its inner `NonZeroScalar` publicly. The bytes came from our
        // own valid signing key, so this parse cannot fail in practice — if
        // it does, the k256 API has changed under us, not anything the user
        // can fix at runtime.
        let secret_key = k256::SecretKey::from_slice(&self.0.to_bytes())
            .unwrap_or_else(|_| unreachable!("our own signing key bytes are always valid"));
        let shared =
            k256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), public_key.as_affine());
        let mut point = [0_u8; 32];
        point.copy_from_slice(shared.raw_secret_bytes().as_slice());
        Ok(point)
    }
}

impl KeypairExt for K256Keypair {
    fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, NostrKeypairError> {
        let field_bytes = k256::FieldBytes::from(*bytes);
        k256::schnorr::SigningKey::from_bytes(&field_bytes)
            .map(Self)
            .map_err(|_| NostrKeypairError::InvalidKey)
    }
}

impl std::str::FromStr for K256Keypair {
    type Err = NostrKeypairError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_any(s)
    }
}

// Nip44 / Nip17 / Nip46 / Nip59 are blanket-implemented in
// `nostro2-nips` for every `NostrKeypair`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_sign() {
        let kp = K256Keypair::generate();
        let mut note = nostro2::NostrNote::text_note("Hello from k256!");
        note.sign_with(&kp).unwrap();
        assert!(note.verify());
    }

    /// BIP-340 §3.2: signing must mix fresh aux randomness on every call
    /// so glitched/peeked signatures can't be combined to recover the key.
    /// Two signatures over the same prehash with the same key MUST differ;
    /// both still verify. The `secp256k1` backend has the same test.
    #[test]
    fn signs_with_fresh_aux_rand() {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};

        let kp = K256Keypair::generate();
        let prehash = [0x42_u8; 32];
        let a = kp.sign_prehash(&prehash).unwrap();
        let b = kp.sign_prehash(&prehash).unwrap();
        assert_ne!(a, b, "k256 sign_prehash must inject fresh aux rand");
        let pk_bytes = kp.pubkey_bytes();
        let vk = VerifyingKey::from_bytes((&pk_bytes).into()).unwrap();
        for sig in [a, b] {
            let s = Signature::try_from(sig.as_slice()).unwrap();
            assert!(vk.verify_prehash(&prehash, &s).is_ok());
        }
    }

    #[test]
    fn test_from_hex_roundtrip() {
        let kp = K256Keypair::generate();
        let hex_sk = kp.secret_key();
        let kp2 = K256Keypair::from_hex(&hex_sk).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());
    }

    #[test]
    fn test_shared_secret_consistency() {
        let alice = K256Keypair::generate();
        let bob = K256Keypair::generate();
        let a = alice.shared_point(&bob.public_key()).unwrap();
        let b = bob.shared_point(&alice.public_key()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_nip44_encrypt_decrypt() {
        use nostro2_nips::Nip44 as _;
        let alice = K256Keypair::generate();
        let bob = K256Keypair::generate();
        let mut note = nostro2::NostrNote {
            content: "Secret k256 message".to_string(),
            kind: 4,
            ..Default::default()
        };
        let bob_pk = bob.public_key();
        let alice_pk = alice.public_key();
        alice.nip44_encrypt_note(&mut note, &bob_pk).unwrap();
        assert_ne!(note.content, "Secret k256 message");
        let decrypted = bob.nip44_decrypt_note(&note, &alice_pk).unwrap();
        assert_eq!(decrypted, "Secret k256 message");
    }

    #[test]
    fn test_bech32_roundtrip() {
        let kp = K256Keypair::generate();
        let nsec = kp.nsec().unwrap();
        let npub = kp.npub().unwrap();
        let restored = K256Keypair::from_nsec(&nsec).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
        assert_eq!(restored.npub().unwrap(), npub);
    }

    #[test]
    fn test_mnemonic_roundtrip() {
        let kp = K256Keypair::generate();
        let mn = kp.mnemonic(xinachtli::Language::English).unwrap();
        let restored = K256Keypair::from_mnemonic(&mn, xinachtli::Language::English).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
    }

    #[test]
    fn test_from_any_tries_all_formats() {
        let hex = "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8";
        let kp1 = K256Keypair::from_any(hex).unwrap();
        assert_eq!(
            kp1.public_key(),
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );
        let nsec = "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53";
        let kp2 = K256Keypair::from_any(nsec).unwrap();
        assert_eq!(kp2.public_key(), kp1.public_key());

        let english = kp1.mnemonic(xinachtli::Language::English).unwrap();
        let kp3 = K256Keypair::from_any(&english).unwrap();
        assert_eq!(kp3.public_key(), kp1.public_key());

        let spanish = kp1.mnemonic(xinachtli::Language::Spanish).unwrap();
        let kp4 = K256Keypair::from_any(&spanish).unwrap();
        assert_eq!(kp4.public_key(), kp1.public_key());

        assert!(K256Keypair::from_any("not-a-valid-key-in-any-format").is_err());
    }

    #[test]
    fn test_deref_to_raw() {
        // Should be usable as &k256::schnorr::SigningKey.
        let kp = K256Keypair::generate();
        let raw: &k256::schnorr::SigningKey = &kp;
        assert_eq!(raw.verifying_key().to_bytes().as_slice().len(), 32);
    }
}
