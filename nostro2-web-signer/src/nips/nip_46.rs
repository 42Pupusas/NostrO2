use secp256k1::rand::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Nip46Method {
    Connect,
    SignEvent,
    Ping,
    GetPublicKey,
    Nip04Encrypt,
    Nip04Decrypt,
    Nip44Encrypt,
    Nip44Decrypt,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Nip46Request {
    id: String,
    method: Nip46Method,
    params: Vec<String>,
}
impl std::fmt::Display for Nip46Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_string(self).unwrap_or_default();
        write!(f, "{value}")
    }
}
impl std::str::FromStr for Nip46Request {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Nip46Response {
    id: String,
    result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
impl std::fmt::Display for Nip46Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_string(self).unwrap_or_default();
        write!(f, "{value}")
    }
}
impl std::str::FromStr for Nip46Response {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

pub trait Nip46: nostro2::NostrSigner + crate::Nip44 {
    fn nip46_request(
        &self,
        method: Nip46Method,
        params: Vec<String>,
        signer_pk: &str,
    ) -> Result<nostro2::note::NostrNote, nostro2::errors::NostrErrors> {
        let mut note = nostro2::note::NostrNote {
            kind: 24133,
            content: self
                .nip_44_encrypt(
                    &Nip46Request {
                        id: secp256k1::rand::thread_rng()
                            .gen_range(0..=u8::MAX)
                            .to_string(),
                        method,
                        params,
                    }
                    .to_string(),
                    signer_pk,
                )
                .map_err(|e| nostro2::errors::NostrErrors::SignatureError(e.to_string()))?
                .to_string(),
            pubkey: self.public_key(),
            ..Default::default()
        };
        note.tags.add_pubkey_tag(signer_pk, None);
        self.sign_nostr_note(&mut note)?;
        Ok(note)
    }
    fn nip46_response(
        &self,
        request_id: &str,
        result: String,
        error: Option<String>,
        signer_pk: &str,
    ) -> Result<nostro2::note::NostrNote, nostro2::errors::NostrErrors> {
        let response = Nip46Response {
            id: request_id.to_string(),
            result,
            error,
        };
        let mut note = nostro2::note::NostrNote {
            kind: 24133,
            content: self
                .nip_44_encrypt(&response.to_string(), signer_pk)
                .map_err(|e| nostro2::errors::NostrErrors::SignatureError(e.to_string()))?
                .to_string(),
            pubkey: self.public_key(),
            ..Default::default()
        };
        note.tags.add_pubkey_tag(signer_pk, None);
        self.sign_nostr_note(&mut note)?;
        Ok(note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tests::NipTester, Nip44};
    use nostro2::NostrSigner;

    #[test]
    fn nip46_request() {
        let nip_tester = NipTester::generate(false);
        let remote_key = NipTester::generate(false);
        let request = nip_tester.nip46_request(
            Nip46Method::Connect,
            vec!["test".to_string()],
            &remote_key.public_key(),
        );
        assert!(request.is_ok());
        let request = request.unwrap();
        assert_eq!(request.kind, 24133);
        assert_eq!(
            request.tags.find_first_tagged_pubkey(),
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
        let nip_tester = NipTester::generate(false);
        let remote_key = NipTester::generate(false);
        let request = nip_tester.nip46_request(
            Nip46Method::Connect,
            vec!["test".to_string()],
            &remote_key.public_key(),
        );
        assert!(request.is_ok());
        let request = request.unwrap();
        assert_eq!(request.kind, 24133);
        assert_eq!(
            request.tags.find_first_tagged_pubkey(),
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
            response.tags.find_first_tagged_pubkey(),
            Some(request.pubkey.clone())
        );
        let content = nip_tester
            .nip44_decrypt_note(&response, &request.pubkey)
            .unwrap();
        let nip46_response: Nip46Response = content.parse().unwrap();
        assert_eq!(nip46_response.id, nip46_request.id);
    }
}
