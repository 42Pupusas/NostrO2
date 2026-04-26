//! Pure-function bench for `filter::matches`.
//!
//! Isolates the per-call matching cost. The end-to-end fan-out cost is
//! `matches_per_event × time_per_match`; this bench measures the second
//! factor so algorithmic wins (sub indexing, narrowest-field pruning)
//! and per-call wins (tag scan layout) can be separated.

use criterion::{Criterion, criterion_group, criterion_main};
use nostro2::{NostrNote, NostrSubscription};
use ring_relay_nostr::matches;
use std::hint::black_box;

fn note_for_match() -> NostrNote {
    let mut note = NostrNote {
        pubkey: "a".repeat(64),
        created_at: 1_700_000_000,
        kind: 1,
        content: "bench content, medium length to be realistic".into(),
        id: Some("b".repeat(64)),
        sig: Some("c".repeat(128)),
        ..Default::default()
    };
    // Give it a handful of tags — realistic for a kind-1 note.
    note.tags.add_custom_tag("t", "nostr");
    note.tags.add_custom_tag("t", "bench");
    note.tags.add_pubkey_tag(&"d".repeat(64), None);
    note.tags.add_event_tag(&"e".repeat(64));
    note
}

fn bench_kind_filter(c: &mut Criterion) {
    let note = note_for_match();
    let filter = NostrSubscription::new().kinds(vec![1, 7, 30023]);
    c.bench_function("filter/kind_hit", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter)));
    });

    let miss = NostrSubscription::new().kinds(vec![6, 7, 42]);
    c.bench_function("filter/kind_miss", |b| {
        b.iter(|| matches(black_box(&note), black_box(&miss)));
    });
}

fn bench_authors_filter(c: &mut Criterion) {
    let note = note_for_match();

    // Small authors list — typical follow list stub.
    let small: Vec<String> = (0..10)
        .map(|i| format!("{i:064}"))
        .chain(std::iter::once(note.pubkey.clone()))
        .collect();
    let filter_small = NostrSubscription::new().authors(small);
    c.bench_function("filter/authors_10_hit_last", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter_small)));
    });

    // Large authors list — firehose-ish follow set; worst case is author at end.
    let mut big: Vec<String> = (0..1000).map(|i| format!("{i:064}")).collect();
    big.push(note.pubkey.clone());
    let filter_big = NostrSubscription::new().authors(big);
    c.bench_function("filter/authors_1000_hit_last", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter_big)));
    });

    let miss: Vec<String> = (0..1000).map(|i| format!("{i:064}")).collect();
    let filter_miss = NostrSubscription::new().authors(miss);
    c.bench_function("filter/authors_1000_miss", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter_miss)));
    });
}

fn bench_tag_filter(c: &mut Criterion) {
    let note = note_for_match();
    let filter_hit = NostrSubscription::new().tag("#t", "nostr");
    c.bench_function("filter/tag_hit", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter_hit)));
    });

    let filter_miss = NostrSubscription::new().tag("#t", "bitcoin");
    c.bench_function("filter/tag_miss", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter_miss)));
    });
}

fn bench_empty_filter(c: &mut Criterion) {
    // Firehose filter — matches everything.
    let note = note_for_match();
    let filter = NostrSubscription::default();
    c.bench_function("filter/firehose", |b| {
        b.iter(|| matches(black_box(&note), black_box(&filter)));
    });
}

criterion_group!(
    benches,
    bench_kind_filter,
    bench_authors_filter,
    bench_tag_filter,
    bench_empty_filter
);
criterion_main!(benches);
