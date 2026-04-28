//! Matcher-only microbench for the fan-out hot path.
//!
//! Isolates the cost of the loop currently inside
//! `ShardDispatcher::complete_event`:
//!
//! 1. `serde_json::from_slice::<NostrNoteView>` — re-parse the verified
//!    event because the `NostrNoteView` is borrowed and didn't survive
//!    the cross-thread hop from the verify worker.
//! 2. For every `(client × sub × filter)`: `filter::matches_view`.
//! 3. `Arc::clone(sub_id) + Arc::clone(note_bytes)` per delivery and a
//!    push to a stand-in writer ring (here a black-boxed sink).
//!
//! This is the precise work that today runs serially on each shard's I/O
//! thread once verify completes. The bench omits TCP, io_uring, and the
//! verify pool entirely so a matcher-pool split's CPU win lands directly
//! in this number.
//!
//! Knobs:
//! - `subs`: number of connected subscribers, each holding a single
//!   `NostrSubscription`. Bench runs at {64, 256, 1024, 4096}.
//! - `selectivity`: `firehose` (every sub matches every event) vs
//!   `kinds_disjoint` (subs split across kinds so most filters miss).
//!   The dispatcher shape walks every sub regardless of selectivity, but
//!   matching cost differs — both ends of the spectrum are useful.
//!
//! Reports throughput in events/s. The matcher pool's expected win is
//! near-linear with available cores at high `subs` because the inner
//! loop is independent across client_ids.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nostro2::{NostrNote, NostrNoteView, NostrSigner, NostrSubscription};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{MatchView, matches_match_view, matches_view};

/// Pre-signed event bytes (raw JSON) shared across the run. Fan-out delivers
/// the same `Arc<[u8]>` to every match; we mirror that.
fn presign_one() -> Arc<[u8]> {
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("matcher bench payload — typical short note");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    let json = serde_json::to_vec(&note).expect("ser");
    Arc::from(json.into_boxed_slice())
}

/// Build the owned matcher snapshot the verify worker would now hand
/// the shard. Mirrors `verify_pool::snapshot_match_view` (which is
/// crate-private) so the bench can populate a `MatchView` from the
/// borrowed `NostrNoteView`.
fn build_match_view(view: &NostrNoteView<'_>, id: Arc<str>) -> MatchView {
    let row_count = view.tags.len();
    let mut tag_cells: Vec<Box<str>> = Vec::with_capacity(row_count * 2);
    let mut tag_offsets: Vec<u32> = Vec::with_capacity(row_count + 1);
    tag_offsets.push(0);
    for row in view.tags.iter() {
        for cell in row {
            tag_cells.push(Box::<str>::from(cell.as_ref()));
        }
        tag_offsets.push(tag_cells.len() as u32);
    }
    MatchView {
        id,
        pubkey: Arc::<str>::from(view.pubkey.as_ref()),
        kind: view.kind,
        created_at: view.created_at,
        tag_cells: tag_cells.into_boxed_slice(),
        tag_offsets: tag_offsets.into_boxed_slice(),
    }
}

/// Each client owns one sub. Mirrors `ClientState` but flattened — the
/// matcher loop today walks `clients[i].subs` for every event regardless,
/// so a flat layout has the same iteration profile.
struct ClientSub {
    sub_id: Arc<str>,
    filters: Arc<[NostrSubscription]>,
}

fn build_subs_firehose(n: usize) -> Vec<ClientSub> {
    (0..n)
        .map(|i| ClientSub {
            sub_id: Arc::from(format!("s{i}")),
            // Empty subscription — matches every event ("firehose" client).
            filters: Arc::from(vec![NostrSubscription::default()].into_boxed_slice()),
        })
        .collect()
}

fn build_subs_kinds_disjoint(n: usize) -> Vec<ClientSub> {
    // Each sub asks for a single kind drawn from a wide pool; only the kind-1
    // shard hits. `complete_event` still iterates every sub.
    (0..n)
        .map(|i| {
            let kind = (i as u32 % 64) + 100; // none equal 1
            let f = NostrSubscription::new().kinds(vec![kind]);
            ClientSub {
                sub_id: Arc::from(format!("s{i}")),
                filters: Arc::from(vec![f].into_boxed_slice()),
            }
        })
        .collect()
}

