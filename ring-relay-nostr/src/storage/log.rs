//! On-disk bucket log: fixed-size slot table backed by a single file.
//!
//! The file is pre-sized to `slot_count * stride` bytes at open. Writes go
//! through `pwrite`; reads through `pread`. One log per bucket. Shared
//! read-only access from reader threads is handled by cloning the `File`
//! handle (dup'd fd); writes stay on the storage thread.
//!
//! ## Durability
//!
//! `fsync` is called by the storage thread on the group-commit interval.
//! Per-write fsync would halve throughput and is not needed for NIP-01 —
//! events that aren't yet durable are still visible to live subscribers via
//! the shard-level fan-out, and a crash only loses the last <10ms of tail.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::slot::{self, SLOT_HEADER_SIZE, Slot, slot_stride};

/// A single bucket's log file on disk.
pub struct BucketLog {
    file: File,
    slot_count: usize,
    max_payload: usize,
    stride: usize,
}

impl BucketLog {
    /// Open (or create + size) the log file for a bucket.
    pub fn open(path: &Path, slot_count: usize, max_payload: usize) -> io::Result<Self> {
        let stride = slot_stride(max_payload);
        let total = stride * slot_count;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let current_len = file.seek(SeekFrom::End(0))?;
        if (current_len as usize) < total {
            // Extend to full capacity.
            file.set_len(total as u64)?;
        }
        Ok(Self {
            file,
            slot_count,
            max_payload,
            stride,
        })
    }

    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    #[must_use]
    pub fn max_payload(&self) -> usize {
        self.max_payload
    }

    #[inline]
    fn slot_offset(&self, idx: usize) -> u64 {
        (idx * self.stride) as u64
    }

    /// Write one slot (header + payload). Caller must have already encoded
    /// the header via `slot::encode_header` using the same payload bytes.
    pub fn write_slot(
        &self,
        idx: usize,
        header: &[u8; SLOT_HEADER_SIZE],
        payload: &[u8],
    ) -> io::Result<()> {
        assert!(idx < self.slot_count);
        assert!(payload.len() <= self.max_payload);
        let off = self.slot_offset(idx);
        self.file.write_all_at(header, off)?;
        if !payload.is_empty() {
            self.file
                .write_all_at(payload, off + SLOT_HEADER_SIZE as u64)?;
        }
        Ok(())
    }

    /// Read the header + payload of a slot. Returns `None` if the slot is
    /// empty or fails CRC validation. Only used by tests — production
    /// readers go through `ReadOnlyLog`.
    #[cfg(test)]
    pub fn read_slot(&self, idx: usize) -> io::Result<Option<(Slot, Vec<u8>)>> {
        assert!(idx < self.slot_count);
        let off = self.slot_offset(idx);
        let mut header = [0u8; SLOT_HEADER_SIZE];
        self.file.read_exact_at(&mut header, off)?;
        if header.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let payload_len =
            u32::from_le_bytes(header[36..40].try_into().unwrap_or_default()) as usize;
        if payload_len > self.max_payload {
            return Ok(None);
        }
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            self.file
                .read_exact_at(&mut payload, off + SLOT_HEADER_SIZE as u64)?;
        }
        Ok(slot::decode_header(&header, &payload).map(|s| (s, payload)))
    }

    pub fn fsync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// Read just the payload bytes at `idx`. Mirrors [`ReadOnlyLog::read_payload`]
    /// but on the writable handle owned by the storage thread, used by
    /// reopen-time helpers (e.g. NIP-09 deletion replay).
    pub fn read_payload(&self, idx: usize, payload_len: u32) -> io::Result<Vec<u8>> {
        assert!(idx < self.slot_count);
        let off = self.slot_offset(idx) + SLOT_HEADER_SIZE as u64;
        let mut buf = vec![0u8; payload_len as usize];
        if payload_len > 0 {
            self.file.read_exact_at(&mut buf, off)?;
        }
        Ok(buf)
    }

    /// Clone the underlying file handle for read-only access from another
    /// thread (reader pool). The dup'd fd shares the kernel file offset,
    /// but we exclusively use positional I/O so that's fine.
    pub fn reopen_readonly(&self, path: &Path) -> io::Result<ReadOnlyLog> {
        let file = OpenOptions::new().read(true).open(path)?;
        Ok(ReadOnlyLog {
            file,
            stride: self.stride,
            max_payload: self.max_payload,
            slot_count: self.slot_count,
        })
    }

    /// Iterate every non-empty slot on disk for index rebuild at startup.
    pub fn iter_slots(&mut self) -> io::Result<Vec<(usize, Slot, Vec<u8>)>> {
        let mut out = Vec::new();
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = vec![0u8; self.stride];
        for idx in 0..self.slot_count {
            self.file.read_exact(&mut buf)?;
            let header: &[u8; SLOT_HEADER_SIZE] = buf[..SLOT_HEADER_SIZE].try_into().unwrap();
            if header.iter().all(|&b| b == 0) {
                continue;
            }
            let payload_len =
                u32::from_le_bytes(header[36..40].try_into().unwrap_or_default()) as usize;
            if payload_len > self.max_payload {
                continue;
            }
            let payload = buf[SLOT_HEADER_SIZE..SLOT_HEADER_SIZE + payload_len].to_vec();
            if let Some(slot) = slot::decode_header(header, &payload) {
                out.push((idx, slot, payload));
            }
        }
        Ok(out)
    }
}

