//! Fixed-size on-disk slot layout.
//!
//! Each bucket log is `slot_count * SLOT_STRIDE(max_payload)` bytes.
//! A slot consists of a fixed-width header followed by up to `max_payload`
//! raw JSON bytes. The full stride is written on every overwrite so there
//! are no torn reads at the header/payload boundary.
//!
//! Header (128 bytes) — POD, no serde, memcpy-friendly:
//!
//! ```text
//!   0..8   magic  = b"RNOSTR01" (bucket header not yet filled on fresh init)
//!   8..16  seq             u64 LE — monotonic per slot; reused on overwrite
//!  16..24  gen             u64 LE — storage current_gen at write time
//!  24..32  created_at      i64 LE — event created_at, for since/until
//!  32..36  kind            u32 LE
//!  36..40  payload_len     u32 LE — bytes in the JSON region
//!  40..72  event_id        [u8; 32] — hex-decoded
//!  72..104 pubkey          [u8; 32] — hex-decoded
//!  104..108 d_tag_off      u32 LE — offset of d-tag value in payload, or 0
//!  108..112 d_tag_len      u32 LE — len of d-tag value in payload, or 0
//!  112..120 reserved
//!  120..128 crc            u64 LE — CRC-64 of bytes [0..120] + payload
//! ```
//!
//! `seq == 0` means the slot is empty (never written).

use std::num::NonZeroU64;

pub const SLOT_HEADER_SIZE: usize = 128;
pub const MAGIC: &[u8; 8] = b"RNOSTR01";

#[inline]
pub const fn slot_stride(max_payload: usize) -> usize {
    SLOT_HEADER_SIZE + max_payload
}

/// Bucket kind derived from the event's `kind` number per NIP-01 / NIP-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketKind {
    Ephemeral,
    Replaceable,
    Parameterized,
}

impl BucketKind {
    #[must_use]
    pub fn classify(kind: u32) -> Self {
        // NIP-01 replaceable: 0 and 3 — we fold these into replaceable.
        // NIP-16 replaceable: 10000..20000.
        // NIP-33 parameterized: 30000..40000.
        // Everything else (including kind 1) → ephemeral.
        if kind == 0 || kind == 3 || (10_000..20_000).contains(&kind) {
            Self::Replaceable
        } else if (30_000..40_000).contains(&kind) {
            Self::Parameterized
        } else {
            Self::Ephemeral
        }
    }
}

/// In-memory representation of a slot. Mirrors the on-disk header layout
/// plus a handle to the payload bytes; the storage thread uses this as its
/// authoritative slot table (payloads live in a separate per-bucket mmap or
/// just read from the log on demand).
#[derive(Debug, Clone)]
pub struct Slot {
    /// Monotonic per-slot sequence; 0 = empty. `NonZeroU64` so `Option<Slot>`
    /// is the preferred emptiness marker at the table level.
    pub seq: NonZeroU64,
    /// Generation at which this slot was written. Reader CoW filter uses this.
    pub generation: u64,
    pub created_at: i64,
    pub kind: u32,
    pub event_id: [u8; 32],
    pub pubkey: [u8; 32],
    /// Offset of d-tag value inside the payload (parameterized only), or None.
    pub d_tag_range: Option<(u32, u32)>,
    pub payload_len: u32,
}

