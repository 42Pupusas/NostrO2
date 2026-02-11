use k256::schnorr::signature::hazmat::PrehashSigner;
use nostro2::NostrSigner;
use nostro2_nips::{Nip04, Nip44};

use crate::errors::NostrKeypairError;

/// Nostr keypair using the k256 (pure Rust) backend
///
/// Drop-in alternative to [`crate::NostrKeypair`] for benchmarking
/// k256 against the C-based secp256k1 library.
pub struct K256Keypair {
    signing_key: k256::schnorr::SigningKey,
    extractable: bool,
}

impl std::fmt::Debug for K256Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("K256Keypair")
            .field("extractable", &self.extractable)
            .finish_non_exhaustive()
    }
}

impl K256Keypair {
    #[must_use]
    pub fn new() -> Self {
        Self::generate(false)
    }

    #[must_use]
    pub fn new_extractable() -> Self {
        Self::generate(true)
    }

    /// Create a keypair from a hexadecimal private key string
    ///
    /// # Errors
    ///
    /// Returns an error if the hex string is invalid or the key is malformed
    pub fn from_hex(hex: &str, extractable: bool) -> Result<Self, NostrKeypairError> {
        let bytes = hex::decode(hex)?;
        let field_bytes = k256::FieldBytes::from_slice(&bytes);
        let signing_key =
            k256::schnorr::SigningKey::from_bytes(field_bytes).map_err(|_| NostrKeypairError::InvalidKey)?;
        Ok(Self {
            signing_key,
            extractable,
        })
    }

    #[must_use]
    #[inline]
    pub fn pubkey(&self) -> String {
        self.public_key()
    }

    #[must_use]
    #[inline]
    pub fn pubkey_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes().into()
    }

    /// Sign a Nostr note using the k256 backend
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or signing fails
    pub fn sign_note(&self, note: &mut nostro2::NostrNote) -> Result<(), NostrKeypairError> {
        self.sign_nostr_note(note)?;
        Ok(())
    }

    /// Generate a shared secret point for ECDH key exchange
    ///
    /// # Errors
    ///
    /// Returns an error if the public key string is invalid or ECDH fails
    pub fn shared_point(&self, public_key_string: &str) -> Result<[u8; 32], NostrKeypairError> {
        let hex_pk = hex::decode(public_key_string)?;
        // Build compressed SEC1 point: 0x02 prefix (even parity) + 32-byte x-coordinate
        let mut compressed = [0_u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(&hex_pk);
        let public_key = k256::PublicKey::from_sec1_bytes(&compressed)?;
        let secret_key = k256::SecretKey::from_slice(&self.signing_key.to_bytes())
            .map_err(|_| NostrKeypairError::SharedSecretError)?;
        let shared =
            k256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), public_key.as_affine());
        let mut point = [0_u8; 32];
        point.copy_from_slice(shared.raw_secret_bytes().as_slice());
        Ok(point)
    }
}

impl std::str::FromStr for K256Keypair {
    type Err = NostrKeypairError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() == 64 {
            return Self::from_hex(value, false);
        }
        Err(NostrKeypairError::InvalidKey)
    }
}

impl Default for K256Keypair {
    fn default() -> Self {
        Self::new()
    }
}

impl Nip04 for K256Keypair {
    fn shared_secret(
        &self,
        public_key_string: &str,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, nostro2_nips::Nip04Error> {
        Ok(self
            .shared_point(public_key_string)
            .map_err(|_| nostro2_nips::Nip04Error::SharedSecretError)?
            .into())
    }
}

impl Nip44 for K256Keypair {
    fn shared_secret(
        &self,
        public_key_string: &str,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, nostro2_nips::Nip44Error> {
        Ok(self
            .shared_point(public_key_string)
            .map_err(|_| nostro2_nips::Nip44Error::SharedSecretError)?
            .into())
    }
}

impl nostro2_nips::Nip17 for K256Keypair {}
impl nostro2_nips::Nip46 for K256Keypair {}
impl nostro2_nips::Nip59 for K256Keypair {}
impl nostro2_nips::Nip82 for K256Keypair {}

impl NostrSigner for K256Keypair {
    fn sign_nostr_note(
        &self,
        note: &mut nostro2::NostrNote,
    ) -> Result<(), nostro2::errors::NostrErrors> {
        note.pubkey = self.public_key();
        note.serialize_id()?;
        let id = note.id_bytes().unwrap_or([0_u8; 32]);
        let sig = self
            .signing_key
            .sign_prehash(&id)
            .map_err(|_| nostro2::errors::NostrErrors::InvalidSignature)?;
        note.sig.replace(hex::encode(sig.to_bytes()));
        Ok(())
    }

