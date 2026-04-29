use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use nostro2::{NostrClientEvent, NostrNote, NostrRelayEvent, NostrSubscription, NostrTags, RelayEventTag};
use std::collections::BTreeMap;

/// Build a `NostrTags` from row literals — replaces the dropped
/// `From<Vec<Vec<String>>>` impl that benches used to call.
fn tags_from_rows<I, R, S>(rows: I) -> NostrTags
where
    I: IntoIterator<Item = R>,
    R: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut tags = NostrTags::new();
    for row in rows {
        tags.add_row(row.into_iter().map(Into::into));
    }
    tags
}

/// Helper to create a sample note for benchmarking
fn create_sample_note() -> NostrNote {
    NostrNote {
        id: Some("abc123def456".to_string()),
        pubkey: "deadbeef".repeat(8),
        created_at: 1234567890,
        kind: 1,
        tags: tags_from_rows([["e", "event_id"], ["p", "pubkey"]]),
        content: "Hello Nostr! This is a test message.".to_string(),
        sig: Some("signature".repeat(16)),
    }
}

/// Helper to create a sample subscription
fn create_sample_subscription() -> NostrSubscription {
    NostrSubscription {
        authors: Some(vec!["author1".to_string(), "author2".to_string()]),
        ids: Some(vec!["id1".to_string(), "id2".to_string()]),
        kinds: Some(vec![1, 2, 3]),
        since: Some(1234567890),
        until: Some(9876543210),
        limit: Some(100),
        tags: {
            let mut tags = BTreeMap::new();
            tags.insert("e".to_string(), vec!["event1".to_string()]);
            tags.insert("p".to_string(), vec!["pubkey1".to_string()]);
            Some(tags)
        },
    }
}

fn bench_client_event_serialization(c: &mut Criterion) {
    let note = create_sample_note();
    let subscription = create_sample_subscription();

    let mut group = c.benchmark_group("client_event_serialization");

    // Benchmark SendNoteEvent serialization
    group.bench_function("send_note", |b| {
        b.iter(|| {
            let event: NostrClientEvent = black_box(note.clone()).into();
            serde_json::to_string(&event).unwrap()
        });
    });

    // Benchmark Subscribe serialization
    group.bench_function("subscribe", |b| {
        b.iter(|| {
            let event: NostrClientEvent = black_box(subscription.clone()).into();
            serde_json::to_string(&event).unwrap()
        });
    });

    // Benchmark CloseSubscriptionEvent serialization
    group.bench_function("close_subscription", |b| {
        b.iter(|| {
            let event = NostrClientEvent::close_subscription("sub_id");
            serde_json::to_string(&event).unwrap()
        });
    });

    group.finish();
}

fn bench_relay_event_serialization(c: &mut Criterion) {
    let note = create_sample_note();

    let mut group = c.benchmark_group("relay_event_serialization");

    // Benchmark NewNote serialization
    group.bench_function("new_note", |b| {
        b.iter(|| {
            let event = NostrRelayEvent::NewNote(
                RelayEventTag::Event,
                "sub_id".to_string(),
                black_box(note.clone()),
            );
            serde_json::to_string(&event).unwrap()
        });
    });

    // Benchmark SentOk serialization
    group.bench_function("sent_ok", |b| {
        b.iter(|| {
            let event = NostrRelayEvent::SentOk(
                RelayEventTag::Ok,
                "event_id".to_string(),
                true,
                "OK".to_string(),
            );
            serde_json::to_string(&event).unwrap()
        });
    });

    // Benchmark EndOfSubscription serialization
    group.bench_function("eose", |b| {
        b.iter(|| {
            let event =
                NostrRelayEvent::EndOfSubscription(RelayEventTag::Eose, "sub_id".to_string());
            serde_json::to_string(&event).unwrap()
        });
    });

    // Benchmark Notice serialization
    group.bench_function("notice", |b| {
        b.iter(|| {
            let event = NostrRelayEvent::Notice(
                RelayEventTag::Notice,
                "This is a notice message".to_string(),
            );
            serde_json::to_string(&event).unwrap()
        });
    });

    group.finish();
}