/// **Reparse path** — the body of `complete_event` *before* the verify
/// worker started shipping `MatchView` snapshots back. Re-parses the JSON
/// on every event then runs `matches_view` against the borrowed view.
/// Kept as an A/B baseline for the snapshot path below.
fn match_loop_reparse(note_bytes: &Arc<[u8]>, subs: &[ClientSub]) -> usize {
    let view: NostrNoteView<'_> = match serde_json::from_slice(note_bytes) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let mut delivered = 0usize;
    for cs in subs {
        for filter in cs.filters.iter() {
            if matches_view(&view, filter) {
                let sub_id = Arc::clone(&cs.sub_id);
                let bytes = Arc::clone(note_bytes);
                black_box((sub_id, bytes));
                delivered += 1;
                break;
            }
        }
    }
    delivered
}

/// **Snapshot path** — the body of `complete_event` *after* the verify
/// worker started shipping `MatchView` back. The snapshot is built once
/// on the worker (cost amortized against schnorr verify there) and the
/// shard's matcher loop walks owned strings without parsing JSON. This
/// is the path live fan-out actually exercises today.
///
/// The "writer push" is modeled as `black_box((sub_id, note_bytes))` —
/// same `Arc::clone` cost the real code pays before the WriteCmd lands
/// in the ring. We don't hit a ring here because that's measured by
/// `write_ring_topology`; mixing the two would muddy the signal.
fn match_loop_snapshot(view: &MatchView, note_bytes: &Arc<[u8]>, subs: &[ClientSub]) -> usize {
    let mut delivered = 0usize;
    for cs in subs {
        for filter in cs.filters.iter() {
            if matches_match_view(view, filter) {
                let sub_id = Arc::clone(&cs.sub_id);
                let bytes = Arc::clone(note_bytes);
                black_box((sub_id, bytes));
                delivered += 1;
                break;
            }
        }
    }
    delivered
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout_match");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));

    let note_bytes = presign_one();
    // Build the snapshot once — same lifecycle as the real path, where
    // the verify worker builds it before pushing the `VerifyResult`.
    let parsed: NostrNoteView<'_> =
        serde_json::from_slice(&note_bytes).expect("parse for snapshot");
    let id_arc: Arc<str> = parsed
        .id
        .as_ref()
        .map(|c| Arc::<str>::from(c.as_ref()))
        .unwrap_or_else(|| Arc::<str>::from(""));
    let match_view = build_match_view(&parsed, id_arc);

    for &n in &[64usize, 256, 1024, 4096] {
        group.throughput(Throughput::Elements(1));

        let subs_fire = build_subs_firehose(n);
        group.bench_with_input(BenchmarkId::new("reparse_firehose", n), &n, |b, _| {
            b.iter(|| {
                let d = match_loop_reparse(black_box(&note_bytes), black_box(&subs_fire));
                black_box(d);
            });
        });
        group.bench_with_input(BenchmarkId::new("snapshot_firehose", n), &n, |b, _| {
            b.iter(|| {
                let d = match_loop_snapshot(
                    black_box(&match_view),
                    black_box(&note_bytes),
                    black_box(&subs_fire),
                );
                black_box(d);
            });
        });

        let subs_miss = build_subs_kinds_disjoint(n);
        group.bench_with_input(BenchmarkId::new("reparse_kinds_disjoint", n), &n, |b, _| {
            b.iter(|| {
                let d = match_loop_reparse(black_box(&note_bytes), black_box(&subs_miss));
                black_box(d);
            });
        });
        group.bench_with_input(BenchmarkId::new("snapshot_kinds_disjoint", n), &n, |b, _| {
            b.iter(|| {
                let d = match_loop_snapshot(
                    black_box(&match_view),
                    black_box(&note_bytes),
                    black_box(&subs_miss),
                );
                black_box(d);
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
