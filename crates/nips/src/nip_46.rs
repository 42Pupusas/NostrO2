fn fresh_request_id() -> String {
    let mut b = [0_u8; 1];
    getrandom::fill(&mut b).expect("getrandom failed");
    b[0].to_string()
}

bourne::json! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Nip46Method {
        #[bourne(rename = "connect")]
        Connect,
        #[bourne(rename = "sign_event")]
        SignEvent,
        #[bourne(rename = "ping")]
        Ping,
        #[bourne(rename = "get_public_key")]
        GetPublicKey,
        #[bourne(rename = "nip44_encrypt")]
        Nip44Encrypt,
        #[bourne(rename = "nip44_decrypt")]
        Nip44Decrypt,
    }
}

bourne::json! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Nip46Request {
        id: String,
        method: Nip46Method,
        params: Vec<String>,
    }
}
impl std::fmt::Display for Nip46Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = bourne::to_string(self).unwrap_or_default();
        write!(f, "{value}")
    }
}
impl std::str::FromStr for Nip46Request {
    type Err = bourne::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        bourne::parse_str(s)
    }
}

bourne::json! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Nip46Response {
        id: String,
        result: String,
        #[bourne(skip_if_none)]
        error: Option<String>,
    }
}
impl std::fmt::Display for Nip46Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = bourne::to_string(self).unwrap_or_default();
        write!(f, "{value}")
    }
}
impl std::str::FromStr for Nip46Response {
    type Err = bourne::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        bourne::parse_str(s)
    }
}

#[derive(Debug)]
pub enum Nip46Error {
    NostrNoteError(nostro2::errors::NostrErrors),
    Nip44Error(crate::nip_44::Nip44Error),
}

impl std::fmt::Display for Nip46Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NostrNoteError(e) => write!(f, "{e}"),
            Self::Nip44Error(e) => write!(f, "failed to encrypt message: {e}"),
        }
    }
}

impl std::error::Error for Nip46Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NostrNoteError(e) => Some(e),
            Self::Nip44Error(e) => Some(e),
        }
    }
}

impl From<nostro2::errors::NostrErrors> for Nip46Error {
    fn from(e: nostro2::errors::NostrErrors) -> Self { Self::NostrNoteError(e) }
}
impl From<crate::nip_44::Nip44Error> for Nip46Error {
    fn from(e: crate::nip_44::Nip44Error) -> Self { Self::Nip44Error(e) }
}

pub trait Nip46: nostro2::NostrSigner + crate::Nip44 {
    /// Creates a NIP-46 request note.
    ///
    /// # Errors
    ///
    /// Returns an error if the note fails to sign or encrypt.
    fn nip46_request(
        &self,
        method: Nip46Method,
        params: Vec<String>,
        signer_pk: &str,
    ) -> Result<nostro2::NostrNote, Nip46Error> {
        let mut note = nostro2::NostrNote {
            kind: 24133,
            content: self
                .nip_44_encrypt(
                    &Nip46Request {
                        id: fresh_request_id(),
                        method,
                        params,
                    }
                    .to_string(),
                    signer_pk,
                )?
                .to_string(),
            pubkey: self.public_key(),
            ..nostro2::NostrNote::new()
        };
        note.tags.add_pubkey_tag(signer_pk, None);
        note.sign_with(self)?;
        Ok(note)
    }
    /// Creates a NIP-46 response note.
    ///
    /// # Errors
    ///
    /// Returns an error if the note fails to sign or encrypt.
    fn nip46_response(
        &self,
        request_id: &str,
        result: String,
        error: Option<String>,
        signer_pk: &str,
    ) -> Result<nostro2::NostrNote, Nip46Error> {
        let response = Nip46Response {
            id: request_id.to_string(),
            result,
            error,
        };
        let mut note = nostro2::NostrNote {
            kind: 24133,
            content: self
                .nip_44_encrypt(&response.to_string(), signer_pk)?
                .to_string(),
            pubkey: self.public_key(),
            ..nostro2::NostrNote::new()
        };
        note.tags.add_pubkey_tag(signer_pk, None);
        note.sign_with(self)?;
        Ok(note)
    }
}

impl<T: nostro2::NostrSigner + crate::Nip44 + ?Sized> Nip46 for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tests::NipTester, Nip44};
    use nostro2::{NostrKeypair, NostrSigner};

    #[test]
    fn nip46_request() {
        let nip_tester = NipTester::generate();
        let remote_key = NipTester::generate();
        let request = nip_tester.nip46_request(
            Nip46Method::Connect,
            vec!["test".to_string()],
            &remote_key.public_key(),
        );
        assert!(request.is_ok());
        let request = request.unwrap();
        assert_eq!(request.kind, 24133);
        assert_eq!(
            request.tags.first_tagged_pubkey(),
            Some(remote_key.public_key())
        );

        let pubkey = nip_tester.public_key();
        let content = remote_key.nip44_decrypt_note(&request, &pubkey).unwrap();
        let nip46_request: Nip46Request = content.parse().unwrap();
        assert_eq!(nip46_request.method, Nip46Method::Connect);
        assert_eq!(nip46_request.params.len(), 1);
    }
    #[test]
    fn nip46_response() {
        let nip_tester = NipTester::generate();
        let remote_key = NipTester::generate();
        let request = nip_tester.nip46_request(
            Nip46Method::Connect,
            vec!["test".to_string()],
            &remote_key.public_key(),
        );
        assert!(request.is_ok());
        let request = request.unwrap();
        assert_eq!(request.kind, 24133);
        assert_eq!(
            request.tags.first_tagged_pubkey(),
            Some(remote_key.public_key())
        );

        let pubkey = nip_tester.public_key();
        let content = remote_key.nip44_decrypt_note(&request, &pubkey).unwrap();
        let nip46_request: Nip46Request = content.parse().unwrap();
        assert_eq!(nip46_request.method, Nip46Method::Connect);
        assert_eq!(nip46_request.params.len(), 1);

        let response =
            nip_tester.nip46_response(&nip46_request.id, "test".to_string(), None, &request.pubkey);
        assert!(response.is_ok());
        let response = response.unwrap();
        assert_eq!(response.kind, 24133);
        assert_eq!(
            response.tags.first_tagged_pubkey(),
            Some(request.pubkey.clone())
        );
        let content = nip_tester
            .nip44_decrypt_note(&response, &request.pubkey)
            .unwrap();
        let nip46_response: Nip46Response = content.parse().unwrap();
        assert_eq!(nip46_response.id, nip46_request.id);
    }

    #[test]
    fn error_display_and_source() {
        use std::error::Error;

        let note_err = Nip46Error::NostrNoteError(nostro2::errors::NostrErrors::MissingId);
        let msg = format!("{note_err}");
        assert!(!msg.is_empty());
        assert!(note_err.source().is_some());

        let nip44_err = Nip46Error::Nip44Error(crate::Nip44Error::SharedSecretError);
        let msg = format!("{nip44_err}");
        assert!(msg.contains("encrypt"));
        assert!(nip44_err.source().is_some());
    }
}
