#[derive(Debug)]
pub enum Nip17Error {
    SigningError(nostro2::errors::NostrErrors),
    Nip44Error(crate::nip_44::Nip44Error),
    ParseError(String),
    Nip59Error(crate::nip_59::Nip59Error),
}
impl std::fmt::Display for Nip17Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nip44Error(e) => write!(f, "Nip44Error: {e}"),
            Self::ParseError(e) => write!(f, "ParseError: {e}"),
            Self::SigningError(e) => write!(f, "SigningError: {e}"),
            Self::Nip59Error(e) => write!(f, "Nip59Error: {e}"),
        }
    }
}
impl std::error::Error for Nip17Error {}
impl From<crate::nip_44::Nip44Error> for Nip17Error {
    fn from(err: crate::nip_44::Nip44Error) -> Self {
        Self::Nip44Error(err)
    }
}
impl From<crate::nip_59::Nip59Error> for Nip17Error {
    fn from(err: crate::nip_59::Nip59Error) -> Self {
        Self::Nip59Error(err)
    }
}

pub trait Nip17: crate::nip_59::Nip59 {
    /// Creates a sealed and giftwrapped rumor note.
    ///
    /// # Arguments
    ///
    /// * `rumor` - The rumor note to be sealed and giftwrapped.
    /// * `recipient` - The recipient's public key.
    ///
    /// # Errors
    ///
    /// Can fail while sealing or encrypting.
    fn private_dm(&self, dm: &str, recipient: &str) -> Result<nostro2::NostrNote, Nip17Error>
    where
        // Forwards to `Nip59::giftwrap`, which spawns a throwaway keypair via
        // `NostrKeypair::generate` and so requires `Self: Sized`.
        Self: Sized,
    {
        let mut dm_note = nostro2::NostrNote {
            content: dm.to_string(),
            kind: 14,
            ..nostro2::NostrNote::new()
        };
        Ok(self.giftwrap(&mut dm_note, recipient)?)
    }
    /// Public relay inbox list
    ///
    /// Creates a note with the kind 10050 and adds the relays as tags.
    /// These relays are where clients should address their messages.
    ///
    /// # Errors
    ///
    /// Can fail while signing the note.
    fn preffered_relays(&self, relays: &[&str]) -> Result<nostro2::NostrNote, Nip17Error> {
        let mut note = nostro2::NostrNote {
            kind: 10050,
            ..nostro2::NostrNote::new()
        };
        let mut relay_row = Vec::with_capacity(relays.len() + 1);
        relay_row.push("relay".to_string());
        relay_row.extend(relays.iter().map(|r| (*r).to_string()));
        note.tags.add_row(relay_row);
        note.sign_with(self).map_err(Nip17Error::SigningError)?;
        Ok(note)
    }
}

impl<T: crate::nip_59::Nip59 + ?Sized> Nip17 for T {}

#[cfg(test)]
mod tests {
    use nostro2::{NostrKeypair, NostrSigner};

    use super::*;
    use crate::{nip_59::Nip59, tests::NipTester};
    #[test]
    fn test_nip_17() {
        let keys = NipTester::generate();
        let recipient = NipTester::generate();
        let dm = "Hello, world!";
        let sealed_dm = keys.private_dm(dm, &recipient.public_key()).unwrap();
        assert_eq!(sealed_dm.kind, 1059);
        assert_ne!(sealed_dm.content, dm);

        let received_dm = recipient.rumor(&sealed_dm).unwrap();
        assert_eq!(received_dm.content, dm);
        assert_eq!(received_dm.kind, 14);
    }
    #[test]
    fn test_nip_17_preffered_relays() {
        let keys = NipTester::generate();
        let relays = vec!["wss://relay1.com", "wss://relay2.com"];
        let note = keys.preffered_relays(&relays).unwrap();
        assert!(note.verify());
        assert_eq!(note.kind, 10050);
        let tags = note.tags.find_tags("relay");
        assert_eq!(tags.len(), relays.len());
        for relay in relays {
            assert!(tags.contains(&relay.to_string()));
        }
    }
}
