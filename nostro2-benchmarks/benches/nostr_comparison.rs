//! `nostro2` vs upstream `nostr` crate.
//!
//! Single source of truth for "are we faster / slower than the canonical
//! Rust Nostr library on the operations we care about." Each criterion
//! group runs both impls back-to-back so the report tables show them
//! side-by-side.
//!
//! Coverage map — keep this honest if you add or remove benches:
//!
//! - keygen / signing / verification           (per-op crypto)
//! - event JSON serialize / deserialize        (wire format)
//! - subscription filter match (`matches` vs `Filter::match_event`)
//! - zero-copy view parse (`NostrNoteView` vs `nostr::Event::from_json`)
//! - tag construction (flat-cells `add_pubkey_tag` vs `Tag::public_key`)
//! - NIP-44 encrypt / decrypt                  (DM crypto path)
//! - varying content sizes (serialize)
//! - full round-trip                           (end-to-end)
//!
//! Run with `cargo bench -p nostro2-benchmarks`. To save a baseline before a
//! refactor and diff after, see Criterion's `--save-baseline` / `--baseline`
//! workflow.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use nostr::JsonUtil;
use nostro2::{NostrKeypair as _, NostrSigner as _};

// Curve-backend selector. The harness is generic over which `nostro2-signer`
// keypair is active — flip the feature on this crate, not the bench code.
#[cfg(feature = "k256")]
use nostro2_signer::K256Keypair as Nostro2Keypair;
#[cfg(feature = "secp256k1")]
use nostro2_signer::Secp256k1Keypair as Nostro2Keypair;

#[cfg(feature = "k256")]
const NOSTRO2_BACKEND: &str = "nostro2_k256";
#[cfg(feature = "secp256k1")]
const NOSTRO2_BACKEND: &str = "nostro2_secp256k1";

// ── Helpers ────────────────────────────────────────────────────────

fn nostro2_signed_note() -> (Nostro2Keypair, nostro2::NostrNote) {
    let kp = Nostro2Keypair::generate();
    let mut note = nostro2::NostrNote::text_note("Hello Nostr! Benchmark vs the nostr crate.");
    note.sign_with(&kp).expect("nostro2 signing failed");
    (kp, note)
}

fn nostr_signed_event() -> (nostr::Keys, nostr::Event) {
    let keys = nostr::Keys::generate();
    let event = nostr::EventBuilder::text_note("Hello Nostr! Benchmark vs the nostr crate.")
        .sign_with_keys(&keys)
        .expect("nostr signing failed");
    (keys, event)
}

// ── Key Generation ─────────────────────────────────────────────────

fn bench_keygen(c: &mut Criterion) {
    let mut group = c.benchmark_group("keygen");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| black_box(Nostro2Keypair::generate()));
    });
    group.bench_function("nostr", |b| {
        b.iter(|| black_box(nostr::Keys::generate()));
    });
    group.finish();
}

// ── Signing ────────────────────────────────────────────────────────

fn bench_signing(c: &mut Criterion) {
    let kp = Nostro2Keypair::generate();
    let keys = nostr::Keys::generate();
    let mut group = c.benchmark_group("signing");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| {
            let mut note = nostro2::NostrNote::text_note("Benchmark signing");
            note.sign_with(black_box(&kp)).unwrap();
        });
    });
    group.bench_function("nostr", |b| {
        b.iter(|| {
            black_box(
                nostr::EventBuilder::text_note("Benchmark signing")
                    .sign_with_keys(&keys)
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
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| black_box(&nostro2_note).verify());
    });
    group.bench_function("nostr", |b| {
        b.iter(|| black_box(&nostr_event).verify());
    });
    group.finish();
}

// ── Serialize event → JSON ────────────────────────────────────────

fn bench_serialization(c: &mut Criterion) {
    let (_, nostro2_note) = nostro2_signed_note();
    let (_, nostr_event) = nostr_signed_event();
    let mut group = c.benchmark_group("event_serialize");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| serde_json::to_string(black_box(&nostro2_note)).unwrap());
    });
    group.bench_function("nostr", |b| {
        b.iter(|| black_box(&nostr_event).as_json());
    });
    group.finish();
}

