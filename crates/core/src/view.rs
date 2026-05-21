//! Zero-allocation borrowed mirror of [`crate::NostrNote`] for relay-style
//! consumers that only parse and forward events, never mutate them.

use bourne::{Error as BourneError, ErrorKind as BourneErrorKind, FromJson, JsonWrite, Lexer};
use nostro2_traits::hex::FromHex as _;
use std::borrow::Cow;

/// Borrowed view over the tag array of a note.
///
/// Custom wire format (`[[String]]`) — must stay hand-written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsView<'a> {
    cells: Vec<Cow<'a, str>>,
    offsets: Vec<u32>,
}

impl<'a> TagsView<'a> {
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    #[inline]
    pub fn row(&self, i: usize) -> Option<&[Cow<'a, str>]> {
        let start = *self.offsets.get(i)? as usize;
        let end = *self.offsets.get(i + 1)? as usize;
        Some(&self.cells[start..end])
    }

    pub fn iter(&self) -> impl Iterator<Item = &[Cow<'a, str>]> {
        self.offsets
            .windows(2)
            .map(|w| &self.cells[w[0] as usize..w[1] as usize])
    }
}

impl<'input> FromJson<'input> for TagsView<'input> {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        let (cells, offsets) = crate::tags::parse_tag_rows(lex)?;
        Ok(Self { cells, offsets })
    }
}

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

fn require<T>(opt: Option<T>, lex: &Lexer<'_>) -> Result<T, BourneError> {
    opt.ok_or_else(|| BourneError::new(BourneErrorKind::MissingField, lex.position()))
}

#[derive(Default)]
struct ViewFields<'a> {
    pubkey: Option<Cow<'a, str>>,
    created_at: Option<i64>,
    kind: Option<u32>,
    tags: Option<TagsView<'a>>,
    content: Option<Cow<'a, str>>,
    id: Option<Cow<'a, str>>,
    sig: Option<Cow<'a, str>>,
}

