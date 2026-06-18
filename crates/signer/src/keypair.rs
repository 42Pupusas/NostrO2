use nostro2_traits::SignerError;
use nostro2_traits::{KeypairBech32, NostrKeypair as NostrKeypairTrait, NostrSigner, SignerBech32};

use crate::errors::NostrKeypairError;

/// Nostr keypair backed by the active cryptographic backend (k256 or secp256k1).
///
/// The backend is selected via Cargo features — exactly one of `k256` (default,
/// pure Rust) or `secp256k1` (C library, faster) must be active. The public API
/// is identical regardless; only recompile with the other feature flag to A/B
/// test performance or behavior.
#[derive(Clone)]
pub struct NostrKeypair {
    #[cfg(feature = "k256")]
    inner: k256::schnorr::SigningKey,
    #[cfg(feature = "secp256k1")]
    inner: secp256k1::Keypair,
}

// ── Debug ──────────────────────────────────────────────────────────────

impl std::fmt::Debug for NostrKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NostrKeypair").finish_non_exhaustive()
    }
}

// ── NostrSigner ────────────────────────────────────────────────────────

impl NostrSigner for NostrKeypair {
    #[cfg(feature = "k256")]
    fn sign_prehash(&self, id: &[u8; 32]) -> Result<[u8; 64], SignerError> {
        let mut aux_rand = [0_u8; 32];
        getrandom::fill(&mut aux_rand)
            .map_err(|e| SignerError::Backend(format!("getrandom: {e}")))?;
        let sig = self
            .inner
            .sign_raw(id, &aux_rand)
            .map_err(|_| SignerError::InvalidSignature)?;
        Ok(sig.to_bytes())
    }

    #[cfg(feature = "secp256k1")]
    fn sign_prehash(&self, id: &[u8; 32]) -> Result<[u8; 64], SignerError> {
        let mut aux_rand = [0_u8; 32];
        getrandom::fill(&mut aux_rand)
            .map_err(|e| SignerError::Backend(format!("getrandom: {e}")))?;
        let sig = secp256k1::SECP256K1.sign_schnorr_with_aux_rand(id, &self.inner, &aux_rand);
        Ok(*sig.as_ref())
    }

    #[cfg(feature = "k256")]
    fn pubkey_bytes(&self) -> [u8; 32] {
        self.inner.verifying_key().to_bytes().into()
    }

    #[cfg(feature = "secp256k1")]
    fn pubkey_bytes(&self) -> [u8; 32] {
        self.inner.x_only_public_key().0.serialize()
    }
}

// ── NostrKeypair ───────────────────────────────────────────────────────

impl NostrKeypairTrait for NostrKeypair {
    #[cfg(feature = "k256")]
    fn secret_bytes(&self) -> [u8; 32] {
        self.inner.to_bytes().into()
    }

    #[cfg(feature = "secp256k1")]
    fn secret_bytes(&self) -> [u8; 32] {
        self.inner.secret_key().secret_bytes()
    }

    #[cfg(feature = "k256")]
    fn generate() -> Self {
        let mut secret = [0_u8; 32];
        for _ in 0..3 {
            getrandom::fill(&mut secret).expect("getrandom failed");
            let field_bytes = k256::FieldBytes::from(secret);
            if let Ok(sk) = k256::schnorr::SigningKey::from_bytes(&field_bytes) {
                return Self { inner: sk };
            }
        }
        panic!("k256::SigningKey::from_bytes rejected three CSPRNG draws — RNG is broken");
    }

    #[cfg(feature = "secp256k1")]
    fn generate() -> Self {
        let mut secret = [0_u8; 32];
        for _ in 0..3 {
            getrandom::fill(&mut secret).expect("getrandom failed");
            if let Ok(sk) = secp256k1::SecretKey::from_byte_array(secret) {
                return Self {
                    inner: secp256k1::Keypair::from_secret_key(secp256k1::SECP256K1, &sk),
                };
            }
        }
        panic!(
            "secp256k1::SecretKey::from_byte_array rejected three CSPRNG draws — RNG is broken"
        );
    }

    #[cfg(feature = "k256")]
    fn ecdh_x(&self, peer_xonly: &[u8; 32]) -> Result<[u8; 32], SignerError> {
        let mut compressed = [0_u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(peer_xonly);
        let public_key = k256::PublicKey::from_sec1_bytes(&compressed)
            .map_err(|_| SignerError::InvalidPublicKey)?;
        let secret_key = k256::SecretKey::from_slice(&self.inner.to_bytes())
            .unwrap_or_else(|_| unreachable!("our own signing key bytes are always valid"));
        let shared =
            k256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), public_key.as_affine());
        let mut point = [0_u8; 32];
        point.copy_from_slice(shared.raw_secret_bytes().as_slice());
        Ok(point)
    }

    #[cfg(feature = "secp256k1")]
    fn ecdh_x(&self, peer_xonly: &[u8; 32]) -> Result<[u8; 32], SignerError> {
        let mut compressed = [0_u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(peer_xonly);
        let pk = secp256k1::PublicKey::from_byte_array_compressed(compressed)
            .map_err(|_| SignerError::InvalidPublicKey)?;
        let shared = secp256k1::ecdh::shared_secret_point(&pk, &self.inner.secret_key());
        let mut point = [0_u8; 32];
        point.copy_from_slice(&shared[..32]);
        Ok(point)
    }
}

