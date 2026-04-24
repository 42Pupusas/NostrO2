use nostro2::NostrSigner;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use nostr::JsonUtil;

// ── Helpers ────────────────────────────────────────────────────────

/// Create a signed nostro2 note for benchmarking
fn nostro2_signed_note() -> (nostro2_signer::K256Keypair, nostro2::NostrNote) {
    let kp = nostro2_signer::K256Keypair::generate();
    let mut note =
        nostro2::NostrNote::text_note("Hello Nostr! Benchmarking against the nostr crate.");
    kp.sign_nostr_note(&mut note).expect("nostro2 signing failed");
    (kp, note)
}

/// Create a signed nostr-crate event for benchmarking
fn nostr_signed_event() -> (nostr::Keys, nostr::Event) {
    let keys = nostr::Keys::generate();
    let event =
        nostr::EventBuilder::text_note("Hello Nostr! Benchmarking against the nostr crate.")
            .sign_with_keys(&keys)
            .expect("nostr signing failed");
    (keys, event)
}

// ── Key Generation ─────────────────────────────────────────────────

fn bench_keygen(c: &mut Criterion) {
    let mut group = c.benchmark_group("keygen");

    group.bench_function("nostro2", |b| {
        b.iter(|| black_box(nostro2_signer::K256Keypair::generate()));
    });

    group.bench_function("nostr", |b| {
        b.iter(|| black_box(nostr::Keys::generate()));
    });

    group.finish();
}

// ── Signing ────────────────────────────────────────────────────────

fn bench_signing(c: &mut Criterion) {
    let nostro2_kp = nostro2_signer::K256Keypair::generate();
    let nostr_keys = nostr::Keys::generate();

    let mut group = c.benchmark_group("signing");

    group.bench_function("nostro2", |b| {
        b.iter(|| {
            let mut note = nostro2::NostrNote::text_note("Benchmark signing");
            nostro2_kp.sign_nostr_note(black_box(&mut note)).unwrap();
        });
    });

    group.bench_function("nostr", |b| {
        b.iter(|| {
            black_box(
                nostr::EventBuilder::text_note("Benchmark signing")
                    .sign_with_keys(&nostr_keys)
                    .unwrap(),
            );
        });
    });

    group.finish();
}

// ── Verification ───────────────────────────────────────────────────

fn bench_verification(c: &mut Criterion) {
    let (_, nostro2_note) = nostro2_signed_note();
    let (_, nostr_event) = nostr_signed_event();

    assert!(nostro2_note.verify(), "nostro2 verify sanity check");
    assert!(nostr_event.verify().is_ok(), "nostr verify sanity check");

    let mut group = c.benchmark_group("verification");

    group.bench_function("nostro2", |b| {
        b.iter(|| black_box(&nostro2_note).verify());
    });

    group.bench_function("nostr", |b| {
        b.iter(|| black_box(&nostr_event).verify());
    });

    group.finish();
}

// ── Serialization (Event → JSON) ──────────────────────────────────

fn bench_serialization(c: &mut Criterion) {
    let (_, nostro2_note) = nostro2_signed_note();
    let (_, nostr_event) = nostr_signed_event();

    let mut group = c.benchmark_group("event_serialize");

    group.bench_function("nostro2", |b| {
        b.iter(|| serde_json::to_string(black_box(&nostro2_note)).unwrap());
    });

    group.bench_function("nostr", |b| {
        b.iter(|| black_box(&nostr_event).as_json());
    });

    group.finish();
}

// ── Deserialization (JSON → Event) ─────────────────────────────────

fn bench_deserialization(c: &mut Criterion) {
    let (_, nostro2_note) = nostro2_signed_note();
    let (_, nostr_event) = nostr_signed_event();

    let nostro2_json = serde_json::to_string(&nostro2_note).unwrap();
    let nostr_json = nostr_event.as_json();

    let mut group = c.benchmark_group("event_deserialize");

    group.bench_function("nostro2", |b| {
        b.iter(|| serde_json::from_str::<nostro2::NostrNote>(black_box(&nostro2_json)).unwrap());
    });

    group.bench_function("nostr", |b| {
        b.iter(|| nostr::Event::from_json(black_box(&nostr_json)).unwrap());
    });

    group.finish();
}

// ── Note/Event Construction (unsigned) ─────────────────────────────

