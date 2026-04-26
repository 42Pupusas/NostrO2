//! Reader pool: N threads that service historical REQ scans.
//!
//! Each reader owns its own `AtomicU64` generation slot inside
//! `SharedState::reader_active_gens`. At REQ start it loads
//! `current_gen` into its slot; at REQ end it writes `u64::MAX` back.
//! The storage thread computes `g_floor = min(active_gens)` for eviction.
//!
//! Readers emit `EVENT` frames via the shared `ServerSender` (same one the
//! shards use). Writer routing (`fd % writer_shards`) handles delivery.
//!
//! Per-REQ timeout: ~500ms wall-clock. If a scan takes longer we abort it,
//! release the gen slot, and send `CLOSED` with a reason. Without the
//! timeout a pathologically slow scan could block eviction indefinitely.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ring_relay_server::ServerSender;

use super::bucket::Bucket;
use super::engine::SharedState;
use super::handle::{ReqJob, ReqQueue};
use super::index::SlotMeta;
use super::log::ReadOnlyLog;
use crate::protocol;

const REQ_TIMEOUT: Duration = Duration::from_millis(500);

pub struct ReaderPoolShutdown {
    pub flag: Arc<AtomicBool>,
    pub threads: Vec<std::thread::JoinHandle<()>>,
}

impl ReaderPoolShutdown {
    pub fn stop(&mut self) {
        self.flag.store(true, Ordering::Release);
        for h in &self.threads {
            h.thread().unpark();
        }
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}

pub struct ReaderPool;

impl ReaderPool {
    pub fn spawn(
        shared: Arc<SharedState>,
        req_queue: Arc<ReqQueue>,
        sender: ServerSender,
        data_dir: std::path::PathBuf,
        reader_threads: usize,
    ) -> std::io::Result<(ReaderPoolShutdown, Vec<std::thread::Thread>)> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(reader_threads);
        let mut thread_handles: Vec<std::thread::Thread> = Vec::with_capacity(reader_threads);
        for i in 0..reader_threads {
            let shared_c = Arc::clone(&shared);
            let q_c = Arc::clone(&req_queue);
            let sender_c = sender.clone();
            let sd_c = Arc::clone(&shutdown);
            let dir_c = data_dir.clone();
            let handle = std::thread::Builder::new()
                .name(format!("nostr-reader-{i}"))
                .spawn(move || {
                    reader_loop(i, shared_c, q_c, sender_c, sd_c, dir_c);
                })?;
            thread_handles.push(handle.thread().clone());
            threads.push(handle);
        }
        Ok((
            ReaderPoolShutdown {
                flag: shutdown,
                threads,
            },
            thread_handles,
        ))
    }
}

