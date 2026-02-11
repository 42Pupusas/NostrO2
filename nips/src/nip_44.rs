use base64::engine::{general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hmac::Mac;
use rand_core::RngCore;
use zeroize::Zeroize;

#[derive(Debug, thiserror::Error)]
pub enum Nip44Error {
    #[error("Shared secret error")]
    SharedSecretError,
    #[error("Hex decoding error {0}")]
    FromHexError(#[from] hex::FromHexError),
    #[error("Nostr note error {0}")]
    NostrNoteError(#[from] nostro2::errors::NostrErrors),
    #[error("Invalid input length")]
    InvalidLength,
    #[error("Base64 decoding error {0}")]
    Base64DecodingError(#[from] base64::DecodeError),
    #[error("UTF-8 conversion error {0}")]
    FromUtf8Error(#[from] std::str::Utf8Error),
    #[error("HKDF key derivation failed")]
    HkdfError,
    #[error("HMAC failure")]
    HmacError,
    #[error("ChaCha20 slice error")]
    SliceError(#[from] chacha20::cipher::InvalidLength),
    #[error("Invalid length prefix")]
    InvalidPrefixLen,
    #[error("Decryption error {0}")]
    FromArrayError(#[from] std::array::TryFromSliceError),
    #[error("Buffer too small")]
    BufferTooSmall,
    #[error("Encryption error {0}")]
    FromIntError(#[from] std::num::TryFromIntError),
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

        Ok(std::str::from_utf8(decrypted)?.to_string().into())
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
            return Err(Nip44Error::InvalidLength);
        }

        buffer[..ciphertext.len()].copy_from_slice(ciphertext);

        let mut cipher = chacha20::ChaCha20::new_from_slices(key.as_slice(), nonce.as_slice())?;
        cipher.apply_keystream(&mut buffer[..ciphertext.len()]);

        if ciphertext.len() < 2 {
            return Err(Nip44Error::InvalidLength);
        }

        let len = u16::from_be_bytes([buffer[0], buffer[1]]) as usize;

        if len > ciphertext.len() - 2 {
            return Err(Nip44Error::InvalidPrefixLen);
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
    fn extract_components(decoded: &[u8]) -> Result<MacComponents<'_>, Nip44Error> {
        if decoded.len() < 1 + 12 + 32 {
            return Err(Nip44Error::InvalidLength);
        }
        Ok(MacComponents {
            nonce: zeroize::Zeroizing::new(decoded[1..13].try_into()?),
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
            return Err(Nip44Error::InvalidLength);
        }

        let total_len = (plaintext.len() + 2).next_power_of_two().max(32);

        if buffer.len() < total_len {
            return Err(Nip44Error::BufferTooSmall);
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
        rand_core::OsRng.fill_bytes(&mut nonce);
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
    use nostro2::NostrSigner;

    #[test]
    fn test_encrypt_decrypt_success() {
        // Use the NipTester from lib.rs which uses k256
        let sender = crate::tests::NipTester::generate(false);
        let receiver = crate::tests::NipTester::generate(false);

        let plaintext = "Hello NIP-44 encryption!";
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let ciphertext = sender.nip_44_encrypt(plaintext, &receiver_pk).unwrap();
        let decrypted = receiver.nip_44_decrypt(&ciphertext, &sender_pk).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_invalid_decryption_key() {
        let sender = crate::tests::NipTester::generate(false);
        let receiver = crate::tests::NipTester::generate(false);
        let wrong_receiver = crate::tests::NipTester::generate(false);

        let plaintext = "Hello NIP-44 encryption!";
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let ciphertext = sender.nip_44_encrypt(plaintext, &receiver_pk).unwrap();
        let result = wrong_receiver.nip_44_decrypt(&ciphertext, &sender_pk);

        assert!(result.is_err());
    }

    use std::fmt::Write as _;
    #[test]
    fn encrypt_very_large_note() {
        let sender = crate::tests::NipTester::generate(false);
        let receiver = crate::tests::NipTester::generate(false);

        let mut plaintext = String::new();
        for i in 0..15329 {
            let _ = write!(plaintext, "{i}");
        }
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let ciphertext = sender.nip_44_encrypt(&plaintext, &receiver_pk).unwrap();
        let decrypted = receiver.nip_44_decrypt(&ciphertext, &sender_pk).unwrap();

        assert_eq!(decrypted, plaintext);
    }
}
