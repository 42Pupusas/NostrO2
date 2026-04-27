//! The three bounded buckets.
//!
//! Each owns:
//! - its slot count and `max_payload`
//! - a [`BucketLog`] (on-disk file)
//! - a [`BucketIndex`] (in-memory inverted indexes)
//! - a bucket-specific "next slot to reuse" policy
//!
//! ## Eviction under generational CoW
//!
//! Every write goes through [`Bucket::try_write`]. The caller supplies the
//! target slot index and the storage thread's current `g_floor`. If the
//! slot is currently occupied by an entry whose `generation >= g_floor`,
//! the write is *refused* ([`WriteOutcome::Stalled`]) — the storage thread
//! stages the event until a reader advances. Otherwise it commits:
//! remove the old slot from the indexes, write the new payload to disk,
//! insert the new entry.
//!
//! A "refused" write is not an error. The storage thread re-attempts after
//! waking and re-reading `g_floor`.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;

use super::index::{BucketIndex, IndexedTags, SlotMeta, extract_tags};
use super::log::BucketLog;
use super::slot::{self, Slot};
use nostro2::NostrNoteView;

/// The outcome of a write attempt against a bucket.
#[derive(Debug)]
pub enum WriteOutcome {
    /// Write committed; carries the slot index it landed in and the
    /// freshly-built `SlotMeta` so the engine can broadcast an
    /// `IndexUpdate` to reader threads without re-reading.
    Committed { slot_idx: u32, meta: SlotMeta },
    /// Write refused: the target slot is still visible to an active reader
    /// with `g_req >= slot.generation`. Caller should stage the event and
    /// retry after `g_floor` advances.
    Stalled,
    /// Duplicate event (id already present) — drop silently.
    Duplicate,
    /// Payload exceeds the bucket's `max_payload` cap. Drop and don't retry.
    TooBig,
}

/// A decoded event ready to persist. Borrowed from the shard's inbound
/// frame buffer; the bucket copies what it needs into its own structures
/// before returning.
pub struct EventPayload<'a> {
    pub note: &'a NostrNoteView<'a>,
    pub raw_json: &'a [u8],
    /// Hex-decoded event id.
    pub event_id: [u8; 32],
    /// Hex-decoded pubkey.
    pub pubkey: [u8; 32],
}

/// Common bucket interface. Each bucket decides its own eviction target.
pub trait Bucket {
    fn try_write(
        &mut self,
        event: &EventPayload<'_>,
        generation: u64,
        g_floor: u64,
        next_seq: &mut u64,
    ) -> WriteOutcome;

    fn index(&self) -> &BucketIndex;
    fn log(&self) -> &BucketLog;

    /// Mutable handle on the index for the bootstrap-time handoff in the
    /// reader pool. Storage path itself never calls this — it goes
    /// through `try_write` / `commit_write`. Reader threads use it to
    /// `mem::replace` the freshly-rebuilt index out of a temporary
    /// bucket they're about to discard.
    fn index_mut_for_handoff(&mut self) -> &mut BucketIndex;

    /// Rebuild the in-memory index from the on-disk log. Called once at
    /// startup.
    fn rebuild(&mut self) -> std::io::Result<()>;
}

/// Ephemeral bucket: a true circular log. Oldest slot overwritten on
/// capacity.
pub struct EphemeralBucket {
    log: BucketLog,
    index: BucketIndex,
    /// Next slot index to overwrite. Wraps at `slot_count`.
    write_head: u32,
}

impl EphemeralBucket {
    pub fn open(path: &Path, slot_count: usize, max_payload: usize) -> std::io::Result<Self> {
        let log = BucketLog::open(path, slot_count, max_payload)?;
        let index = BucketIndex::new(slot_count);
        Ok(Self {
            log,
            index,
            write_head: 0,
        })
    }
}

impl Bucket for EphemeralBucket {
    fn try_write(
        &mut self,
        event: &EventPayload<'_>,
        generation: u64,
        g_floor: u64,
        next_seq: &mut u64,
    ) -> WriteOutcome {
        if event.raw_json.len() > self.log.max_payload() {
            return WriteOutcome::TooBig;
        }
        // Dedup: do we already have this event id? Cheap: look up prefix then
        // verify full match.
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&event.event_id[..8]);
        if let Some(cands) = self.index.by_id_prefix.get(&prefix) {
            for &slot_idx in cands {
                if let Some(meta) = &self.index.meta[slot_idx as usize]
                    && meta.event_id == event.event_id
                {
                    return WriteOutcome::Duplicate;
                }
            }
        }

