use k256::schnorr::signature::hazmat::PrehashSigner;
use nostro2::NostrSigner;
use nostro2_nips::{Nip04, Nip44, Nip59};

use crate::errors::NostrKeypairError;

/// Encryption scheme for encrypted Nostr messages
///
/// Nostr supports multiple encryption standards for private messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionScheme {
    /// NIP-04: Legacy encryption (deprecated, use Nip44)
    Nip04,
    /// NIP-44: Modern encryption standard
    Nip44,
}

/// Gift wrap scheme for sealed sender privacy (NIP-59)
///
/// Gift wrapping provides sender privacy by encrypting notes in multiple layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GiftwrapScheme {
    /// Persistent event (kind < 10000)
    Persistent,
    /// Replaceable event (kind 10000-19999)
    Replaceable,
    /// Ephemeral event (kind 20000-29999)
    Ephemeral,
    /// Parameterized replaceable event with d-tag
    Parameterized(String),
}

/// Nostr keypair using the k256 (pure Rust) backend
///
/// A keypair consists of a k256 signing key and an extractability flag.
/// Non-extractable keypairs (default) prevent accidental key export, providing
/// better security for keys stored in memory.
///
/// # Examples
///
/// ```rust
/// use nostro2_signer::K256Keypair;
/// use nostro2::NostrSigner;
///
/// // Create new random keypair
/// let keypair = K256Keypair::new();
///
/// // Get public key
/// let pubkey = keypair.pubkey();
///
/// // Create extractable keypair (allows key export)
/// let keypair = K256Keypair::new_extractable();
/// let nsec = keypair.nsec().unwrap();
/// ```
#[derive(Clone)]
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

    /// Create from nsec bech32 string
    ///
    /// # Errors
    ///
    /// Returns an error if the bech32 string is invalid
    pub fn from_nsec(nsec: &str, extractable: bool) -> Result<Self, NostrKeypairError> {
        if !nsec.starts_with("nsec") {
            return Err(NostrKeypairError::HrpParseError);
        }
        let (hrp, data) = bech32::decode(nsec)?;
        if hrp.to_string() != "nsec" {
            return Err(NostrKeypairError::HrpParseError);
        }
        let mut keypair = Self::try_from(data.as_slice())?;
        keypair.extractable = extractable;
        Ok(keypair)
    }

    /// Get the public key as a bech32 string starting with "npub"
    ///
    /// # Errors
    ///
    /// Should never fail unless the keypair is invalid
    pub fn npub(&self) -> Result<String, NostrKeypairError> {
        let hrp = bech32::Hrp::parse("npub").map_err(|_| NostrKeypairError::HrpParseError)?;
        Ok(bech32::encode::<bech32::Bech32>(
            hrp,
            &self.signing_key.verifying_key().to_bytes(),
        )?)
    }

    /// Get the secret key as a bech32 string starting with "nsec"
    ///
    /// # Errors
    ///
    /// Returns an error if the keypair is not extractable
    /// or if the bech32 string cannot be generated
    pub fn nsec(&self) -> Result<String, NostrKeypairError> {
        if !self.extractable {
            return Err(NostrKeypairError::NotExtractable);
        }
        let secret_key = self.signing_key.to_bytes();
        let hrp = bech32::Hrp::parse("nsec").map_err(|_| NostrKeypairError::HrpParseError)?;
        let string = bech32::encode::<bech32::Bech32>(hrp, &secret_key)?;
        Ok(string)
    }

    /// Create from mnemonic phrase
    ///
    /// # Errors
    ///
    /// Returns an error if the mnemonic is invalid
    pub fn from_mnemonic(
        mnemonic: &str,
        language: bip39::Language,
        extractable: bool,
    ) -> Result<Self, NostrKeypairError> {
        Self::parse_mnemonic(mnemonic, language, extractable)
    }

    /// Get the mnemonic for the keypair separated by spaces
    ///
    /// # Errors
    ///
    /// Returns an error if the keypair is not extractable
    /// or if the mnemonic cannot be generated
    pub fn mnemonic(&self, language: bip39::Language) -> Result<String, NostrKeypairError> {
        if !self.extractable {
            return Err(NostrKeypairError::NotExtractable);
        }
        let secret_key = self.signing_key.to_bytes();
        let mnemonic = bip39::Mnemonic::from_entropy_in(language, &secret_key)?;
        let mut out = String::with_capacity(256); // heuristically sized
        for word in mnemonic.words() {
            out.push_str(word);
            out.push(' ');
        }
        out.pop(); // remove trailing space
        Ok(out)
    }

    /// Parse a mnemonic 12 or 24 words into a keypair separated by spaces
    ///
    /// # Errors
    ///
    /// Returns an error if the mnemonic is invalid
    pub fn parse_mnemonic(
        mnemonic: &str,
        language: bip39::Language,
        extractable: bool,
    ) -> Result<Self, NostrKeypairError> {
        let mnemonic = bip39::Mnemonic::parse_in(language, mnemonic)?;
        let mut keypair: Self = mnemonic.try_into()?;
        keypair.extractable = extractable;
        Ok(keypair)
    }

    /// Get public key as bytes
    #[must_use]
    #[inline]
    pub fn public_key_slice(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes().into()
    }

    /// Set whether the keypair allows key extraction
    pub const fn set_extractable(&mut self, extractable: bool) {
        self.extractable = extractable;
    }

    /// Sign and encrypt a note
    ///
    /// # Errors
    ///
    /// Returns an error if the encryption scheme is not supported
    /// or if the note cannot be encrypted
    pub fn sign_encrypted_note(
        &self,
        note: &mut nostro2::NostrNote,
        peer_pubkey: &str,
        encryption_scheme: &EncryptionScheme,
    ) -> Result<(), NostrKeypairError> {
        match encryption_scheme {
            EncryptionScheme::Nip04 => self.nip04_encrypt_note(note, peer_pubkey)?,
            EncryptionScheme::Nip44 => {
                self.nip44_encrypt_note(note, peer_pubkey)?;
            }
        }
        self.sign_nostr_note(note)?;
        Ok(())
    }

    /// Decrypt a note
    ///
    /// # Errors
    ///
    /// Returns an error if the encryption scheme is not supported
    /// or if the note cannot be decrypted
    pub fn decrypt_note<'a>(
        &self,
        note: &'a nostro2::NostrNote,
        peer_pubkey: &'a str,
        encryption_scheme: &EncryptionScheme,
    ) -> Result<std::borrow::Cow<'a, str>, NostrKeypairError> {
        match encryption_scheme {
            EncryptionScheme::Nip04 => Ok(self.nip04_decrypt_note(note, peer_pubkey)?),
            EncryptionScheme::Nip44 => Ok(self.nip44_decrypt_note(note, peer_pubkey)?),
        }
    }

    /// Giftwrap a note (NIP-59)
    ///
    /// - A `rumor` is a regular nostr event, but is not signed. This means that if it is leaked, it cannot be verified.
    /// - A `rumor` is serialized to JSON, encrypted, and placed in the `content` field of a `seal`.
    ///   The `seal` is then signed by the author of the note. The only information publicly available on a `seal` is who signed it, but not what was said.
    /// - A `seal` is serialized to `JSON`, encrypted, and placed in the `content` field of a `gift wrap`.
    ///
    /// # Errors
    ///
    /// Returns an error if the encryption scheme is not supported
    /// or if the note cannot be encrypted.
    pub fn giftwrap_note(
        &self,
        note: &mut nostro2::NostrNote,
        peer_pubkey: &str,
        scheme: &GiftwrapScheme,
    ) -> Result<nostro2::NostrNote, NostrKeypairError> {
        match scheme {
            GiftwrapScheme::Persistent => Ok(self.giftwrap(note, peer_pubkey)?),
            GiftwrapScheme::Replaceable => Ok(self.replaceable_giftwrap(note, peer_pubkey)?),
            GiftwrapScheme::Ephemeral => Ok(self.ephemeral_giftwrap(note, peer_pubkey)?),
            GiftwrapScheme::Parameterized(tag) => {
                Ok(self.parameterized_giftwrap(note, peer_pubkey, tag)?)
            }
        }
    }

    /// Extract a rumor from a note (NIP-59)
    ///
    /// # Errors
    ///
    /// Returns an error if the note cannot be decrypted
    /// or if the note is not a rumor.
    pub fn extract_rumor(
        &self,
        note: &nostro2::NostrNote,
    ) -> Result<nostro2::NostrNote, NostrKeypairError> {
        Ok(self.rumor(note)?)
    }
}