// ── Deserialize JSON → event ───────────────────────────────────────

fn bench_deserialization(c: &mut Criterion) {
    let (_, nostro2_note) = nostro2_signed_note();
    let (_, nostr_event) = nostr_signed_event();
    let nostro2_json = serde_json::to_string(&nostro2_note).unwrap();
    let nostr_json = nostr_event.as_json();

    let mut group = c.benchmark_group("event_deserialize");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| serde_json::from_str::<nostro2::NostrNote>(black_box(&nostro2_json)).unwrap());
    });
    group.bench_function("nostr", |b| {
        b.iter(|| nostr::Event::from_json(black_box(&nostr_json)).unwrap());
    });
    group.finish();
}

// ── Zero-copy view parse ───────────────────────────────────────────
//
// `NostrNoteView` is the headline allocation win — borrows from the
// source buffer instead of allocating a `String` per field. This
// bench exists to make that visible relative to the `nostr` crate's
// owned `from_json` path; if the view advantage ever regresses, this
// number is the canary.

fn bench_view_parse(c: &mut Criterion) {
    let (_, nostro2_note) = nostro2_signed_note();
    let (_, nostr_event) = nostr_signed_event();
    let nostro2_json = serde_json::to_string(&nostro2_note).unwrap();
    let nostr_json = nostr_event.as_json();

    let mut group = c.benchmark_group("view_parse");
    group.bench_function("nostro2_view", |b| {
        b.iter(|| {
            let view: nostro2::NostrNoteView<'_> =
                serde_json::from_str(black_box(&nostro2_json)).unwrap();
            black_box(view);
        });
    });
    group.bench_function("nostro2_owned", |b| {
        b.iter(|| {
            let owned: nostro2::NostrNote =
                serde_json::from_str(black_box(&nostro2_json)).unwrap();
            black_box(owned);
        });
    });
    group.bench_function("nostr", |b| {
        b.iter(|| nostr::Event::from_json(black_box(&nostr_json)).unwrap());
    });
    group.finish();
}

// ── Filter match (subscription matcher) ───────────────────────────
//
// Both libraries expose a per-event predicate. We feed each 1000 notes
// of mixed pubkey/kind so neither implementation can short-circuit
// after the first event. nostro2's `matches` is the method shipped in
// the previous commit; `Filter::match_event` is what nostr-sdk calls
// internally on every cached event.

fn bench_filter_match(c: &mut Criterion) {
    // Build 1000 nostro2 notes.
    let kp = Nostro2Keypair::generate();
    let mut nostro2_notes: Vec<nostro2::NostrNote> = (0..1000)
        .map(|i| {
            let mut n = nostro2::NostrNote::text_note(format!("note {i}"));
            n.kind = if i % 3 == 0 { 1 } else { 7 };
            n.sign_with(&kp).unwrap();
            n
        })
        .collect();
    // Re-stamp pubkey on a third of them so author filter has variance.
    for (i, n) in nostro2_notes.iter_mut().enumerate() {
        if i % 7 == 0 {
            n.pubkey = format!("{:064x}", i);
            n.id = None; // invalidates id, but matcher doesn't recompute it
        }
    }
    let nostro2_filter = nostro2::NostrSubscription::new()
        .kind(1)
        .since(0)
        .until(u64::MAX >> 1);

    // Build the same workload for nostr.
    let nostr_keys = nostr::Keys::generate();
    let nostr_events: Vec<nostr::Event> = (0..1000)
        .map(|i| {
            let kind = if i % 3 == 0 {
                nostr::Kind::TextNote
            } else {
                nostr::Kind::Reaction
            };
            nostr::EventBuilder::new(kind, format!("note {i}"))
                .sign_with_keys(&nostr_keys)
                .unwrap()
        })
        .collect();
    let nostr_filter = nostr::Filter::new().kind(nostr::Kind::TextNote);
    let opts = nostr::filter::MatchEventOptions::default();

    let mut group = c.benchmark_group("filter_match");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| {
            let n: usize = nostro2_notes
                .iter()
                .filter(|note| nostro2_filter.matches(note))
                .count();
            black_box(n)
        });
    });
    group.bench_function("nostr", |b| {
        b.iter(|| {
            let n: usize = nostr_events
                .iter()
                .filter(|ev| nostr_filter.match_event(ev, opts))
                .count();
            black_box(n)
        });
    });
    group.finish();
}

