use base64::engine::{general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hmac::Mac;
use secp256k1::rand::RngCore;
use zeroize::Zeroize;

#[derive(Debug)]
pub enum Nip44Error {
    CustomError(String),
    ConversionError(std::convert::Infallible),
    StandardError(Box<dyn std::error::Error>),
    SharedSecretError(String),
    DecryptionError(String),
    EncryptionError(String),
    InvalidLength,
    Base64DecodingError(base64::DecodeError),
    FromUtf8Error(std::string::FromUtf8Error),
    HkdfError,
    HmacError,
    SliceError,
}

impl std::fmt::Display for Nip44Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CustomError(e) => write!(f, "Custom error: {e}"),
            Self::ConversionError(e) => write!(f, "Conversion error: {e}"),
            Self::StandardError(e) => write!(f, "Standard error: {e}"),
            Self::SharedSecretError(e) => write!(f, "Shared secret error: {e}"),
            Self::DecryptionError(e) => write!(f, "Decryption error: {e}"),
            Self::EncryptionError(e) => write!(f, "Encryption error: {e}"),
            Self::InvalidLength => write!(f, "Invalid input length"),
            Self::Base64DecodingError(e) => write!(f, "Base64 decoding error: {e}"),
            Self::HkdfError => write!(f, "HKDF key derivation failed"),
            Self::HmacError => write!(f, "HMAC failure"),
            Self::SliceError => write!(f, "ChaCha20 slice error"),
            Self::FromUtf8Error(e) => write!(f, "UTF-8 conversion error: {e}"),
        }
    }
}
impl std::error::Error for Nip44Error {}
impl From<hex::FromHexError> for Nip44Error {
    fn from(e: hex::FromHexError) -> Self {
        Self::SharedSecretError(e.to_string())
    }
}
impl From<std::convert::Infallible> for Nip44Error {
    fn from(e: std::convert::Infallible) -> Self {
        Self::ConversionError(e)
    }
}
impl From<Box<dyn std::error::Error>> for Nip44Error {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Self::StandardError(e)
    }
}
impl From<std::num::TryFromIntError> for Nip44Error {
    fn from(e: std::num::TryFromIntError) -> Self {
        Self::DecryptionError(e.to_string())
    }
}
impl From<secp256k1::Error> for Nip44Error {
    fn from(e: secp256k1::Error) -> Self {
        Self::SharedSecretError(e.to_string())
    }
}
impl From<chacha20::cipher::InvalidLength> for Nip44Error {
    fn from(_: chacha20::cipher::InvalidLength) -> Self {
        Self::SliceError
    }
}
impl From<chacha20::cipher::StreamCipherError> for Nip44Error {
    fn from(_: chacha20::cipher::StreamCipherError) -> Self {
        Self::SliceError
    }
}
impl From<base64::DecodeSliceError> for Nip44Error {
    fn from(_: base64::DecodeSliceError) -> Self {
        Self::SliceError
    }
}
impl From<base64::DecodeError> for Nip44Error {
    fn from(e: base64::DecodeError) -> Self {
        Self::Base64DecodingError(e)
    }
}
impl From<std::string::FromUtf8Error> for Nip44Error {
    fn from(e: std::string::FromUtf8Error) -> Self {
        Self::FromUtf8Error(e)
    }
}
pub struct MacComponents<'a> {
    nonce: zeroize::Zeroizing<[u8; 12]>,
    ciphertext: &'a [u8],
}

pub trait Nip44 {
    /// Computes the shared secret used for encryption and decryption.
    ///
    /// # Errors
    /// Returns `Nip44Error::SharedSecretError` if the ECDH computation fails.
    fn shared_secret(&self, peer_pubkey: &str) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip44Error>;

