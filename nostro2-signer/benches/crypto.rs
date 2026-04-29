//! Microbenchmarks for the active curve backend.
//!
//! `nostro2-signer`'s `k256` and `secp256k1` features are mutually exclusive
//! (the crate's `compile_error!` enforces it), so this binary can only
//! measure one backend per build. To compare backends, run twice:
//!
//! ```text
//! cargo bench -p nostro2-signer --bench crypto
//! cargo bench -p nostro2-signer --bench crypto --no-default-features --features secp256k1
//! ```
//!
//! and diff the criterion reports. There is no in-process comparison because
//! you can't link two implementations of the same NIP traits into one binary.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::{NostrKeypair, NostrNote, NostrSigner};

#[cfg(feature = "k256")]
use nostro2_signer::K256Keypair as ActiveKeypair;
#[cfg(feature = "secp256k1")]
use nostro2_signer::Secp256k1Keypair as ActiveKeypair;

#[cfg(feature = "k256")]
const BACKEND: &str = "k256";
#[cfg(feature = "secp256k1")]
const BACKEND: &str = "secp256k1";

fn bench_keygen(c: &mut Criterion) {
    c.bench_function(&format!("keygen_{BACKEND}"), |b| {
        b.iter(|| black_box(ActiveKeypair::generate()));
    });
}

fn bench_signing(c: &mut Criterion) {
    let kp = ActiveKeypair::generate();
    c.bench_function(&format!("signing_{BACKEND}"), |b| {
        b.iter(|| {
            let mut note = NostrNote::text_note("Benchmark signing");
            note.sign_with(black_box(&kp)).unwrap();
        });
    });
}

fn bench_verification(c: &mut Criterion) {
    let kp = ActiveKeypair::generate();
    let mut note = NostrNote::text_note("Benchmark verification");
    note.sign_with(&kp).unwrap();
    c.bench_function(&format!("verification_{BACKEND}"), |b| {
        b.iter(|| black_box(&note).verify());
    });
}

fn bench_ecdh(c: &mut Criterion) {
    let alice = ActiveKeypair::generate();
    let bob = ActiveKeypair::generate();
    let bob_pubkey = bob.public_key();
    c.bench_function(&format!("ecdh_{BACKEND}"), |b| {
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
