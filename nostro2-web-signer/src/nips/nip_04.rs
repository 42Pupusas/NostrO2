use base64::{engine::general_purpose, Engine as _};
use secp256k1::rand::{thread_rng, Rng};
use zeroize::Zeroize;

#[derive(Debug)]
pub enum Nip04Error {
    CustomError(String),
    StandardError(Box<dyn std::error::Error>),
    SharedSecretError(String),
    Base64DecodingError(base64::DecodeError),
    Utf8Error(std::string::FromUtf8Error),
    MissingCiphertext,
    MissingIv,
    MalformedIv,
    ConversionError(std::convert::Infallible),
}
impl std::fmt::Display for Nip04Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CustomError(msg) => write!(f, "Custom error: {msg}"),
            Self::SharedSecretError(msg) => write!(f, "Shared secret error: {msg}"),
            Self::Base64DecodingError(err) => write!(f, "Base64 decoding error: {err}"),
            Self::Utf8Error(err) => write!(f, "UTF-8 error: {err}"),
            Self::MissingCiphertext => write!(f, "Missing ciphertext"),
            Self::MissingIv => write!(f, "Missing IV"),
            Self::MalformedIv => write!(f, "Malformed IV"),
            Self::ConversionError(err) => write!(f, "Conversion error: {err}"),
            Self::StandardError(err) => write!(f, "Standard error: {err}"),
        }
    }
}
impl std::error::Error for Nip04Error {}
impl From<Box<dyn std::error::Error>> for Nip04Error {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        Self::StandardError(err)
    }
}
impl std::convert::From<std::convert::Infallible> for Nip04Error {
    fn from(err: std::convert::Infallible) -> Self {
        Self::ConversionError(err)
    }
}
impl From<base64::DecodeError> for Nip04Error {
    fn from(err: base64::DecodeError) -> Self {
        Self::Base64DecodingError(err)
    }
}
impl From<std::string::FromUtf8Error> for Nip04Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Self::Utf8Error(err)
    }
}
impl From<secp256k1::Error> for Nip04Error {
    fn from(err: secp256k1::Error) -> Self {
        Self::SharedSecretError(err.to_string())
    }
}
impl From<hex::FromHexError> for Nip04Error {
    fn from(err: hex::FromHexError) -> Self {
        Self::SharedSecretError(err.to_string())
    }
}
pub trait Nip04 {
    /// Generates a shared secret using the private keypair and the public key of the peer
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot be decoded or if the shared secret cannot be
    /// generated.
    fn shared_secret(&self, pubkey: &str) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip04Error>;

