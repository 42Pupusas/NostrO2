//! Microbenchmarks for `NostrSubscription::matches`.
//!
//! Previously these benches reimplemented the filter logic in closures —
//! measuring `Vec::contains` rather than anything `nostro2` ships. Now they
//! exercise the library's matcher on a 1000-note workload, so a regression
//! in `subscriptions::matches` (or in the underlying `NostrTags::iter`)
//! actually shows up here.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::{NostrNote, NostrSubscription};

fn create_test_notes(count: usize) -> Vec<NostrNote> {
    (0..count)
        .map(|i| {
            let mut note = NostrNote {
                id: Some(format!("event_{i:016x}")),
                pubkey: format!("pubkey_{}", i % 10), // 10 distinct pubkeys
                created_at: 1_234_567_890 + i as i64,
                kind: if i % 3 == 0 { 1 } else { 2 },
                content: format!("Message {i}"),
                sig: Some("sig".repeat(16)),
                ..Default::default()
            };
            note.tags.add_event_tag(&format!("ref_{i}"));
            // Sprinkle a couple of `t` tags so tag filters have something
            // to match against without making every note identical.
            if i % 5 == 0 {
                note.tags.add_custom_tag("t", "nostr");
            }
            note
        })
        .collect()
}

fn bench_filter_by_author(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::new().author("pubkey_5");
    c.bench_function("filter_by_author", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });
}

fn bench_filter_by_kind(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::new().kind(1);
    c.bench_function("filter_by_kind", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });
}

fn bench_filter_by_timestamp(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::new()
        .since(1_234_567_890 + 500)
        .until(1_234_567_890 + 700);
    c.bench_function("filter_by_timestamp", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });
}

fn bench_filter_by_ids(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::new()
        .id(format!("event_{:016x}", 100))
        .id(format!("event_{:016x}", 200))
        .id(format!("event_{:016x}", 300));
    c.bench_function("filter_by_ids", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });
}

fn bench_filter_multi(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::new()
        .author("pubkey_3")
        .author("pubkey_7")
        .kind(1)
        .since(1_234_567_890 + 100);
    c.bench_function("filter_multi", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });
}

fn bench_filter_with_tag(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::new().kind(1).tag("#t", "nostr");
    c.bench_function("filter_with_tag", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });
}

fn bench_filter_empty(c: &mut Criterion) {
    let notes = create_test_notes(1000);
    let f = NostrSubscription::default();
    let mut group = c.benchmark_group("filter_empty");

    // Naive path: caller blindly runs `matches` on every note even though
    // the filter is wildcard. This is the hot path the `is_wildcard` fast
    // path inside `matches` is meant to short-circuit — but the floor is
    // still ~2-3ns per note for the iterator + branch cost.
    group.bench_function("matches_per_note", |b| {
        b.iter(|| {
            let n: usize = notes.iter().filter(|n| f.matches(n)).count();
            black_box(n)
        });
    });

    // Smart caller: pre-checks `is_wildcard` once and skips the filter
    // entirely. This is what relays/caches should actually do — the
    // ~2-3ns/note of `Iterator::filter` overhead disappears completely.
    group.bench_function("is_wildcard_then_skip", |b| {
        b.iter(|| {
            let n: usize = if f.is_wildcard() {
                notes.len()
            } else {
                notes.iter().filter(|n| f.matches(n)).count()
            };
            black_box(n)
        });
    });
    group.finish();
}

criterion_group!(
    subscription_benches,
    bench_filter_by_author,
    bench_filter_by_kind,
    bench_filter_by_timestamp,
    bench_filter_by_ids,
    bench_filter_multi,
    bench_filter_with_tag,
    bench_filter_empty,
);
criterion_main!(subscription_benches);