// ── Inherent constructors / encoders (was KeypairExt) ─────────────────

impl NostrKeypair {
    /// Build a keypair from raw 32-byte secret material.
    ///
    /// # Errors
    /// Returns an error if the bytes are not a valid scalar for the curve.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, NostrKeypairError> {
        #[cfg(feature = "k256")]
        {
            let field_bytes = k256::FieldBytes::from(*bytes);
            k256::schnorr::SigningKey::from_bytes(&field_bytes)
                .map(|sk| Self { inner: sk })
                .map_err(|_| NostrKeypairError::InvalidKey)
        }
        #[cfg(feature = "secp256k1")]
        {
            let sk = secp256k1::SecretKey::from_byte_array(*bytes)?;
            Ok(Self {
                inner: secp256k1::Keypair::from_secret_key(secp256k1::SECP256K1, &sk),
            })
        }
    }

    /// Parse from a 64-char hex secret key string.
    ///
    /// # Errors
    /// Returns an error if the string is not 64 hex chars or not a valid scalar.
    pub fn from_hex(hex: &str) -> Result<Self, NostrKeypairError> {
        let mut buf = [0_u8; 32];
        nostro2_traits::hex::FromHex::decode_hex_to_slice(hex, &mut buf)
            .map_err(|_| NostrKeypairError::InvalidKey)?;
        Self::from_secret_bytes(&buf)
    }

    /// Parse from an `nsec1…` bech32 string.
    ///
    /// # Errors
    /// Returns an error if the HRP is not `nsec` or the payload is not 32 bytes.
    pub fn from_nsec(nsec: &str) -> Result<Self, NostrKeypairError> {
        let (hrp, data) = nostro2_traits::bech32::Bech32Crypto::decode(nsec)?;
        if hrp != "nsec" {
            return Err(NostrKeypairError::InvalidKey);
        }
        let bytes: &[u8; 32] = data
            .as_slice()
            .try_into()
            .map_err(|_| NostrKeypairError::InvalidKey)?;
        Self::from_secret_bytes(bytes)
    }

    /// Parse from a BIP-39 mnemonic phrase.
    ///
    /// # Errors
    /// Returns an error if the mnemonic is invalid or the entropy is not a
    /// valid scalar.
    pub fn from_mnemonic(
        mnemonic: &str,
        language: xinachtli::Language,
    ) -> Result<Self, NostrKeypairError> {
        let mnemonic = xinachtli::Mnemonic::from_phrase(mnemonic, language)?;
        let bytes: &[u8; 32] = mnemonic
            .entropy()
            .try_into()
            .map_err(|_| NostrKeypairError::InvalidKey)?;
        Self::from_secret_bytes(bytes)
    }

    /// Try every supported encoding (nsec → hex → mnemonic in English then
    /// Spanish) and return the first that parses.
    ///
    /// # Errors
    /// Returns `InvalidKey` if no encoding matches.
    pub fn from_any(value: &str) -> Result<Self, NostrKeypairError> {
        if value.starts_with("nsec1") {
            if let Ok(kp) = Self::from_nsec(value) {
                return Ok(kp);
            }
        }
        if value.len() == 64 {
            if let Ok(kp) = Self::from_hex(value) {
                return Ok(kp);
            }
        }
        for language in [xinachtli::Language::English, xinachtli::Language::Spanish] {
            if let Ok(kp) = Self::from_mnemonic(value, language) {
                return Ok(kp);
            }
        }
        Err(NostrKeypairError::InvalidKey)
    }

    /// Render the secret key as a BIP-39 mnemonic in the given language.
    ///
    /// # Errors
    /// Returns an error if the entropy cannot be encoded.
    pub fn mnemonic(&self, language: xinachtli::Language) -> Result<String, NostrKeypairError> {
        let secret = self.secret_bytes();
        let m = xinachtli::Mnemonic::from_entropy(&secret, language)?;
        Ok(m.phrase())
    }

    /// Encode the public key as `npub1…` bech32.
    ///
    /// # Errors
    /// Returns an error if bech32 encoding fails.
    pub fn npub(&self) -> Result<String, NostrKeypairError> {
        Ok(SignerBech32::to_npub(self)?)
    }

    /// Encode the secret key as `nsec1…` bech32.
    ///
    /// # Errors
    /// Returns an error if bech32 encoding fails.
    pub fn nsec(&self) -> Result<String, NostrKeypairError> {
        Ok(KeypairBech32::to_nsec(self)?)
    }

}