impl<'a> ViewFields<'a> {
    #[allow(unknown_lints, crappy)]
    fn parse_field(&mut self, key: &str, lex: &mut Lexer<'a>) -> Result<(), BourneError> {
        match key {
            "pubkey" => self.pubkey = Some(<Cow<'a, str>>::from_lex(lex)?),
            "created_at" => self.created_at = Some(lex.parse_i64_value()?),
            "kind" => {
                self.kind = Some(u32::try_from(lex.parse_i64_value()?).map_err(|_| {
                    BourneError::new(BourneErrorKind::NumberOutOfRange, lex.position())
                })?);
            }
            "tags" => self.tags = Some(TagsView::from_lex(lex)?),
            "content" => self.content = Some(<Cow<'a, str>>::from_lex(lex)?),
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
        let mut fields = ViewFields::default();

        let mut maybe_key = lex.object_first_key()?;
        while let Some(key) = maybe_key {
            fields.parse_field(key, lex)?;
            maybe_key = lex.object_next_key()?;
        }

        Ok(Self {
            pubkey: require(fields.pubkey, lex)?,
            created_at: require(fields.created_at, lex)?,
            kind: require(fields.kind, lex)?,
            tags: fields.tags.unwrap_or_default(),
            content: require(fields.content, lex)?,
            id: fields.id,
            sig: fields.sig,
        })
    }
}

impl NostrNoteView<'_> {
    /// SHA-256 of the canonical serialization, computed without allocating
    /// an intermediate string — same scheme as `NostrNote::compute_id_bytes`.
    ///
    /// # Errors
    ///
    /// Infallible in practice — the `Result` wrapper exists for API symmetry
    /// with `NostrNote`.
    pub fn compute_id_bytes(&self) -> Result<[u8; 32], crate::errors::NostrErrors> {
        use sha2::Digest as _;

        let mut hasher = sha2::Sha256::new();
        let mut sink = crate::note::Sha256Sink(&mut hasher);

        let _: Result<(), core::convert::Infallible> = (|| {
            sink.write_byte(b'[')?;
            sink.write_int_i64(0)?;
            sink.write_byte(b',')?;
            sink.write_escaped_str(self.pubkey.as_ref())?;
            sink.write_byte(b',')?;
            sink.write_int_i64(self.created_at)?;
            sink.write_byte(b',')?;
            sink.write_int_u64(u64::from(self.kind))?;
            sink.write_byte(b',')?;
            // Serialize tags as [[...],[...],...]
            sink.write_byte(b'[')?;
            for (i, row) in self.tags.iter().enumerate() {
                if i > 0 {
                    sink.write_byte(b',')?;
                }
                sink.write_byte(b'[')?;
                for (j, cell) in row.iter().enumerate() {
                    if j > 0 {
                        sink.write_byte(b',')?;
                    }
                    sink.write_escaped_str(cell.as_ref())?;
                }
                sink.write_byte(b']')?;
            }
            sink.write_byte(b']')?;
            sink.write_byte(b',')?;
            sink.write_escaped_str(self.content.as_ref())?;
            sink.write_byte(b']')
        })();

        Ok(hasher.finalize().into())
    }

    #[must_use]
    pub fn id_bytes(&self) -> Option<[u8; 32]> {
        let mut out = [0_u8; 32];
        self.id.as_deref()?.decode_hex_to_slice(&mut out).ok()?;
        Some(out)
    }

    #[must_use]
    pub fn sig_bytes(&self) -> Option<[u8; 64]> {
        let mut out = [0_u8; 64];
        self.sig.as_deref()?.decode_hex_to_slice(&mut out).ok()?;
        Some(out)
    }

    #[must_use]
    pub fn pubkey_bytes(&self) -> Option<[u8; 32]> {
        let mut out = [0_u8; 32];
        self.pubkey.decode_hex_to_slice(&mut out).ok()?;
        Some(out)
    }

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[must_use]
    pub fn verify(&self) -> bool {
        let Some(stored) = self.id_bytes() else {
            return false;
        };
        let Ok(computed) = self.compute_id_bytes() else {
            return false;
        };
        if stored != computed {
            return false;
        }
        self.verify_signature().unwrap_or(false)
    }

    #[cfg(feature = "k256")]
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
        let id = self
            .id_bytes()
            .ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let pubkey = self
            .pubkey_bytes()
            .ok_or(crate::errors::NostrErrors::InvalidPublicKey)?;
        let vk = VerifyingKey::from_bytes((&pubkey).into())
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let signature = Signature::try_from(sig.as_slice())
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        Ok(vk.verify_prehash(&id, &signature).is_ok())
    }

    #[cfg(feature = "secp256k1")]
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey, SECP256K1};
        let id = self
            .id_bytes()
            .ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig_bytes = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let pk = self
            .pubkey_bytes()
            .ok_or(crate::errors::NostrErrors::InvalidPublicKey)?;
        let xonly = XOnlyPublicKey::from_slice(&pk)
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let sig = Signature::from_slice(&sig_bytes)
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        let msg = Message::from_digest(id);
        Ok(SECP256K1.verify_schnorr(&sig, &msg, &xonly).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_note_json() -> String {
        let mut note = crate::NostrNote {
            pubkey: "a".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            content: "hello view".into(),
            id: Some("b".repeat(64)),
            sig: Some("c".repeat(128)),
            ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"d".repeat(64), None);
        note.tags.add_event_tag(&"e".repeat(64));
        bourne::to_string(&note).unwrap()
    }

    #[test]
    fn parses_all_fields() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert_eq!(view.pubkey.as_ref(), "a".repeat(64));
        assert_eq!(view.created_at, 1_700_000_000);
        assert_eq!(view.kind, 1);
        assert_eq!(view.content.as_ref(), "hello view");
        let expected_id = "b".repeat(64);
        assert_eq!(view.id.as_deref(), Some(expected_id.as_str()));
        assert_eq!(view.tags.len(), 3);
    }

