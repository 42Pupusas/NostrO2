//! In-memory inverted indexes for a single bucket.
//!
//! Values are slot indices (`u32`) into that bucket's slot table — never
//! pointers to separate entry objects. This is the design call that kills
//! GC: when a slot is overwritten, we synchronously remove the old slot's
//! participation in every index and add the new one. Index size is always
//! bounded by `sum(vecs) == live_slot_count`.
//!
//! Generational CoW is enforced at the *slot* level (storage thread stages
//! an overwrite if `slot.generation >= g_floor`). By the time the index
//! mutation runs here, no live reader can still reach the slot's old
//! contents, so the mutation is safe.
//!
//! ## Cached slot metadata
//!
//! Each `SlotMeta` entry mirrors the fields readers need to evaluate a
//! filter *without* reading the payload: kind, created_at, pubkey prefix,
//! event id prefix, tag list (short vec of `(name, value)` pairs for
//! `#p`/`#e`/`#d`/...). Payload is only fetched from the log when we're
//! about to emit. This keeps the index small and the filter scan cache-hot.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use nostro2::{NostrNoteView, NostrSubscription};

use super::slot::Slot;

/// Per-slot metadata kept in RAM. Mirrors the fields filter matching needs.
#[derive(Debug, Clone)]
pub struct SlotMeta {
    pub seq: NonZeroU64,
    pub generation: u64,
    pub kind: u32,
    pub created_at: i64,
    pub event_id: [u8; 32],
    pub pubkey: [u8; 32],
    /// Indexable tags only: (name, value). We keep `#p`, `#e`, `#d` and any
    /// single-letter tag NIP-01 blesses. Storage trims anything else.
    pub tags: Arc<[(u8, Box<str>)]>,
    pub payload_len: u32,
}

impl SlotMeta {
    /// Check if a NIP-01 filter matches this metadata (payload-free).
    #[must_use]
    pub fn matches(&self, filter: &NostrSubscription) -> bool {
        if let Some(ids) = &filter.ids {
            let id_hex = hex_encode32(&self.event_id);
            if !ids
                .iter()
                .any(|i| id_hex.starts_with(i.as_str()) || *i == id_hex)
            {
                return false;
            }
        }
        if let Some(authors) = &filter.authors {
            let pk_hex = hex_encode32(&self.pubkey);
            if !authors
                .iter()
                .any(|a| pk_hex.starts_with(a.as_str()) || *a == pk_hex)
            {
                return false;
            }
        }
        if let Some(kinds) = &filter.kinds
            && !kinds.contains(&self.kind)
        {
            return false;
        }
        if let Some(since) = filter.since
            && (self.created_at as i128) < (since as i128)
        {
            return false;
        }
        if let Some(until) = filter.until
            && (self.created_at as i128) > (until as i128)
        {
            return false;
        }
        if let Some(tag_filters) = &filter.tags {
            for (key, values) in tag_filters {
                let Some(tag_name) = key.strip_prefix('#') else {
                    continue;
                };
                let Some(byte) = tag_name.as_bytes().first().copied() else {
                    continue;
                };
                let matched = self.tags.iter().any(|(n, v)| {
                    *n == byte && values.iter().any(|candidate| candidate == v.as_ref())
                });
                if !matched {
                    return false;
                }
            }
        }
        true
    }
}

