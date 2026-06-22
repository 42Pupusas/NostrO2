use base64::engine::{general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hmac::{KeyInit, Mac};
use zeroize::Zeroize;

#[derive(Debug)]
pub enum Nip44Error {
    SharedSecretError,
    FromHexError(nostro2_traits::hex::HexError),
    NostrNoteError(nostro2::errors::NostrErrors),
    InvalidLength,
    Base64DecodingError(base64::DecodeError),
    FromUtf8Error(std::str::Utf8Error),
    HkdfError,
    HmacError,
    SliceError(chacha20::cipher::InvalidLength),
    InvalidPrefixLen,
    FromArrayError(std::array::TryFromSliceError),
    BufferTooSmall,
    FromIntError(std::num::TryFromIntError),
    /// The payload's version byte is not one this library can decrypt.
    UnknownVersion(u8),
    /// The authentication tag did not match — payload is forged or corrupt.
    MacMismatch,
}

impl std::fmt::Display for Nip44Error {
    #[allow(unknown_lints, crappy)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SharedSecretError => f.write_str("shared secret error"),
            Self::FromHexError(e) => write!(f, "hex decoding error: {e}"),
            Self::NostrNoteError(e) => write!(f, "{e}"),
            Self::InvalidLength => f.write_str("invalid input length"),
            Self::Base64DecodingError(e) => write!(f, "base64 decoding error: {e}"),
            Self::FromUtf8Error(e) => write!(f, "UTF-8 conversion error: {e}"),
            Self::HkdfError => f.write_str("HKDF key derivation failed"),
            Self::HmacError => f.write_str("HMAC failure"),
            Self::SliceError(e) => write!(f, "ChaCha20 slice error: {e}"),
            Self::InvalidPrefixLen => f.write_str("invalid length prefix"),
            Self::FromArrayError(e) => write!(f, "decryption error: {e}"),
            Self::BufferTooSmall => f.write_str("buffer too small"),
            Self::FromIntError(e) => write!(f, "encryption error: {e}"),
            Self::UnknownVersion(v) => write!(f, "unsupported NIP-44 version byte: {v:#04x}"),
            Self::MacMismatch => f.write_str("MAC mismatch: payload not authentic"),
        }
    }
}

impl std::error::Error for Nip44Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FromHexError(e) => Some(e),
            Self::NostrNoteError(e) => Some(e),
            Self::Base64DecodingError(e) => Some(e),
            Self::FromUtf8Error(e) => Some(e),
            Self::FromArrayError(e) => Some(e),
            Self::FromIntError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<nostro2_traits::hex::HexError> for Nip44Error {
    fn from(e: nostro2_traits::hex::HexError) -> Self {
        Self::FromHexError(e)
    }
}
impl From<nostro2::errors::NostrErrors> for Nip44Error {
    fn from(e: nostro2::errors::NostrErrors) -> Self {
        Self::NostrNoteError(e)
    }
}
impl From<base64::DecodeError> for Nip44Error {
    fn from(e: base64::DecodeError) -> Self {
        Self::Base64DecodingError(e)
    }
}
impl From<std::str::Utf8Error> for Nip44Error {
    fn from(e: std::str::Utf8Error) -> Self {
        Self::FromUtf8Error(e)
    }
}
impl From<chacha20::cipher::InvalidLength> for Nip44Error {
    fn from(e: chacha20::cipher::InvalidLength) -> Self {
        Self::SliceError(e)
    }
}
impl From<std::array::TryFromSliceError> for Nip44Error {
    fn from(e: std::array::TryFromSliceError) -> Self {
        Self::FromArrayError(e)
    }
}
impl From<std::num::TryFromIntError> for Nip44Error {
    fn from(e: std::num::TryFromIntError) -> Self {
        Self::FromIntError(e)
    }
}

/// NIP-44 version 2 — the spec-compliant format (secp256k1 ECDH, HKDF,
/// `ChaCha20`, HMAC-SHA256). Interoperates with Primal, Iris, and every other
/// conformant client.
pub const VERSION_V2: u8 = 0x02;

