//! Pure-function bench for `NostrNote::verify` (schnorr + id hash).
//!
//! Isolates the per-event verify cost. Today this runs on the single
//! dispatch thread and caps EVENT ingest at roughly one core's verify
//! rate. This bench is the baseline for moving verify off the dispatch
//! thread (into reader shards or a small verify pool).

use criterion::{Criterion, criterion_group, criterion_main};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::hint::black_box;

fn signed_note() -> NostrNote {
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("bench payload, typical short note");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    assert!(note.verify());
    note
}

fn bench_verify(c: &mut Criterion) {
    let note = signed_note();
    c.bench_function("verify/valid", |b| {
        b.iter(|| black_box(&note).verify());
    });
}

criterion_group!(benches, bench_verify);
criterion_main!(benches);
