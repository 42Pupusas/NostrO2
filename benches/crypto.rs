//! Microbenchmarks for the active curve backend.
//!
//! A/B test by swapping the Cargo feature flag:
//! ```bash
//! cargo bench -p nostro2-benchmarks                  # k256 (default)
//! cargo bench -p nostro2-benchmarks --no-default-features --features secp256k1
//! ```

use divan::black_box;
use nostro2::{NostrEvent, NostrNote, NostrNoteBuilder};
use nostro2_signer::NostrKeypair;
use nostro2_traits::{NostrKeypair as _, NostrSigner as _};

fn main() {
    divan::main();
}

#[divan::bench]
fn keygen() -> NostrKeypair {
    black_box(NostrKeypair::generate())
}

#[divan::bench]
fn signing(bencher: divan::Bencher) {
    let kp = NostrKeypair::generate();
    bencher.bench(|| {
        let mut note = NostrNoteBuilder::text_note("Benchmark signing").build();
        note.sign_with(black_box(&kp)).unwrap();
    });
}

#[divan::bench]
fn verification(bencher: divan::Bencher) {
    let kp = NostrKeypair::generate();
    let mut note = NostrNoteBuilder::text_note("Benchmark verification").build();
    note.sign_with(&kp).unwrap();
    bencher.bench(|| black_box(&note).verify());
}

#[divan::bench]
fn ecdh(bencher: divan::Bencher) {
    let alice = NostrKeypair::generate();
    let bob = NostrKeypair::generate();
    let bob_pubkey = bob.public_key();
    bencher.bench(|| alice.shared_point(black_box(&bob_pubkey)).unwrap());
}

/// Raw Schnorr verify only — no JSON serialization, no SHA-256 ID
/// recomputation. This isolates the `s*G + e*P` double-scalar-mult that is
/// the curve-level hot path (and what the local k256 `schnorr-verify-perf`
/// branch optimizes). `verification` above measures the full `note.verify()`
/// which also re-serializes + re-hashes the event, diluting the curve cost.
#[divan::bench]
fn verify_sig_only(bencher: divan::Bencher) {
    let kp = NostrKeypair::generate();
    // Fixed 32-byte prehash (stands in for the event id digest).
    let prehash = [0x42_u8; 32];
    let sig = kp.sign_prehash(&prehash).unwrap();
    let pubkey = kp.pubkey_bytes();

    #[cfg(feature = "k256")]
    bencher.bench(|| {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
        let vk = VerifyingKey::from_bytes((&pubkey).into()).unwrap();
        let s = Signature::try_from(sig.as_slice()).unwrap();
        black_box(vk.verify_prehash(black_box(&prehash), &s).is_ok())
    });

    #[cfg(feature = "secp256k1")]
    bencher.bench(|| {
        use secp256k1::{schnorr::Signature, XOnlyPublicKey, SECP256K1};
        let xonly = XOnlyPublicKey::from_byte_array(pubkey).unwrap();
        let s = Signature::from_byte_array(sig);
        black_box(
            SECP256K1
                .verify_schnorr(&s, black_box(&prehash), &xonly)
                .is_ok(),
        )
    });
}