    fn generate(extractable: bool) -> Self {
        let signing_key = k256::schnorr::SigningKey::random(&mut rand_core::OsRng);
        Self {
            signing_key,
            extractable,
        }
    }

    #[inline]
    fn public_key(&self) -> String {
        hex::encode(self.signing_key.verifying_key().to_bytes())
    }

    #[inline]
    fn secret_key(&self) -> String {
        if self.extractable {
            hex::encode(self.signing_key.to_bytes())
        } else {
            hex::encode([0_u8; 32])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostro2::NostrSigner;

    #[test]
    fn test_generate_and_sign() {
        let kp = K256Keypair::new_extractable();
        let mut note = nostro2::NostrNote::text_note("Hello from k256!");
        kp.sign_note(&mut note).unwrap();
        // Verify with k256
        assert!(note.verify_k256());
        // Verify with secp256k1
        assert!(note.verify());
    }

    #[test]
    fn test_from_hex_roundtrip() {
        let kp = K256Keypair::new_extractable();
        let hex_sk = kp.secret_key();
        let kp2 = K256Keypair::from_hex(&hex_sk, true).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());
    }

    #[test]
    fn test_shared_secret_consistency() {
        let alice = K256Keypair::new_extractable();
        let bob = K256Keypair::new_extractable();
        let alice_secret = alice.shared_point(&bob.pubkey()).unwrap();
        let bob_secret = bob.shared_point(&alice.pubkey()).unwrap();
        assert_eq!(alice_secret, bob_secret);
    }

    #[test]
    fn test_cross_backend_shared_secret() {
        let secp_kp = crate::NostrKeypair::new_extractable();
        let sk_hex = NostrSigner::secret_key(&secp_kp);
        let k256_kp = K256Keypair::from_hex(&sk_hex, true).unwrap();
        // Same key, same pubkey
        assert_eq!(secp_kp.pubkey(), k256_kp.pubkey());

        let peer = crate::NostrKeypair::new_extractable();
        let secp_shared = secp_kp.shared_point(&peer.pubkey()).unwrap();
        let k256_shared = k256_kp.shared_point(&peer.pubkey()).unwrap();
        assert_eq!(secp_shared, k256_shared);
    }

    #[test]
    fn test_cross_backend_sign_verify() {
        // Sign with k256, verify with secp256k1
        let kp = K256Keypair::new();
        let mut note = nostro2::NostrNote::text_note("cross-backend test");
        kp.sign_note(&mut note).unwrap();
        assert!(note.verify());
        assert!(note.verify_k256());

        // Sign with secp256k1, verify with k256
        let secp_kp = crate::NostrKeypair::new();
        let mut note2 = nostro2::NostrNote::text_note("cross-backend test 2");
        secp_kp.sign_note(&mut note2).unwrap();
        assert!(note2.verify_k256());
        assert!(note2.verify());
    }

    #[test]
    fn test_extractable_protection() {
        let kp = K256Keypair::new(); // not extractable
        assert_eq!(kp.secret_key(), hex::encode([0_u8; 32]));
    }

    #[test]
    fn test_nip44_encrypt_decrypt() {
        let alice = K256Keypair::new_extractable();
        let bob = K256Keypair::new_extractable();

        let mut note = nostro2::NostrNote {
            content: "Secret k256 message".to_string(),
            kind: 4,
            ..Default::default()
        };

        let bob_pk = bob.pubkey();
        let alice_pk = alice.pubkey();

        alice.nip44_encrypt_note(&mut note, &bob_pk).unwrap();
        assert_ne!(note.content, "Secret k256 message");

        let decrypted = bob.nip44_decrypt_note(&note, &alice_pk).unwrap();
        assert_eq!(decrypted, "Secret k256 message");
    }
}
