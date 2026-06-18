//! Zero-allocation borrowed mirrors of owned Nostr types for relay-style
//! consumers that only parse and forward events, never mutate them.

use std::borrow::Cow;
use std::collections::BTreeMap;

use bourne::{Error as BourneError, ErrorKind as BourneErrorKind, FromJson, Lexer};

use crate::event::NostrEvent;

// ── TagsView ─────────────────────────────────────────────────────

/// Borrowed view over the tag array of a nostr event. Tag cells are
/// `&str` — escape sequences are rejected at parse time (nostr tag
/// values are identifiers, not free-form text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsView<'a> {
    cells: Vec<&'a str>,
    offsets: Vec<u32>,
}

impl<'a> TagsView<'a> {
    #[must_use] #[inline]
    pub const fn len(&self) -> usize { self.offsets.len().saturating_sub(1) }
    #[must_use] #[inline]
    pub const fn is_empty(&self) -> bool { self.len() == 0 }
    #[must_use] #[inline]
    pub fn row(&self, i: usize) -> Option<&[&'a str]> {
        let start = *self.offsets.get(i)? as usize;
        let end = *self.offsets.get(i + 1)? as usize;
        Some(&self.cells[start..end])
    }
    pub fn iter(&self) -> impl Iterator<Item = &[&'a str]> {
        self.offsets.windows(2).map(|w| &self.cells[w[0] as usize..w[1] as usize])
    }

    fn parse_cell(lex: &mut Lexer<'a>) -> Result<&'a str, BourneError> {
        match Cow::from_lex(lex)? {
            Cow::Borrowed(s) => Ok(s),
            Cow::Owned(_) => Err(BourneError::new(BourneErrorKind::UnknownField, lex.position())),
        }
    }

    fn parse_rows(lex: &mut Lexer<'a>) -> Result<(Vec<&'a str>, Vec<u32>), BourneError> {
        let mut cells = Vec::new();
        let mut offsets: Vec<u32> = vec![0];
        if lex.array_start()? { return Ok((cells, offsets)); }
        loop {
            if lex.array_start()? { /* empty row */ }
            else { loop { cells.push(Self::parse_cell(lex)?); if lex.array_continue(b']')? { break; } } }
            offsets.push(u32::try_from(cells.len()).map_err(|_| BourneError::new(BourneErrorKind::NumberOutOfRange, lex.position()))?);
            if lex.array_continue(b']')? { break; }
        }
        Ok((cells, offsets))
    }
}

impl<'input> FromJson<'input> for TagsView<'input> {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        let (cells, offsets) = Self::parse_rows(lex)?;
        Ok(Self { cells, offsets })
    }
}

// ── NostrNoteView ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NostrNoteView<'a> {
    pub pubkey: Cow<'a, str>,
    pub created_at: i64,
    pub kind: u32,
    pub tags: TagsView<'a>,
    pub content: Cow<'a, str>,
    pub id: Option<Cow<'a, str>>,
    pub sig: Option<Cow<'a, str>>,
}

#[derive(Default)]
struct NoteFields<'a> {
    pubkey: Option<Cow<'a, str>>, created_at: Option<i64>, kind: Option<u32>,
    tags: Option<TagsView<'a>>, content: Option<Cow<'a, str>>,
    id: Option<Cow<'a, str>>, sig: Option<Cow<'a, str>>,
}

impl<'a> NoteFields<'a> {
    fn require<T>(opt: Option<T>, lex: &Lexer<'_>) -> Result<T, BourneError> {
        opt.ok_or_else(|| BourneError::new(BourneErrorKind::MissingField, lex.position()))
    }
    #[allow(unknown_lints, crappy)]
    fn parse_field(&mut self, key: &str, lex: &mut Lexer<'a>) -> Result<(), BourneError> {
        match key {
            "pubkey" => self.pubkey = Some(Cow::from_lex(lex)?),
            "created_at" => self.created_at = Some(lex.parse_i64_value()?),
            "kind" => self.kind = Some(u32::try_from(lex.parse_i64_value()?).map_err(|_| BourneError::new(BourneErrorKind::NumberOutOfRange, lex.position()))?),
            "tags" => self.tags = Some(TagsView::from_lex(lex)?),
            "content" => self.content = Some(Cow::from_lex(lex)?),
            "id" => self.id = Option::<Cow<'a, str>>::from_lex(lex)?,
            "sig" => self.sig = Option::<Cow<'a, str>>::from_lex(lex)?,
            _ => lex.skip_value()?,
        }
        Ok(())
    }
}