        let slot_idx = self.write_head;
        if let Some(existing) = &self.index.meta[slot_idx as usize]
            && existing.generation >= g_floor
        {
            return WriteOutcome::Stalled;
        }

        let meta = commit_write(
            &mut self.log,
            &mut self.index,
            slot_idx,
            event,
            generation,
            next_seq,
        );

        self.write_head = (self.write_head + 1) % self.log.slot_count() as u32;
        WriteOutcome::Committed { slot_idx, meta }
    }

    fn index(&self) -> &BucketIndex {
        &self.index
    }
    fn log(&self) -> &BucketLog {
        &self.log
    }
    fn index_mut_for_handoff(&mut self) -> &mut BucketIndex {
        &mut self.index
    }

    fn rebuild(&mut self) -> std::io::Result<()> {
        let rebuilt = self.log.iter_slots()?;
        let mut highest_slot_with_data: i32 = -1;
        for (slot_idx, slot, payload) in rebuilt {
            let tags = tags_from_payload(&payload);
            self.index.rebuild_from_disk(slot_idx as u32, &slot, tags);
            if slot_idx as i32 > highest_slot_with_data {
                highest_slot_with_data = slot_idx as i32;
            }
        }
        // Resume write_head after the highest-written slot so we don't
        // immediately overwrite our most recent data. If the log is full,
        // we'll wrap on the next write anyway.
        if highest_slot_with_data >= 0 {
            self.write_head = ((highest_slot_with_data + 1) as u32) % self.log.slot_count() as u32;
        }
        Ok(())
    }
}

/// Replaceable bucket: keyed on `pubkey`, LRU eviction of the least-
/// recently-written pubkey when full. Kinds: 0, 3, 10000..20000.
pub struct ReplaceableBucket {
    log: BucketLog,
    index: BucketIndex,
    /// pubkey → slot_idx. When a pubkey already has a slot, we overwrite
    /// it in place (classic NIP-16 behavior).
    by_pubkey: std::collections::HashMap<[u8; 32], u32>,
    /// LRU order of slots (least-recently-written first). Used to pick
    /// eviction victim when all slots are full and a new pubkey arrives.
    lru: VecDeque<u32>,
    /// Slots that were never written yet; pop here first before evicting.
    free_slots: Vec<u32>,
}

impl ReplaceableBucket {
    pub fn open(path: &Path, slot_count: usize, max_payload: usize) -> std::io::Result<Self> {
        let log = BucketLog::open(path, slot_count, max_payload)?;
        let index = BucketIndex::new(slot_count);
        let free_slots = (0..slot_count as u32).collect();
        Ok(Self {
            log,
            index,
            by_pubkey: std::collections::HashMap::new(),
            lru: VecDeque::new(),
            free_slots,
        })
    }

    fn promote(&mut self, slot_idx: u32) {
        if let Some(pos) = self.lru.iter().position(|&s| s == slot_idx) {
            self.lru.remove(pos);
        }
        self.lru.push_back(slot_idx);
    }
}

impl Bucket for ReplaceableBucket {
    fn try_write(
        &mut self,
        event: &EventPayload<'_>,
        generation: u64,
        g_floor: u64,
        next_seq: &mut u64,
    ) -> WriteOutcome {
        if event.raw_json.len() > self.log.max_payload() {
            return WriteOutcome::TooBig;
        }
        // Existing entry for this pubkey? NIP-16: newer created_at wins.
        if let Some(&slot_idx) = self.by_pubkey.get(&event.pubkey) {
            if let Some(existing) = &self.index.meta[slot_idx as usize] {
                if existing.generation >= g_floor {
                    return WriteOutcome::Stalled;
                }
                if existing.created_at > event.note.created_at {
                    // Incoming is older than what we have — drop per NIP-16.
                    return WriteOutcome::Duplicate;
                }
                if existing.event_id == event.event_id {
                    return WriteOutcome::Duplicate;
                }
            }
            let meta = commit_write(
                &mut self.log,
                &mut self.index,
                slot_idx,
                event,
                generation,
                next_seq,
            );
            self.promote(slot_idx);
            return WriteOutcome::Committed { slot_idx, meta };
        }
        // New pubkey. Use a free slot if available, otherwise evict LRU.
        let slot_idx = if let Some(s) = self.free_slots.pop() {
            s
        } else {
            let Some(&victim) = self.lru.front() else {
                return WriteOutcome::Stalled;
            };
            if let Some(existing) = &self.index.meta[victim as usize]
                && existing.generation >= g_floor
            {
                return WriteOutcome::Stalled;
            }
            self.lru.pop_front();
            if let Some(old_meta) = &self.index.meta[victim as usize] {
                self.by_pubkey.remove(&old_meta.pubkey);
            }
            victim
        };

        let meta = commit_write(
            &mut self.log,
            &mut self.index,
            slot_idx,
            event,
            generation,
            next_seq,
        );
        self.by_pubkey.insert(event.pubkey, slot_idx);
        self.lru.push_back(slot_idx);
        WriteOutcome::Committed { slot_idx, meta }
    }

