#[derive(Debug)]
pub enum Nip59Error {
    Nip44Error(crate::nip_44::Nip44Error),
    SerializationError(bourne::Error),
    ParseError(String),
    SigningError,
}
impl std::fmt::Display for Nip59Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nip44Error(e) => write!(f, "Nip44 error: {e}"),
            Self::SerializationError(e) => write!(f, "serialization error: {e}"),
            Self::ParseError(e) => write!(f, "parse error: {e}"),
            Self::SigningError => f.write_str("signing error"),
        }
    }
}
impl std::error::Error for Nip59Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nip44Error(e) => Some(e),
            Self::SerializationError(e) => Some(e),
            _ => None,
        }
    }
}
impl From<crate::nip_44::Nip44Error> for Nip59Error { fn from(err: crate::nip_44::Nip44Error) -> Self { Self::Nip44Error(err) } }

pub trait Nip59: crate::nip_44::Nip44 + nostro2::NostrSigner {
    /// Current Unix time minus a random jitter (0..=172800s, i.e. up to 2
    /// days), per NIP-59's recommendation to randomize seal/giftwrap
    /// timestamps so the real send time is obscured. Never returns a future
    /// or zero timestamp.
    #[must_use]
    fn jittered_timestamp() -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0);
        let mut buf = [0_u8; 8];
        let _ = getrandom::fill(&mut buf);
        let jitter = i64::from_le_bytes(buf).rem_euclid(172_800);
        now - jitter
    }

    /// Unwraps a giftwrapped rumor, verifying all layers.
    ///
    /// # Errors
    ///
    /// - `ParseError` if any layer fails signature verification or parsing.
    /// - `Nip44Error` if decryption of the giftwrap or seal fails.
    fn rumor(&self, giftwrap: &nostro2::NostrNote) -> Result<nostro2::NostrNote, Nip59Error> {
        use nostro2::NostrEvent;
        if !giftwrap.verify() { return Err(Nip59Error::ParseError("Giftwrap signature verification failed".into())); }
        let seal_note = self.nip_44_decrypt(&giftwrap.content, &giftwrap.pubkey)?.parse::<nostro2::NostrNote>().map_err(|_| Nip59Error::ParseError("Failed to parse NostrNote from giftwrap".into()))?;
        if !seal_note.verify() { return Err(Nip59Error::ParseError("Seal note signature verification failed".into())); }
        let rumor_note: nostro2::NostrNote = self.nip_44_decrypt(&seal_note.content.clone(), &seal_note.pubkey)?.parse().map_err(|_| Nip59Error::ParseError("Failed to parse NostrNote from seal".into()))?;
        if seal_note.pubkey != rumor_note.pubkey { return Err(Nip59Error::ParseError("Seal note pubkey does not match rumor note pubkey".into())); }
        Ok(rumor_note)
    }
    /// Signs a rumor and encrypts it into a seal note for the peer.
    ///
    /// # Errors
    ///
    /// - `ParseError` if signing fails.
    /// - `SigningError` if the freshly-signed rumor fails verification.
    /// - `SerializationError` if the rumor cannot be serialized.
    /// - `Nip44Error` if NIP-44 encryption fails.
    fn seal(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error> {
        use nostro2::NostrEvent;
        if rumor.created_at == 0 { rumor.created_at = Self::jittered_timestamp(); }
        rumor.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        if !rumor.verify() { return Err(Nip59Error::SigningError); }
        rumor.sig.take();
        let mut seal = nostro2::NostrNote { content: bourne::to_string(rumor).map_err(Nip59Error::SerializationError)?, kind: 13, created_at: Self::jittered_timestamp(), ..Default::default() };
        self.nip44_encrypt_note(&mut seal, peer_pubkey)?;
        seal.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        if !seal.verify() { return Err(Nip59Error::SigningError); }
        Ok(seal)
    }
    /// Seals a rumor then wraps it in an ephemeral giftwrap (kind 1059).
    ///
    /// # Errors
    ///
    /// - `SerializationError` if the sealed note cannot be serialized.
    /// - `ParseError` if NIP-44 encryption or signing of the giftwrap fails.
    /// - `SigningError` propagated from [`seal`](Self::seal).
    fn giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error>
    where Self: Sized {
        let tk = Self::generate();
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 1059, pubkey: tk.public_key(), created_at: Self::jittered_timestamp(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None);
        tk.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(&tk).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
    /// Like [`giftwrap`](Self::giftwrap) but uses a replaceable event (kind 10059)
    /// signed by `self`.
    ///
    /// # Errors
    ///
    /// - `SerializationError` if the sealed note cannot be serialized.
    /// - `ParseError` if NIP-44 encryption or signing of the giftwrap fails.
    fn replaceable_giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error> {
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 10059, pubkey: self.public_key(), created_at: Self::jittered_timestamp(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None);
        self.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
    /// Like [`giftwrap`](Self::giftwrap) but uses an ephemeral event (kind 20059).
    ///
    /// # Errors
    ///
    /// - `SerializationError` if the sealed note cannot be serialized.
    /// - `ParseError` if NIP-44 encryption or signing of the giftwrap fails.
    fn ephemeral_giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error>
    where Self: Sized {
        let tk = Self::generate();
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 20059, pubkey: tk.public_key(), created_at: Self::jittered_timestamp(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None);
        tk.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(&tk).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
    /// Like [`giftwrap`](Self::giftwrap) but uses a parameterized replaceable event
    /// (kind 30059) with the given `d_tag`.
    ///
    /// # Errors
    ///
    /// - `SerializationError` if the sealed note cannot be serialized.
    /// - `ParseError` if NIP-44 encryption or signing of the giftwrap fails.
    fn parameterized_giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str, d_tag: &str) -> Result<nostro2::NostrNote, Nip59Error> {
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 30059, pubkey: self.public_key(), created_at: Self::jittered_timestamp(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None); gw.tags.add_parameter_tag(d_tag);
        self.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
}

impl<T: crate::nip_44::Nip44 + nostro2::NostrSigner + ?Sized> Nip59 for T {}

#[cfg(test)]
mod tests {
    use crate::tests::NipTester;
    use super::*;
    use nostro2::{NostrEvent, NostrKeypair, NostrNote, NostrSigner};
    fn mknote(c: &str) -> NostrNote { NostrNote { content: c.into(), kind: 1, ..Default::default() } }
    #[test] fn test_seal_and_rumor_roundtrip() {
        let sender = NipTester::generate(); let recv = NipTester::generate();
        let mut n = mknote("secret rumor");
        let g = sender.giftwrap(&mut n, &recv.public_key()).unwrap();
        assert_eq!(g.kind, 1059); assert!(g.verify());
        assert_eq!(recv.rumor(&g).unwrap().content, n.content);
    }
    #[test] fn test_parameterized_rumor() {
        let sender = NipTester::generate(); let recv = NipTester::generate();
        let mut n = mknote("secret rumor");
        let g = sender.parameterized_giftwrap(&mut n, &recv.public_key(), "test-d").unwrap();
        assert_eq!(g.kind, 30059); assert!(g.verify());
        assert_eq!(recv.rumor(&g).unwrap().content, n.content);
    }
    #[test] fn test_replaceable_giftwrap_kind() {
        let sender = NipTester::generate(); let recv = NipTester::generate();
        let mut s = sender.seal(&mut mknote("replaceable"), &recv.public_key()).unwrap();
        assert_eq!(sender.replaceable_giftwrap(&mut s, &recv.public_key()).unwrap().kind, 10059);
    }
    #[test] fn test_ephemeral_giftwrap_kind() {
        let sender = NipTester::generate(); let recv = NipTester::generate();
        let mut s = sender.seal(&mut mknote("ephemeral"), &recv.public_key()).unwrap();
        assert_eq!(sender.ephemeral_giftwrap(&mut s, &recv.public_key()).unwrap().kind, 20059);
    }
    #[test] fn test_parameterized_giftwrap_tag_and_kind() {
        let sender = NipTester::generate(); let recv = NipTester::generate();
        let mut s = sender.seal(&mut mknote("param"), &recv.public_key()).unwrap();
        let g = sender.parameterized_giftwrap(&mut s, &recv.public_key(), "test-d").unwrap();
        assert_eq!(g.kind, 30059);
        assert_eq!(g.tags.first_parameter(), Some("test-d".into()));
    }
    #[test] fn error_display_covers_all_variants() {
        for err in &[
            Nip59Error::Nip44Error(crate::Nip44Error::SharedSecretError),
            Nip59Error::SerializationError(bourne::parse_str::<i32>("!!!").unwrap_err()),
            Nip59Error::ParseError("bad input".into()),
            Nip59Error::SigningError,
        ] {
            assert!(!format!("{err}").is_empty());
        }
    }
    #[test] fn error_source_delegates() {
        use std::error::Error;
        assert!(Nip59Error::SigningError.source().is_none());
        assert!(Nip59Error::ParseError("x".into()).source().is_none());
        assert!(Nip59Error::Nip44Error(crate::Nip44Error::SharedSecretError).source().is_some());
        assert!(Nip59Error::SerializationError(bourne::parse_str::<i32>("!!!").unwrap_err()).source().is_some());
    }
}
