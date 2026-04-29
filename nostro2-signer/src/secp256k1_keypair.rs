//! `NostrSigner` / `NostrKeypair` / `KeypairExt` impls for the `secp256k1`
//! C-library Schnorr backend.
//!
//! `Secp256k1Keypair` is a thin newtype around `secp256k1::Keypair`. The
//! orphan rule forbids implementing the foreign signer/NIP traits directly
//! on the foreign `Keypair`, so we wrap it. Users who need the raw type
//! reach it via `kp.0` or `&*kp` (Deref).

use std::ops::Deref;

use nostro2_traits::{NostrKeypair, NostrSigner, SignerError};
use secp256k1::{Message, SECP256K1};

use crate::errors::NostrKeypairError;
use crate::ext::KeypairExt;

/// Nostr keypair backed by the `secp256k1` C-library Schnorr implementation.
#[derive(Clone)]
pub struct Secp256k1Keypair(pub secp256k1::Keypair);

impl std::fmt::Debug for Secp256k1Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Secp256k1Keypair").finish_non_exhaustive()
    }
}

impl Deref for Secp256k1Keypair {
    type Target = secp256k1::Keypair;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for Secp256k1Keypair {
    fn default() -> Self {
        Self::generate()
    }
}

impl NostrSigner for Secp256k1Keypair {
    fn sign_prehash(&self, id: &[u8; 32]) -> Result<[u8; 64], SignerError> {
        let msg = Message::from_digest(*id);
        let sig = SECP256K1.sign_schnorr_no_aux_rand(&msg, &self.0);
        Ok(*sig.as_ref())
    }

    fn pubkey_bytes(&self) -> [u8; 32] {
        self.0.x_only_public_key().0.serialize()
    }
}

impl NostrKeypair for Secp256k1Keypair {
    fn secret_bytes(&self) -> [u8; 32] {
        self.0.secret_key().secret_bytes()
    }

    fn generate() -> Self {
        // See `K256Keypair::generate` for the bounded-retry rationale.
        let mut secret = [0_u8; 32];
        for _ in 0..3 {
            getrandom::fill(&mut secret).expect("getrandom failed");
            if let Ok(sk) = secp256k1::SecretKey::from_slice(&secret) {
                return Self(secp256k1::Keypair::from_secret_key(SECP256K1, &sk));
            }
        }
        panic!("secp256k1::SecretKey::from_slice rejected three CSPRNG draws — RNG is broken");
    }

    fn ecdh_x(&self, peer_xonly: &[u8; 32]) -> Result<[u8; 32], SignerError> {
        // Nostr convention: x-only pubkey → reconstruct compressed SEC1 point
        // with even-parity prefix (0x02). Lossy for y-parity but matches NIP-04.
        let mut compressed = [0_u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(peer_xonly);
        let pk = secp256k1::PublicKey::from_slice(&compressed)
            .map_err(|_| SignerError::InvalidPublicKey)?;
        let shared = secp256k1::ecdh::shared_secret_point(&pk, &self.0.secret_key());
        // shared_secret_point returns a 64-byte serialized point; first 32 bytes are x.
        let mut point = [0_u8; 32];
        point.copy_from_slice(&shared[..32]);
        Ok(point)
    }
}

impl KeypairExt for Secp256k1Keypair {
    fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, NostrKeypairError> {
        let sk = secp256k1::SecretKey::from_slice(bytes)?;
        Ok(Self(secp256k1::Keypair::from_secret_key(SECP256K1, &sk)))
    }
}

impl std::str::FromStr for Secp256k1Keypair {
    type Err = NostrKeypairError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_any(s)
    }
}

// Nip04 / Nip44 / Nip17 / Nip46 / Nip59 are blanket-implemented in
// `nostro2-nips` for every `NostrKeypair`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_sign() {
        let kp = Secp256k1Keypair::generate();
        let mut note = nostro2::NostrNote::text_note("Hello from secp256k1!");
        note.sign_with(&kp).unwrap();
        assert!(note.verify());
    }

    #[test]
    fn test_from_hex_roundtrip() {
        let kp = Secp256k1Keypair::generate();
        let hex_sk = kp.secret_key();
        let kp2 = Secp256k1Keypair::from_hex(&hex_sk).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());
    }

    #[test]
    fn test_shared_secret_consistency() {
        let alice = Secp256k1Keypair::generate();
        let bob = Secp256k1Keypair::generate();
        let a = alice.shared_point(&bob.public_key()).unwrap();
        let b = bob.shared_point(&alice.public_key()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_nip44_encrypt_decrypt() {
        use nostro2_nips::Nip44 as _;
        let alice = Secp256k1Keypair::generate();
        let bob = Secp256k1Keypair::generate();
        let mut note = nostro2::NostrNote {
            content: "Secret secp256k1 message".to_string(),
            kind: 4,
            ..Default::default()
        };
        let bob_pk = bob.public_key();
        let alice_pk = alice.public_key();
        alice.nip44_encrypt_note(&mut note, &bob_pk).unwrap();
        assert_ne!(note.content, "Secret secp256k1 message");
        let decrypted = bob.nip44_decrypt_note(&note, &alice_pk).unwrap();
        assert_eq!(decrypted, "Secret secp256k1 message");
    }

    #[test]
    fn test_bech32_roundtrip() {
        let kp = Secp256k1Keypair::generate();
        let nsec = kp.nsec().unwrap();
        let npub = kp.npub().unwrap();
        let restored = Secp256k1Keypair::from_nsec(&nsec).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
        assert_eq!(restored.npub().unwrap(), npub);
    }

    #[test]
    fn test_mnemonic_roundtrip() {
        let kp = Secp256k1Keypair::generate();
        let mn = kp.mnemonic(bip39::Language::English).unwrap();
        let restored = Secp256k1Keypair::from_mnemonic(&mn, bip39::Language::English).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
    }

    #[test]
    fn test_known_vector() {
        let hex = "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8";
        let kp = Secp256k1Keypair::from_any(hex).unwrap();
        assert_eq!(
            kp.public_key(),
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );
    }

    #[test]
    fn test_deref_to_raw() {
        let kp = Secp256k1Keypair::generate();
        let raw: &secp256k1::Keypair = &kp;
        assert_eq!(raw.x_only_public_key().0.serialize().len(), 32);
    }
}