    /// Encrypts a message using NIP-04
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot create a shared secret with the private keypair,
    /// or if the encryption fails.
    fn nip04_encrypt<'a>(
        &self,
        plaintext: &'a str,
        pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip04Error> {
        let iv = thread_rng().gen::<[u8; 16]>();
        let mut shared_secret = self.shared_secret(pubkey)?;
        let mut cipher = libaes::Cipher::new_256(&shared_secret);
        shared_secret.zeroize();
        cipher.set_auto_padding(true);
        let ciphertext = cipher.cbc_encrypt(&iv, plaintext.as_bytes());
        let base_64_ciphertext = general_purpose::STANDARD.encode(&ciphertext);
        let base_64_iv = general_purpose::STANDARD.encode(iv);
        Ok(format!("{base_64_ciphertext}?iv={base_64_iv}").into())
    }
    /// Decrypts a NIP-04 encrypted message
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot create a shared secret with the private keypair,
    /// or if the ciphertext is not in the correct format,
    /// or if the decryption fails.
    fn nip04_decrypt<'a>(
        &self,
        ciphertext: &'a str,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip04Error> {
        let mut parts = ciphertext.split('?');
        let base_64_ciphertext = parts.next().ok_or(Nip04Error::MissingCiphertext)?;
        let iv_part = parts.next().ok_or(Nip04Error::MissingIv)?;
        let base_64_iv = iv_part.strip_prefix("iv=").ok_or(Nip04Error::MalformedIv)?;
        let ciphertext = general_purpose::STANDARD.decode(base_64_ciphertext.as_bytes())?;
        let iv = general_purpose::STANDARD.decode(base_64_iv.as_bytes())?;
        let mut shared_secret = self.shared_secret(peer_pubkey)?;
        let mut cipher = libaes::Cipher::new_256(&shared_secret);
        shared_secret.zeroize();
        cipher.set_auto_padding(true);
        let plaintext = cipher.cbc_decrypt(&iv, &ciphertext);
        Ok(String::from_utf8(plaintext)?.into())
    }
    /// Encrypts a Nostr note using NIP-04
    ///
    /// Will replace the content of the note with the encrypted content.
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot create a shared secret with the private keypair,
    /// or if the encryption fails.
    fn nip04_encrypt_note(
        &self,
        note: &mut nostro2::note::NostrNote,
        pubkey: &str,
    ) -> Result<(), Nip04Error> {
        note.content = self.nip04_encrypt(&note.content, pubkey)?.into_owned();
        Ok(())
    }
    /// Decrypts a Nostr note using NIP-04
    ///
    /// Will return the decrypted content of the note.
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot create a shared secret with the private keypair,
    /// or if the decryption fails.
    fn nip04_decrypt_note<'a>(
        &self,
        note: &'a nostro2::note::NostrNote,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip04Error> {
        self.nip04_decrypt(&note.content, peer_pubkey)
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::NipTester;

    use super::*;
    const CLEAR_TEXT: &str = "{\"id\":\"2fm12v\",\"method\":\"connect\",\"params\":[\"62dfdb53ea2282ef478f7cdbf77938ec1add74b2bcbc8d862cfe1df24ac72cba\",\"\",\"sign_event:1985,sign_event:3,sign_event:30000\"]}";
    #[test]
    fn nip_04() {
        let nip04 = NipTester::_peer_one();
        let nip04_peer = NipTester::_peer_two();
        let pubkey = nip04_peer.private_key.x_only_public_key().0.to_string();
        let _peer_pubkey = nip04.private_key.x_only_public_key().0.to_string();
        let ciphertext = nip04.nip04_encrypt(CLEAR_TEXT, &pubkey).expect("");
        let decrypted = nip04_peer
            .nip04_decrypt(&ciphertext, &_peer_pubkey)
            .expect("Decryption failed");
        assert_eq!(decrypted, CLEAR_TEXT);
    }
    #[test]
    fn decrypt_with_wrong_key_fails() {
        let peer1 = NipTester::_peer_one();
        let peer2 = NipTester::_peer_two();
        let peer3 = NipTester::_peer_three();
        let pubkey = peer2.private_key.x_only_public_key().0.to_string();
        let ciphertext = peer1.nip04_encrypt(CLEAR_TEXT, &pubkey).unwrap();

        match peer3.nip04_decrypt(&ciphertext, &pubkey) {
            Ok(decrypted) => assert_ne!(decrypted, CLEAR_TEXT),
            Err(_e) => {}
        }
    }

    #[test]
    fn malformed_ciphertext_format() {
        let peer = NipTester::_peer_one();
        let result = peer.nip04_decrypt(
            "not_base64?iv=also_not_base64",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(matches!(
            result.unwrap_err(),
            Nip04Error::Base64DecodingError(_)
        ));
    }

    #[test]
    fn missing_iv() {
        let peer = NipTester::_peer_one();
        let result = peer.nip04_decrypt(
            "c2FtcGxl",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(matches!(result.unwrap_err(), Nip04Error::MissingIv));
    }

    #[test]
    fn empty_plaintext_roundtrip() {
        let peer1 = NipTester::_peer_one();
        let peer2 = NipTester::_peer_two();
        let pubkey = peer2.private_key.x_only_public_key().0.to_string();
        let pubkey2 = peer1.private_key.x_only_public_key().0.to_string();
        let ciphertext = peer1.nip04_encrypt("", &pubkey).unwrap();
        let decrypted = peer2.nip04_decrypt(&ciphertext, &pubkey2).unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn various_plaintexts() {
        let texts = [
            "short",
            "🔥 emoji",
            "中文测试",
            "newline\nincluded",
            &"x".repeat(1000),
        ];
        let peer1 = NipTester::_peer_one();
        let peer2 = NipTester::_peer_two();
        let pubkey = peer2.private_key.x_only_public_key().0.to_string();
        let pubkey2 = peer1.private_key.x_only_public_key().0.to_string();
        for &text in &texts {
            let ciphertext = peer1.nip04_encrypt(text, &pubkey).unwrap();
            let decrypted = peer2.nip04_decrypt(&ciphertext, &pubkey2).unwrap();
            assert_eq!(decrypted, text);
        }
    }
}