// ── Tag construction ──────────────────────────────────────────────
//
// nostro2 stores tags flat (`Vec<String>` cells + `Vec<u32>` offsets);
// nostr stores them as a `Vec<Tag>` of structured variants. Different
// trade-offs: nostro2 minimises allocations on parse, nostr keeps
// types narrow at construction. Bench builds 8 tag rows per iteration.

fn bench_tag_construction(c: &mut Criterion) {
    let pk_hex = "deadbeef".repeat(8);
    let ev_hex = "cafebabe".repeat(8);

    let mut group = c.benchmark_group("tag_construction");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| {
            let mut tags = nostro2::NostrTags::new();
            tags.add_pubkey_tag(black_box(&pk_hex), None);
            tags.add_event_tag(black_box(&ev_hex));
            tags.add_custom_tag("t", "rust");
            tags.add_custom_tag("t", "nostr");
            tags.add_relay_tag("wss://relay.example.com");
            tags.add_pubkey_tag(black_box(&pk_hex), Some("wss://hint"));
            tags.add_parameter_tag("d-id");
            tags.add_custom_tag("client", "nostro2");
            black_box(tags);
        });
    });
    group.bench_function("nostr", |b| {
        // Pre-parse the hex once so we measure tag construction, not
        // hex decode (which nostr's typed tags force on every push).
        let pk = nostr::PublicKey::from_hex(&pk_hex).unwrap();
        let ev = nostr::EventId::from_hex(&ev_hex).unwrap();
        b.iter(|| {
            let tags = vec![
                nostr::Tag::public_key(pk),
                nostr::Tag::event(ev),
                nostr::Tag::hashtag("rust"),
                nostr::Tag::hashtag("nostr"),
                nostr::Tag::relay_metadata(
                    nostr::RelayUrl::parse("wss://relay.example.com").unwrap(),
                    None,
                ),
                nostr::Tag::public_key(pk),
                nostr::Tag::identifier("d-id"),
                nostr::Tag::custom(nostr::TagKind::Custom("client".into()), ["nostro2"]),
            ];
            black_box(tags);
        });
    });
    group.finish();
}

// ── NIP-44 encrypt / decrypt ──────────────────────────────────────