impl<'input> FromJson<'input> for NostrNoteView<'input> {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        lex.object_start()?;
        let mut f = NoteFields::default();
        let mut maybe_key = lex.object_first_key()?;
        while let Some(key) = maybe_key { f.parse_field(key, lex)?; maybe_key = lex.object_next_key()?; }
        Ok(Self { pubkey: NoteFields::require(f.pubkey, lex)?, created_at: NoteFields::require(f.created_at, lex)?, kind: NoteFields::require(f.kind, lex)?, tags: f.tags.unwrap_or_default(), content: NoteFields::require(f.content, lex)?, id: f.id, sig: f.sig })
    }
}

impl NostrEvent for NostrNoteView<'_> {
    fn pubkey_str(&self) -> Cow<'_, str> { Cow::Borrowed(self.pubkey.as_ref()) }
    fn created_at(&self) -> i64 { self.created_at }
    fn kind(&self) -> u32 { self.kind }
    fn content_str(&self) -> Cow<'_, str> { Cow::Borrowed(self.content.as_ref()) }
    fn id_hex(&self) -> Option<Cow<'_, str>> { self.id.as_deref().map(Cow::Borrowed) }
    fn sig_hex(&self) -> Option<Cow<'_, str>> { self.sig.as_deref().map(Cow::Borrowed) }
    fn write_tags<W: bourne::JsonWrite + ?Sized>(&self, sink: &mut W) -> Result<(), W::Error> {
        sink.write_byte(b'[')?;
        for (i, row) in self.tags.iter().enumerate() {
            if i > 0 { sink.write_byte(b',')?; }
            sink.write_byte(b'[')?;
            for (j, cell) in row.iter().enumerate() {
                if j > 0 { sink.write_byte(b',')?; }
                sink.write_escaped_str(cell)?;
            }
            sink.write_byte(b']')?;
        }
        sink.write_byte(b']')
    }
}

// ── NostrRelayEventView ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NostrRelayEventView<'a> {
    NewNote(Cow<'a, str>, NostrNoteView<'a>),
    SentOk(Cow<'a, str>, bool, Cow<'a, str>),
    EndOfSubscription(Cow<'a, str>),
    ClosedSubscription(Cow<'a, str>),
    Notice(Cow<'a, str>),
    Auth(Cow<'a, str>),
}

impl<'input> FromJson<'input> for NostrRelayEventView<'input> {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        if lex.array_start()? { return Err(BourneError::new(BourneErrorKind::TypeMismatch, lex.position())); }
        let tag = crate::RelayEventTag::from_str_wire(lex.parse_str_value()?)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, lex.position()))?;
        if lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::MissingField, lex.position())); }
        match tag {
            crate::RelayEventTag::Event => {
                let sub = Cow::from_lex(lex)?;
                if lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::MissingField, lex.position())); }
                let note = NostrNoteView::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(Self::NewNote(sub, note))
            }
            crate::RelayEventTag::Ok => {
                let id = Cow::from_lex(lex)?;
                if lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::MissingField, lex.position())); }
                let ok = bool::from_lex(lex)?;
                if lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::MissingField, lex.position())); }
                let msg = Cow::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(Self::SentOk(id, ok, msg))
            }
            crate::RelayEventTag::Eose | crate::RelayEventTag::Closed | crate::RelayEventTag::Notice | crate::RelayEventTag::Auth => {
                let val = Cow::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(match tag {
                    crate::RelayEventTag::Eose => Self::EndOfSubscription(val),
                    crate::RelayEventTag::Closed => Self::ClosedSubscription(val),
                    crate::RelayEventTag::Notice => Self::Notice(val),
                    crate::RelayEventTag::Auth => Self::Auth(val),
                    _ => unreachable!(),
                })
            }
            _ => Err(BourneError::new(BourneErrorKind::UnknownField, lex.position())),
        }
    }
}

impl<'a> NostrRelayEventView<'a> {
    pub fn parse(s: &'a str) -> Result<Self, bourne::Error> { bourne::parse_str(s) }
}

// ── NostrSubscriptionView ────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NostrSubscriptionView<'a> {
    pub authors: Option<Vec<Cow<'a, str>>>,
    pub ids: Option<Vec<Cow<'a, str>>>,
    pub kinds: Option<Vec<u32>>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<u32>,
    pub tags: Option<BTreeMap<String, Vec<Cow<'a, str>>>>,
}

