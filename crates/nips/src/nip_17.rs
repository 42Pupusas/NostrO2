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
    fn from(err: crate::nip_44::Nip44Error) -> Self { Self::Nip44Error(err) }
}
impl From<crate::nip_59::Nip59Error> for Nip17Error {
    fn from(err: crate::nip_59::Nip59Error) -> Self { Self::Nip59Error(err) }
}

pub trait Nip17: crate::nip_59::Nip59 {
    /// Wraps a direct message (kind 14) in a NIP-59 giftwrap for the recipient.
    ///
    /// # Errors
    ///
    /// - `Nip59Error` if giftwrapping fails.
    fn private_dm(&self, dm: &str, recipient: &str) -> Result<nostro2::NostrNote, Nip17Error>
    where Self: Sized {
        let mut dm_note = nostro2::NostrNote { content: dm.to_string(), kind: 14, ..Default::default() };
        Ok(self.giftwrap(&mut dm_note, recipient)?)
    }
    /// Creates a signed kind-10050 note listing preferred relays.
    ///
    /// # Errors
    ///
    /// - `SigningError` if the note cannot be signed.
    fn preffered_relays(&self, relays: &[&str]) -> Result<nostro2::NostrNote, Nip17Error> {
        let mut note = nostro2::NostrNote { kind: 10050, ..Default::default() };
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
    use nostro2::{NostrEvent, NostrKeypair, NostrSigner};
    use super::*;
    use crate::{nip_59::Nip59, tests::NipTester};
    #[test] fn test_nip_17() {
        let keys = NipTester::generate(); let recipient = NipTester::generate();
        let dm = "Hello, world!";
        let sealed_dm = keys.private_dm(dm, &recipient.public_key()).unwrap();
        assert_eq!(sealed_dm.kind, 1059);
        assert_ne!(sealed_dm.content, dm);
        let received_dm = recipient.rumor(&sealed_dm).unwrap();
        assert_eq!(received_dm.content, dm);
        assert_eq!(received_dm.kind, 14);
    }
    #[test] fn test_nip_17_preffered_relays() {
        let keys = NipTester::generate();
        let relays = vec!["wss://relay1.com", "wss://relay2.com"];
        let note = keys.preffered_relays(&relays).unwrap();
        assert!(note.verify());
        assert_eq!(note.kind, 10050);
        let tags = note.tags.find_tags("relay");
        assert_eq!(tags.len(), relays.len());
        assert!(tags.contains(&"wss://relay1.com".to_string()));
        assert!(tags.contains(&"wss://relay2.com".to_string()));
    }
    #[test] fn error_display_covers_all_variants() {
        for err in &[Nip17Error::SigningError(nostro2::errors::NostrErrors::MissingId), Nip17Error::Nip44Error(crate::Nip44Error::SharedSecretError), Nip17Error::ParseError("bad input".into()), Nip17Error::Nip59Error(crate::Nip59Error::SigningError)] {
            assert!(!format!("{err}").is_empty());
        }
    }
}
