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

fn bench_verify_secp256k1(c: &mut Criterion) {
    let note = create_signed_note();
    assert!(note.verify(), "secp256k1 verify sanity check");

    c.bench_function("verify_secp256k1", |b| {
        b.iter(|| black_box(&note).verify());
    });
}

fn bench_verify_k256(c: &mut Criterion) {
    let note = create_signed_note();
    assert!(note.verify_k256(), "k256 verify sanity check");

    c.bench_function("verify_k256", |b| {
        b.iter(|| black_box(&note).verify_k256());
    });
}

criterion_group!(verification_benches, bench_verify_secp256k1, bench_verify_k256);
criterion_main!(verification_benches);