/// Hex-decode a 32-byte hex string into `[u8; 32]`. Returns `None` on any
/// non-hex character or wrong length.
#[must_use]
pub fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = hex_digit(bytes[i * 2])?;
        let lo = hex_digit(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

#[inline]
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// CRC-64 (ECMA-182) incremental, table-free. Not the fastest in the world
/// but adequate for torn-write detection at our ingest rates.
#[must_use]
pub fn crc64(bytes: &[u8]) -> u64 {
    const POLY: u64 = 0xC96C_5795_D787_0F42;
    let mut crc: u64 = !0;
    for &b in bytes {
        crc ^= u64::from(b);
        for _ in 0..8 {
            let lsb = crc & 1;
            crc >>= 1;
            if lsb != 0 {
                crc ^= POLY;
            }
        }
    }
    !crc
}

/// Encode a slot header into its on-disk byte form. `payload` is the raw
/// JSON body that will follow; its CRC is folded into the header trailer.
pub fn encode_header(slot: &Slot, payload: &[u8]) -> [u8; SLOT_HEADER_SIZE] {
    let mut buf = [0u8; SLOT_HEADER_SIZE];
    buf[0..8].copy_from_slice(MAGIC);
    buf[8..16].copy_from_slice(&slot.seq.get().to_le_bytes());
    buf[16..24].copy_from_slice(&slot.generation.to_le_bytes());
    buf[24..32].copy_from_slice(&slot.created_at.to_le_bytes());
    buf[32..36].copy_from_slice(&slot.kind.to_le_bytes());
    buf[36..40].copy_from_slice(&slot.payload_len.to_le_bytes());
    buf[40..72].copy_from_slice(&slot.event_id);
    buf[72..104].copy_from_slice(&slot.pubkey);
    let (d_off, d_len) = slot.d_tag_range.unwrap_or((0, 0));
    buf[104..108].copy_from_slice(&d_off.to_le_bytes());
    buf[108..112].copy_from_slice(&d_len.to_le_bytes());
    // bytes 112..120 reserved, left zero.
    // Compute CRC over [0..120] with payload appended conceptually.
    let mut hasher_input = [0u8; 120];
    hasher_input.copy_from_slice(&buf[..120]);
    // Fold payload bytes into crc by sequential pass.
    let crc = {
        let prefix = crc64(&hasher_input);
        // Continue the CRC over the payload bytes. Easiest: recompute combined.
        let mut combined = Vec::with_capacity(120 + payload.len());
        combined.extend_from_slice(&hasher_input);
        combined.extend_from_slice(payload);
        let _ = prefix;
        crc64(&combined)
    };
    buf[120..128].copy_from_slice(&crc.to_le_bytes());
    buf
}

/// Decode a header. Returns None if the slot is empty (seq == 0), the magic
/// is wrong, or the CRC fails. The payload must be supplied so the CRC can
/// be validated end-to-end.
pub fn decode_header(header: &[u8; SLOT_HEADER_SIZE], payload: &[u8]) -> Option<Slot> {
    if &header[0..8] != MAGIC {
        return None;
    }
    let seq = u64::from_le_bytes(header[8..16].try_into().ok()?);
    let seq = NonZeroU64::new(seq)?;
    let generation = u64::from_le_bytes(header[16..24].try_into().ok()?);
    let created_at = i64::from_le_bytes(header[24..32].try_into().ok()?);
    let kind = u32::from_le_bytes(header[32..36].try_into().ok()?);
    let payload_len = u32::from_le_bytes(header[36..40].try_into().ok()?);
    if payload_len as usize > payload.len() {
        return None;
    }
    let mut event_id = [0u8; 32];
    event_id.copy_from_slice(&header[40..72]);
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&header[72..104]);
    let d_off = u32::from_le_bytes(header[104..108].try_into().ok()?);
    let d_len = u32::from_le_bytes(header[108..112].try_into().ok()?);
    let d_tag_range = if d_len == 0 {
        None
    } else {
        Some((d_off, d_len))
    };
    let stored_crc = u64::from_le_bytes(header[120..128].try_into().ok()?);

    let mut combined = Vec::with_capacity(120 + payload_len as usize);
    combined.extend_from_slice(&header[..120]);
    combined.extend_from_slice(&payload[..payload_len as usize]);
    if crc64(&combined) != stored_crc {
        return None;
    }
    Some(Slot {
        seq,
        generation,
        created_at,
        kind,
        event_id,
        pubkey,
        d_tag_range,
        payload_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_kinds() {
        assert_eq!(BucketKind::classify(1), BucketKind::Ephemeral);
        assert_eq!(BucketKind::classify(0), BucketKind::Replaceable);
        assert_eq!(BucketKind::classify(3), BucketKind::Replaceable);
        assert_eq!(BucketKind::classify(10_002), BucketKind::Replaceable);
        assert_eq!(BucketKind::classify(19_999), BucketKind::Replaceable);
        assert_eq!(BucketKind::classify(20_000), BucketKind::Ephemeral);
        assert_eq!(BucketKind::classify(30_000), BucketKind::Parameterized);
        assert_eq!(BucketKind::classify(39_999), BucketKind::Parameterized);
        assert_eq!(BucketKind::classify(40_000), BucketKind::Ephemeral);
    }

    #[test]
    fn hex_roundtrip() {
        let s = "a".repeat(64);
        let b = decode_hex32(&s).unwrap();
        assert_eq!(b, [0xaau8; 32]);
        assert!(decode_hex32("zz").is_none());
        assert!(decode_hex32(&"z".repeat(64)).is_none());
    }

    #[test]
    fn header_roundtrip() {
        let slot = Slot {
            seq: NonZeroU64::new(42).unwrap(),
            generation: 99,
            created_at: 1_700_000_000,
            kind: 10_002,
            event_id: [0x11u8; 32],
            pubkey: [0x22u8; 32],
            d_tag_range: Some((5, 8)),
            payload_len: 16,
        };
        let payload = b"0123456789abcdef";
        let hdr = encode_header(&slot, payload);
        let decoded = decode_header(&hdr, payload).unwrap();
        assert_eq!(decoded.seq, slot.seq);
        assert_eq!(decoded.generation, slot.generation);
        assert_eq!(decoded.kind, slot.kind);
        assert_eq!(decoded.event_id, slot.event_id);
        assert_eq!(decoded.d_tag_range, slot.d_tag_range);
    }

    #[test]
    fn header_empty_slot_fails_decode() {
        let hdr = [0u8; SLOT_HEADER_SIZE];
        assert!(decode_header(&hdr, &[]).is_none());
    }

    #[test]
    fn header_corrupted_crc_fails() {
        let slot = Slot {
            seq: NonZeroU64::new(1).unwrap(),
            generation: 1,
            created_at: 0,
            kind: 1,
            event_id: [0; 32],
            pubkey: [0; 32],
            d_tag_range: None,
            payload_len: 0,
        };
        let mut hdr = encode_header(&slot, &[]);
        hdr[120] ^= 0xff;
        assert!(decode_header(&hdr, &[]).is_none());
    }
}