    fn index(&self) -> &BucketIndex {
        &self.index
    }
    fn log(&self) -> &BucketLog {
        &self.log
    }
    fn index_mut_for_handoff(&mut self) -> &mut BucketIndex {
        &mut self.index
    }

    fn rebuild(&mut self) -> std::io::Result<()> {
        let rebuilt = self.log.iter_slots()?;
        self.free_slots.clear();
        let total_slots = self.log.slot_count();
        let mut used = vec![false; total_slots];
        // Sort by slot_seq ascending so LRU order matches write order.
        let mut rebuilt: Vec<_> = rebuilt.into_iter().collect();
        rebuilt.sort_by_key(|(_, slot, _)| slot.seq.get());
        for (slot_idx, slot, payload) in rebuilt {
            let tags = tags_from_payload(&payload);
            self.index.rebuild_from_disk(slot_idx as u32, &slot, tags);
            self.by_pubkey.insert(slot.pubkey, slot_idx as u32);
            self.lru.push_back(slot_idx as u32);
            used[slot_idx] = true;
        }
        for (i, was_used) in used.iter().enumerate() {
            if !was_used {
                self.free_slots.push(i as u32);
            }
        }
        Ok(())
    }
}

/// Parameterized bucket: keyed on `(pubkey, kind, d-tag)`. Otherwise same
/// shape as replaceable. Kinds 30000..40000.
/// Composite key for the parameterized bucket: `(pubkey, kind, d-tag)`.
type ParamKey = (Box<[u8; 32]>, u32, Box<str>);

pub struct ParameterizedBucket {
    log: BucketLog,
    index: BucketIndex,
    /// (pubkey, kind, d_tag) → slot_idx.
    by_key: std::collections::HashMap<ParamKey, u32>,
    /// Reverse map: slot_idx → key. `None` for empty / freed slots. Kept
    /// in sync with `by_key` so eviction is O(1) instead of O(slot_count)
    /// — without this, every LRU eviction would scan the entire `by_key`
    /// map to recover the old key for removal.
    by_slot: Vec<Option<ParamKey>>,
    lru: VecDeque<u32>,
    free_slots: Vec<u32>,
}

impl ParameterizedBucket {
    pub fn open(path: &Path, slot_count: usize, max_payload: usize) -> std::io::Result<Self> {
        let log = BucketLog::open(path, slot_count, max_payload)?;
        let index = BucketIndex::new(slot_count);
        let free_slots = (0..slot_count as u32).collect();
        let by_slot = (0..slot_count).map(|_| None).collect();
        Ok(Self {
            log,
            index,
            by_key: std::collections::HashMap::new(),
            by_slot,
            lru: VecDeque::new(),
            free_slots,
        })
    }

    fn extract_d_tag(note: &NostrNoteView<'_>) -> Box<str> {
        for row in note.tags.iter() {
            if row.first().map(|s| s.as_ref()) == Some("d")
                && let Some(v) = row.get(1)
            {
                return Box::from(v.as_ref());
            }
        }
        Box::from("")
    }

    fn promote(&mut self, slot_idx: u32) {
        if let Some(pos) = self.lru.iter().position(|&s| s == slot_idx) {
            self.lru.remove(pos);
        }
        self.lru.push_back(slot_idx);
    }
}