/// A read-only file handle for a reader thread. Same positional-I/O interface,
/// no write access.
pub struct ReadOnlyLog {
    file: File,
    stride: usize,
    max_payload: usize,
    slot_count: usize,
}

impl ReadOnlyLog {
    pub fn read_payload(&self, idx: usize, payload_len: u32) -> io::Result<Vec<u8>> {
        assert!(idx < self.slot_count);
        let off = (idx * self.stride + SLOT_HEADER_SIZE) as u64;
        let mut buf = vec![0u8; payload_len as usize];
        if payload_len > 0 {
            self.file.read_exact_at(&mut buf, off)?;
        }
        Ok(buf)
    }

    /// Read header + payload for verifying `seq` still matches the reader's
    /// expected value before emitting.
    pub fn read_slot(&self, idx: usize) -> io::Result<Option<(Slot, Vec<u8>)>> {
        assert!(idx < self.slot_count);
        let off = (idx * self.stride) as u64;
        let mut header = [0u8; SLOT_HEADER_SIZE];
        self.file.read_exact_at(&mut header, off)?;
        if header.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let payload_len =
            u32::from_le_bytes(header[36..40].try_into().unwrap_or_default()) as usize;
        if payload_len > self.max_payload {
            return Ok(None);
        }
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            self.file
                .read_exact_at(&mut payload, off + SLOT_HEADER_SIZE as u64)?;
        }
        Ok(slot::decode_header(&header, &payload).map(|s| (s, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_single_slot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bucket.log");
        let log = BucketLog::open(&path, 4, 1024).unwrap();

        let slot = Slot {
            seq: NonZeroU64::new(7).unwrap(),
            generation: 3,
            created_at: 1_700_000_000,
            kind: 1,
            event_id: [0xaa; 32],
            pubkey: [0xbb; 32],
            d_tag_range: None,
            payload_len: 5,
        };
        let payload = b"hello";
        let hdr = slot::encode_header(&slot, payload);
        log.write_slot(2, &hdr, payload).unwrap();

        let (got_slot, got_payload) = log.read_slot(2).unwrap().unwrap();
        assert_eq!(got_slot.seq, slot.seq);
        assert_eq!(got_slot.generation, slot.generation);
        assert_eq!(got_payload, payload);

        // Empty slot returns None.
        assert!(log.read_slot(0).unwrap().is_none());
    }

    #[test]
    fn reopen_preserves_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bucket.log");
        {
            let log = BucketLog::open(&path, 4, 1024).unwrap();
            let slot = Slot {
                seq: NonZeroU64::new(1).unwrap(),
                generation: 1,
                created_at: 1,
                kind: 1,
                event_id: [1; 32],
                pubkey: [2; 32],
                d_tag_range: None,
                payload_len: 3,
            };
            let hdr = slot::encode_header(&slot, b"abc");
            log.write_slot(1, &hdr, b"abc").unwrap();
            log.fsync().unwrap();
        }
        let mut log = BucketLog::open(&path, 4, 1024).unwrap();
        let all = log.iter_slots().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, 1);
        assert_eq!(all[0].2, b"abc");
    }
}
