use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::NostrNote;
use nostro2_signer::NostrKeypair;

/// Create a properly signed note for benchmarking verification
fn create_signed_note() -> NostrNote {
    let keypair = NostrKeypair::new();
    let mut note = NostrNote::text_note("Hello Nostr! Benchmarking signature verification.");
    keypair.sign_note(&mut note).expect("signing failed");
    note
}

/// Benchmark k256 (pure Rust) signature verification
///
/// Note: This now uses k256 by default (post-migration from secp256k1).
/// Historical benchmarks showed k256 at ~94µs vs secp256k1 at ~67µs (1.4x slower),
/// which was accepted for WASM compatibility and pure Rust benefits.
fn bench_verify_k256(c: &mut Criterion) {
    let note = create_signed_note();
    assert!(note.verify(), "k256 verify sanity check");

    c.bench_function("verify_k256", |b| {
        b.iter(|| black_box(&note).verify());
    });
}

criterion_group!(verification_benches, bench_verify_k256);
criterion_main!(verification_benches);
