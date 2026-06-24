//! Microbenchmarks for NIP-104 (Double Ratchet E2EE).
//!
//! Two surfaces are measured, both through the **public manager API** with the
//! production curve keypair (`nostro2_signer::NostrKeypair`):
//!
//! * **1:1 Double Ratchet** via [`SessionManager`] — the invite handshake plus
//!   the steady-state `send` / `process_event` hot paths (ratchet step + NIP-44
//!   + Schnorr sign/verify).
//! * **Group sender-keys** via [`GroupManager`] — minting a chain, applying a
//!   distribution, and the per-message symmetric chain step (`encrypt` /
//!   `decrypt`) plus the signed outer-event round trip.
//!
//! Run:
//! ```bash
//! cargo bench -p nostro2-benchmarks --bench nip104
//! cargo bench -p nostro2-benchmarks --bench nip104 --no-default-features --features secp256k1
//! ```

use divan::{black_box, Bencher};
use nostro2_nips::{GroupManager, Invite, SessionManager};
use nostro2_signer::nostro2_traits::{NostrKeypair as _, NostrSigner as _};
use nostro2_signer::NostrKeypair;

fn main() {
    divan::main();
}

const NOW: i64 = 1_700_000_000;
const GROUP: &str = "bench-group";
const PAYLOAD: &[u8] = b"the quick brown fox jumps over the lazy dog";

// ── 1:1 helpers ──────────────────────────────────────────────────

/// A pair of managers that have completed the invite handshake, with the
/// initiator (`bob`) having spoken once so both send-chains are open. Returns
/// `(alice, bob, alice_pubkey, bob_pubkey)` ready for steady-state traffic.
fn ready_pair() -> (
    SessionManager<NostrKeypair>,
    SessionManager<NostrKeypair>,
    String,
    String,
) {
    let mut alice = SessionManager::new(NostrKeypair::generate());
    let mut bob = SessionManager::new(NostrKeypair::generate());
    let alice_pk = alice.our_pubkey().to_owned();
    let bob_pk = bob.our_pubkey().to_owned();

    let invite = Invite::create_new::<NostrKeypair>(&alice_pk, None).unwrap();
    let response = bob.accept_invite(&invite, None, NOW).unwrap();
    alice.receive_invite_response(&invite, &response).unwrap();

    // Initiator (bob) must send first to open Alice's receiving chain; have
    // Alice consume it so both directions are live.
    let first = bob.send(&alice_pk, b"open", NOW).unwrap();
    alice.process_event(&first[0]).unwrap();

    (alice, bob, alice_pk, bob_pk)
}

/// The whole invite handshake: create → accept → receive. Setup cost paid once
/// per conversation.
#[divan::bench]
fn dm_handshake(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let alice = SessionManager::new(NostrKeypair::generate());
            let bob = SessionManager::new(NostrKeypair::generate());
            let alice_pk = alice.our_pubkey().to_owned();
            (alice, bob, alice_pk)
        })
        .bench_values(|(mut alice, mut bob, alice_pk)| {
            let invite = Invite::create_new::<NostrKeypair>(&alice_pk, None).unwrap();
            let response = bob.accept_invite(black_box(&invite), None, NOW).unwrap();
            alice
                .receive_invite_response(&invite, black_box(&response))
                .unwrap();
            black_box(alice)
        });
}

/// Steady-state outbound: ratchet encrypt + NIP-44 + Schnorr sign of one event.
#[divan::bench]
fn dm_send(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let (alice, _bob, _apk, bpk) = ready_pair();
            (alice, bpk)
        })
        .bench_values(|(mut alice, bpk)| {
            black_box(alice.send(black_box(&bpk), PAYLOAD, NOW).unwrap())
        });
}

/// Steady-state inbound: trial-route + ratchet decrypt + signature verify.
#[divan::bench]
fn dm_receive(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let (alice, mut bob, apk, _bpk) = ready_pair();
            let event = bob.send(&apk, PAYLOAD, NOW).unwrap().pop().unwrap();
            // Keep `alice` as the receiver; `bob` is dropped.
            (alice, event)
        })
        .bench_values(|(mut alice, event)| {
            black_box(alice.process_event(black_box(&event)).unwrap())
        });
}

