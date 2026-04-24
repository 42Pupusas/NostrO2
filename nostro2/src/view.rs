//! Zero-allocation borrowed mirror of [`crate::NostrNote`] for relay-style
//! consumers that only parse and forward events, never mutate them.
//!
//! Use this when you have a JSON frame in a buffer and want to:
//! - verify the event (hash + signature),
//! - match it against subscription filters,
//! - re-emit its JSON to peers,
//!
//! without allocating a `String` per field or a `Vec<String>` per tag. The
//! view borrows directly from the source `&str`, so the frame buffer must
//! outlive every read of the view. In practice that means "parse → verify →
//! fan-out all inside the same reader callback," which is exactly what the
//! `ring-relay-nostr` shard dispatcher does today.
//!
//! For producers — signers, builders, anything that mutates a note — keep
//! using [`crate::NostrNote`]. This module is read-only by design.
//!
//! ## Allocations
//!
//! One parse of a typical 5-tag note produces **2 allocations**: the flat
//! `Vec<&str>` holding every tag cell, and the `Vec<u32>` holding the
//! start-offset of each tag row. Everything else (pubkey, content, id, sig,
//! tag cells themselves) is a slice into the input. Compare against the
//! owned `NostrNote` path, which allocates ~22 times for the same frame.

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;

/// Borrowed view over the tag array of a note.
///
/// Stores every tag cell in one flat vector with row offsets in a second
/// vector — two allocations total, no matter how many tags. Iteration is a
/// simple slice walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsView<'a> {
    /// Every cell of every row, flattened in row-major order.
    cells: Vec<&'a str>,
    /// `offsets[i]` is the start index in `cells` for row `i`;
    /// `offsets.last()` is always `cells.len()` (closing sentinel).
    offsets: Vec<u32>,
}

impl<'a> TagsView<'a> {
    /// Number of tag rows.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        // `offsets` always has `rows + 1` entries (including the sentinel).
        // On a default (no rows) view, `offsets` is empty, so guard.
        self.offsets.len().saturating_sub(1)
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow row `i` as a slice of cells, or `None` if out of range.
    #[must_use]
    #[inline]
    pub fn row(&self, i: usize) -> Option<&[&'a str]> {
        let start = *self.offsets.get(i)? as usize;
        let end = *self.offsets.get(i + 1)? as usize;
        Some(&self.cells[start..end])
    }

    /// Iterate rows as `&[&str]` slices. Walks the offset table directly,
    /// so no per-row bounds recheck.
    pub fn iter(&self) -> impl Iterator<Item = &[&'a str]> {
        self.offsets
            .windows(2)
            .map(|w| &self.cells[w[0] as usize..w[1] as usize])
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for TagsView<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct TagsVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de, 'a> Visitor<'de> for TagsVisitor<'a>
        where
            'de: 'a,
        {
            type Value = TagsView<'a>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Nostr tag array (array of string arrays)")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let cap = seq.size_hint().unwrap_or(4);
                let mut cells: Vec<&'a str> = Vec::with_capacity(cap * 2);
                let mut offsets: Vec<u32> = Vec::with_capacity(cap + 1);
                offsets.push(0);

                // Seed writes the next row's cells directly into `cells`,
                // skipping the per-row `Vec` that a `Deserialize`-based
                // approach would force.
                while seq
                    .next_element_seed(RowSeed { cells: &mut cells })?
                    .is_some()
                {
                    let len = u32::try_from(cells.len())
                        .map_err(|_| de::Error::custom("tag cell count overflow"))?;
                    offsets.push(len);
                }

                Ok(TagsView { cells, offsets })
            }
        }

        d.deserialize_seq(TagsVisitor(std::marker::PhantomData))
    }
}

/// Deserializes one tag row directly into a caller-provided cell buffer.
/// Avoids the transient `Vec` a `Deserialize`-based row type would need.
struct RowSeed<'buf, 'a> {
    cells: &'buf mut Vec<&'a str>,
}

impl<'de: 'a, 'a> DeserializeSeed<'de> for RowSeed<'_, 'a> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        struct RowVisitor<'buf, 'a> {
            cells: &'buf mut Vec<&'a str>,
        }

        impl<'de: 'a, 'a> Visitor<'de> for RowVisitor<'_, 'a> {
            type Value = ();

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a tag row (array of strings)")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                while let Some(cell) = seq.next_element::<&'a str>()? {
                    self.cells.push(cell);
                }
                Ok(())
            }
        }

        d.deserialize_seq(RowVisitor { cells: self.cells })
    }
}