fn bench_client_event_deserialization(c: &mut Criterion) {
    let note = create_sample_note();
    let subscription = create_sample_subscription();

    // Pre-serialize events
    let send_note_json = serde_json::to_string(&NostrClientEvent::from(note.clone())).unwrap();
    let subscribe_json =
        serde_json::to_string(&NostrClientEvent::from(subscription.clone())).unwrap();
    let close_json =
        serde_json::to_string(&NostrClientEvent::close_subscription("sub_id")).unwrap();

    let mut group = c.benchmark_group("client_event_deserialization");

    group.bench_function("send_note", |b| {
        b.iter(|| serde_json::from_str::<NostrClientEvent>(black_box(&send_note_json)).unwrap());
    });

    group.bench_function("subscribe", |b| {
        b.iter(|| serde_json::from_str::<NostrClientEvent>(black_box(&subscribe_json)).unwrap());
    });

    group.bench_function("close_subscription", |b| {
        b.iter(|| serde_json::from_str::<NostrClientEvent>(black_box(&close_json)).unwrap());
    });

    group.finish();
}

fn bench_relay_event_deserialization(c: &mut Criterion) {
    let note = create_sample_note();

    // Pre-serialize events
    let new_note_json = serde_json::to_string(&NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        note.clone(),
    ))
    .unwrap();
    let sent_ok_json = serde_json::to_string(&NostrRelayEvent::SentOk(
        RelayEventTag::Ok,
        "event_id".to_string(),
        true,
        "OK".to_string(),
    ))
    .unwrap();
    let eose_json = serde_json::to_string(&NostrRelayEvent::EndOfSubscription(
        RelayEventTag::Eose,
        "sub_id".to_string(),
    ))
    .unwrap();
    let notice_json = serde_json::to_string(&NostrRelayEvent::Notice(
        RelayEventTag::Notice,
        "This is a notice message".to_string(),
    ))
    .unwrap();

    let mut group = c.benchmark_group("relay_event_deserialization");

    group.bench_function("new_note", |b| {
        b.iter(|| serde_json::from_str::<NostrRelayEvent>(black_box(&new_note_json)).unwrap());
    });

    group.bench_function("sent_ok", |b| {
        b.iter(|| serde_json::from_str::<NostrRelayEvent>(black_box(&sent_ok_json)).unwrap());
    });

    group.bench_function("eose", |b| {
        b.iter(|| serde_json::from_str::<NostrRelayEvent>(black_box(&eose_json)).unwrap());
    });

    group.bench_function("notice", |b| {
        b.iter(|| serde_json::from_str::<NostrRelayEvent>(black_box(&notice_json)).unwrap());
    });

    group.finish();
}

fn bench_roundtrip_serialization(c: &mut Criterion) {
    let note = create_sample_note();
    let subscription = create_sample_subscription();

    let mut group = c.benchmark_group("roundtrip");

    // Benchmark full roundtrip: serialize then deserialize
    group.bench_function("client_send_note", |b| {
        b.iter(|| {
            let event: NostrClientEvent = black_box(note.clone()).into();
            let json = serde_json::to_string(&event).unwrap();
            serde_json::from_str::<NostrClientEvent>(&json).unwrap()
        });
    });

    group.bench_function("client_subscribe", |b| {
        b.iter(|| {
            let event: NostrClientEvent = black_box(subscription.clone()).into();
            let json = serde_json::to_string(&event).unwrap();
            serde_json::from_str::<NostrClientEvent>(&json).unwrap()
        });
    });

    group.bench_function("relay_new_note", |b| {
        b.iter(|| {
            let event = NostrRelayEvent::NewNote(
                RelayEventTag::Event,
                "sub_id".to_string(),
                black_box(note.clone()),
            );
            let json = serde_json::to_string(&event).unwrap();
            serde_json::from_str::<NostrRelayEvent>(&json).unwrap()
        });
    });

    group.finish();
}

fn bench_varying_note_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("note_size_serialization");

    for size in [10, 100, 1000, 5000].iter() {
        let content = "x".repeat(*size);
        let note = NostrNote {
            id: Some("abc123".to_string()),
            pubkey: "deadbeef".repeat(8),
            created_at: 1234567890,
            kind: 1,
            tags: tags_from_rows([["e", "event_id"]]),
            content,
            sig: Some("sig".repeat(16)),
        };

        group.bench_with_input(BenchmarkId::new("serialize", size), size, |b, _| {
            b.iter(|| {
                let event: NostrClientEvent = black_box(note.clone()).into();
                serde_json::to_string(&event).unwrap()
            });
        });
    }

    group.finish();
}

criterion_group!(
    serialization_benches,
    bench_client_event_serialization,
    bench_relay_event_serialization,
    bench_client_event_deserialization,
    bench_relay_event_deserialization,
    bench_roundtrip_serialization,
    bench_varying_note_sizes,
);
criterion_main!(serialization_benches);
