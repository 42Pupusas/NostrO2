use nostro2::{NostrKeypair, NostrSigner};
use nostro2_nips::{Nip04, Nip44, Nip59};
use secp256k1::{Message, SECP256K1};

use crate::errors::NostrKeypairError;

/// Nostr keypair backed by the `secp256k1` C-library Schnorr implementation.
#[derive(Clone)]
pub struct Secp256k1Keypair(secp256k1::Keypair);

impl std::fmt::Debug for Secp256k1Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Secp256k1Keypair").finish_non_exhaustive()
    }
}

impl Secp256k1Keypair {
    /// Parse from a 64-char hex secret key string.
    ///
    /// # Errors
    /// Returns an error if the string is not valid hex or not a valid scalar.
    pub fn from_hex(hex: &str) -> Result<Self, NostrKeypairError> {
        let bytes = hex::decode(hex)?;
        let sk = secp256k1::SecretKey::from_slice(&bytes)?;
        Ok(Self(secp256k1::Keypair::from_secret_key(SECP256K1, &sk)))
    }

    /// Parse from an `nsec1…` bech32 string.
    ///
    /// # Errors
    /// Returns an error if the string is not a valid nsec.
    pub fn from_nsec(nsec: &str) -> Result<Self, NostrKeypairError> {
        if !nsec.starts_with("nsec") {
            return Err(NostrKeypairError::HrpParseError);
        }
        let (hrp, data) = bech32::decode(nsec)?;
        if hrp.to_string() != "nsec" {
            return Err(NostrKeypairError::HrpParseError);
        }
        let sk = secp256k1::SecretKey::from_slice(&data)?;
        Ok(Self(secp256k1::Keypair::from_secret_key(SECP256K1, &sk)))
    }

    /// Parse from a BIP-39 mnemonic phrase.
    ///
    /// # Errors
    /// Returns an error if the mnemonic is invalid or the entropy is not a valid key.
    pub fn from_mnemonic(
        mnemonic: &str,
        language: bip39::Language,
    ) -> Result<Self, NostrKeypairError> {
        let mnemonic = bip39::Mnemonic::parse_in(language, mnemonic)?;
        let entropy = mnemonic.to_entropy();
        let sk = secp256k1::SecretKey::from_slice(&entropy)?;
        Ok(Self(secp256k1::Keypair::from_secret_key(SECP256K1, &sk)))
    }

    /// Return the mnemonic for this key in the given language.
    ///
    /// # Errors
    /// Returns an error if the entropy is not valid for BIP-39.
    pub fn mnemonic(&self, language: bip39::Language) -> Result<String, NostrKeypairError> {
        let secret_bytes = self.0.secret_key().secret_bytes();
        let mnemonic = bip39::Mnemonic::from_entropy_in(language, &secret_bytes)?;
        let mut out = String::with_capacity(256);
        for word in mnemonic.words() {
            out.push_str(word);
            out.push(' ');
        }
        out.pop();
        Ok(out)
    }

    /// Return the public key as 32 raw bytes (x-only).
    #[must_use]
    pub fn pubkey_bytes(&self) -> [u8; 32] {
        self.0.x_only_public_key().0.serialize()
    }
}

impl Default for Secp256k1Keypair {
    fn default() -> Self {
        Self::generate()
    }
}

impl std::str::FromStr for Secp256k1Keypair {
    type Err = NostrKeypairError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.starts_with("nsec") {
            if let Ok(kp) = Self::from_nsec(value) {
                return Ok(kp);
            }
        }
        if value.len() == 64 {
            if let Ok(kp) = Self::from_hex(value) {
                return Ok(kp);
            }
        }
        for language in [bip39::Language::English, bip39::Language::Spanish] {
            if let Ok(kp) = Self::from_mnemonic(value, language) {
                return Ok(kp);
            }
        }
        Err(NostrKeypairError::InvalidKey)
    }
}

impl NostrSigner for Secp256k1Keypair {
    fn sign_nostr_note(
        &self,
        note: &mut nostro2::NostrNote,
    ) -> Result<(), nostro2::errors::NostrErrors> {
        note.pubkey = self.public_key();
        let id = note.serialize_id_raw()?;
        let msg = Message::from_digest(id);
        let sig = SECP256K1.sign_schnorr_no_aux_rand(&msg, &self.0);
        note.sig.replace(hex::encode(sig.as_ref()));
        Ok(())
    }