/// Legacy nostro2 format (`b"1"`).
///
/// Produced by nostro2-nips <= 0.3.0. **Not** spec-compliant and not
/// interoperable, but retained for decrypt so existing stored data and old
/// peers keep working. New messages are never written in this format.
pub const VERSION_LEGACY: u8 = 0x31;

/// Per-message key material derived from the conversation key and nonce
/// (NIP-44 `get_message_keys`).
pub struct MessageKeys {
    chacha_key: zeroize::Zeroizing<[u8; 32]>,
    chacha_nonce: zeroize::Zeroizing<[u8; 12]>,
    hmac_key: zeroize::Zeroizing<[u8; 32]>,
}

pub trait Nip44: nostro2::NostrKeypair {
    /// Computes the shared secret used for encryption and decryption.
    ///
    /// # Errors
    /// Returns `Nip44Error::SharedSecretError` if the ECDH computation fails.
    fn shared_secret(&self, peer_pubkey: &str) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip44Error> {
        Ok(nostro2::NostrKeypair::shared_point(self, peer_pubkey)
            .map_err(|_| Nip44Error::SharedSecretError)?
            .into())
    }

    /// Encrypts a note's content in place using NIP-44 v2.
    ///
    /// # Errors
    /// Propagates any failure from [`nip_44_encrypt`](Self::nip_44_encrypt).
    fn nip44_encrypt_note<'a>(
        &self,
        note: &'a mut nostro2::NostrNote,
        peer_pubkey: &'a str,
    ) -> Result<(), Nip44Error> {
        note.content = self.nip_44_encrypt(&note.content, peer_pubkey)?.to_string();
        Ok(())
    }

    /// Decrypts a note's NIP-44 content, auto-detecting the version.
    ///
    /// # Errors
    /// Propagates any failure from [`nip_44_decrypt`](Self::nip_44_decrypt).
    fn nip44_decrypt_note<'a>(
        &self,
        note: &'a nostro2::NostrNote,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip44Error> {
        self.nip_44_decrypt(&note.content, peer_pubkey)
    }

    /// Encrypts `plaintext` for `peer_pubkey` using **NIP-44 version 2**.
    ///
    /// The output is a base64 payload beginning with version byte `0x02`,
    /// interoperable with any spec-compliant NIP-44 implementation.
    ///
    /// # Errors
    /// - `SharedSecretError` / `HkdfError`: key derivation failure.
    /// - `InvalidLength`: plaintext empty or longer than 65535 bytes.
    /// - `HmacError` / `SliceError`: MAC or cipher construction failure.
    fn nip_44_encrypt<'a>(
        &self,
        plaintext: &'a str,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip44Error> {
        let shared = self.shared_secret(peer_pubkey)?;
        let conversation_key = Self::conversation_key_v2(shared)?;
        let nonce = Self::generate_nonce_32();
        let payload = Self::encrypt_v2(&conversation_key, &nonce, plaintext.as_bytes())?;
        Ok(payload.into())
    }

    /// Decrypts a NIP-44 payload from `peer_pubkey`, dispatching on the
    /// version byte: `0x02` (spec v2) or `0x31` (legacy nostro2).
    ///
    /// # Errors
    /// - `UnknownVersion`: leading `#` flag or an unrecognised version byte.
    /// - `MacMismatch`: v2 authentication tag did not verify.
    /// - `Base64DecodingError` / `InvalidLength` / `InvalidPrefixLen`: malformed payload.
    /// - `FromUtf8Error`: decrypted bytes are not valid UTF-8.
    fn nip_44_decrypt<'a>(
        &self,
        payload: &'a str,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip44Error> {
        // A leading '#' is the spec's reserved flag for a future non-base64
        // encoding; we must report it as unsupported rather than a b64 error.
        if payload.as_bytes().first() == Some(&b'#') {
            return Err(Nip44Error::UnknownVersion(b'#'));
        }
        let mut decoded = zeroize::Zeroizing::new(general_purpose::STANDARD.decode(payload)?);
        let version = *decoded.first().ok_or(Nip44Error::InvalidLength)?;
        let shared = self.shared_secret(peer_pubkey)?;

        let plaintext = match version {
            VERSION_V2 => {
                let conversation_key = Self::conversation_key_v2(shared)?;
                Self::decrypt_v2(&conversation_key, &decoded)?
            }
            VERSION_LEGACY => {
                let conversation_key = Self::derive_conversation_key_legacy(shared, b"nip44-v2")?;
                Self::decrypt_legacy(&conversation_key, &decoded)?
            }
            other => {
                decoded.zeroize();
                return Err(Nip44Error::UnknownVersion(other));
            }
        };
        decoded.zeroize();
        Ok(plaintext.into())
    }

    // ── NIP-44 v2 (spec-compliant) ────────────────────────────────────

    /// `get_conversation_key`: HKDF-extract over the unhashed ECDH x-coordinate
    /// with `salt = utf8("nip44-v2")`. The extract output (PRK) **is** the
    /// conversation key — no expand step.
    ///
    /// # Errors
    /// Never fails; returns `Result` for signature symmetry.
    fn conversation_key_v2(
        mut shared_x: zeroize::Zeroizing<[u8; 32]>,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip44Error> {
        let (prk, _hk) = hkdf::Hkdf::<sha2::Sha256>::extract(Some(b"nip44-v2"), shared_x.as_slice());
        shared_x.zeroize();
        let mut key = zeroize::Zeroizing::new([0_u8; 32]);
        key.copy_from_slice(&prk);
        Ok(key)
    }

    /// `get_message_keys`: HKDF-expand `PRK = conversation_key`, `info = nonce`,
    /// `L = 76` → `chacha_key`(32) ‖ `chacha_nonce`(12) ‖ `hmac_key`(32).
    ///
    /// # Errors
    /// `HkdfError` if expansion fails (never for L = 76).
    fn get_message_keys(
        conversation_key: &[u8; 32],
        nonce: &[u8; 32],
    ) -> Result<MessageKeys, Nip44Error> {
        let hkdf = hkdf::Hkdf::<sha2::Sha256>::from_prk(conversation_key)
            .map_err(|_| Nip44Error::HkdfError)?;
        let mut okm = zeroize::Zeroizing::new([0_u8; 76]);
        hkdf.expand(nonce, okm.as_mut_slice())
            .map_err(|_| Nip44Error::HkdfError)?;
        let mut chacha_key = zeroize::Zeroizing::new([0_u8; 32]);
        chacha_key.copy_from_slice(&okm[0..32]);
        let mut chacha_nonce = zeroize::Zeroizing::new([0_u8; 12]);
        chacha_nonce.copy_from_slice(&okm[32..44]);
        let mut hmac_key = zeroize::Zeroizing::new([0_u8; 32]);
        hmac_key.copy_from_slice(&okm[44..76]);
        Ok(MessageKeys {
            chacha_key,
            chacha_nonce,
            hmac_key,
        })
    }

    /// Encrypts already-derived `conversation_key` material with an explicit
    /// `nonce`. Split out from [`nip_44_encrypt`](Self::nip_44_encrypt) so the
    /// official test vectors (fixed nonce) can be exercised deterministically.
    ///
    /// # Errors
    /// - `InvalidLength`: plaintext empty or > 65535 bytes.
    /// - `HkdfError` / `HmacError` / `SliceError`: derivation or cipher failure.
    fn encrypt_v2(
        conversation_key: &[u8; 32],
        nonce: &[u8; 32],
        plaintext: &[u8],
    ) -> Result<String, Nip44Error> {
        let keys = Self::get_message_keys(conversation_key, nonce)?;
        let mut padded = Self::pad_v2(plaintext)?;

        let mut cipher =
            chacha20::ChaCha20::new_from_slices(keys.chacha_key.as_slice(), keys.chacha_nonce.as_slice())?;
        cipher.apply_keystream(padded.as_mut_slice());
        // `padded` is now the ciphertext.

        let mac = Self::hmac_with_aad(keys.hmac_key.as_slice(), padded.as_slice(), nonce)?;
        let payload = Self::base64_encode_params(&[VERSION_V2], nonce, padded.as_slice(), &mac);
        padded.zeroize();
        Ok(payload)
    }

    /// Decrypts a decoded v2 payload (`decoded[0] == 0x02`), verifying the MAC
    /// in constant time **before** decrypting.
    ///
    /// # Errors
    /// - `InvalidLength`: decoded payload outside the 99..=65603 byte range.
    /// - `MacMismatch`: authentication tag mismatch.
    /// - `InvalidPrefixLen`: padding/length-prefix inconsistency.
    /// - `FromUtf8Error`: plaintext not valid UTF-8.
    fn decrypt_v2(
        conversation_key: &[u8; 32],
        decoded: &[u8],
    ) -> Result<String, Nip44Error> {
        let dlen = decoded.len();
        // version(1) + nonce(32) + ciphertext(>=34) + mac(32)
        if !(99..=65603).contains(&dlen) {
            return Err(Nip44Error::InvalidLength);
        }
        let nonce: [u8; 32] = decoded[1..33].try_into()?;
        let ciphertext = &decoded[33..dlen - 32];
        let mac = &decoded[dlen - 32..];

        let keys = Self::get_message_keys(conversation_key, &nonce)?;
        Self::verify_hmac_with_aad(keys.hmac_key.as_slice(), ciphertext, &nonce, mac)?;

        let mut buffer = zeroize::Zeroizing::new(ciphertext.to_vec());
        let mut cipher =
            chacha20::ChaCha20::new_from_slices(keys.chacha_key.as_slice(), keys.chacha_nonce.as_slice())?;
        cipher.apply_keystream(buffer.as_mut_slice());

        let plaintext = Self::unpad_v2(&buffer)?.to_string();
        buffer.zeroize();
        Ok(plaintext)
    }

    /// Like [`decrypt_v2`](Self::decrypt_v2) but returns the raw plaintext
    /// **bytes** instead of a `String`. Used by the NIP-104 double ratchet,
    /// whose payloads are opaque (not guaranteed UTF-8 at this layer).
    ///
    /// # Errors
    /// - `InvalidLength`: decoded payload outside the 99..=65603 byte range.
    /// - `MacMismatch`: authentication tag mismatch.
    /// - `InvalidPrefixLen`: padding/length-prefix inconsistency.
    fn decrypt_v2_bytes(
        conversation_key: &[u8; 32],
        decoded: &[u8],
    ) -> Result<Vec<u8>, Nip44Error> {
        let dlen = decoded.len();
        if !(99..=65603).contains(&dlen) {
            return Err(Nip44Error::InvalidLength);
        }
        let nonce: [u8; 32] = decoded[1..33].try_into()?;
        let ciphertext = &decoded[33..dlen - 32];
        let mac = &decoded[dlen - 32..];

        let keys = Self::get_message_keys(conversation_key, &nonce)?;
        Self::verify_hmac_with_aad(keys.hmac_key.as_slice(), ciphertext, &nonce, mac)?;

        let mut buffer = zeroize::Zeroizing::new(ciphertext.to_vec());
        let mut cipher = chacha20::ChaCha20::new_from_slices(
            keys.chacha_key.as_slice(),
            keys.chacha_nonce.as_slice(),
        )?;
        cipher.apply_keystream(buffer.as_mut_slice());

        // Validate the length prefix exactly as `unpad_v2` does, then return
        // the plaintext bytes (without forcing UTF-8).
        if buffer.len() < 2 {
            return Err(Nip44Error::InvalidLength);
        }
        let unpadded_len = u16::from_be_bytes([buffer[0], buffer[1]]) as usize;
        if unpadded_len == 0
            || 2 + unpadded_len > buffer.len()
            || buffer.len() != 2 + Self::calc_padded_len(unpadded_len)
        {
            return Err(Nip44Error::InvalidPrefixLen);
        }
        let out = buffer[2..2 + unpadded_len].to_vec();
        buffer.zeroize();
        Ok(out)
    }

    /// `calc_padded_len`: NIP-44 power-of-two-chunked padding size for an
    /// unpadded plaintext length (excludes the 2-byte length prefix).
    #[must_use]
    fn calc_padded_len(unpadded_len: usize) -> usize {
        if unpadded_len <= 32 {
            return 32;
        }
        let next_power = 1_usize << ((unpadded_len - 1).ilog2() + 1);
        let chunk = if next_power <= 256 { 32 } else { next_power / 8 };
        chunk * ((unpadded_len - 1) / chunk + 1)
    }

    /// `pad`: `[u16_be(len)][plaintext][zeros]`, total = `2 + calc_padded_len`.
    ///
    /// # Errors
    /// `InvalidLength` if plaintext is empty or longer than 65535 bytes.
    fn pad_v2(plaintext: &[u8]) -> Result<zeroize::Zeroizing<Vec<u8>>, Nip44Error> {
        let len = plaintext.len();
        if !(1..=65535).contains(&len) {
            return Err(Nip44Error::InvalidLength);
        }
        let total = 2 + Self::calc_padded_len(len);
        let mut buf = zeroize::Zeroizing::new(vec![0_u8; total]);
        buf[..2].copy_from_slice(&u16::try_from(len)?.to_be_bytes());
        buf[2..2 + len].copy_from_slice(plaintext);
        Ok(buf)
    }

    /// `unpad`: validate the length prefix and that the total padding matches
    /// what encryption would have produced, then return the plaintext slice.
    ///
    /// # Errors
    /// - `InvalidLength`: padded blob shorter than 2 bytes.
    /// - `InvalidPrefixLen`: zero length, truncation, or padding mismatch.
    /// - `FromUtf8Error`: plaintext not valid UTF-8.
    fn unpad_v2(padded: &[u8]) -> Result<&str, Nip44Error> {
        if padded.len() < 2 {
            return Err(Nip44Error::InvalidLength);
        }
        let unpadded_len = u16::from_be_bytes([padded[0], padded[1]]) as usize;
        if unpadded_len == 0
            || 2 + unpadded_len > padded.len()
            || padded.len() != 2 + Self::calc_padded_len(unpadded_len)
        {
            return Err(Nip44Error::InvalidPrefixLen);
        }
        Ok(std::str::from_utf8(&padded[2..2 + unpadded_len])?)
    }

    /// `hmac_aad`: HMAC-SHA256 over `concat(aad, message)`, where `aad` (the
    /// 32-byte nonce) is prepended to the ciphertext per NIP-44.
    ///
    /// # Errors
    /// `HmacError` if the key is rejected (never for a 32-byte key).
    fn hmac_with_aad(key: &[u8], message: &[u8], aad: &[u8; 32]) -> Result<[u8; 32], Nip44Error> {
        let mut mac =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(key).map_err(|_| Nip44Error::HmacError)?;
        mac.update(aad);
        mac.update(message);
        Ok(mac.finalize().into_bytes().into())
    }

    /// Constant-time verification of an `hmac_aad` tag.
    ///
    /// # Errors
    /// `MacMismatch` if the tag is wrong; `HmacError` on key rejection.
    fn verify_hmac_with_aad(
        key: &[u8],
        message: &[u8],
        aad: &[u8; 32],
        expected: &[u8],
    ) -> Result<(), Nip44Error> {
        let mut mac =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(key).map_err(|_| Nip44Error::HmacError)?;
        mac.update(aad);
        mac.update(message);
        mac.verify_slice(expected).map_err(|_| Nip44Error::MacMismatch)
    }

    #[must_use]
    fn generate_nonce_32() -> zeroize::Zeroizing<[u8; 32]> {
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce).expect("getrandom failed");
        nonce.into()
    }

    // ── Legacy (decrypt-only, frozen) ─────────────────────────────────

    /// Legacy conversation-key derivation as shipped in nostro2-nips <= 0.3.0:
    /// HKDF-extract **then** expand with empty `info`. Not spec-correct, but
    /// reproduced verbatim so legacy payloads still decrypt.
    ///
    /// # Errors
    /// `HkdfError` if expansion fails.
    fn derive_conversation_key_legacy(
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

    /// Decrypts a decoded legacy payload (`decoded[0] == 0x31`): 12-byte nonce,
    /// conversation key used directly as the `ChaCha20` key, no MAC verification.
    ///
    /// # Errors
    /// - `InvalidLength`: payload too short.
    /// - `InvalidPrefixLen`: length prefix exceeds available bytes.
    /// - `FromUtf8Error`: plaintext not valid UTF-8.
    fn decrypt_legacy(
        conversation_key: &[u8; 32],
        decoded: &[u8],
    ) -> Result<String, Nip44Error> {
        if decoded.len() < 1 + 12 + 32 {
            return Err(Nip44Error::InvalidLength);
        }
        let nonce: [u8; 12] = decoded[1..13].try_into()?;
        let ciphertext = &decoded[13..decoded.len() - 32];

        let mut buffer = zeroize::Zeroizing::new(ciphertext.to_vec());
        let mut cipher = chacha20::ChaCha20::new_from_slices(conversation_key, &nonce)?;
        cipher.apply_keystream(buffer.as_mut_slice());

        if buffer.len() < 2 {
            return Err(Nip44Error::InvalidLength);
        }
        let len = u16::from_be_bytes([buffer[0], buffer[1]]) as usize;
        if len > buffer.len() - 2 {
            return Err(Nip44Error::InvalidPrefixLen);
        }
        Ok(std::str::from_utf8(&buffer[2..2 + len])?.to_string())
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

impl<T: nostro2::NostrKeypair + ?Sized> Nip44 for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use nostro2::{NostrKeypair, NostrSigner};

    type Tester = crate::tests::NipTester;

    #[test]
    fn test_encrypt_decrypt_success() {
        let sender = Tester::generate();
        let receiver = Tester::generate();

        let plaintext = "Hello NIP-44 encryption!";
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let ciphertext = sender.nip_44_encrypt(plaintext, &receiver_pk).unwrap();
        // New ciphertext must be spec v2 (version byte 0x02).
        let raw = general_purpose::STANDARD.decode(ciphertext.as_ref()).unwrap();
        assert_eq!(raw[0], VERSION_V2, "expected v2 (0x02) payload");
        let decrypted = receiver.nip_44_decrypt(&ciphertext, &sender_pk).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_invalid_decryption_key() {
        let sender = Tester::generate();
        let receiver = Tester::generate();
        let wrong_receiver = Tester::generate();

        let plaintext = "Hello NIP-44 encryption!";
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let ciphertext = sender.nip_44_encrypt(plaintext, &receiver_pk).unwrap();
        // Wrong key now fails on MAC verification, not garbage UTF-8.
        let result = wrong_receiver.nip_44_decrypt(&ciphertext, &sender_pk);
        assert!(result.is_err());
    }

    // ── Official NIP-44 v2 test vectors (paulmillr/nip44) ──────────────

    /// `valid.get_conversation_key`: sec1 = …01, sec2 = …02.
    #[test]
    fn vector_conversation_key() {
        let sec1 = "0000000000000000000000000000000000000000000000000000000000000001";
        let sec2 = "0000000000000000000000000000000000000000000000000000000000000002";
        let a = Tester::from_hex(sec1).unwrap();
        let b = Tester::from_hex(sec2).unwrap();

        let shared = a.shared_secret(&b.public_key()).unwrap();
        let conv = Tester::conversation_key_v2(shared).unwrap();
        let hex = nostro2_traits::hex::Hexable::to_hex(conv.as_slice());
        assert_eq!(
            hex,
            "c41c775356fd92eadc63ff5a0dc1da211b268cbea22316767095b2871ea1412d"
        );
    }

    /// `valid.encrypt_decrypt`: fixed `conversation_key` + nonce + "a" → payload.
    #[test]
    fn vector_known_answer_payload() {
        use nostro2_traits::hex::FromHex as _;
        let conv: [u8; 32] = "c41c775356fd92eadc63ff5a0dc1da211b268cbea22316767095b2871ea1412d"
            .decode_hex()
            .unwrap()
            .try_into()
            .unwrap();
        let nonce: [u8; 32] = "0000000000000000000000000000000000000000000000000000000000000001"
            .decode_hex()
            .unwrap()
            .try_into()
            .unwrap();
        let payload = Tester::encrypt_v2(&conv, &nonce, b"a").unwrap();
        assert_eq!(
            payload,
            "AgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABee0G5VSK0/9YypIObAtDKfYEAjD35uVkHyB0F4DwrcNaCXlCWZKaArsGrY6M9wnuTMxWfp1RTN9Xga8no+kF5Vsb"
        );
    }

    /// `valid.calc_padded_len` spot checks.
    #[test]
    fn vector_calc_padded_len() {
        let cases = [
            (1_usize, 32_usize),
            (32, 32),
            (33, 64),
            (37, 64),
            (45, 64),
            (49, 64),
            (64, 64),
            (65, 96),
            (100, 128),
            (111, 128),
            (200, 224),
            (250, 256),
            (320, 320),
            (361, 384),
            (512, 512),
            (1000, 1024),
            (1024, 1024),
            (1025, 1280),
            (65535, 65536),
        ];
        for (unpadded, expected) in cases {
            assert_eq!(
                Tester::calc_padded_len(unpadded),
                expected,
                "calc_padded_len({unpadded})"
            );
        }
    }

    /// Tampering with the ciphertext must be caught by the MAC.
    #[test]
    fn v2_mac_rejects_tampering() {
        let sender = Tester::generate();
        let receiver = Tester::generate();
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let ct = sender.nip_44_encrypt("authentic", &receiver_pk).unwrap();
        // Flip a byte in the middle of the base64 payload.
        let mut bytes = general_purpose::STANDARD.decode(ct.as_ref()).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        let tampered = general_purpose::STANDARD.encode(&bytes);
        let result = receiver.nip_44_decrypt(&tampered, &sender_pk);
        assert!(matches!(result, Err(Nip44Error::MacMismatch)));
    }

    /// Legacy (`0x31`) payloads must still decrypt for backward compatibility.
    #[test]
    fn legacy_payload_still_decrypts() {
        // Reproduce a legacy payload exactly as the old code would have:
        // 12-byte nonce, conversation key used directly, MAC over ciphertext.
        let sender = Tester::generate();
        let receiver = Tester::generate();
        let receiver_pk = receiver.public_key();
        let sender_pk = sender.public_key();
        let shared = sender.shared_secret(&receiver_pk).unwrap();
        let conv = Tester::derive_conversation_key_legacy(shared, b"nip44-v2").unwrap();

        let plaintext = b"legacy data at rest";
        // pad like the old next_power_of_two scheme
        let total = (plaintext.len() + 2).next_power_of_two().max(32);
        let mut padded = vec![0_u8; total];
        padded[..2].copy_from_slice(&u16::try_from(plaintext.len()).unwrap().to_be_bytes());
        padded[2..2 + plaintext.len()].copy_from_slice(plaintext);

        let mut nonce = [0_u8; 12];
        getrandom::fill(&mut nonce).unwrap();
        let mut cipher = chacha20::ChaCha20::new_from_slices(conv.as_slice(), &nonce).unwrap();
        cipher.apply_keystream(&mut padded);
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(conv.as_slice()).unwrap();
        mac.update(&padded);
        let tag: [u8; 32] = mac.finalize().into_bytes().into();
        let legacy_payload = Tester::base64_encode_params(b"1", &nonce, &padded, &tag);

        // The receiver decrypts it through the dispatching API.
        let out = receiver
            .nip_44_decrypt(&legacy_payload, &sender_pk)
            .unwrap();
        assert_eq!(out, "legacy data at rest");
    }

    #[test]
    fn unknown_version_is_reported() {
        let sender = Tester::generate();
        let receiver = Tester::generate();
        // version byte 0x09, then enough filler bytes
        let mut raw = vec![0x09_u8];
        raw.extend_from_slice(&[0_u8; 80]);
        let payload = general_purpose::STANDARD.encode(&raw);
        let sender_pk = sender.public_key();
        let result = receiver.nip_44_decrypt(&payload, &sender_pk);
        assert!(matches!(result, Err(Nip44Error::UnknownVersion(0x09))));
    }

    #[test]
    fn hash_flag_is_unsupported() {
        let sender = Tester::generate();
        let receiver = Tester::generate();
        let sender_pk = sender.public_key();
        let result = receiver.nip_44_decrypt("#unsupported", &sender_pk);
        assert!(matches!(result, Err(Nip44Error::UnknownVersion(b'#'))));
    }

    use std::fmt::Write as _;
    #[test]
    fn encrypt_very_large_note() {
        let sender = Tester::generate();
        let receiver = Tester::generate();

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

    fn utf8_err() -> std::str::Utf8Error {
        let bad = [0xff_u8];
        std::str::from_utf8(bad.as_slice()).unwrap_err()
    }

    fn slice_err() -> std::array::TryFromSliceError {
        <[u8; 4]>::try_from([0_u8; 3].as_slice()).unwrap_err()
    }

    fn int_err() -> std::num::TryFromIntError {
        u8::try_from(256_u16).unwrap_err()
    }

    #[test]
    fn error_display_covers_all_variants() {
        let cases: Vec<Nip44Error> = vec![
            Nip44Error::SharedSecretError,
            Nip44Error::FromHexError(nostro2_traits::hex::HexError::OddLength),
            Nip44Error::NostrNoteError(nostro2::errors::NostrErrors::MissingId),
            Nip44Error::InvalidLength,
            Nip44Error::Base64DecodingError(
                base64::engine::general_purpose::STANDARD
                    .decode("!!!")
                    .unwrap_err(),
            ),
            Nip44Error::FromUtf8Error(utf8_err()),
            Nip44Error::HkdfError,
            Nip44Error::HmacError,
            Nip44Error::InvalidPrefixLen,
            Nip44Error::FromArrayError(slice_err()),
            Nip44Error::BufferTooSmall,
            Nip44Error::FromIntError(int_err()),
            Nip44Error::UnknownVersion(0x09),
            Nip44Error::MacMismatch,
        ];
        for err in &cases {
            let msg = format!("{err}");
            assert!(!msg.is_empty(), "Display empty for {err:?}");
        }
    }

    #[test]
    fn error_source_delegates_correctly() {
        use std::error::Error;

        assert!(Nip44Error::SharedSecretError.source().is_none());
        assert!(Nip44Error::InvalidLength.source().is_none());
        assert!(Nip44Error::HkdfError.source().is_none());
        assert!(Nip44Error::HmacError.source().is_none());
        assert!(Nip44Error::InvalidPrefixLen.source().is_none());
        assert!(Nip44Error::BufferTooSmall.source().is_none());
        assert!(Nip44Error::UnknownVersion(0x09).source().is_none());
        assert!(Nip44Error::MacMismatch.source().is_none());

        assert!(
            Nip44Error::FromHexError(nostro2_traits::hex::HexError::OddLength)
                .source()
                .is_some()
        );
        assert!(
            Nip44Error::NostrNoteError(nostro2::errors::NostrErrors::MissingId)
                .source()
                .is_some()
        );
        assert!(Nip44Error::Base64DecodingError(
            base64::engine::general_purpose::STANDARD
                .decode("!!!")
                .unwrap_err()
        )
        .source()
        .is_some());
        assert!(Nip44Error::FromUtf8Error(utf8_err()).source().is_some());
        assert!(Nip44Error::FromArrayError(slice_err()).source().is_some());
        assert!(Nip44Error::FromIntError(int_err()).source().is_some());
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn encrypt_decrypt_round_trip(plaintext in ".{1,256}") {
                let sender = Tester::generate();
                let receiver = Tester::generate();
                let receiver_pk = receiver.public_key();
                let sender_pk = sender.public_key();

                let ciphertext = sender.nip_44_encrypt(&plaintext, &receiver_pk).unwrap();
                let decrypted = receiver.nip_44_decrypt(&ciphertext, &sender_pk).unwrap();
                prop_assert_eq!(&plaintext, decrypted.as_ref());
            }

            #[test]
            fn encrypt_is_non_deterministic(plaintext in ".{1,64}") {
                let sender = Tester::generate();
                let receiver = Tester::generate();
                let receiver_pk = receiver.public_key();

                let a = sender.nip_44_encrypt(&plaintext, &receiver_pk).unwrap();
                let b = sender.nip_44_encrypt(&plaintext, &receiver_pk).unwrap();
                prop_assert_ne!(a, b, "same plaintext must produce different ciphertexts");
            }

            #[test]
            fn padded_len_is_valid(unpadded in 1_usize..65535) {
                let padded = Tester::calc_padded_len(unpadded);
                prop_assert!(padded >= 32);
                prop_assert!(padded >= unpadded);
                // padded sizes are multiples of 32
                prop_assert_eq!(padded % 32, 0);
            }
        }
    }
}
