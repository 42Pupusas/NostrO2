use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::NostrNote;
use nostro2_signer::{K256Keypair, NostrKeypair};

// Re-export hex from nostro2_signer's dependency for the bench
extern crate hex;

// ── Key Generation ──────────────────────────────────────────────────

fn bench_keygen(c: &mut Criterion) {
    let mut group = c.benchmark_group("keygen");

    group.bench_function("secp256k1", |b| {
        b.iter(|| black_box(NostrKeypair::new()));
    });

    group.bench_function("k256", |b| {
        b.iter(|| black_box(K256Keypair::new()));
    });

    group.finish();
}

// ── Signing ─────────────────────────────────────────────────────────

fn bench_signing(c: &mut Criterion) {
    let secp_kp = NostrKeypair::new();
    let k256_kp = K256Keypair::new();

    let mut group = c.benchmark_group("signing");

    group.bench_function("secp256k1", |b| {
        b.iter(|| {
            let mut note = NostrNote::text_note("Benchmark signing");
            secp_kp.sign_note(black_box(&mut note)).unwrap();
        });
    });

    group.bench_function("k256", |b| {
        b.iter(|| {
            let mut note = NostrNote::text_note("Benchmark signing");
            k256_kp.sign_note(black_box(&mut note)).unwrap();
        });
    });

    group.finish();
}

// ── Verification ────────────────────────────────────────────────────

fn bench_verification(c: &mut Criterion) {
    let secp_kp = NostrKeypair::new();
    let mut note = NostrNote::text_note("Benchmark verification");
    secp_kp.sign_note(&mut note).unwrap();

    let mut group = c.benchmark_group("verification");

    group.bench_function("secp256k1", |b| {
        b.iter(|| black_box(&note).verify());
    });

    group.bench_function("k256", |b| {
        b.iter(|| black_box(&note).verify_k256());
    });

    group.finish();
}

// ── ECDH Shared Secret ─────────────────────────────────────────────

fn bench_ecdh(c: &mut Criterion) {
    let secp_alice = NostrKeypair::new_extractable();
    let secp_bob = NostrKeypair::new_extractable();
    let sk_hex = hex::encode(secp_alice.secret_key());
    let k256_alice = K256Keypair::from_hex(&sk_hex, true).unwrap();

    let bob_pubkey = secp_bob.pubkey();

    let mut group = c.benchmark_group("ecdh");

    group.bench_function("secp256k1", |b| {
        b.iter(|| secp_alice.shared_point(black_box(&bob_pubkey)).unwrap());
    });

    group.bench_function("k256", |b| {
        b.iter(|| k256_alice.shared_point(black_box(&bob_pubkey)).unwrap());
    });

    group.finish();
}

criterion_group!(
    crypto_benches,
    bench_keygen,
    bench_signing,
    bench_verification,
    bench_ecdh,
);
criterion_main!(crypto_benches);