fn bench_nip44(c: &mut Criterion) {
    use nostro2_nips::Nip44 as _;

    let alice = Nostro2Keypair::generate();
    let bob = Nostro2Keypair::generate();
    let bob_pk = bob.public_key();
    let alice_pk = alice.public_key();
    let plaintext = "Hello, Nostr! NIP-44 round-trip benchmark payload.";

    // Pre-build a ciphertext for the decrypt bench so timing isn't
    // dominated by encrypt+decrypt back-to-back.
    let mut nostro2_note = nostro2::NostrNote {
        kind: 14,
        content: plaintext.into(),
        ..Default::default()
    };
    alice.nip44_encrypt_note(&mut nostro2_note, &bob_pk).unwrap();
    let nostro2_ciphertext = nostro2_note.content.clone();

    // nostr crate side
    let nostr_alice = nostr::Keys::generate();
    let nostr_bob = nostr::Keys::generate();
    let nostr_bob_pk = nostr_bob.public_key();
    let nostr_alice_pk = nostr_alice.public_key();
    let nostr_ciphertext = nostr::nips::nip44::encrypt(
        nostr_alice.secret_key(),
        &nostr_bob_pk,
        plaintext,
        nostr::nips::nip44::Version::V2,
    )
    .unwrap();

    let mut group = c.benchmark_group("nip44_encrypt");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| {
            let mut note = nostro2::NostrNote {
                kind: 14,
                content: plaintext.into(),
                ..Default::default()
            };
            alice
                .nip44_encrypt_note(&mut note, black_box(&bob_pk))
                .unwrap();
            black_box(note);
        });
    });
    group.bench_function("nostr", |b| {
        b.iter(|| {
            black_box(
                nostr::nips::nip44::encrypt(
                    nostr_alice.secret_key(),
                    &nostr_bob_pk,
                    plaintext,
                    nostr::nips::nip44::Version::V2,
                )
                .unwrap(),
            );
        });
    });
    group.finish();

    let mut group = c.benchmark_group("nip44_decrypt");
    let mut nostro2_locked = nostro2::NostrNote {
        kind: 14,
        content: nostro2_ciphertext.clone(),
        ..Default::default()
    };
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| {
            // Reset content to the ciphertext each iteration so the
            // bench is repeatable; decrypt mutates `note.content`.
            nostro2_locked.content = nostro2_ciphertext.clone();
            let pt = bob
                .nip44_decrypt_note(&nostro2_locked, black_box(&alice_pk))
                .unwrap();
            black_box(pt);
        });
    });
    group.bench_function("nostr", |b| {
        b.iter(|| {
            black_box(
                nostr::nips::nip44::decrypt(
                    nostr_bob.secret_key(),
                    &nostr_alice_pk,
                    &nostr_ciphertext,
                )
                .unwrap(),
            );
        });
    });
    group.finish();
}

// ── Varying content sizes (serialize) ─────────────────────────────

fn bench_serialization_varying_sizes(c: &mut Criterion) {
    let kp = Nostro2Keypair::generate();
    let nostr_keys = nostr::Keys::generate();
    let mut group = c.benchmark_group("serialize_by_content_size");
    for size in [64, 256, 1024, 4096] {
        let content = "x".repeat(size);
        let mut nostro2_note = nostro2::NostrNote::text_note(&content);
        nostro2_note.sign_with(&kp).unwrap();
        let nostr_event = nostr::EventBuilder::text_note(&content)
            .sign_with_keys(&nostr_keys)
            .unwrap();

        group.bench_with_input(BenchmarkId::new(NOSTRO2_BACKEND, size), &size, |b, _| {
            b.iter(|| serde_json::to_string(black_box(&nostro2_note)).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("nostr", size), &size, |b, _| {
            b.iter(|| black_box(&nostr_event).as_json());
        });
    }
    group.finish();
}

// ── Full round-trip ───────────────────────────────────────────────

fn bench_full_roundtrip(c: &mut Criterion) {
    let kp = Nostro2Keypair::generate();
    let nostr_keys = nostr::Keys::generate();
    let mut group = c.benchmark_group("full_roundtrip");
    group.bench_function(NOSTRO2_BACKEND, |b| {
        b.iter(|| {
            let mut note = nostro2::NostrNote::text_note("Roundtrip");
            note.sign_with(&kp).unwrap();
            let json = serde_json::to_string(&note).unwrap();
            let parsed: nostro2::NostrNote = serde_json::from_str(&json).unwrap();
            assert!(parsed.verify());
        });
    });
    group.bench_function("nostr", |b| {
        b.iter(|| {
            let event = nostr::EventBuilder::text_note("Roundtrip")
                .sign_with_keys(&nostr_keys)
                .unwrap();
            let json = event.as_json();
            let parsed = nostr::Event::from_json(&json).unwrap();
            assert!(parsed.verify().is_ok());
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_keygen,
    bench_signing,
    bench_verification,
    bench_serialization,
    bench_deserialization,
    bench_view_parse,
    bench_filter_match,
    bench_tag_construction,
    bench_nip44,
    bench_serialization_varying_sizes,
    bench_full_roundtrip,
);
criterion_main!(benches);