/// Hex-encode 32 bytes into a 64-char lowercase string.
#[must_use]
pub fn hex_encode32(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Extract the indexable single-letter tags from a parsed note view. Used
/// when the storage thread accepts an EVENT; result goes into `SlotMeta`.
#[must_use]
pub fn extract_tags(note: &NostrNoteView<'_>) -> Arc<[(u8, Box<str>)]> {
    let mut out: Vec<(u8, Box<str>)> = Vec::new();
    for row in note.tags.iter() {
        let Some(name) = row.first() else { continue };
        if name.len() != 1 {
            continue;
        }
        let Some(byte) = name.as_bytes().first().copied() else {
            continue;
        };
        if !byte.is_ascii_lowercase() && !byte.is_ascii_uppercase() {
            continue;
        }
        let Some(value) = row.get(1) else { continue };
        out.push((byte, value.to_string().into_boxed_str()));
        if out.len() >= 64 {
            break;
        }
    }
    Arc::from(out.into_boxed_slice())
}

/// Inverted indexes for one bucket. All values are slot indices.
pub struct BucketIndex {
    /// slot_idx → metadata (None = empty). Length equals slot_count.
    pub meta: Vec<Option<SlotMeta>>,
    pub by_author: HashMap<[u8; 32], Vec<u32>>,
    pub by_kind: HashMap<u32, Vec<u32>>,
    /// Single-letter tag → value → slot list. Top-level key is the tag
    /// letter byte (e.g. b'p', b'e', b'd').
    pub by_tag: HashMap<u8, HashMap<Box<str>, Vec<u32>>>,
    /// Event id (first 8 bytes) → slot list. Full match verified at emit.
    pub by_id_prefix: HashMap<[u8; 8], Vec<u32>>,
}

impl BucketIndex {
    #[must_use]
    pub fn new(slot_count: usize) -> Self {
        Self {
            meta: vec![None; slot_count],
            by_author: HashMap::new(),
            by_kind: HashMap::new(),
            by_tag: HashMap::new(),
            by_id_prefix: HashMap::new(),
        }
    }

    /// Remove the old slot's participation in every secondary index.
    pub fn remove_slot(&mut self, slot_idx: u32) {
        let Some(meta) = self.meta[slot_idx as usize].take() else {
            return;
        };
        remove_from_vec(&mut self.by_author, &meta.pubkey, slot_idx);
        remove_from_vec(&mut self.by_kind, &meta.kind, slot_idx);
        for (byte, value) in meta.tags.iter() {
            if let Some(inner) = self.by_tag.get_mut(byte) {
                remove_from_vec(inner, value.as_ref(), slot_idx);
                if inner.is_empty() {
                    self.by_tag.remove(byte);
                }
            }
        }
        let mut id_prefix = [0u8; 8];
        id_prefix.copy_from_slice(&meta.event_id[..8]);
        remove_from_vec(&mut self.by_id_prefix, &id_prefix, slot_idx);
    }

    /// Add a new slot's participation.
    pub fn insert_slot(&mut self, slot_idx: u32, meta: SlotMeta) {
        self.by_author
            .entry(meta.pubkey)
            .or_default()
            .push(slot_idx);
        self.by_kind.entry(meta.kind).or_default().push(slot_idx);
        for (byte, value) in meta.tags.iter() {
            self.by_tag
                .entry(*byte)
                .or_default()
                .entry(value.clone())
                .or_default()
                .push(slot_idx);
        }
        let mut id_prefix = [0u8; 8];
        id_prefix.copy_from_slice(&meta.event_id[..8]);
        self.by_id_prefix
            .entry(id_prefix)
            .or_default()
            .push(slot_idx);
        self.meta[slot_idx as usize] = Some(meta);
    }

    /// Rebuild `SlotMeta` from a decoded on-disk `Slot` plus its payload,
    /// for startup index recovery. The payload is re-parsed into a view so
    /// we can extract tags identically to the fresh-write path.
    pub fn rebuild_from_disk(&mut self, slot_idx: u32, slot: &Slot, tags: Arc<[(u8, Box<str>)]>) {
        let meta = SlotMeta {
            seq: slot.seq,
            generation: slot.generation,
            kind: slot.kind,
            created_at: slot.created_at,
            event_id: slot.event_id,
            pubkey: slot.pubkey,
            tags,
            payload_len: slot.payload_len,
        };
        self.insert_slot(slot_idx, meta);
    }

    /// Pick the cheapest-to-scan index set for a given filter. If `authors`
    /// or `ids` or a tag filter is present, return the candidate slot set
    /// from the most selective one. Otherwise fall back to every live slot.
    pub fn candidates(&self, filter: &NostrSubscription) -> Vec<u32> {
        // Priority: ids > single-author > single-kind > tag > full scan.
        if let Some(ids) = &filter.ids
            && let Some(first) = ids.first()
            && first.len() >= 16
            && let Some(bytes) = hex_to_prefix8(first)
            && let Some(slots) = self.by_id_prefix.get(&bytes)
        {
            return slots.clone();
        }
        if let Some(authors) = &filter.authors
            && authors.len() == 1
            && let Some(first) = authors.first()
            && first.len() == 64
            && let Some(pk) = super::slot::decode_hex32(first)
            && let Some(slots) = self.by_author.get(&pk)
        {
            return slots.clone();
        }
        if let Some(kinds) = &filter.kinds {
            // Union of per-kind lists.
            let mut out = Vec::new();
            for k in kinds {
                if let Some(slots) = self.by_kind.get(k) {
                    out.extend_from_slice(slots);
                }
            }
            return out;
        }
        if let Some(tag_filters) = &filter.tags {
            for (key, values) in tag_filters {
                let Some(tag_name) = key.strip_prefix('#') else {
                    continue;
                };
                let Some(byte) = tag_name.as_bytes().first().copied() else {
                    continue;
                };
                let Some(inner) = self.by_tag.get(&byte) else {
                    continue;
                };
                let mut out = Vec::new();
                for v in values {
                    if let Some(slots) = inner.get(v.as_str()) {
                        out.extend_from_slice(slots);
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
        // Full scan.
        (0..self.meta.len() as u32)
            .filter(|&i| self.meta[i as usize].is_some())
            .collect()
    }
}

fn remove_from_vec<K, Q>(map: &mut HashMap<Q, Vec<u32>>, key: &K, slot_idx: u32)
where
    K: std::hash::Hash + Eq + ?Sized,
    Q: std::borrow::Borrow<K> + std::hash::Hash + Eq,
{
    if let Some(v) = map.get_mut(key) {
        if let Some(pos) = v.iter().position(|&s| s == slot_idx) {
            v.swap_remove(pos);
        }
        if v.is_empty() {
            map.remove(key);
        }
    }
}

fn hex_to_prefix8(s: &str) -> Option<[u8; 8]> {
    if s.len() < 16 {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = hex_digit(s.as_bytes()[i * 2])?;
        let lo = hex_digit(s.as_bytes()[i * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(kind: u32, pk: u8, id: u8) -> SlotMeta {
        SlotMeta {
            seq: NonZeroU64::new(1).unwrap(),
            generation: 1,
            kind,
            created_at: 1,
            event_id: [id; 32],
            pubkey: [pk; 32],
            tags: Arc::from(Vec::new().into_boxed_slice()),
            payload_len: 0,
        }
    }

    #[test]
    fn insert_remove_roundtrip() {
        let mut idx = BucketIndex::new(4);
        idx.insert_slot(0, meta(1, 0xaa, 0x11));
        idx.insert_slot(1, meta(1, 0xbb, 0x22));
        assert_eq!(idx.by_author.get(&[0xaa; 32]).unwrap(), &vec![0u32]);
        assert_eq!(idx.by_kind.get(&1).unwrap().len(), 2);
        idx.remove_slot(0);
        assert!(!idx.by_author.contains_key(&[0xaa; 32]));
        assert_eq!(idx.by_kind.get(&1).unwrap(), &vec![1u32]);
    }

    #[test]
    fn replace_in_place_updates_indexes() {
        let mut idx = BucketIndex::new(4);
        idx.insert_slot(0, meta(1, 0xaa, 0x11));
        idx.remove_slot(0);
        idx.insert_slot(0, meta(2, 0xbb, 0x22));
        assert!(!idx.by_author.contains_key(&[0xaa; 32]));
        assert_eq!(idx.by_author.get(&[0xbb; 32]).unwrap(), &vec![0u32]);
        assert!(!idx.by_kind.contains_key(&1));
        assert_eq!(idx.by_kind.get(&2).unwrap(), &vec![0u32]);
    }

    #[test]
    fn candidates_prefers_id_then_author_then_kind() {
        let mut idx = BucketIndex::new(8);
        idx.insert_slot(0, meta(1, 0xaa, 0x11));
        idx.insert_slot(1, meta(2, 0xaa, 0x22));
        idx.insert_slot(2, meta(3, 0xbb, 0x33));

        let mut filter = NostrSubscription::default();
        filter.authors = Some(vec![hex_encode32(&[0xaa; 32])]);
        let cands = idx.candidates(&filter);
        assert_eq!(cands.len(), 2);
        assert!(cands.contains(&0));
        assert!(cands.contains(&1));

        let mut filter2 = NostrSubscription::default();
        filter2.kinds = Some(vec![3]);
        let cands2 = idx.candidates(&filter2);
        assert_eq!(cands2, vec![2u32]);
    }
}
