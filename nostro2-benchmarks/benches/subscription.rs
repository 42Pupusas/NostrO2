use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::{NostrNote, NostrSubscription};

/// Create a variety of notes for filtering benchmarks
fn create_test_notes(count: usize) -> Vec<NostrNote> {
    (0..count)
        .map(|i| NostrNote {
            id: Some(format!("event_{:016x}", i)),
            pubkey: format!("pubkey_{}", i % 10), // 10 different pubkeys
            created_at: 1234567890 + i as i64,
            kind: if i % 3 == 0 { 1 } else { 2 },
            tags: vec![vec!["e".to_string(), format!("ref_{}", i)]].into(),
            content: format!("Message {}", i),
            sig: Some("sig".repeat(16)),
        })
        .collect()
}

fn bench_filter_by_author(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_by_author", |b| {
        b.iter(|| {
            let subscription = NostrSubscription {
                authors: Some(vec!["pubkey_5".to_string()]),
                ..Default::default()
            };

            let filtered: Vec<_> = notes
                .iter()
                .filter(|note| {
                    if let Some(authors) = &subscription.authors {
                        authors.contains(&note.pubkey)
                    } else {
                        true
                    }
                })
                .collect();

            black_box(filtered.len())
        });
    });
}

fn bench_filter_by_kind(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_by_kind", |b| {
        b.iter(|| {
            let subscription = NostrSubscription {
                kinds: Some(vec![1]),
                ..Default::default()
            };

            let filtered: Vec<_> = notes
                .iter()
                .filter(|note| {
                    if let Some(kinds) = &subscription.kinds {
                        kinds.contains(&note.kind)
                    } else {
                        true
                    }
                })
                .collect();

            black_box(filtered.len())
        });
    });
}

fn bench_filter_by_timestamp(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_by_timestamp", |b| {
        b.iter(|| {
            let subscription = NostrSubscription {
                since: Some(1234567890 + 500),
                until: Some(1234567890 + 700),
                ..Default::default()
            };

            let filtered: Vec<_> = notes
                .iter()
                .filter(|note| {
                    let created_at = note.created_at;
                    if let Some(since) = subscription.since {
                        if created_at < since as i64 {
                            return false;
                        }
                    }
                    if let Some(until) = subscription.until {
                        if created_at > until as i64 {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            black_box(filtered.len())
        });
    });
}

fn bench_filter_by_ids(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_by_ids", |b| {
        b.iter(|| {
            let subscription = NostrSubscription {
                ids: Some(vec![
                    format!("event_{:016x}", 100),
                    format!("event_{:016x}", 200),
                    format!("event_{:016x}", 300),
                ]),
                ..Default::default()
            };

            let filtered: Vec<_> = notes
                .iter()
                .filter(|note| {
                    if let (Some(ids), Some(id)) = (&subscription.ids, &note.id) {
                        ids.contains(id)
                    } else {
                        true
                    }
                })
                .collect();

            black_box(filtered.len())
        });
    });
}

fn bench_filter_multi(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_multi", |b| {
        b.iter(|| {
            let subscription = NostrSubscription {
                authors: Some(vec!["pubkey_3".to_string(), "pubkey_7".to_string()]),
                kinds: Some(vec![1]),
                since: Some(1234567890 + 100),
                ..Default::default()
            };

            let filtered: Vec<_> = notes
                .iter()
                .filter(|note| {
                    // Filter by authors
                    if let Some(authors) = &subscription.authors {
                        if !authors.contains(&note.pubkey) {
                            return false;
                        }
                    }
                    // Filter by kinds
                    if let Some(kinds) = &subscription.kinds {
                        if !kinds.contains(&note.kind) {
                            return false;
                        }
                    }
                    // Filter by time
                    let created_at = note.created_at;
                    if let Some(since) = subscription.since {
                        if created_at < since as i64 {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            black_box(filtered.len())
        });
    });
}

fn bench_filter_with_limit(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_with_limit", |b| {
        b.iter(|| {
            let subscription = NostrSubscription {
                kinds: Some(vec![1]),
                limit: Some(50),
                ..Default::default()
            };

            let filtered: Vec<_> = notes
                .iter()
                .filter(|note| {
                    if let Some(kinds) = &subscription.kinds {
                        kinds.contains(&note.kind)
                    } else {
                        true
                    }
                })
                .take(subscription.limit.unwrap_or(u32::MAX) as usize)
                .collect();

            black_box(filtered.len())
        });
    });
}

fn bench_empty_filter(c: &mut Criterion) {
    let notes = create_test_notes(1000);

    c.bench_function("filter_empty", |b| {
        b.iter(|| {
            let _subscription = NostrSubscription::default();

            let filtered: Vec<_> = notes
                .iter()
                .filter(|_note| {
                    // Empty filter matches everything
                    true
                })
                .collect();

            black_box(filtered.len())
        });
    });
}

criterion_group!(
    subscription_benches,
    bench_filter_by_author,
    bench_filter_by_kind,
    bench_filter_by_timestamp,
    bench_filter_by_ids,
    bench_filter_multi,
    bench_filter_with_limit,
    bench_empty_filter,
);
criterion_main!(subscription_benches);
