#[derive(Debug)]
pub enum Nip59Error {
    Nip44Error(crate::nip_44::Nip44Error),
    SerializationError(serde_json::Error),
    ParseError(String),
    SigningError,
}
impl std::fmt::Display for Nip59Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Nip59Error: {self:?}")
    }
}
impl std::error::Error for Nip59Error {}
impl From<crate::nip_44::Nip44Error> for Nip59Error {
    fn from(err: crate::nip_44::Nip44Error) -> Self {
        Self::Nip44Error(err)
    }
}

pub trait Nip59: crate::nip_44::Nip44 + nostro2::NostrSigner {
    /// Unwraps a giftwrapped and sealed rumor note.
    ///
    /// Decrypts the giftwrap to reveal a sealed note, then decrypts the sealed note to extract the original rumor.
    ///
    /// # Errors
    ///
    /// Returns `Nip59Error::Nip44Error` if NIP-44 decryption fails.
    /// Returns `Nip59Error::ParseError` if either decrypted note cannot be parsed.
    fn rumor(
        &self,
        giftwrap: &nostro2::NostrNote,
    ) -> Result<nostro2::NostrNote, Nip59Error> {
        if !giftwrap.verify() {
            return Err(Nip59Error::ParseError(
                "Giftwrap signature verification failed".to_string(),
            ));
        }
        let seal_note = self
            .nip_44_decrypt(&giftwrap.content, &giftwrap.pubkey)?
            .parse::<nostro2::NostrNote>()
            .map_err(|_| {
                Nip59Error::ParseError("Failed to parse NostrNote from giftwrap".to_string())
            })?;
        if !seal_note.verify() {
            return Err(Nip59Error::ParseError(
                "Seal note signature verification failed".to_string(),
            ));
        }
        let rumor_note: nostro2::NostrNote = self
            .nip_44_decrypt(&seal_note.content.to_string(), &seal_note.pubkey)?
            .parse()
            .map_err(|_| {
                Nip59Error::ParseError("Failed to parse NostrNote from seal".to_string())
            })?;
        if seal_note.pubkey != rumor_note.pubkey {
            return Err(Nip59Error::ParseError(
                "Seal note pubkey does not match rumor note pubkey".to_string(),
            ));
        }
        Ok(rumor_note)
    }
    /// Encrypts a note's content into a sealed note.
    ///
    /// Clears the signature and encrypts the content using the note's `pubkey`.
    ///
    /// # Errors
    ///
    /// Returns `Nip59Error::Nip44Error` if encryption fails.
    /// Returns `Nip59Error::ParseError` if signing the sealed note fails.
    fn seal(
        &self,
        rumor: &mut nostro2::NostrNote,
        peer_pubkey: &str,
    ) -> Result<nostro2::NostrNote, Nip59Error> {
        self.sign_nostr_note(rumor)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        if !rumor.verify() {
            return Err(Nip59Error::SigningError);
        }
        rumor.sig.take();
        let mut seal = nostro2::NostrNote {
            content: serde_json::to_string(rumor).map_err(Nip59Error::SerializationError)?,
            kind: 13,
            ..Default::default()
        };
        self.nip44_encrypt_note(&mut seal, peer_pubkey)?;
        self.sign_nostr_note(&mut seal)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        if !seal.verify() {
            return Err(Nip59Error::SigningError);
        }
        Ok(seal)
    }
    /// Wraps a sealed note into a persistent giftwrap.
    ///
    /// The giftwrap uses a throwaway keypair and kind `1059`.
    ///
    /// # Errors
    ///
    /// Returns `Nip59Error::Nip44Error` if encryption of the note fails.
    fn giftwrap(
        &self,
        rumor: &mut nostro2::NostrNote,
        peer_pubkey: &str,
    ) -> Result<nostro2::NostrNote, Nip59Error>
    where
        Self: Sized,
    {
        let throwaway_key = Self::generate(false);
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut giftwrap = nostro2::NostrNote {
            content: serde_json::to_string(&sealed).map_err(Nip59Error::SerializationError)?,
            kind: 1059,
            pubkey: throwaway_key.public_key(),
            ..Default::default()
        };
        giftwrap.tags.add_pubkey_tag(peer_pubkey, None);
        throwaway_key
            .nip44_encrypt_note(&mut giftwrap, peer_pubkey)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        throwaway_key
            .sign_nostr_note(&mut giftwrap)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        Ok(giftwrap)
    }
    /// Wraps a sealed note into a replaceable giftwrap.
    ///
    /// The giftwrap uses kind `10059`.
    ///
    /// # Errors
    ///
    /// Returns `Nip59Error::Nip44Error` if encryption of the note fails.
    fn replaceable_giftwrap(
        &self,
        rumor: &mut nostro2::NostrNote,
        peer_pubkey: &str,
    ) -> Result<nostro2::NostrNote, Nip59Error>
    where
        Self: Sized,
    {
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut giftwrap = nostro2::NostrNote {
            content: serde_json::to_string(&sealed).map_err(Nip59Error::SerializationError)?,
            kind: 10059,
            pubkey: self.public_key(),
            ..Default::default()
        };
        giftwrap.tags.add_pubkey_tag(peer_pubkey, None);
        self.nip44_encrypt_note(&mut giftwrap, peer_pubkey)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        self.sign_nostr_note(&mut giftwrap)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        Ok(giftwrap)
    }
    /// Wraps a sealed note into an ephemeral giftwrap.
    ///
    /// The giftwrap uses kind `20059`.
    ///
    /// # Errors
    ///
    /// Returns `Nip59Error::Nip44Error` if encryption of the note fails.
    fn ephemeral_giftwrap(
        &self,
        rumor: &mut nostro2::NostrNote,
        peer_pubkey: &str,
    ) -> Result<nostro2::NostrNote, Nip59Error>
    where
        Self: Sized,
    {
        let throwaway_key = Self::generate(false);
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut giftwrap = nostro2::NostrNote {
            content: serde_json::to_string(&sealed).map_err(Nip59Error::SerializationError)?,
            kind: 20059,
            pubkey: throwaway_key.public_key(),
            ..Default::default()
        };
        giftwrap.tags.add_pubkey_tag(peer_pubkey, None);
        throwaway_key
            .nip44_encrypt_note(&mut giftwrap, peer_pubkey)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        throwaway_key
            .sign_nostr_note(&mut giftwrap)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        Ok(giftwrap)
    }
    /// Wraps a sealed note into a parameterized giftwrap.
    ///
    /// The giftwrap uses kind `30059` and includes a `d` tag.
    ///
    /// # Errors
    ///
    /// Returns `Nip59Error::Nip44Error` if encryption of the note fails.
    fn parameterized_giftwrap(
        &self,
        rumor: &mut nostro2::NostrNote,
        peer_pubkey: &str,
        d_tag: &str,
    ) -> Result<nostro2::NostrNote, Nip59Error>
    where
        Self: Sized,
    {
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut giftwrap = nostro2::NostrNote {
            content: serde_json::to_string(&sealed).map_err(Nip59Error::SerializationError)?,
            kind: 30059,
            pubkey: self.public_key(),
            ..Default::default()
        };
        giftwrap.tags.add_pubkey_tag(peer_pubkey, None);
        giftwrap.tags.add_parameter_tag(d_tag);
        self.nip44_encrypt_note(&mut giftwrap, peer_pubkey)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        self.sign_nostr_note(&mut giftwrap)
            .map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".to_string()))?;
        Ok(giftwrap)
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::NipTester;

    use super::*;
    use nostro2::{NostrNote, NostrSigner};

    fn make_test_note(content: &str) -> NostrNote {
        NostrNote {
            content: content.to_string(),
            kind: 1,
            ..Default::default()
        }
    }

    #[test]
    fn test_seal_and_rumor_roundtrip() {
        let sender = NipTester::generate(false);
        let receiver = NipTester::generate(false);
        let mut original_note = make_test_note("This is a secret rumor");

        let gift = sender
            .giftwrap(&mut original_note, &receiver.public_key())
            .unwrap();

        assert_eq!(gift.kind, 1059);
        assert!(gift.verify());
        let result = receiver.rumor(&gift).unwrap();

        assert_eq!(result.content, original_note.content);
        assert!(result.sig.is_none());
    }
    #[test]
    fn test_parameterized_rumor() {
        let sender = NipTester::generate(false);
        let receiver = NipTester::generate(false);
        let mut original_note = make_test_note("This is a secret rumor");

        let gift = sender
            .parameterized_giftwrap(&mut original_note, &receiver.public_key(), "test-d")
            .unwrap();
        assert_eq!(gift.kind, 30059);
        assert!(gift.verify());

        let result = receiver.rumor(&gift).unwrap();

        assert_eq!(result.content, original_note.content);
        assert!(result.sig.is_none());
    }

    #[test]
    fn test_replaceable_giftwrap_kind() {
        let sender = NipTester::generate(false);
        let receiver = NipTester::generate(false);
        let mut seal = sender
            .seal(&mut make_test_note("replaceable"), &receiver.public_key())
            .unwrap();
        let gift = sender
            .replaceable_giftwrap(&mut seal, &receiver.public_key())
            .unwrap();

        assert_eq!(gift.kind, 10059);
    }

    #[test]
    fn test_ephemeral_giftwrap_kind() {
        let sender = NipTester::generate(false);
        let receiver = NipTester::generate(false);
        let mut seal = sender
            .seal(&mut make_test_note("ephemeral"), &receiver.public_key())
            .unwrap();
        let gift = sender
            .ephemeral_giftwrap(&mut seal, &receiver.public_key())
            .unwrap();

        assert_eq!(gift.kind, 20059);
    }

    #[test]
    fn test_parameterized_giftwrap_tag_and_kind() {
        let sender = NipTester::generate(false);
        let receiver = NipTester::generate(false);
        let mut seal = sender
            .seal(&mut make_test_note("param"), &receiver.public_key())
            .unwrap();
        let gift = sender
            .parameterized_giftwrap(&mut seal, &receiver.public_key(), "test-d")
            .unwrap();

        assert_eq!(gift.kind, 30059);
        assert_eq!(gift.tags.first_parameter(), Some("test-d".to_string()));
    }
}