/// Borrowed view over a Nostr note parsed from a JSON frame.
///
/// All string fields are slices into the source buffer. The view itself
/// holds two small `Vec`s for the tag index (see [`TagsView`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NostrNoteView<'a> {
    pub pubkey: &'a str,
    pub created_at: i64,
    pub kind: u32,
    pub tags: TagsView<'a>,
    pub content: &'a str,
    pub id: Option<&'a str>,
    pub sig: Option<&'a str>,
}

impl<'de: 'a, 'a> Deserialize<'de> for NostrNoteView<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Field tokens, matched against map keys. Unknown fields are ignored
        // (forward-compatible with future NIP additions).
        enum Field {
            Pubkey,
            CreatedAt,
            Kind,
            Tags,
            Content,
            Id,
            Sig,
            Ignore,
        }

        impl<'de> Deserialize<'de> for Field {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct FieldVisitor;
                impl Visitor<'_> for FieldVisitor {
                    type Value = Field;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a NostrNote field name")
                    }
                    fn visit_str<E: de::Error>(self, v: &str) -> Result<Field, E> {
                        Ok(match v {
                            "pubkey" => Field::Pubkey,
                            "created_at" => Field::CreatedAt,
                            "kind" => Field::Kind,
                            "tags" => Field::Tags,
                            "content" => Field::Content,
                            "id" => Field::Id,
                            "sig" => Field::Sig,
                            _ => Field::Ignore,
                        })
                    }
                }
                d.deserialize_str(FieldVisitor)
            }
        }

        struct NoteVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de, 'a> Visitor<'de> for NoteVisitor<'a>
        where
            'de: 'a,
        {
            type Value = NostrNoteView<'a>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Nostr note object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pubkey: Option<&'a str> = None;
                let mut created_at: Option<i64> = None;
                let mut kind: Option<u32> = None;
                let mut tags: Option<TagsView<'a>> = None;
                let mut content: Option<&'a str> = None;
                let mut id: Option<&'a str> = None;
                let mut sig: Option<&'a str> = None;

                while let Some(key) = map.next_key::<Field>()? {
                    match key {
                        Field::Pubkey => pubkey = Some(map.next_value::<&'a str>()?),
                        Field::CreatedAt => created_at = Some(map.next_value::<i64>()?),
                        Field::Kind => kind = Some(map.next_value::<u32>()?),
                        Field::Tags => tags = Some(map.next_value::<TagsView<'a>>()?),
                        Field::Content => content = Some(map.next_value::<&'a str>()?),
                        Field::Id => id = map.next_value::<Option<&'a str>>()?,
                        Field::Sig => sig = map.next_value::<Option<&'a str>>()?,
                        Field::Ignore => {
                            let _ = map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }

                Ok(NostrNoteView {
                    pubkey: pubkey.ok_or_else(|| de::Error::missing_field("pubkey"))?,
                    created_at: created_at.ok_or_else(|| de::Error::missing_field("created_at"))?,
                    kind: kind.ok_or_else(|| de::Error::missing_field("kind"))?,
                    tags: tags.unwrap_or_default(),
                    content: content.ok_or_else(|| de::Error::missing_field("content"))?,
                    id,
                    sig,
                })
            }
        }

        const FIELDS: &[&str] = &[
            "pubkey",
            "created_at",
            "kind",
            "tags",
            "content",
            "id",
            "sig",
        ];
        d.deserialize_struct("NostrNote", FIELDS, NoteVisitor(std::marker::PhantomData))
    }
}

impl NostrNoteView<'_> {
    /// SHA-256 of the canonical serialization, computed without allocating
    /// an intermediate string — same scheme as `NostrNote::compute_id_bytes`.
    ///
    /// # Errors
    /// Returns an error if serde fails to serialize (unreachable in practice).
    pub fn compute_id_bytes(&self) -> Result<[u8; 32], crate::errors::NostrErrors> {
        use sha2::Digest as _;

        // Canonical form: [0, pubkey, created_at, kind, tags, content]. We
        // serialize tags as a nested array of arrays by walking the view.
        struct TagsSer<'b, 'a>(&'b TagsView<'a>);
        impl serde::Serialize for TagsSer<'_, '_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut seq = s.serialize_seq(Some(self.0.len()))?;
                for row in self.0.iter() {
                    seq.serialize_element(row)?;
                }
                seq.end()
            }
        }

        let payload = (
            0,
            self.pubkey,
            self.created_at,
            self.kind,
            TagsSer(&self.tags),
            self.content,
        );
        let mut hasher = sha2::Sha256::new();
        serde_json::to_writer(Sha256Writer(&mut hasher), &payload)?;
        Ok(hasher.finalize().into())
    }

    /// Decode the stored `id` hex into raw bytes (no allocation).
    #[must_use]
    pub fn id_bytes(&self) -> Option<[u8; 32]> {
        let id = self.id?;
        let mut out = [0_u8; 32];
        hex::decode_to_slice(id, &mut out).ok()?;
        Some(out)
    }

    /// Decode the stored `sig` hex into raw bytes (no allocation).
    #[must_use]
    pub fn sig_bytes(&self) -> Option<[u8; 64]> {
        let sig = self.sig?;
        let mut out = [0_u8; 64];
        hex::decode_to_slice(sig, &mut out).ok()?;
        Some(out)
    }

    /// Decode the pubkey hex into raw bytes. Returns zeros on malformed input
    /// (mirrors `NostrNote::pubkey_bytes` which is best-effort).
    #[must_use]
    pub fn pubkey_bytes(&self) -> [u8; 32] {
        let mut out = [0_u8; 32];
        let _ = hex::decode_to_slice(self.pubkey, &mut out);
        out
    }

    /// Verify the note's content hash and signature. Returns true iff both pass.
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

    #[cfg(all(feature = "k256", not(feature = "secp256k1")))]
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
        let id = self.id_bytes().ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let vk = VerifyingKey::from_bytes((&self.pubkey_bytes()).into())
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let signature = Signature::try_from(sig.as_slice())
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        Ok(vk.verify_prehash(&id, &signature).is_ok())
    }

    #[cfg(feature = "secp256k1")]
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey, SECP256K1};
        let id = self.id_bytes().ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig_bytes = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let pk = self.pubkey_bytes();
        let xonly = XOnlyPublicKey::from_slice(&pk)
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let sig = Signature::from_slice(&sig_bytes)
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        let msg = Message::from_digest(id);
        Ok(SECP256K1.verify_schnorr(&sig, &msg, &xonly).is_ok())
    }
}