    /// Encrypts a NIP-44 encrypted message.
    ///
    /// Will modify the note's content in place.
    ///
    /// # Errors
    ///
    /// - `SharedSecretError`: if shared secret derivation fails.
    /// - `HkdfError`: if key derivation via HKDF fails.
    /// - `Base64DecodingError`: if input is not valid base64.
    /// - `InvalidLength`: if input does not include all required components.
    /// - `DecryptionError`: if decryption fails or the decrypted length prefix is invalid.
    fn nip44_encrypt_note<'a>(
        &self,
        note: &'a mut nostro2::NostrNote,
        peer_pubkey: &'a str,
    ) -> Result<(), Nip44Error> {
        note.content = self.nip_44_encrypt(&note.content, peer_pubkey)?.to_string();
        Ok(())
    }
    /// Decrypts a NIP-44 encrypted message.
    ///
    /// Will return the decrypted content as a `Cow` to avoid unnecessary allocations.
    ///
    /// # Errors
    ///
    /// - `SharedSecretError`: if shared secret derivation fails.
    /// - `HkdfError`: if key derivation via HKDF fails.
    /// - `Base64DecodingError`: if input is not valid base64.
    /// - `InvalidLength`: if input does not include all required components.
    /// - `DecryptionError`: if decryption fails or the decrypted length prefix is invalid.
    fn nip44_decrypt_note<'a>(
        &self,
        note: &'a nostro2::NostrNote,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip44Error> {
        self.nip_44_decrypt(&note.content, peer_pubkey)
    }

    /// Encrypts the given plaintext using the NIP-44 protocol.
    ///
    /// # Errors
    /// - `SharedSecretError`: if shared secret derivation fails.
    /// - `HkdfError`: if key derivation via HKDF fails.
    /// - `EncryptionError`: if the plaintext length is invalid or encryption fails.
    /// - `HmacError`: if MAC calculation fails.
    fn nip_44_encrypt<'a>(
        &self,
        plaintext: &'a str,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip44Error> {
        let mut buffer =
            zeroize::Zeroizing::new(vec![
                0_u8;
                (plaintext.len() + 2).next_power_of_two().max(32)
            ]);
        let shared_secret = self.shared_secret(peer_pubkey)?;
        let mut conversation_key = Self::derive_conversation_key(shared_secret, b"nip44-v2")?;
        let mut nonce = Self::generate_nonce();

        let ciphertext = Self::encrypt(
            plaintext.as_bytes(),
            conversation_key.as_slice(),
            nonce.as_slice(),
            buffer.as_mut_slice(),
        )?;

        let mac = Self::calculate_mac(ciphertext, conversation_key.as_slice())?;
        let encoded = Self::base64_encode_params(b"1", nonce.as_slice(), ciphertext, &mac);
        conversation_key.zeroize();
        nonce.zeroize();
        Ok(encoded.into())
    }

    /// Decrypts a NIP-44 encrypted message.
    ///
    /// # Errors
    /// - `SharedSecretError`: if shared secret derivation fails.
    /// - `HkdfError`: if key derivation via HKDF fails.
    /// - `Base64DecodingError`: if input is not valid base64.
    /// - `InvalidLength`: if input does not include all required components.
    /// - `DecryptionError`: if decryption fails or the decrypted length prefix is invalid.
    /// - `Utf8Error`: if decrypted content is not valid UTF-8.
    fn nip_44_decrypt<'a>(
        &self,
        ciphertext: &'a str,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip44Error> {
        let mut buffer = zeroize::Zeroizing::new(vec![0_u8; ciphertext.len()]);
        let shared_secret = self.shared_secret(peer_pubkey)?;
        let conversation_key = Self::derive_conversation_key(shared_secret, b"nip44-v2")?;
        let mut decoded = zeroize::Zeroizing::new(general_purpose::STANDARD.decode(ciphertext)?);
        let MacComponents { nonce, ciphertext } = Self::extract_components(&decoded)?;

        let decrypted = Self::decrypt(ciphertext, conversation_key, nonce, buffer.as_mut_slice())?;

        // Zeroize sensitive data after use
        decoded.zeroize();

        Ok(std::str::from_utf8(decrypted)
            .map_err(|_| Nip44Error::SliceError)?
            .to_string()
            .into())
    }
    /// Encrypts bytes with the given key and nonce using `ChaCha20`.
    ///
    /// # Errors
    /// - `SliceError`: if key or nonce length is invalid.
    /// - `EncryptionError`: if input padding fails.
    fn encrypt<'a>(
        content: &[u8],
        key: &[u8],
        nonce: &[u8],
        buffer: &'a mut [u8],
    ) -> Result<&'a [u8], Nip44Error> {
        let padded = Self::pad_string(content, buffer)?;
        let mut cipher = chacha20::ChaCha20::new_from_slices(key, nonce)?;
        cipher.apply_keystream(padded);
        Ok(&padded[..])
    }

    /// Decrypts a ChaCha20-encrypted message and removes NIP-44 padding.
    ///
    /// # Errors
    /// - `SliceError`: if key or nonce is invalid length.
    /// - `DecryptionError`: if decrypted data is too short or length prefix is invalid.
    fn decrypt<'a>(
        ciphertext: &[u8],
        mut key: zeroize::Zeroizing<[u8; 32]>,
        mut nonce: zeroize::Zeroizing<[u8; 12]>,
        buffer: &'a mut [u8],
    ) -> Result<&'a [u8], Nip44Error> {
        if key.len() != 32 || nonce.len() != 12 {
            return Err(Nip44Error::InvalidLength);
        }

        if buffer.len() < ciphertext.len() {
            return Err(Nip44Error::DecryptionError("Buffer too small".into()));
        }

        buffer[..ciphertext.len()].copy_from_slice(ciphertext);

        let mut cipher = chacha20::ChaCha20::new_from_slices(key.as_slice(), nonce.as_slice())?;
        cipher.apply_keystream(&mut buffer[..ciphertext.len()]);

        if ciphertext.len() < 2 {
            return Err(Nip44Error::DecryptionError("Too short".into()));
        }

        let len = u16::from_be_bytes([buffer[0], buffer[1]]) as usize;

        if len > ciphertext.len() - 2 {
            return Err(Nip44Error::DecryptionError("Invalid prefix len".into()));
        }

        // Zeroize key, nonce, and buffer after use
        key.zeroize();
        nonce.zeroize();

        Ok(&buffer[2..2 + len])
    }

    /// Derives a conversation key using HKDF.
    ///
    /// # Errors
    /// - `HkdfError`: if HKDF expansion fails.
    fn derive_conversation_key(
        mut shared_secret: zeroize::Zeroizing<[u8; 32]>,
        salt: &[u8],
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip44Error> {
        let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), shared_secret.as_slice());
        shared_secret.zeroize();
        let mut okm = [0_u8; 32];
        hkdf.expand(&[], &mut okm)
            .map_err(|_| Nip44Error::HkdfError)?;
        Ok(okm.into())
    }

    /// Extracts nonce and ciphertext from the decoded payload.
    ///
    /// # Errors
    /// - `InvalidLength`: if the input is too short to contain required components.
    fn extract_components(decoded: &[u8]) -> Result<MacComponents, Nip44Error> {
        if decoded.len() < 1 + 12 + 32 {
            return Err(Nip44Error::InvalidLength);
        }
        Ok(MacComponents {
            nonce: zeroize::Zeroizing::new(
                decoded[1..13]
                    .try_into()
                    .map_err(|_| Nip44Error::SliceError)?,
            ),
            ciphertext: &decoded[13..decoded.len() - 32],
        })
    }
    /// Calculates the HMAC-SHA256 MAC for the given data and key.
    ///
    /// # Errors
    /// - `HmacError`: if the MAC construction fails.
    fn calculate_mac(data: &[u8], key: &[u8]) -> Result<[u8; 32], Nip44Error> {
        let mut mac =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(key).map_err(|_| Nip44Error::HmacError)?;
        mac.update(data);
        let result = mac.finalize().into_bytes();
        Ok(result.into())
    }

    /// Adds a length prefix and pads plaintext to a power-of-two size.
    ///
    /// # Errors
    /// - `EncryptionError`: if the plaintext is empty or too long.
    fn pad_string<'a>(plaintext: &[u8], buffer: &'a mut [u8]) -> Result<&'a mut [u8], Nip44Error> {
        if plaintext.is_empty() || plaintext.len() > 65535 {
            return Err(Nip44Error::EncryptionError(
                "Invalid plaintext length".into(),
            ));
        }

        let total_len = (plaintext.len() + 2).next_power_of_two().max(32);

        if buffer.len() < total_len {
            return Err(Nip44Error::EncryptionError("Buffer too small".into()));
        }

        let len_bytes = u16::try_from(plaintext.len())?.to_be_bytes();
        buffer[..2].copy_from_slice(&len_bytes);
        buffer[2..2 + plaintext.len()].copy_from_slice(plaintext);

        // zero pad the rest
        for b in &mut buffer[2 + plaintext.len()..total_len] {
            *b = 0;
        }

        Ok(&mut buffer[..total_len])
    }

    #[must_use]
    fn generate_nonce() -> zeroize::Zeroizing<[u8; 12]> {
        let mut nonce = [0_u8; 12];
        secp256k1::rand::rngs::OsRng.fill_bytes(&mut nonce);
        nonce.into()
    }
    #[must_use]
    fn base64_encode_params(version: &[u8], nonce: &[u8], ciphertext: &[u8], mac: &[u8]) -> String {
        let mut buf =
            Vec::with_capacity(version.len() + nonce.len() + ciphertext.len() + mac.len());
        buf.extend_from_slice(version);
        buf.extend_from_slice(nonce);
        buf.extend_from_slice(ciphertext);
        buf.extend_from_slice(mac);

        let mut out = String::with_capacity((buf.len() * 4).div_ceil(3));
        general_purpose::STANDARD.encode_string(&buf, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey};

    struct TestNip44 {
        sender_sk: SecretKey,
        receiver_pk: PublicKey,
    }

    impl Nip44 for TestNip44 {
        fn shared_secret(
            &self,
            _peer_pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip44Error> {
            let shared_point =
                secp256k1::ecdh::SharedSecret::new(&self.receiver_pk, &self.sender_sk);
            let shared_point_slice: [u8; 32] = shared_point.as_ref().try_into().map_err(|_| {
                Nip44Error::SharedSecretError("Shared secret slice is wrong length".into())
            })?;
            Ok(shared_point_slice.into())
        }
    }

    #[test]
    fn test_encrypt_decrypt_success() {
        let secp = Secp256k1::new();

        // Simulate two parties
        let sender_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());
        let receiver_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());

        let sender = TestNip44 {
            sender_sk: sender_kp.secret_key(),
            receiver_pk: receiver_kp.public_key(),
        };

        let receiver = TestNip44 {
            sender_sk: receiver_kp.secret_key(),
            receiver_pk: sender_kp.public_key(),
        };

        let plaintext = "Hello NIP-44 encryption!";
        let receiver_pk = receiver.receiver_pk.to_string();
        let sender_pk = sender.receiver_pk.to_string();
        let ciphertext = sender.nip_44_encrypt(plaintext, &receiver_pk).unwrap();
        let decrypted = receiver.nip_44_decrypt(&ciphertext, &sender_pk).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_invalid_decryption_key() {
        let secp = Secp256k1::new();

        let sender_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());
        let receiver_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());
        let wrong_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());

        let sender = TestNip44 {
            sender_sk: sender_kp.secret_key(),
            receiver_pk: receiver_kp.public_key(),
        };

        let wrong_receiver = TestNip44 {
            sender_sk: wrong_kp.secret_key(),
            receiver_pk: sender_kp.public_key(),
        };

        let plaintext = "Hello NIP-44 encryption!";
        let receiver_pk = wrong_receiver.receiver_pk.to_string();
        let sender_pk = sender.receiver_pk.to_string();
        let ciphertext = sender.nip_44_encrypt(plaintext, &receiver_pk).unwrap();
        let result = wrong_receiver.nip_44_decrypt(&ciphertext, &sender_pk);

        assert!(result.is_err());
    }
    use std::fmt::Write as _;
    #[test]
    fn encrypt_very_large_note() {
        let secp = Secp256k1::new();
        let sender_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());
        let receiver_kp = Keypair::new(&secp, &mut secp256k1::rand::thread_rng());

        let sender = TestNip44 {
            sender_sk: sender_kp.secret_key(),
            receiver_pk: receiver_kp.public_key(),
        };

        let receiver = TestNip44 {
            sender_sk: receiver_kp.secret_key(),
            receiver_pk: sender_kp.public_key(),
        };

        let mut plaintext = String::new();
        for i in 0..15329 {
            // plaintext.push_str(&format!("{i}"));
            let _ = write!(plaintext, "{i}");
        }
        let receiver_pk = receiver.receiver_pk.to_string();
        let sender_pk = sender.receiver_pk.to_string();
        let ciphertext = sender.nip_44_encrypt(&plaintext, &receiver_pk).unwrap();
        let decrypted = receiver.nip_44_decrypt(&ciphertext, &sender_pk).unwrap();

        assert_eq!(decrypted, plaintext);
    }
}