/// Full duplex: Bob encrypts+signs one message and Alice routes+decrypts+
/// verifies it. The end-to-end cost of moving one byte-string between peers.
#[divan::bench]
fn dm_round_trip(bencher: Bencher) {
    bencher
        .with_inputs(ready_pair)
        .bench_values(|(mut alice, mut bob, apk, _bpk)| {
            let event = bob.send(black_box(&apk), PAYLOAD, NOW).unwrap().pop().unwrap();
            black_box(alice.process_event(&event).unwrap())
        });
}

// ── Group helpers ────────────────────────────────────────────────

/// A `(sender, receiver)` group pair: the sender has minted a chain and the
/// receiver has installed its distribution, so messages flow.
fn ready_group() -> (GroupManager<NostrKeypair>, GroupManager<NostrKeypair>) {
    let mut sender = GroupManager::new(NostrKeypair::generate().public_key());
    let mut receiver = GroupManager::new(NostrKeypair::generate().public_key());
    let dist = sender.rotate_sending_chain(GROUP, 1, NOW).unwrap();
    receiver.apply_distribution(&dist).unwrap();
    (sender, receiver)
}

/// Mint a fresh sending chain: sender-event keypair + chain key + distribution.
#[divan::bench]
fn group_rotate_chain(bencher: Bencher) {
    bencher
        .with_inputs(|| GroupManager::<NostrKeypair>::new(NostrKeypair::generate().public_key()))
        .bench_values(|mut g| black_box(g.rotate_sending_chain(GROUP, 1, NOW).unwrap()));
}

/// Install a received distribution (decode hex chain key, seed receiving chain).
#[divan::bench]
fn group_apply_distribution(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let mut sender =
                GroupManager::<NostrKeypair>::new(NostrKeypair::generate().public_key());
            let dist = sender.rotate_sending_chain(GROUP, 1, NOW).unwrap();
            let receiver =
                GroupManager::<NostrKeypair>::new(NostrKeypair::generate().public_key());
            (receiver, dist)
        })
        .bench_values(|(mut receiver, dist)| {
            black_box(receiver.apply_distribution(black_box(&dist)).unwrap());
            black_box(receiver)
        });
}

/// One symmetric chain step + NIP-44 encrypt — the per-message group send cost
/// (note: *one* event regardless of group size, the whole point of sender-keys).
#[divan::bench]
fn group_encrypt(bencher: Bencher) {
    bencher
        .with_inputs(|| ready_group().0)
        .bench_values(|mut sender| {
            black_box(sender.encrypt(GROUP, PAYLOAD, NOW).unwrap())
        });
}

/// One chain step + NIP-44 decrypt on the receiving side.
#[divan::bench]
fn group_decrypt(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let (mut sender, receiver) = ready_group();
            let msg = sender.encrypt(GROUP, PAYLOAD, NOW).unwrap();
            (receiver, msg)
        })
        .bench_values(|(mut receiver, msg)| {
            black_box(receiver.decrypt(black_box(&msg)).unwrap())
        });
}

/// Encrypt **and** build+sign the publishable outer kind-1060 event.
#[divan::bench]
fn group_encrypt_to_event(bencher: Bencher) {
    bencher
        .with_inputs(|| ready_group().0)
        .bench_values(|mut sender| {
            black_box(sender.encrypt_to_event(GROUP, PAYLOAD, NOW).unwrap())
        });
}

/// Verify + route + decrypt an inbound outer event.
#[divan::bench]
fn group_decrypt_event(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let (mut sender, receiver) = ready_group();
            let event = sender.encrypt_to_event(GROUP, PAYLOAD, NOW).unwrap();
            (receiver, event)
        })
        .bench_values(|(mut receiver, event)| {
            black_box(receiver.decrypt_event(black_box(&event)).unwrap())
        });
}
