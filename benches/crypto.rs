//! Microbenchmarks for the active curve backend.

use divan::black_box;
use nostro2::{NostrKeypair, NostrNote, NostrSigner};

#[cfg(feature = "k256")]
use nostro2_signer::K256Keypair as ActiveKeypair;
#[cfg(feature = "secp256k1")]
use nostro2_signer::Secp256k1Keypair as ActiveKeypair;

fn main() {
    divan::main();
}

#[divan::bench]
fn keygen() -> ActiveKeypair {
    black_box(ActiveKeypair::generate())
}

#[divan::bench]
fn signing(bencher: divan::Bencher) {
    let kp = ActiveKeypair::generate();
    bencher.bench(|| {
        let mut note = NostrNote::text_note("Benchmark signing");
        note.sign_with(black_box(&kp)).unwrap();
    });
}

#[divan::bench]
fn verification(bencher: divan::Bencher) {
    let kp = ActiveKeypair::generate();
    let mut note = NostrNote::text_note("Benchmark verification");
    note.sign_with(&kp).unwrap();
    bencher.bench(|| black_box(&note).verify());
}

#[divan::bench]
fn ecdh(bencher: divan::Bencher) {
    let alice = ActiveKeypair::generate();
    let bob = ActiveKeypair::generate();
    let bob_pubkey = bob.public_key();
    bencher.bench(|| alice.shared_point(black_box(&bob_pubkey)).unwrap());
}