    #[test]
    fn tag_rows_preserved() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        let row0 = view.tags.row(0).unwrap();
        assert_eq!(row0[0].as_ref(), "t");
        assert_eq!(row0[1].as_ref(), "nostr");
        let row1 = view.tags.row(1).unwrap();
        assert_eq!(row1[0].as_ref(), "p");
        assert_eq!(row1[1].as_ref(), &"d".repeat(64));
    }

    #[test]
    fn iter_yields_every_row() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        let rows: Vec<_> = view.tags.iter().collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], "t");
        assert_eq!(rows[1][0], "p");
        assert_eq!(rows[2][0], "e");
    }

    #[test]
    fn escape_free_fields_are_borrowed() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert!(matches!(view.pubkey, Cow::Borrowed(_)));
        assert!(matches!(view.content, Cow::Borrowed(_)));
        assert!(matches!(view.id, Some(Cow::Borrowed(_))));
        for row in view.tags.iter() {
            for cell in row {
                assert!(
                    matches!(cell, Cow::Borrowed(_)),
                    "expected borrowed tag cell, got {cell:?}"
                );
            }
        }
    }

    #[test]
    fn escaped_content_falls_back_to_owned() {
        let json = r#"{"pubkey":"a","created_at":1,"kind":1,"tags":[],"content":"hi \"there\""}"#;
        let view: NostrNoteView<'_> = bourne::parse_str(json).unwrap();
        assert_eq!(view.content.as_ref(), "hi \"there\"");
        assert!(matches!(view.content, Cow::Owned(_)));
        assert!(matches!(view.pubkey, Cow::Borrowed(_)));
    }

    #[test]
    fn id_computation_matches_owned() {
        let mut note = crate::NostrNote {
            pubkey: "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d".into(),
            created_at: 1_700_000_000,
            kind: 1,
            content: "canonical test".into(),
            ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.serialize_id().unwrap();
        let expected_id = note.id.clone().unwrap();

        let json = bourne::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        let computed = view.compute_id_bytes().unwrap();
        assert_eq!(nostro2_traits::hex::Hexable::to_hex(&computed), expected_id);
    }

    #[test]
    fn rejects_missing_required_fields() {
        let missing_pubkey = r#"{"created_at":1,"kind":1,"tags":[],"content":"hi"}"#;
        assert!(bourne::parse_str::<NostrNoteView<'_>>(missing_pubkey).is_err());

        let missing_created_at = r#"{"pubkey":"aa","kind":1,"tags":[],"content":"hi"}"#;
        assert!(bourne::parse_str::<NostrNoteView<'_>>(missing_created_at).is_err());

        let missing_kind = r#"{"pubkey":"aa","created_at":1,"tags":[],"content":"hi"}"#;
        assert!(bourne::parse_str::<NostrNoteView<'_>>(missing_kind).is_err());

        let missing_content = r#"{"pubkey":"aa","created_at":1,"kind":1,"tags":[]}"#;
        assert!(bourne::parse_str::<NostrNoteView<'_>>(missing_content).is_err());
    }

    #[test]
    fn skips_unknown_fields() {
        let json = r#"{"pubkey":"aa","created_at":1,"kind":1,"tags":[],"content":"hi","extra":true}"#;
        let view: NostrNoteView<'_> = bourne::parse_str(json).unwrap();
        assert_eq!(view.content.as_ref(), "hi");
    }

    #[test]
    fn kind_rejects_negative() {
        let json = r#"{"pubkey":"aa","created_at":1,"kind":-1,"tags":[],"content":"hi"}"#;
        assert!(bourne::parse_str::<NostrNoteView<'_>>(json).is_err());
    }

    #[cfg(feature = "k256")]
    #[test]
    fn view_verify_signature_round_trips() {
        use nostro2_signer::nostro2_traits::NostrKeypair as _;
        let kp = nostro2_signer::K256Keypair::generate();

        let mut note = crate::NostrNote::text_note("view verify test");
        note.tags.add_custom_tag("t", "nostr");
        note.sign_with(&kp).expect("sign");
        assert!(note.verify(), "owned note must verify");

        let json = bourne::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        assert!(view.verify(), "view of signed note must verify");

        let tampered = json.replace("view verify test", "tampered content");
        let bad_view: NostrNoteView<'_> = bourne::parse_str(&tampered).unwrap();
        assert!(!bad_view.verify(), "tampered view must not verify");
    }
}