impl<'input> FromJson<'input> for NostrSubscriptionView<'input> {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        lex.object_start()?;
        let mut v = Self::default();
        let mut maybe_key = lex.object_first_key()?;
        while let Some(key) = maybe_key {
            match key {
                "authors" => v.authors = Some(Vec::from_lex(lex)?),
                "ids" => v.ids = Some(Vec::from_lex(lex)?),
                "kinds" => v.kinds = Some(Vec::from_lex(lex)?),
                "since" => v.since = Option::from_lex(lex)?,
                "until" => v.until = Option::from_lex(lex)?,
                "limit" => { v.limit = Some(u32::try_from(lex.parse_i64_value()?).map_err(|_| BourneError::new(BourneErrorKind::NumberOutOfRange, lex.position()))?); }
                _ if key.starts_with('#') => { v.tags.get_or_insert_with(Default::default).insert(key[1..].to_string(), Vec::from_lex(lex)?); }
                _ => { lex.skip_value()?; }
            }
            maybe_key = lex.object_next_key()?;
        }
        Ok(v)
    }
}

// ── NostrClientEventView ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NostrClientEventView<'a> {
    SendNoteEvent(NostrNoteView<'a>),
    Subscribe(Cow<'a, str>, NostrSubscriptionView<'a>),
    CloseSubscription(Cow<'a, str>),
    Auth(NostrNoteView<'a>),
}

impl<'input> FromJson<'input> for NostrClientEventView<'input> {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        if lex.array_start()? { return Err(BourneError::new(BourneErrorKind::TypeMismatch, lex.position())); }
        let tag = crate::RelayEventTag::from_str_wire(lex.parse_str_value()?)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, lex.position()))?;
        if lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::MissingField, lex.position())); }
        match tag {
            crate::RelayEventTag::Event => {
                let note = NostrNoteView::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(Self::SendNoteEvent(note))
            }
            crate::RelayEventTag::Auth => {
                let note = NostrNoteView::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(Self::Auth(note))
            }
            crate::RelayEventTag::Req => {
                let sub_id = Cow::from_lex(lex)?;
                if lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::MissingField, lex.position())); }
                let filter = NostrSubscriptionView::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(Self::Subscribe(sub_id, filter))
            }
            crate::RelayEventTag::Close => {
                let sub_id = Cow::from_lex(lex)?;
                if !lex.array_continue(b']')? { return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position())); }
                Ok(Self::CloseSubscription(sub_id))
            }
            _ => Err(BourneError::new(BourneErrorKind::UnknownField, lex.position())),
        }
    }
}

impl<'a> NostrClientEventView<'a> {
    pub fn parse(s: &'a str) -> Result<Self, bourne::Error> { bourne::parse_str(s) }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_note_json() -> String {
        let mut note = crate::NostrNote {
            pubkey: "a".repeat(64), created_at: 1_700_000_000, kind: 1, content: "hello view".into(),
            id: Some("b".repeat(64)), sig: Some("c".repeat(128)), ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"d".repeat(64), None);
        note.tags.add_event_tag(&"e".repeat(64));
        bourne::to_string(&note).unwrap()
    }