struct Sha256Writer<'a>(&'a mut sha2::Sha256);

impl std::io::Write for Sha256Writer<'_> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        self.0.update(buf);
        Ok(buf.len())
    }
    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
        serde_json::to_string(&note).unwrap()
    }

    #[test]
    fn parses_all_fields() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = serde_json::from_str(&json).unwrap();
        assert_eq!(view.pubkey, "a".repeat(64));
        assert_eq!(view.created_at, 1_700_000_000);
        assert_eq!(view.kind, 1);
        assert_eq!(view.content, "hello view");
        let expected_id = "b".repeat(64);
        assert_eq!(view.id, Some(expected_id.as_str()));
        assert_eq!(view.tags.len(), 3);
    }

    #[test]
    fn tag_rows_preserved() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = serde_json::from_str(&json).unwrap();
        let row0 = view.tags.row(0).unwrap();
        assert_eq!(row0, &["t", "nostr"]);
        let row1 = view.tags.row(1).unwrap();
        assert_eq!(row1[0], "p");
        assert_eq!(row1[1], &"d".repeat(64));
    }

    #[test]
    fn iter_yields_every_row() {
        let json = sample_note_json();
        let view: NostrNoteView<'_> = serde_json::from_str(&json).unwrap();
        let rows: Vec<_> = view.tags.iter().collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], "t");
        assert_eq!(rows[1][0], "p");
        assert_eq!(rows[2][0], "e");
    }

    #[test]
    fn id_computation_matches_owned() {
        // Produce a real signed note via the owned path, then verify the
        // view recomputes the same id bytes.
        let mut note = crate::NostrNote {
            pubkey: "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d"
                .into(),
            created_at: 1_700_000_000,
            kind: 1,
            content: "canonical test".into(),
            ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.serialize_id().unwrap();
        let expected_id = note.id.clone().unwrap();

        let json = serde_json::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = serde_json::from_str(&json).unwrap();
        let computed = view.compute_id_bytes().unwrap();
        assert_eq!(hex::encode(computed), expected_id);
    }
}
