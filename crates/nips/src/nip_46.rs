bourne::json! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Nip46Method {
        #[bourne(rename = "connect")] Connect,
        #[bourne(rename = "sign_event")] SignEvent,
        #[bourne(rename = "ping")] Ping,
        #[bourne(rename = "get_public_key")] GetPublicKey,
        #[bourne(rename = "nip44_encrypt")] Nip44Encrypt,
        #[bourne(rename = "nip44_decrypt")] Nip44Decrypt,
    }
}

bourne::json! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Nip46Request { id: String, method: Nip46Method, params: Vec<String> }
}
impl std::fmt::Display for Nip46Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", bourne::to_string(self).unwrap_or_default())
    }
}
impl Nip46Request {
    fn fresh_id() -> String { let mut b = [0_u8; 1]; getrandom::fill(&mut b).expect("getrandom failed"); b[0].to_string() }
}
impl std::str::FromStr for Nip46Request {
    type Err = bourne::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> { bourne::parse_str(s) }
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
        write!(f, "{}", bourne::to_string(self).unwrap_or_default())
    }
}
impl std::str::FromStr for Nip46Response {
    type Err = bourne::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> { bourne::parse_str(s) }
}

#[derive(Debug)]
pub enum Nip46Error { NostrNoteError(nostro2::errors::NostrErrors), Nip44Error(crate::nip_44::Nip44Error) }
impl std::fmt::Display for Nip46Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::NostrNoteError(e) => write!(f, "{e}"), Self::Nip44Error(e) => write!(f, "failed to encrypt message: {e}") }
    }
}
impl std::error::Error for Nip46Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self { Self::NostrNoteError(e) => Some(e), Self::Nip44Error(e) => Some(e) }
    }
}
impl From<nostro2::errors::NostrErrors> for Nip46Error { fn from(e: nostro2::errors::NostrErrors) -> Self { Self::NostrNoteError(e) } }
impl From<crate::nip_44::Nip44Error> for Nip46Error { fn from(e: crate::nip_44::Nip44Error) -> Self { Self::Nip44Error(e) } }

pub trait Nip46: nostro2::NostrSigner + crate::Nip44 {
    /// Creates a NIP-46 request note (kind 24133) encrypted for the signer.
    ///
    /// # Errors
    ///
    /// - `Nip44Error` if NIP-44 encryption of the request fails.
    /// - `NostrNoteError` if signing fails.
    fn nip46_request(&self, method: Nip46Method, params: Vec<String>, signer_pk: &str) -> Result<nostro2::NostrNote, Nip46Error> {
        let mut note = nostro2::NostrNote { kind: 24133, content: self.nip_44_encrypt(&Nip46Request { id: Nip46Request::fresh_id(), method, params }.to_string(), signer_pk)?.to_string(), pubkey: self.public_key(), ..Default::default() };
        note.tags.add_pubkey_tag(signer_pk, None);
        note.sign_with(self)?;
        Ok(note)
    }
    /// Creates a NIP-46 response note (kind 24133) encrypted for the signer.
    ///
    /// # Errors
    ///
    /// - `Nip44Error` if NIP-44 encryption of the response fails.
    /// - `NostrNoteError` if signing fails.
    fn nip46_response(&self, request_id: &str, result: String, error: Option<String>, signer_pk: &str) -> Result<nostro2::NostrNote, Nip46Error> {
        let response = Nip46Response { id: request_id.to_string(), result, error };
        let mut note = nostro2::NostrNote { kind: 24133, content: self.nip_44_encrypt(&response.to_string(), signer_pk)?.to_string(), pubkey: self.public_key(), ..Default::default() };
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
    #[test] fn nip46_request() {
        let a = NipTester::generate(); let b = NipTester::generate();
        let req = a.nip46_request(Nip46Method::Connect, vec!["test".into()], &b.public_key()).unwrap();
        assert_eq!(req.kind, 24133);
        let pk_a = a.public_key();
        let content = b.nip44_decrypt_note(&req, &pk_a).unwrap();
        let r: Nip46Request = content.parse().unwrap();
        assert_eq!(r.method, Nip46Method::Connect);
        assert_eq!(r.params, vec!["test"]);
    }
    #[test] fn nip46_response() {
        let a = NipTester::generate(); let b = NipTester::generate();
        let req = a.nip46_request(Nip46Method::Connect, vec!["test".into()], &b.public_key()).unwrap();
        let pk_a = a.public_key();
        let content = b.nip44_decrypt_note(&req, &pk_a).unwrap();
        let r: Nip46Request = content.parse().unwrap();
        let resp = a.nip46_response(&r.id, "test".into(), None, &req.pubkey).unwrap();
        assert_eq!(resp.kind, 24133);
        let c = a.nip44_decrypt_note(&resp, &req.pubkey).unwrap();
        let r2: Nip46Response = c.parse().unwrap();
        assert_eq!(r2.id, r.id);
    }
    #[test] fn error_display_and_source() {
        use std::error::Error;
        let e = Nip46Error::NostrNoteError(nostro2::errors::NostrErrors::MissingId);
        assert!(!format!("{e}").is_empty()); assert!(e.source().is_some());
    }
}