fn bench_note_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("note_construction");

    group.bench_function("nostro2", |b| {
        b.iter(|| {
            black_box(
                nostro2::NostrNote::builder()
                    .content("Hello, Nostr!")
                    .kind(1)
                    .tag_pubkey("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
                    .tag_event("cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe")
                    .tag("t", "benchmark")
                    .build(),
            );
        });
    });

    group.bench_function("nostr", |b| {
        b.iter(|| {
            black_box(
                nostr::EventBuilder::new(nostr::Kind::TextNote, "Hello, Nostr!")
                    .tag(nostr::Tag::public_key(
                        nostr::PublicKey::from_hex(
                            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                        )
                        .unwrap(),
                    ))
                    .tag(nostr::Tag::event(
                        nostr::EventId::from_hex(
                            "cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe",
                        )
                        .unwrap(),
                    ))
                    .tag(nostr::Tag::hashtag("benchmark")),
            );
        });
    });

    group.finish();
}

// ── Filter/Subscription Construction ───────────────────────────────

fn bench_filter_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("filter_construction");

    group.bench_function("nostro2", |b| {
        b.iter(|| {
            black_box(nostro2::NostrSubscription {
                kinds: Some(vec![1, 4, 30023]),
                authors: Some(vec![
                    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
                    "cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe".to_string(),
                ]),
                limit: Some(100),
                since: Some(1_700_000_000),
                until: Some(1_800_000_000),
                ..Default::default()
            });
        });
    });

    let pk1 = nostr::PublicKey::from_hex(
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    )
    .unwrap();
    let pk2 = nostr::PublicKey::from_hex(
        "cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe",
    )
    .unwrap();

    group.bench_function("nostr", |b| {
        b.iter(|| {
            black_box(
                nostr::Filter::new()
                    .kinds([
                        nostr::Kind::TextNote,
                        nostr::Kind::EncryptedDirectMessage,
                        nostr::Kind::Custom(30023),
                    ])
                    .authors([pk1, pk2])
                    .limit(100)
                    .since(nostr::Timestamp::from(1_700_000_000))
                    .until(nostr::Timestamp::from(1_800_000_000)),
            );
        });
    });

    group.finish();
}

// ── Varying Content Sizes ──────────────────────────────────────────

fn bench_serialization_varying_sizes(c: &mut Criterion) {
    let nostro2_kp = nostro2_signer::K256Keypair::generate();
    let nostr_keys = nostr::Keys::generate();

    let mut group = c.benchmark_group("serialize_by_content_size");

    for size in [64, 256, 1024, 4096] {
        let content = "x".repeat(size);

        // Prepare signed nostro2 note
        let mut nostro2_note = nostro2::NostrNote::text_note(&content);
        nostro2_kp.sign_nostr_note(&mut nostro2_note).unwrap();

        // Prepare signed nostr event
        let nostr_event = nostr::EventBuilder::text_note(&content)
            .sign_with_keys(&nostr_keys)
            .unwrap();

        group.bench_with_input(BenchmarkId::new("nostro2", size), &size, |b, _| {
            b.iter(|| serde_json::to_string(black_box(&nostro2_note)).unwrap());
        });

        group.bench_with_input(BenchmarkId::new("nostr", size), &size, |b, _| {
            b.iter(|| black_box(&nostr_event).as_json());
        });
    }

    group.finish();
}

// ── Full Roundtrip (create → sign → serialize → deserialize → verify) ──

fn bench_full_roundtrip(c: &mut Criterion) {
    let nostro2_kp = nostro2_signer::K256Keypair::generate();
    let nostr_keys = nostr::Keys::generate();

    let mut group = c.benchmark_group("full_roundtrip");

    group.bench_function("nostro2", |b| {
        b.iter(|| {
            let mut note = nostro2::NostrNote::text_note("Roundtrip benchmark");
            nostro2_kp.sign_nostr_note(&mut note).unwrap();
            let json = serde_json::to_string(&note).unwrap();
            let deserialized: nostro2::NostrNote = serde_json::from_str(&json).unwrap();
            assert!(deserialized.verify());
        });
    });

    group.bench_function("nostr", |b| {
        b.iter(|| {
            let event = nostr::EventBuilder::text_note("Roundtrip benchmark")
                .sign_with_keys(&nostr_keys)
                .unwrap();
            let json = event.as_json();
            let deserialized = nostr::Event::from_json(&json).unwrap();
            assert!(deserialized.verify().is_ok());
        });
    });

    group.finish();
}

criterion_group!(
    nostr_comparison_benches,
    bench_keygen,
    bench_signing,
    bench_verification,
    bench_serialization,
    bench_deserialization,
    bench_note_construction,
    bench_filter_construction,
    bench_serialization_varying_sizes,
    bench_full_roundtrip,
);
criterion_main!(nostr_comparison_benches);
