#[derive(Debug)]
pub enum Nip59Error {
    MissingPubkey, MissingId, MissingSig,
    Nip44Error(crate::nip_44::Nip44Error), SerializationError(bourne::Error),
    ParseError(String), SigningError,
}
impl std::fmt::Display for Nip59Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Nip59Error: {self:?}") }
}
impl std::error::Error for Nip59Error {}
impl From<crate::nip_44::Nip44Error> for Nip59Error { fn from(err: crate::nip_44::Nip44Error) -> Self { Self::Nip44Error(err) } }

pub trait Nip59: crate::nip_44::Nip44 + nostro2::NostrSigner {
    fn rumor(&self, giftwrap: &nostro2::NostrNote) -> Result<nostro2::NostrNote, Nip59Error> {
        use nostro2::NostrEvent;
        if !giftwrap.verify() { return Err(Nip59Error::ParseError("Giftwrap signature verification failed".into())); }
        let seal_note = self.nip_44_decrypt(&giftwrap.content, &giftwrap.pubkey)?.parse::<nostro2::NostrNote>().map_err(|_| Nip59Error::ParseError("Failed to parse NostrNote from giftwrap".into()))?;
        if !seal_note.verify() { return Err(Nip59Error::ParseError("Seal note signature verification failed".into())); }
        let rumor_note: nostro2::NostrNote = self.nip_44_decrypt(&seal_note.content.to_string(), &seal_note.pubkey)?.parse().map_err(|_| Nip59Error::ParseError("Failed to parse NostrNote from seal".into()))?;
        if seal_note.pubkey != rumor_note.pubkey { return Err(Nip59Error::ParseError("Seal note pubkey does not match rumor note pubkey".into())); }
        Ok(rumor_note)
    }
    fn seal(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error> {
        use nostro2::NostrEvent;
        rumor.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        if !rumor.verify() { return Err(Nip59Error::SigningError); }
        rumor.sig.take();
        let mut seal = nostro2::NostrNote { content: bourne::to_string(rumor).map_err(Nip59Error::SerializationError)?, kind: 13, ..Default::default() };
        self.nip44_encrypt_note(&mut seal, peer_pubkey)?;
        seal.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        if !seal.verify() { return Err(Nip59Error::SigningError); }
        Ok(seal)
    }
    fn giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error>
    where Self: Sized {
        let tk = Self::generate();
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 1059, pubkey: tk.public_key(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None);
        tk.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(&tk).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
    fn replaceable_giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error> {
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 10059, pubkey: self.public_key(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None);
        self.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(self).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
    fn ephemeral_giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str) -> Result<nostro2::NostrNote, Nip59Error>
    where Self: Sized {
        let tk = Self::generate();
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 20059, pubkey: tk.public_key(), ..Default::default() };
        gw.tags.add_pubkey_tag(peer_pubkey, None);
        tk.nip44_encrypt_note(&mut gw, peer_pubkey).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        gw.sign_with(&tk).map_err(|_| Nip59Error::ParseError("Failed to sign NostrNote".into()))?;
        Ok(gw)
    }
    fn parameterized_giftwrap(&self, rumor: &mut nostro2::NostrNote, peer_pubkey: &str, d_tag: &str) -> Result<nostro2::NostrNote, Nip59Error> {
        let sealed = self.seal(rumor, peer_pubkey)?;
        let mut gw = nostro2::NostrNote { content: bourne::to_string(&sealed).map_err(Nip59Error::SerializationError)?, kind: 30059, pubkey: self.public_key(), ..Default::default() };
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
}