    fn generate() -> Self {
        let mut secret = [0_u8; 32];
        loop {
            getrandom::fill(&mut secret).expect("getrandom failed");
            if let Ok(sk) = secp256k1::SecretKey::from_slice(&secret) {
                return Self(secp256k1::Keypair::from_secret_key(SECP256K1, &sk));
            }
        }
    }

    fn public_key(&self) -> String {
        hex::encode(self.0.x_only_public_key().0.serialize())
    }
}

impl NostrKeypair for Secp256k1Keypair {
    fn secret_key(&self) -> Option<String> {
        Some(hex::encode(self.0.secret_key().secret_bytes()))
    }

    fn shared_point(&self, peer_pubkey: &str) -> nostro2::Result<[u8; 32]> {
        let hex_pk = hex::decode(peer_pubkey)
            .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
        // Reconstruct compressed SEC1 point from x-only key (even parity)
        let mut compressed = [0_u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(&hex_pk);
        let pk = secp256k1::PublicKey::from_slice(&compressed)
            .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
        let shared = secp256k1::ecdh::shared_secret_point(&pk, &self.0.secret_key());
        // shared_secret_point returns a 64-byte compressed point; first 32 bytes are x
        let mut point = [0_u8; 32];
        point.copy_from_slice(&shared[..32]);
        Ok(point)
    }

    fn npub(&self) -> nostro2::Result<String> {
        let hrp = bech32::Hrp::parse("npub")
            .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
        bech32::encode::<bech32::Bech32>(hrp, &self.0.x_only_public_key().0.serialize())
            .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)
    }

    fn nsec(&self) -> nostro2::Result<String> {
        let hrp = bech32::Hrp::parse("nsec")
            .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
        bech32::encode::<bech32::Bech32>(hrp, &self.0.secret_key().secret_bytes())
            .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)
    }
}

impl Nip04 for Secp256k1Keypair {
    fn shared_secret(
        &self,
        public_key_string: &str,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, nostro2_nips::Nip04Error> {
        Ok(NostrKeypair::shared_point(self, public_key_string)
            .map_err(|_| nostro2_nips::Nip04Error::SharedSecretError)?
            .into())
    }
}

impl Nip44 for Secp256k1Keypair {
    fn shared_secret(
        &self,
        public_key_string: &str,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, nostro2_nips::Nip44Error> {
        Ok(NostrKeypair::shared_point(self, public_key_string)
            .map_err(|_| nostro2_nips::Nip44Error::SharedSecretError)?
            .into())
    }
}

impl nostro2_nips::Nip17 for Secp256k1Keypair {}
impl nostro2_nips::Nip46 for Secp256k1Keypair {}
impl Nip59 for Secp256k1Keypair {}
impl nostro2_nips::Nip82 for Secp256k1Keypair {}

#[cfg(test)]
mod tests {
    use super::*;
    use nostro2::{NostrKeypair, NostrSigner};

    #[test]
    fn test_generate_and_sign() {
        let kp = Secp256k1Keypair::generate();
        let mut note = nostro2::NostrNote::text_note("Hello from secp256k1!");
        kp.sign_nostr_note(&mut note).unwrap();
        assert!(note.verify());
    }

    #[test]
    fn test_from_hex_roundtrip() {
        let kp = Secp256k1Keypair::generate();
        let hex_sk = kp.secret_key().unwrap();
        let kp2 = Secp256k1Keypair::from_hex(&hex_sk).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());
    }

    #[test]
    fn test_shared_secret_consistency() {
        let alice = Secp256k1Keypair::generate();
        let bob = Secp256k1Keypair::generate();
        let alice_secret = alice.shared_point(&bob.public_key()).unwrap();
        let bob_secret = bob.shared_point(&alice.public_key()).unwrap();
        assert_eq!(alice_secret, bob_secret);
    }

    #[test]
    fn test_nip44_encrypt_decrypt() {
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
        let nsec_str = NostrKeypair::nsec(&kp).unwrap();
        let npub_str = NostrKeypair::npub(&kp).unwrap();
        let restored = Secp256k1Keypair::from_nsec(&nsec_str).unwrap();
        assert_eq!(restored.public_key(), kp.public_key());
        assert_eq!(NostrKeypair::npub(&restored).unwrap(), npub_str);
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
        let kp: Secp256k1Keypair = hex.parse().unwrap();
        assert_eq!(
            kp.public_key(),
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );
    }
}