fn reader_loop(
    reader_idx: usize,
    shared: Arc<SharedState>,
    req_queue: Arc<ReqQueue>,
    sender: ServerSender,
    shutdown: Arc<AtomicBool>,
    data_dir: std::path::PathBuf,
) {
    // Open read-only handles to all three logs.
    let eph_path = data_dir.join("ephemeral.log");
    let rep_path = data_dir.join("replaceable.log");
    let par_path = data_dir.join("parameterized.log");
    let eph_log = {
        let b = shared.ephemeral.read().unwrap();
        match b.log().reopen_readonly(&eph_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("reader {reader_idx}: open eph log: {e}");
                return;
            }
        }
    };
    let rep_log = {
        let b = shared.replaceable.read().unwrap();
        match b.log().reopen_readonly(&rep_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("reader {reader_idx}: open rep log: {e}");
                return;
            }
        }
    };
    let par_log = {
        let b = shared.parameterized.read().unwrap();
        match b.log().reopen_readonly(&par_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("reader {reader_idx}: open par log: {e}");
                return;
            }
        }
    };
    let gen_slot: &AtomicU64 = &shared.reader_active_gens[reader_idx];

    while !shutdown.load(Ordering::Acquire) {
        let Some(job) = req_queue.pop() else {
            std::thread::park_timeout(Duration::from_millis(50));
            continue;
        };

        let g_req = shared.current_gen.load(Ordering::Acquire);
        gen_slot.store(g_req, Ordering::Release);

        let deadline = Instant::now() + REQ_TIMEOUT;
        let _timed_out = serve_req(
            &job, g_req, deadline, &shared, &sender, &eph_log, &rep_log, &par_log,
        );

        // Release gen slot so the storage thread can advance g_floor.
        gen_slot.store(u64::MAX, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_req(
    job: &ReqJob,
    g_req: u64,
    deadline: Instant,
    shared: &SharedState,
    sender: &ServerSender,
    eph_log: &ReadOnlyLog,
    rep_log: &ReadOnlyLog,
    par_log: &ReadOnlyLog,
) -> bool {
    // Aggregate cumulative limit across all filters.
    let mut total_limit: Option<usize> = None;
    let mut per_filter_limit: Vec<Option<usize>> = Vec::with_capacity(job.filters.len());
    for f in job.filters.iter() {
        per_filter_limit.push(f.limit.map(|n| n as usize));
        if let Some(n) = f.limit {
            total_limit = Some(total_limit.unwrap_or(0) + n as usize);
        }
    }

    let mut emitted: usize = 0;
    let mut seen_ids: std::collections::HashSet<[u8; 32]> =
        std::collections::HashSet::with_capacity(64);

    // Scan each bucket. Collection order doesn't matter for NIP-01 (clients
    // sort by created_at); we deliver oldest-first to keep timeline UIs
    // happy.
    let mut matches: Vec<EmitEntry> = Vec::new();

    for (filter_idx, filter) in job.filters.iter().enumerate() {
        if Instant::now() >= deadline {
            sender.send_text(job.client_fd, protocol::closed(&job.sub_id, "timeout"));
            return true;
        }
        let limit = per_filter_limit[filter_idx];

        // Each bucket contributes candidate slot indices.
        scan_bucket(
            shared,
            BucketRef::Ephemeral,
            filter,
            g_req,
            limit,
            &mut matches,
            &mut seen_ids,
        );
        scan_bucket(
            shared,
            BucketRef::Replaceable,
            filter,
            g_req,
            limit,
            &mut matches,
            &mut seen_ids,
        );
        scan_bucket(
            shared,
            BucketRef::Parameterized,
            filter,
            g_req,
            limit,
            &mut matches,
            &mut seen_ids,
        );
    }

    // Sort oldest-first (lowest created_at first, ties broken by event_id).
    matches.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });

    for entry in matches {
        if Instant::now() >= deadline {
            sender.send_text(job.client_fd, protocol::closed(&job.sub_id, "timeout"));
            return true;
        }
        if let Some(limit) = total_limit
            && emitted >= limit
        {
            break;
        }
        let payload = match entry.bucket {
            BucketRef::Ephemeral => {
                eph_log.read_payload(entry.slot_idx as usize, entry.payload_len)
            }
            BucketRef::Replaceable => {
                rep_log.read_payload(entry.slot_idx as usize, entry.payload_len)
            }
            BucketRef::Parameterized => {
                par_log.read_payload(entry.slot_idx as usize, entry.payload_len)
            }
        };
        let Ok(bytes) = payload else { continue };

        // Validate seq hasn't changed mid-scan (storage rewrote the slot
        // despite the gen gate — shouldn't happen, but defensive).
        if let Some(slot_head) = match entry.bucket {
            BucketRef::Ephemeral => eph_log.read_slot(entry.slot_idx as usize),
            BucketRef::Replaceable => rep_log.read_slot(entry.slot_idx as usize),
            BucketRef::Parameterized => par_log.read_slot(entry.slot_idx as usize),
        }
        .ok()
        .flatten()
        {
            if slot_head.0.seq != entry.seq {
                continue; // raced; the slot was overwritten
            }
        } else {
            continue;
        }

        sender.send_event_frame(
            job.client_fd,
            Arc::clone(&job.sub_id),
            Arc::from(bytes.into_boxed_slice()),
        );
        emitted += 1;
    }

    // End of stored events for this sub.
    sender.send_text(job.client_fd, protocol::eose(&job.sub_id));
    false
}

#[derive(Clone, Copy)]
enum BucketRef {
    Ephemeral,
    Replaceable,
    Parameterized,
}

struct EmitEntry {
    bucket: BucketRef,
    slot_idx: u32,
    seq: std::num::NonZeroU64,
    payload_len: u32,
    created_at: i64,
    event_id: [u8; 32],
}

fn scan_bucket(
    shared: &SharedState,
    which: BucketRef,
    filter: &nostro2::NostrSubscription,
    g_req: u64,
    filter_limit: Option<usize>,
    out: &mut Vec<EmitEntry>,
    seen_ids: &mut std::collections::HashSet<[u8; 32]>,
) {
    // Collect (slot_idx, metadata) under the read lock, drop the lock, then
    // do the actual filter evaluation outside the critical section.
    let candidates: Vec<(u32, SlotMeta)> = match which {
        BucketRef::Ephemeral => {
            let b = shared.ephemeral.read().unwrap();
            collect_candidates(&b.index().candidates(filter), &b.index().meta)
        }
        BucketRef::Replaceable => {
            let b = shared.replaceable.read().unwrap();
            collect_candidates(&b.index().candidates(filter), &b.index().meta)
        }
        BucketRef::Parameterized => {
            let b = shared.parameterized.read().unwrap();
            collect_candidates(&b.index().candidates(filter), &b.index().meta)
        }
    };

    // Per-filter limit picks the newest N; sort desc by created_at, truncate.
    // Final oldest-first ordering happens after the cross-bucket union.
    let mut filtered: Vec<(u32, SlotMeta)> = candidates
        .into_iter()
        .filter(|(_, m)| m.generation <= g_req && m.matches(filter))
        .collect();
    filtered.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));
    if let Some(limit) = filter_limit {
        filtered.truncate(limit);
    }

    for (slot_idx, meta) in filtered {
        if !seen_ids.insert(meta.event_id) {
            continue;
        }
        out.push(EmitEntry {
            bucket: which,
            slot_idx,
            seq: meta.seq,
            payload_len: meta.payload_len,
            created_at: meta.created_at,
            event_id: meta.event_id,
        });
    }
}

fn collect_candidates(slot_idxs: &[u32], meta: &[Option<SlotMeta>]) -> Vec<(u32, SlotMeta)> {
    let mut out = Vec::with_capacity(slot_idxs.len());
    for &i in slot_idxs {
        if let Some(m) = &meta[i as usize] {
            out.push((i, m.clone()));
        }
    }
    out
}