impl Bucket for ParameterizedBucket {
    fn try_write(
        &mut self,
        event: &EventPayload<'_>,
        generation: u64,
        g_floor: u64,
        next_seq: &mut u64,
    ) -> WriteOutcome {
        if event.raw_json.len() > self.log.max_payload() {
            return WriteOutcome::TooBig;
        }
        let d_tag = Self::extract_d_tag(event.note);
        let key = (Box::new(event.pubkey), event.note.kind, d_tag);

        if let Some(&slot_idx) = self.by_key.get(&key) {
            if let Some(existing) = &self.index.meta[slot_idx as usize] {
                if existing.generation >= g_floor {
                    return WriteOutcome::Stalled;
                }
                if existing.created_at > event.note.created_at {
                    return WriteOutcome::Duplicate;
                }
                if existing.event_id == event.event_id {
                    return WriteOutcome::Duplicate;
                }
            }
            let meta = commit_write(
                &mut self.log,
                &mut self.index,
                slot_idx,
                event,
                generation,
                next_seq,
            );
            self.promote(slot_idx);
            return WriteOutcome::Committed { slot_idx, meta };
        }
        let slot_idx = if let Some(s) = self.free_slots.pop() {
            s
        } else {
            let Some(&victim) = self.lru.front() else {
                return WriteOutcome::Stalled;
            };
            if let Some(existing) = &self.index.meta[victim as usize]
                && existing.generation >= g_floor
            {
                return WriteOutcome::Stalled;
            }
            self.lru.pop_front();
            if let Some(old_key) = self.by_slot[victim as usize].take() {
                self.by_key.remove(&old_key);
            }
            victim
        };

        let meta = commit_write(
            &mut self.log,
            &mut self.index,
            slot_idx,
            event,
            generation,
            next_seq,
        );
        self.by_key.insert(key.clone(), slot_idx);
        self.by_slot[slot_idx as usize] = Some(key);
        self.lru.push_back(slot_idx);
        WriteOutcome::Committed { slot_idx, meta }
    }

    fn index(&self) -> &BucketIndex {
        &self.index
    }
    fn log(&self) -> &BucketLog {
        &self.log
    }
    fn index_mut_for_handoff(&mut self) -> &mut BucketIndex {
        &mut self.index
    }

    fn rebuild(&mut self) -> std::io::Result<()> {
        let rebuilt = self.log.iter_slots()?;
        self.free_slots.clear();
        let total_slots = self.log.slot_count();
        let mut used = vec![false; total_slots];
        let mut rebuilt: Vec<_> = rebuilt.into_iter().collect();
        rebuilt.sort_by_key(|(_, slot, _)| slot.seq.get());
        for (slot_idx, slot, payload) in rebuilt {
            // Re-parse the view so we can pull the d-tag out.
            let note_view: NostrNoteView<'_> = match serde_json::from_slice(&payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let d_tag = Self::extract_d_tag(&note_view);
            let tags = extract_tags(&note_view);
            self.index.rebuild_from_disk(slot_idx as u32, &slot, tags);
            let key: ParamKey = (Box::new(slot.pubkey), slot.kind, d_tag);
            self.by_key.insert(key.clone(), slot_idx as u32);
            self.by_slot[slot_idx] = Some(key);
            self.lru.push_back(slot_idx as u32);
            used[slot_idx] = true;
        }
        for (i, was_used) in used.iter().enumerate() {
            if !was_used {
                self.free_slots.push(i as u32);
            }
        }
        Ok(())
    }
}

/// Shared commit-path used by all three buckets. Removes the old slot's
/// index participation, writes the new slot's bytes to disk, and inserts
/// new index entries. Returns a clone of the freshly-inserted `SlotMeta`
/// so the caller can broadcast it to reader threads. Caller is responsible
/// for the eviction-policy bookkeeping (LRU, write_head, etc.).
fn commit_write(
    log: &mut BucketLog,
    index: &mut BucketIndex,
    slot_idx: u32,
    event: &EventPayload<'_>,
    generation: u64,
    next_seq: &mut u64,
) -> SlotMeta {
    index.remove_slot(slot_idx);

    *next_seq += 1;
    let seq = NonZeroU64::new(*next_seq).expect("next_seq starts at 1");

    // Payload is the raw JSON bytes. Caller already checked
    // `raw_json.len() <= log.max_payload()`.
    let payload = event.raw_json;
    let payload_len = payload.len();

    let tags = extract_tags(event.note);

    let slot = Slot {
        seq,
        generation,
        created_at: event.note.created_at,
        kind: event.note.kind,
        event_id: event.event_id,
        pubkey: event.pubkey,
        d_tag_range: None,
        payload_len: payload_len as u32,
    };
    let header = slot::encode_header(&slot, payload);
    if let Err(e) = log.write_slot(slot_idx as usize, &header, payload) {
        eprintln!("storage write_slot failed: {e}");
    }

    let meta = SlotMeta {
        seq,
        generation,
        kind: slot.kind,
        created_at: slot.created_at,
        event_id: slot.event_id,
        pubkey: slot.pubkey,
        tags,
        payload_len: slot.payload_len,
    };
    index.insert_slot(slot_idx, meta.clone());
    meta
}

