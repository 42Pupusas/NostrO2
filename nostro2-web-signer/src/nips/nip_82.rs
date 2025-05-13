#[derive(Debug)]
pub enum Nip82Error {
    Nip44Error(crate::nip_44::Nip44Error),
    Nostro2Error(nostro2::errors::NostrErrors),
    ParseError(String),
    SigningError,
}
impl std::fmt::Display for Nip82Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Nip82Error: {self:?}")
    }
}
impl std::error::Error for Nip82Error {}
impl From<crate::nip_44::Nip44Error> for Nip82Error {
    fn from(err: crate::nip_44::Nip44Error) -> Self {
        Self::Nip44Error(err)
    }
}
impl From<nostro2::errors::NostrErrors> for Nip82Error {
    fn from(err: nostro2::errors::NostrErrors) -> Self {
        Self::Nostro2Error(err)
    }
}

pub trait Nip82: crate::nip_44::Nip44 + nostro2::NostrSigner + Sized + std::str::FromStr {
    fn encrypted_wrap(
        &self,
        fhir_note: &mut nostro2::note::NostrNote,
        peer_pubkey: &str,
        wrap_id: &str,
    ) -> Result<nostro2::note::NostrNote, Nip82Error> {
        let signing_key = Self::generate(true);
        self.sign_nostr_note(fhir_note)?;
        let mut wrapped = nostro2::note::NostrNote {
            content: signing_key
                .nip_44_encrypt(&fhir_note.to_string(), &signing_key.public_key())?
                .to_string(),
            pubkey: signing_key.public_key(),
            kind: 32225,
            ..Default::default()
        };
        wrapped.tags.add_parameter_tag(wrap_id);
        wrapped.tags.add_pubkey_tag(&self.public_key(), None);
        wrapped.tags.add_pubkey_tag(peer_pubkey, None);
        wrapped.tags.0.push(nostro2::tags::TagList {
            tag_type: nostro2::tags::NostrTag::Custom("key"),
            tags: vec![
                self.public_key(),
                signing_key
                    .nip_44_encrypt(&signing_key.secret_key(), &self.public_key())?
                    .to_string(),
            ],
        });
        wrapped.tags.0.push(nostro2::tags::TagList {
            tag_type: nostro2::tags::NostrTag::Custom("key"),
            tags: vec![
                peer_pubkey.to_string(),
                signing_key
                    .nip_44_encrypt(&signing_key.secret_key(), peer_pubkey)?
                    .to_string(),
            ],
        });
        signing_key.sign_nostr_note(&mut wrapped)?;
        Ok(wrapped)
    }
    fn decrypt_wrap(
        &self,
        fhir_note: &nostro2::note::NostrNote,
    ) -> Result<nostro2::note::NostrNote, Nip82Error> {
        let encrypted_signing_key = fhir_note
            .tags
            .0
            .iter()
            .find_map(|tag_list| {
                (tag_list.tag_type == nostro2::tags::NostrTag::Custom("key")
                    && tag_list.tags.first() == Some(&self.public_key()))
                .then(|| tag_list.tags.get(1))
                .flatten()
            })
            .ok_or_else(|| Nip82Error::ParseError("Failed to get signing key".to_string()))?;
        let decrypted_signing_key =
            self.nip_44_decrypt(encrypted_signing_key.as_str(), &fhir_note.pubkey)?;
        let signing_key: Self = decrypted_signing_key
            .parse()
            .map_err(|_| Nip82Error::ParseError("Failed to parse signing key".to_string()))?;
        let decrypted_note =
            signing_key.nip_44_decrypt(fhir_note.content.as_str(), &fhir_note.pubkey)?;
        let decrypted_wrap = decrypted_note
            .parse::<nostro2::note::NostrNote>()
            .map_err(|_| Nip82Error::ParseError("Failed to parse decrypted note".to_string()))?;
        Ok(decrypted_wrap)
    }
    fn signing_key(&self, fhir_note: &nostro2::note::NostrNote) -> Result<Self, Nip82Error> {
        let encrypted_signing_key = fhir_note
            .tags
            .0
            .iter()
            .find_map(|tag_list| {
                (tag_list.tag_type == nostro2::tags::NostrTag::Custom("key")
                    && tag_list.tags.first() == Some(&self.public_key()))
                .then(|| tag_list.tags.get(1))
                .flatten()
            })
            .ok_or_else(|| Nip82Error::ParseError("Failed to get signing key".to_string()))?;
        let decrypted_signing_key =
            self.nip_44_decrypt(encrypted_signing_key.as_str(), &fhir_note.pubkey)?;
        let signing_key: Self = decrypted_signing_key
            .parse()
            .map_err(|_| Nip82Error::ParseError("Failed to parse signing key".to_string()))?;
        Ok(signing_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::NipTester;
    use nostro2::{note::NostrNote, NostrSigner};

    #[test]
    fn test_nip82() {
        let peer_one = NipTester::_peer_one();
        let peer_two = NipTester::_peer_two();
        let mut note = NostrNote {
            content: "Hello, world!".to_string(),
            ..Default::default()
        };
        let wrapped_note = peer_one
            .encrypted_wrap(&mut note, &peer_two.public_key(), "wrap_id")
            .unwrap();
        let decrypted_note = peer_two.decrypt_wrap(&wrapped_note).unwrap();
        let peer_one_decrypted_note = peer_one.decrypt_wrap(&wrapped_note).unwrap();
        assert_eq!(decrypted_note.content, note.content);
        assert_eq!(peer_one_decrypted_note.content, note.content);
        assert_eq!(decrypted_note.content, peer_one_decrypted_note.content);
    }
}