    #[test] fn parses_all_fields() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert_eq!(view.pubkey.as_ref(), "a".repeat(64));
        assert_eq!(view.created_at, 1_700_000_000);
        assert_eq!(view.kind, 1);
        assert_eq!(view.content.as_ref(), "hello view");
        assert_eq!(view.tags.len(), 3);
    }

    #[test] fn tag_rows_preserved() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert_eq!(view.tags.row(0).unwrap(), ["t", "nostr"]);
        assert_eq!(view.tags.row(1).unwrap(), ["p", &"d".repeat(64)]);
    }

    #[test] fn iter_yields_every_row() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        let rows: Vec<_> = view.tags.iter().collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], "t"); assert_eq!(rows[1][0], "p"); assert_eq!(rows[2][0], "e");
    }

    #[test] fn escape_free_fields_are_borrowed() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert!(matches!(view.pubkey, Cow::Borrowed(_)));
        assert!(matches!(view.content, Cow::Borrowed(_)));
        assert!(matches!(view.id, Some(Cow::Borrowed(_))));
        assert_eq!(view.tags.len(), 3);
    }

    #[test] fn escaped_content_falls_back_to_owned() {
        let view: NostrNoteView<'_> = bourne::parse_str(r#"{"pubkey":"a","created_at":1,"kind":1,"tags":[],"content":"hi \"there\""}"#).unwrap();
        assert_eq!(view.content.as_ref(), "hi \"there\"");
        assert!(matches!(view.content, Cow::Owned(_)));
        assert!(matches!(view.pubkey, Cow::Borrowed(_)));
    }

    #[test] fn id_computation_matches_owned() {
        let mut note = crate::NostrNote {
            pubkey: "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d".into(),
            created_at: 1_700_000_000, kind: 1, content: "canonical test".into(), ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.serialize_id().unwrap();
        let json = bourne::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert_eq!(nostro2_traits::hex::Hexable::to_hex(&view.compute_id_bytes()), note.id.unwrap());
    }

    #[test] fn rejects_missing_required_fields() {
        assert!(bourne::parse_str::<NostrNoteView<'_>>(r#"{"created_at":1,"kind":1,"tags":[],"content":"hi"}"#).is_err());
        assert!(bourne::parse_str::<NostrNoteView<'_>>(r#"{"pubkey":"aa","kind":1,"tags":[],"content":"hi"}"#).is_err());
        assert!(bourne::parse_str::<NostrNoteView<'_>>(r#"{"pubkey":"aa","created_at":1,"tags":[],"content":"hi"}"#).is_err());
        assert!(bourne::parse_str::<NostrNoteView<'_>>(r#"{"pubkey":"aa","created_at":1,"kind":1,"tags":[]}"#).is_err());
    }

    #[test] fn skips_unknown_fields() {
        let view: NostrNoteView<'_> = bourne::parse_str(r#"{"pubkey":"aa","created_at":1,"kind":1,"tags":[],"content":"hi","extra":true}"#).unwrap();
        assert_eq!(view.content.as_ref(), "hi");
    }

    #[test] fn kind_rejects_negative() {
        assert!(bourne::parse_str::<NostrNoteView<'_>>(r#"{"pubkey":"aa","created_at":1,"kind":-1,"tags":[],"content":"hi"}"#).is_err());
    }

    #[cfg(feature = "k256")]
    #[test] fn view_verify_signature_round_trips() {
        use nostro2_signer::nostro2_traits::NostrKeypair as _;
        let kp = nostro2_signer::K256Keypair::generate();
        let mut note = crate::NostrNoteBuilder::text_note("view verify test").build();
        note.tags.add_custom_tag("t", "nostr");
        note.sign_with(&kp).expect("sign");
        let json = bourne::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert!(view.verify(), "view of signed note must verify");
    }

    // ── Relay event view tests ───────────────────────────

    fn sample_note_json_str() -> String {
        let mut note = crate::NostrNote {
            pubkey: "a".repeat(64), created_at: 1_700_000_000, kind: 1, content: "relay test".into(),
            id: Some("b".repeat(64)), sig: Some("c".repeat(128)), ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        bourne::to_string(&note).unwrap()
    }

    #[test] fn relay_view_new_note() {
        let wire = format!(r#"["EVENT","sub1",{}]"#, sample_note_json_str());
        let ev = NostrRelayEventView::parse(&wire).unwrap();
        if let NostrRelayEventView::NewNote(sub_id, note) = ev {
            assert_eq!(sub_id, "sub1"); assert_eq!(note.kind(), 1); assert_eq!(note.tags.len(), 1);
        } else { panic!("expected NewNote"); }
    }

    #[test] fn relay_view_sent_ok() {
        let ev = NostrRelayEventView::parse(r#"["OK","eid",true,"duplicate"]"#).unwrap();
        if let NostrRelayEventView::SentOk(id, ok, msg) = ev {
            assert_eq!(id, "eid"); assert!(ok); assert_eq!(msg, "duplicate");
        } else { panic!("expected SentOk"); }
    }

    #[test]
    fn relay_view_two_element() {
        let ev = NostrRelayEventView::parse(r#"["EOSE","sub42"]"#).unwrap();
        assert!(matches!(ev, NostrRelayEventView::EndOfSubscription(id) if id == "sub42"));

        let ev = NostrRelayEventView::parse(r#"["NOTICE","rate limited"]"#).unwrap();
        assert!(matches!(ev, NostrRelayEventView::Notice(m) if m == "rate limited"));

        let ev = NostrRelayEventView::parse(r#"["AUTH","challenge"]"#).unwrap();
        assert!(matches!(ev, NostrRelayEventView::Auth(c) if c == "challenge"));

        let ev = NostrRelayEventView::parse(r#"["CLOSED","sub7"]"#).unwrap();
        assert!(matches!(ev, NostrRelayEventView::ClosedSubscription(id) if id == "sub7"));
    }

    #[test] fn relay_view_rejects() {
        assert!(NostrRelayEventView::parse(r#"["BOGUS","sub"]"#).is_err());
        assert!(NostrRelayEventView::parse("[]").is_err());
        assert!(NostrRelayEventView::parse(r#"["EVENT"]"#).is_err());
        assert!(NostrRelayEventView::parse(r#"["OK","eid",true]"#).is_err());
    }

    #[test] fn relay_view_borrowed() {
        let wire = format!(r#"["EVENT","sub1",{}]"#, sample_note_json_str());
        let ev = NostrRelayEventView::parse(&wire).unwrap();
        if let NostrRelayEventView::NewNote(sub_id, note) = &ev {
            assert!(matches!(sub_id, Cow::Borrowed(_)));
            assert!(matches!(note.pubkey, Cow::Borrowed(_)));
        }
    }
}