fn tags_from_payload(payload: &[u8]) -> IndexedTags {
    match serde_json::from_slice::<NostrNoteView<'_>>(payload) {
        Ok(view) => extract_tags(&view),
        Err(_) => Arc::from(Vec::new().into_boxed_slice()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_event<'a>(
        note: &'a NostrNoteView<'a>,
        raw: &'a [u8],
        id_byte: u8,
        pk_byte: u8,
    ) -> EventPayload<'a> {
        EventPayload {
            note,
            raw_json: raw,
            event_id: [id_byte; 32],
            pubkey: [pk_byte; 32],
        }
    }

    #[test]
    fn ephemeral_wraps_around() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("eph.log");
        let mut b = EphemeralBucket::open(&path, 3, 4096).unwrap();
        let mut next_seq: u64 = 0;
        for i in 0..5u8 {
            let raw = format!(
                r#"{{"pubkey":"{pk}","created_at":{t},"kind":1,"tags":[],"content":"x","id":"{id}","sig":"00"}}"#,
                pk = "aa".repeat(32),
                t = 1_700_000_000 + i as i64,
                id = format!("{:02x}", i).repeat(32)
            );
            let view: NostrNoteView<'_> = serde_json::from_str(&raw).unwrap();
            let ev = sample_event(&view, raw.as_bytes(), i, 0xaa);
            let out = b.try_write(&ev, 1, u64::MAX, &mut next_seq);
            assert!(
                matches!(out, WriteOutcome::Committed { .. }),
                "write {i}: got {out:?}"
            );
        }
        // After 5 writes into a 3-slot ring, we should have 3 live slots.
        let live = b.index().meta.iter().filter(|m| m.is_some()).count();
        assert_eq!(live, 3);
    }

    #[test]
    fn replaceable_replaces_same_pubkey() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rep.log");
        let mut b = ReplaceableBucket::open(&path, 4, 4096).unwrap();
        let mut next_seq: u64 = 0;

        let raw1 = format!(
            r#"{{"pubkey":"{pk}","created_at":10,"kind":10002,"tags":[],"content":"a","id":"{id}","sig":"00"}}"#,
            pk = "aa".repeat(32),
            id = "11".repeat(32)
        );
        let view1: NostrNoteView<'_> = serde_json::from_str(&raw1).unwrap();
        let ev1 = sample_event(&view1, raw1.as_bytes(), 0x11, 0xaa);
        b.try_write(&ev1, 1, u64::MAX, &mut next_seq);

        let raw2 = format!(
            r#"{{"pubkey":"{pk}","created_at":20,"kind":10002,"tags":[],"content":"b","id":"{id}","sig":"00"}}"#,
            pk = "aa".repeat(32),
            id = "22".repeat(32)
        );
        let view2: NostrNoteView<'_> = serde_json::from_str(&raw2).unwrap();
        let ev2 = sample_event(&view2, raw2.as_bytes(), 0x22, 0xaa);
        b.try_write(&ev2, 2, u64::MAX, &mut next_seq);

        // Still only one slot live for pubkey aa.
        let live = b.index().meta.iter().filter(|m| m.is_some()).count();
        assert_eq!(live, 1);
    }

    #[test]
    fn replaceable_rejects_older_created_at() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rep.log");
        let mut b = ReplaceableBucket::open(&path, 4, 4096).unwrap();
        let mut next_seq: u64 = 0;

        let raw_new = format!(
            r#"{{"pubkey":"{pk}","created_at":100,"kind":10002,"tags":[],"content":"new","id":"{id}","sig":"00"}}"#,
            pk = "aa".repeat(32),
            id = "11".repeat(32)
        );
        let view: NostrNoteView<'_> = serde_json::from_str(&raw_new).unwrap();
        let ev = sample_event(&view, raw_new.as_bytes(), 0x11, 0xaa);
        b.try_write(&ev, 1, u64::MAX, &mut next_seq);

        let raw_old = format!(
            r#"{{"pubkey":"{pk}","created_at":50,"kind":10002,"tags":[],"content":"old","id":"{id}","sig":"00"}}"#,
            pk = "aa".repeat(32),
            id = "22".repeat(32)
        );
        let view2: NostrNoteView<'_> = serde_json::from_str(&raw_old).unwrap();
        let ev2 = sample_event(&view2, raw_old.as_bytes(), 0x22, 0xaa);
        let out = b.try_write(&ev2, 2, u64::MAX, &mut next_seq);
        assert!(matches!(out, WriteOutcome::Duplicate));
    }

    /// Parameterized eviction must keep `by_key` consistent with `by_slot`:
    /// when an LRU victim is evicted, its old key must be gone from
    /// `by_key`. Regression guard: a previous version found the old key by
    /// scanning `by_key` linearly, which was both O(n) per eviction and a
    /// correctness hazard if anyone ever reordered the eviction code.
    #[test]
    fn parameterized_eviction_clears_old_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("par.log");
        // 2 slots so the third write has to evict.
        let mut b = ParameterizedBucket::open(&path, 2, 4096).unwrap();
        let mut next_seq: u64 = 0;

        let mk = |pk_byte: u8, d: &str, id_byte: u8, t: i64| -> String {
            format!(
                r#"{{"pubkey":"{pk}","created_at":{t},"kind":30001,"tags":[["d","{d}"]],"content":"x","id":"{id}","sig":"00"}}"#,
                pk = format!("{:02x}", pk_byte).repeat(32),
                id = format!("{:02x}", id_byte).repeat(32),
            )
        };

        // Fill both slots with distinct (pubkey, d) keys.
        let raw_a = mk(0xaa, "first", 0x11, 100);
        let view_a: NostrNoteView<'_> = serde_json::from_str(&raw_a).unwrap();
        let ev_a = sample_event(&view_a, raw_a.as_bytes(), 0x11, 0xaa);
        b.try_write(&ev_a, 1, u64::MAX, &mut next_seq);

        let raw_b = mk(0xbb, "second", 0x22, 101);
        let view_b: NostrNoteView<'_> = serde_json::from_str(&raw_b).unwrap();
        let ev_b = sample_event(&view_b, raw_b.as_bytes(), 0x22, 0xbb);
        b.try_write(&ev_b, 1, u64::MAX, &mut next_seq);

        assert_eq!(b.by_key.len(), 2);

        // Third write evicts the oldest (key A).
        let raw_c = mk(0xcc, "third", 0x33, 102);
        let view_c: NostrNoteView<'_> = serde_json::from_str(&raw_c).unwrap();
        let ev_c = sample_event(&view_c, raw_c.as_bytes(), 0x33, 0xcc);
        let out = b.try_write(&ev_c, 1, u64::MAX, &mut next_seq);
        assert!(matches!(out, WriteOutcome::Committed { .. }));

        // by_key must no longer contain key A.
        let key_a: ParamKey = (Box::new([0xaa; 32]), 30001, Box::from("first"));
        assert!(
            !b.by_key.contains_key(&key_a),
            "by_key still has the evicted key"
        );
        // And the reverse map must agree.
        assert_eq!(b.by_key.len(), 2);
        assert_eq!(b.by_slot.iter().filter(|s| s.is_some()).count(), 2);
    }

    #[test]
    fn stall_when_gfloor_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("eph.log");
        let mut b = EphemeralBucket::open(&path, 2, 4096).unwrap();
        let mut next_seq: u64 = 0;
        // Fill both slots.
        for i in 0..2u8 {
            let raw = format!(
                r#"{{"pubkey":"{pk}","created_at":{t},"kind":1,"tags":[],"content":"x","id":"{id}","sig":"00"}}"#,
                pk = "aa".repeat(32),
                t = 1_700_000_000 + i as i64,
                id = format!("{:02x}", i).repeat(32)
            );
            let view: NostrNoteView<'_> = serde_json::from_str(&raw).unwrap();
            let ev = sample_event(&view, raw.as_bytes(), i, 0xaa);
            b.try_write(&ev, 10, 0, &mut next_seq);
        }
        // Now try to write again with g_floor = 10 (everything still referenced).
        let raw = format!(
            r#"{{"pubkey":"{pk}","created_at":5,"kind":1,"tags":[],"content":"x","id":"{id}","sig":"00"}}"#,
            pk = "aa".repeat(32),
            id = "99".repeat(32)
        );
        let view: NostrNoteView<'_> = serde_json::from_str(&raw).unwrap();
        let ev = sample_event(&view, raw.as_bytes(), 0x99, 0xaa);
        let out = b.try_write(&ev, 20, 10, &mut next_seq);
        assert!(matches!(out, WriteOutcome::Stalled));
    }
}
