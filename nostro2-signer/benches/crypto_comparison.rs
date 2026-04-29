use nostro2::{NostrKeypair, NostrSigner};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::NostrNote;
use nostro2_signer::K256Keypair;

// Note: This benchmark previously compared secp256k1 vs k256 performance.
// After migration to pure Rust (k256), secp256k1 benchmarks have been removed.
//
// Historical results (before migration):
// - Verification: secp256k1 ~67µs, k256 ~94µs (1.4x slower)
// - Signing: secp256k1 ~21µs, k256 ~96µs (4.5x slower)
// - Keygen: secp256k1 ~21µs, k256 ~105µs (5x slower)
// - ECDH: Similar ~2-3x slower
//
// The migration to k256 was chosen for:
// - Pure Rust (no C dependencies)
// - WASM compatibility
// - Simpler builds
// - Modern rand ecosystem
//
// The performance trade-off was accepted as verification (hot path) is still
// fast enough for relay use, and cold path operations (signing, keygen) are
// not performance-critical.

// ── Key Generation ──────────────────────────────────────────────────

fn bench_keygen(c: &mut Criterion) {
    c.bench_function("keygen_k256", |b| {
        b.iter(|| black_box(K256Keypair::generate()));
    });
}

// ── Signing ─────────────────────────────────────────────────────────

fn bench_signing(c: &mut Criterion) {
    let kp = K256Keypair::generate();

    c.bench_function("signing_k256", |b| {
        b.iter(|| {
            let mut note = NostrNote::text_note("Benchmark signing");
            note.sign_with(black_box(&kp)).unwrap();
        });
    });
}

// ── Verification ────────────────────────────────────────────────────

fn bench_verification(c: &mut Criterion) {
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("Benchmark verification");
    note.sign_with(&kp).unwrap();

    c.bench_function("verification_k256", |b| {
        b.iter(|| black_box(&note).verify());
    });
}

// ── ECDH Shared Secret ─────────────────────────────────────────────

fn bench_ecdh(c: &mut Criterion) {
    let alice = K256Keypair::generate();
    let bob = K256Keypair::generate();
    let bob_pubkey = bob.public_key();

    c.bench_function("ecdh_k256", |b| {
        b.iter(|| alice.shared_point(black_box(&bob_pubkey)).unwrap());
    });
}

criterion_group!(
    crypto_benches,
    bench_keygen,
    bench_signing,
    bench_verification,
    bench_ecdh,
);
criterion_main!(crypto_benches);
