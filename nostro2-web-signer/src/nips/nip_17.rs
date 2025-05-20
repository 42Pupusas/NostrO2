#[derive(Debug)]
pub enum Nip17Error {
    SigningError(String),
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
    fn private_dm(&self, dm: &str, recipient: &str) -> Result<nostro2::note::NostrNote, Nip17Error>
    where
        Self: Sized,
    {
        let mut dm_note = nostro2::note::NostrNote {
            content: dm.to_string(),
            kind: 14,
            ..Default::default()
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
    fn preffered_relays(&self, relays: &[&str]) -> Result<nostro2::note::NostrNote, Nip17Error> {
        let mut note = nostro2::note::NostrNote {
            kind: 10050,
            ..Default::default()
        };
        for relay in relays {
            note.tags
                .add_relay_tag(Box::leak((*relay).to_string().into_boxed_str()));
        }
        self.sign_nostr_note(&mut note)
            .map_err(|_| Nip17Error::SigningError("Failed to sign NostrNote".to_string()))?;
        Ok(note)
    }
}

#[cfg(test)]
mod tests {
    use nostro2::NostrSigner;

    use super::*;
    use crate::{nip_59::Nip59, tests::NipTester};
    #[test]
    fn test_nip_17() {
        let keys = NipTester::generate(false);
        let recipient = NipTester::generate(false);
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
        let keys = NipTester::generate(false);
        let relays = vec!["wss://relay1.com", "wss://relay2.com"];
        let note = keys.preffered_relays(&relays).unwrap();
        assert!(note.verify());
        assert_eq!(note.kind, 10050);
        let tags = note.tags.find_tags(&nostro2::tags::NostrTag::Relay);
        assert_eq!(tags.len(), relays.len());
        for relay in relays {
            assert!(tags.contains(&relay.to_string()));
        }
    }
}