// ── FromStr ────────────────────────────────────────────────────────────

impl std::str::FromStr for NostrKeypair {
    type Err = NostrKeypairError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_any(s)
    }
}

// Nip44 / Nip17 / Nip46 / Nip59 are blanket-implemented in
// `nostro2-nips` for every `NostrKeypair`.

// ── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nostro2::NostrEvent;

    #[test]
    fn test_generate_and_sign() {
        let kp = NostrKeypair::generate();
        let mut note = nostro2::NostrNoteBuilder::text_note("Hello from unified keypair!").build();
        note.sign_with(&kp).unwrap();
        assert!(note.verify());
    }

    #[test]
    fn signs_with_fresh_aux_rand() {
        let kp = NostrKeypair::generate();
        let prehash = [0x42_u8; 32];
        let a = kp.sign_prehash(&prehash).unwrap();
        let b = kp.sign_prehash(&prehash).unwrap();
        assert_ne!(a, b, "sign_prehash must inject fresh aux rand");

        #[cfg(feature = "k256")]
        {
            use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
            let pk_bytes = kp.pubkey_bytes();
            let vk = VerifyingKey::from_bytes((&pk_bytes).into()).unwrap();
            for sig in [a, b] {
                let s = Signature::try_from(sig.as_slice()).unwrap();
                assert!(vk.verify_prehash(&prehash, &s).is_ok());
            }
        }
        #[cfg(feature = "secp256k1")]
        {
            use secp256k1::{schnorr::Signature, XOnlyPublicKey};
            let pk_bytes = kp.pubkey_bytes();
            let xonly = XOnlyPublicKey::from_byte_array(pk_bytes).unwrap();
            for sig in [a, b] {
                let s = Signature::from_byte_array(sig);
                assert!(secp256k1::SECP256K1
                    .verify_schnorr(&s, &prehash, &xonly)
                    .is_ok());
            }
        }
    }

    #[test]
    fn test_from_hex_roundtrip() {
        let kp = NostrKeypair::generate();
        let hex_sk = kp.secret_key();
        let kp2 = NostrKeypair::from_hex(&hex_sk).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());
    }

    #[test]
    fn test_shared_secret_consistency() {
        let alice = NostrKeypair::generate();
        let bob = NostrKeypair::generate();
        let a = alice.shared_point(&bob.public_key()).unwrap();
        let b = bob.shared_point(&alice.public_key()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_nip44_encrypt_decrypt() {
        use nostro2_nips::Nip44 as _;
        let alice = NostrKeypair::generate();
        let bob = NostrKeypair::generate();
        let mut note = nostro2::NostrNote {
            content: "Secret message".to_string(),
            kind: 4,
            ..Default::default()
        };
        let bob_pk = bob.public_key();
        let alice_pk = alice.public_key();
        alice.nip44_encrypt_note(&mut note, &bob_pk).unwrap();
        assert_ne!(note.content, "Secret message");
        let decrypted = bob.nip44_decrypt_note(&note, &alice_pk).unwrap();
        assert_eq!(decrypted, "Secret message");
    }

    #[test]
    fn test_bech32_roundtrip() {
        let kp = NostrKeypair::generate();
        let nsec = kp.nsec().unwrap();
        let npub = kp.npub().unwrap();
        let restored = NostrKeypair::from_nsec(&nsec).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
        assert_eq!(restored.npub().unwrap(), npub);
    }

    #[test]
    fn test_mnemonic_roundtrip() {
        let kp = NostrKeypair::generate();
        let mn = kp.mnemonic(xinachtli::Language::English).unwrap();
        let restored = NostrKeypair::from_mnemonic(&mn, xinachtli::Language::English).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
    }

    #[test]
    fn test_from_any_tries_all_formats() {
        let hex = "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8";
        let kp1 = NostrKeypair::from_any(hex).unwrap();
        assert_eq!(
            kp1.public_key(),
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );
        let nsec = "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53";
        let kp2 = NostrKeypair::from_any(nsec).unwrap();
        assert_eq!(kp2.public_key(), kp1.public_key());

        let english = kp1.mnemonic(xinachtli::Language::English).unwrap();
        let kp3 = NostrKeypair::from_any(&english).unwrap();
        assert_eq!(kp3.public_key(), kp1.public_key());

        let spanish = kp1.mnemonic(xinachtli::Language::Spanish).unwrap();
        let kp4 = NostrKeypair::from_any(&spanish).unwrap();
        assert_eq!(kp4.public_key(), kp1.public_key());

        assert!(NostrKeypair::from_any("not-a-valid-key-in-any-format").is_err());
    }
}
