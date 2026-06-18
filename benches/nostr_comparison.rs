//! `nostro2` vs upstream `nostr` crate — head-to-head benchmarks.
//!
//! Requires exactly one curve feature (`k256` or `secp256k1`).
//! With `--no-default-features` the bench compiles but has no benchmarks.

fn main() {
    divan::main();
}

#[cfg(not(any(feature = "k256", feature = "secp256k1")))]
mod _empty {} // bench binary compiles but runs nothing

#[cfg(any(feature = "k256", feature = "secp256k1"))]
mod comparison {
    use divan::black_box;
    use nostr::JsonUtil;
    use nostro2::{NostrKeypair as _, NostrNoteBuilder, NostrSigner as _};

    #[cfg(feature = "k256")]
    use nostro2_signer::K256Keypair as Nostro2Keypair;
    #[cfg(feature = "secp256k1")]
    use nostro2_signer::Secp256k1Keypair as Nostro2Keypair;

    fn nostro2_signed_note() -> (Nostro2Keypair, nostro2::NostrNote) {
        let kp = Nostro2Keypair::generate();
        let mut note = NostrNoteBuilder::text_note("Hello Nostr! Benchmark vs the nostr crate.").build();
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

    // ── Key Generation ────────────────────────────────────────────────

    #[divan::bench]
    fn keygen_nostro2() -> Nostro2Keypair {
        black_box(Nostro2Keypair::generate())
    }

    #[divan::bench]
    fn keygen_nostr() -> nostr::Keys {
        black_box(nostr::Keys::generate())
    }

    // ── Signing ───────────────────────────────────────────────────────

    #[divan::bench]
    fn signing_nostro2(bencher: divan::Bencher) {
        let kp = Nostro2Keypair::generate();
        bencher.bench(|| {
            let mut note = NostrNoteBuilder::text_note("Benchmark signing").build();
            note.sign_with(black_box(&kp)).unwrap();
        });
    }

    #[divan::bench]
    fn signing_nostr(bencher: divan::Bencher) {
        let keys = nostr::Keys::generate();
        bencher.bench(|| {
            black_box(
                nostr::EventBuilder::text_note("Benchmark signing")
                    .sign_with_keys(&keys)
                    .unwrap(),
            );
        });
    }

    // ── Verification ──────────────────────────────────────────────────

    #[divan::bench]
    fn verification_nostro2(bencher: divan::Bencher) {
        let (_, note) = nostro2_signed_note();
        assert!(note.verify());
        bencher.bench(|| black_box(&note).verify());
    }

    #[divan::bench]
    fn verification_nostr(bencher: divan::Bencher) {
        let (_, event) = nostr_signed_event();
        assert!(event.verify().is_ok());
        bencher.bench(|| black_box(&event).verify());
    }

    // ── Serialize event → JSON ───────────────────────────────────────

    #[divan::bench]
    fn serialize_nostro2(bencher: divan::Bencher) {
        let (_, note) = nostro2_signed_note();
        bencher.bench(|| bourne::to_string(black_box(&note)).unwrap());
    }

    #[divan::bench]
    fn serialize_nostr(bencher: divan::Bencher) {
        let (_, event) = nostr_signed_event();
        bencher.bench(|| black_box(&event).as_json());
    }

    // ── Deserialize JSON → event ─────────────────────────────────────

    #[divan::bench]
    fn deserialize_nostro2(bencher: divan::Bencher) {
        let (_, note) = nostro2_signed_note();
        let json = bourne::to_string(&note).unwrap();
        bencher.bench(|| bourne::parse_str::<nostro2::NostrNote>(black_box(&json)).unwrap());
    }

    #[divan::bench]
    fn deserialize_nostr(bencher: divan::Bencher) {
        let (_, event) = nostr_signed_event();
        let json = event.as_json();
        bencher.bench(|| nostr::Event::from_json(black_box(&json)).unwrap());
    }

    // ── Zero-copy view parse ─────────────────────────────────────────

    #[divan::bench]
    fn view_parse_nostro2(bencher: divan::Bencher) {
        let (_, note) = nostro2_signed_note();
        let json = bourne::to_string(&note).unwrap();
        bencher.bench(|| {
            black_box(bourne::parse_str::<nostro2::NostrNoteView<'_>>(black_box(&json)).unwrap());
        });
    }

    #[divan::bench]
    fn view_parse_nostro2_owned(bencher: divan::Bencher) {
        let (_, note) = nostro2_signed_note();
        let json = bourne::to_string(&note).unwrap();
        bencher.bench(|| {
            black_box(bourne::parse_str::<nostro2::NostrNote>(black_box(&json)).unwrap());
        });
    }

    #[divan::bench]
    fn view_parse_nostr(bencher: divan::Bencher) {
        let (_, event) = nostr_signed_event();
        let json = event.as_json();
        bencher.bench(|| nostr::Event::from_json(black_box(&json)).unwrap());
    }

    // ── Filter match ─────────────────────────────────────────────────

    #[divan::bench]
    fn filter_match_nostro2(bencher: divan::Bencher) {
        let kp = Nostro2Keypair::generate();
        let notes: Vec<nostro2::NostrNote> = (0..1000)
            .map(|i| {
                let mut n = NostrNoteBuilder::text_note(format!("note {i}")).build();
                n.kind = if i % 3 == 0 { 1 } else { 7 };
                n.sign_with(&kp).unwrap();
                n
            })
            .collect();
        let filter = nostro2::NostrSubscription::new()
            .kind(1)
            .since(0)
            .until(u64::MAX >> 1);
        bencher.bench(|| black_box(notes.iter().filter(|n| filter.matches(n)).count()));
    }

    #[divan::bench]
    fn filter_match_nostr(bencher: divan::Bencher) {
        let keys = nostr::Keys::generate();
        let events: Vec<nostr::Event> = (0..1000)
            .map(|i| {
                let kind = if i % 3 == 0 {
                    nostr::Kind::TextNote
                } else {
                    nostr::Kind::Reaction
                };
                nostr::EventBuilder::new(kind, format!("note {i}"))
                    .sign_with_keys(&keys)
                    .unwrap()
            })
            .collect();
        let filter = nostr::Filter::new().kind(nostr::Kind::TextNote);
        let opts = nostr::filter::MatchEventOptions::default();
        bencher.bench(|| {
            black_box(
                events
                    .iter()
                    .filter(|ev| filter.match_event(ev, opts))
                    .count(),
            )
        });
    }

    // ── Tag construction ─────────────────────────────────────────────

    #[divan::bench]
    fn tag_construction_nostro2() {
        let pk_hex = "deadbeef".repeat(8);
        let ev_hex = "cafebabe".repeat(8);
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
    }

    #[divan::bench]
    fn tag_construction_nostr(bencher: divan::Bencher) {
        let pk = nostr::PublicKey::from_hex(&"deadbeef".repeat(8)).unwrap();
        let ev = nostr::EventId::from_hex(&"cafebabe".repeat(8)).unwrap();
        bencher.bench(|| {
            black_box(vec![
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
            ]);
        });
    }

    // ── NIP-44 encrypt / decrypt ─────────────────────────────────────

    #[divan::bench]
    fn nip44_encrypt_nostro2(bencher: divan::Bencher) {
        use nostro2_nips::Nip44 as _;
        let alice = Nostro2Keypair::generate();
        let bob = Nostro2Keypair::generate();
        let bob_pk = bob.public_key();
        let plaintext = "Hello, Nostr! NIP-44 round-trip benchmark payload.";
        bencher.bench(|| {
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
    }

    #[divan::bench]
    fn nip44_encrypt_nostr(bencher: divan::Bencher) {
        let alice = nostr::Keys::generate();
        let bob_pk = nostr::Keys::generate().public_key();
        let plaintext = "Hello, Nostr! NIP-44 round-trip benchmark payload.";
        bencher.bench(|| {
            black_box(
                nostr::nips::nip44::encrypt(
                    alice.secret_key(),
                    &bob_pk,
                    plaintext,
                    nostr::nips::nip44::Version::V2,
                )
                .unwrap(),
            );
        });
    }

    #[divan::bench]
    fn nip44_decrypt_nostro2(bencher: divan::Bencher) {
        use nostro2_nips::Nip44 as _;
        let alice = Nostro2Keypair::generate();
        let bob = Nostro2Keypair::generate();
        let bob_pk = bob.public_key();
        let alice_pk = alice.public_key();
        let mut note = nostro2::NostrNote {
            kind: 14,
            content: "Hello, Nostr! NIP-44 round-trip benchmark payload.".into(),
            ..Default::default()
        };
        alice.nip44_encrypt_note(&mut note, &bob_pk).unwrap();
        let ciphertext = note.content.clone();
        bencher.bench(|| {
            let locked = nostro2::NostrNote {
                kind: 14,
                content: ciphertext.clone(),
                ..Default::default()
            };
            let pt = bob
                .nip44_decrypt_note(&locked, black_box(&alice_pk))
                .unwrap();
            black_box(pt);
        });
    }

    #[divan::bench]
    fn nip44_decrypt_nostr(bencher: divan::Bencher) {
        let alice = nostr::Keys::generate();
        let bob = nostr::Keys::generate();
        let bob_pk = bob.public_key();
        let alice_pk = alice.public_key();
        let ciphertext = nostr::nips::nip44::encrypt(
            alice.secret_key(),
            &bob_pk,
            "Hello, Nostr! NIP-44 round-trip benchmark payload.",
            nostr::nips::nip44::Version::V2,
        )
        .unwrap();
        bencher.bench(|| {
            black_box(
                nostr::nips::nip44::decrypt(bob.secret_key(), &alice_pk, &ciphertext).unwrap(),
            );
        });
    }

    // ── Varying content sizes ────────────────────────────────────────

    const SIZES: &[usize] = &[64, 256, 1024, 4096];

    #[divan::bench(args = SIZES)]
    fn serialize_by_size_nostro2(bencher: divan::Bencher, size: usize) {
        let kp = Nostro2Keypair::generate();
        let mut note = nostro2::NostrNote::text_note("x".repeat(size));
        note.sign_with(&kp).unwrap();
        bencher.bench(|| bourne::to_string(black_box(&note)).unwrap());
    }

    #[divan::bench(args = SIZES)]
    fn serialize_by_size_nostr(bencher: divan::Bencher, size: usize) {
        let keys = nostr::Keys::generate();
        let event = nostr::EventBuilder::text_note("x".repeat(size))
            .sign_with_keys(&keys)
            .unwrap();
        bencher.bench(|| black_box(&event).as_json());
    }

    // ── Full round-trip ──────────────────────────────────────────────

    #[divan::bench]
    fn full_roundtrip_nostro2(bencher: divan::Bencher) {
        let kp = Nostro2Keypair::generate();
        bencher.bench(|| {
            let mut note = nostro2::NostrNote::text_note("Roundtrip");
            note.sign_with(&kp).unwrap();
            let json = bourne::to_string(&note).unwrap();
            let parsed: nostro2::NostrNote = bourne::parse_str(&json).unwrap();
            assert!(parsed.verify());
        });
    }

    #[divan::bench]
    fn full_roundtrip_nostr(bencher: divan::Bencher) {
        let keys = nostr::Keys::generate();
        bencher.bench(|| {
            let event = nostr::EventBuilder::text_note("Roundtrip")
                .sign_with_keys(&keys)
                .unwrap();
            let json = event.as_json();
            let parsed = nostr::Event::from_json(&json).unwrap();
            assert!(parsed.verify().is_ok());
        });
    }
} // mod comparison
