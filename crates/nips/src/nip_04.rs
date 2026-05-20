use base64::{engine::general_purpose, Engine as _};
use zeroize::Zeroize;

#[derive(Debug, thiserror::Error)]
pub enum Nip04Error {
    #[error("Invalid length")]
    InvalidLength,
    #[error("Shared secret error")]
    FromHexError(#[from] hex::FromHexError),
    #[error("Shared secret error")]
    SharedSecretError,
    #[error("Base64 decoding error {0}")]
    Base64DecodingError(#[from] base64::DecodeError),
    #[error("UTF-8 conversion error {0}")]
    Utf8Error(#[from] std::string::FromUtf8Error),
    #[error("Missing ciphertext")]
    MissingCiphertext,
    #[error("Missing IV")]
    MissingIv,
    #[error("Malformed IV")]
    MalformedIv,
    #[error("Conversion error {0}")]
    ConversionError(#[from] std::convert::Infallible),
}
pub trait Nip04: nostro2::NostrKeypair {
    /// Generates a shared secret using the private keypair and the public key of the peer
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot be decoded or if the shared secret cannot be
    /// generated.
    fn shared_secret(&self, pubkey: &str) -> Result<zeroize::Zeroizing<[u8; 32]>, Nip04Error> {
        Ok(nostro2::NostrKeypair::shared_point(self, pubkey)
            .map_err(|_| Nip04Error::SharedSecretError)?
            .into())
    }

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
        let mut iv = [0_u8; 16];
        getrandom::fill(&mut iv).expect("getrandom failed");
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
        note: &mut nostro2::NostrNote,
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
        note: &'a nostro2::NostrNote,
        peer_pubkey: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>, Nip04Error> {
        self.nip04_decrypt(&note.content, peer_pubkey)
    }
}

impl<T: nostro2::NostrKeypair + ?Sized> Nip04 for T {}

#[cfg(test)]
mod tests {
    use crate::tests::NipTester;
    use nostro2::NostrSigner;

    use super::*;
    const CLEAR_TEXT: &str = "{\"id\":\"2fm12v\",\"method\":\"connect\",\"params\":[\"62dfdb53ea2282ef478f7cdbf77938ec1add74b2bcbc8d862cfe1df24ac72cba\",\"\",\"sign_event:1985,sign_event:3,sign_event:30000\"]}";
    #[test]
    fn nip_04() {
        let nip04 = NipTester::_peer_one();
        let nip04_peer = NipTester::_peer_two();
        let pubkey = nip04_peer.public_key();
        let peer_pubkey = nip04.public_key();
        let ciphertext = nip04.nip04_encrypt(CLEAR_TEXT, &pubkey).expect("");
        let decrypted = nip04_peer
            .nip04_decrypt(&ciphertext, &peer_pubkey)
            .expect("Decryption failed");
        assert_eq!(decrypted, CLEAR_TEXT);
    }
    #[test]
    fn decrypt_with_wrong_key_fails() {
        let peer1 = NipTester::_peer_one();
        let peer2 = NipTester::_peer_two();
        let peer3 = NipTester::_peer_three();
        let pubkey = peer2.public_key();
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
        let pubkey = peer2.public_key();
        let pubkey2 = peer1.public_key();
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
        let pubkey = peer2.public_key();
        let pubkey2 = peer1.public_key();
        for &text in &texts {
            let ciphertext = peer1.nip04_encrypt(text, &pubkey).unwrap();
            let decrypted = peer2.nip04_decrypt(&ciphertext, &pubkey2).unwrap();
            assert_eq!(decrypted, text);
        }
    }
}