impl std::str::FromStr for K256Keypair {
    type Err = NostrKeypairError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Try nsec first (most specific)
        if value.starts_with("nsec") {
            if let Ok(keypair) = Self::from_nsec(value, false) {
                return Ok(keypair);
            }
        }

        // Try hex (64 characters)
        if value.len() == 64 {
            if let Ok(keypair) = Self::from_hex(value, false) {
                return Ok(keypair);
            }
        }

        // Try mnemonic (try available languages)
        let languages = [bip39::Language::English, bip39::Language::Spanish];

        for language in languages {
            if let Ok(keypair) = Self::from_mnemonic(value, language, false) {
                return Ok(keypair);
            }
        }

        // If nothing worked, return an error
        Err(NostrKeypairError::InvalidKey)
    }
}

impl TryFrom<&[u8]> for K256Keypair {
    type Error = NostrKeypairError;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let field_bytes = k256::FieldBytes::from_slice(value);
        let signing_key =
            k256::schnorr::SigningKey::from_bytes(field_bytes).map_err(|_| NostrKeypairError::InvalidKey)?;
        Ok(Self {
            signing_key,
            extractable: false,
        })
    }
}

impl TryFrom<bip39::Mnemonic> for K256Keypair {
    type Error = NostrKeypairError;
    fn try_from(value: bip39::Mnemonic) -> Result<Self, Self::Error> {
        Self::try_from(value.to_entropy().as_slice())
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

    #[test]
    fn test_bech32_encoding_decoding() {
        let kp = K256Keypair::new_extractable();
        let nsec = kp.nsec().unwrap();
        let npub = kp.npub().unwrap();

        let restored_kp = nsec.parse::<K256Keypair>().unwrap();
        assert_eq!(restored_kp.public_key(), kp.public_key());
        assert_eq!(restored_kp.npub().unwrap(), npub);
    }

    #[test]
    fn test_mnemonic_roundtrip() {
        let kp = K256Keypair::new_extractable();
        let mnemonic = kp.mnemonic(bip39::Language::English).unwrap();
        let restored_kp =
            K256Keypair::parse_mnemonic(&mnemonic, bip39::Language::English, true).unwrap();
        assert_eq!(restored_kp.secret_key(), kp.secret_key());
        assert_eq!(restored_kp.public_key(), kp.public_key());
    }

    #[test]
    fn test_from_str_tries_all_formats() {
        // Test hex
        let hex = "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8";
        let kp1 = hex.parse::<K256Keypair>().unwrap();
        assert_eq!(
            kp1.pubkey(),
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );

        // Test nsec
        let nsec = "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53";
        let kp2 = nsec.parse::<K256Keypair>().unwrap();
        assert_eq!(kp2.pubkey(), kp1.pubkey());

        // Test mnemonic
        let kp3 = K256Keypair::new_extractable();
        let mnemonic = kp3.mnemonic(bip39::Language::English).unwrap();
        let kp4 = mnemonic.parse::<K256Keypair>().unwrap();
        assert_eq!(kp4.pubkey(), kp3.pubkey());
    }

    #[test]
    fn test_sign_encrypted_note_nip44() {
        let alice = K256Keypair::new_extractable();
        let bob = K256Keypair::new_extractable();

        let mut note = nostro2::NostrNote {
            content: "Hello, Bob!".to_string(),
            kind: 1,
            ..Default::default()
        };

        let res = alice.sign_encrypted_note(&mut note, &bob.public_key(), &EncryptionScheme::Nip44);
        assert!(res.is_ok());
        assert!(note.sig.is_some());

        let pubkey = alice.public_key();
        let decrypted_note = bob.decrypt_note(&note, &pubkey, &EncryptionScheme::Nip44);
        assert!(decrypted_note.is_ok());
        assert_eq!(decrypted_note.unwrap(), "Hello, Bob!");
    }

    #[test]
    fn test_pubkey_alias() {
        let kp = K256Keypair::new();
        assert_eq!(kp.pubkey(), kp.public_key());
        assert_eq!(kp.pubkey_bytes(), kp.public_key_slice());
    }

    #[test]
    fn test_set_extractable() {
        let mut kp = K256Keypair::new();
        assert_eq!(kp.secret_key(), hex::encode([0_u8; 32]));

        kp.set_extractable(true);
        assert_ne!(kp.secret_key(), hex::encode([0_u8; 32]));
    }
}
