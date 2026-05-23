use divan::black_box;
use nostro2::{
    NostrClientEvent, NostrNote, NostrRelayEvent, NostrSubscription, NostrTags, RelayEventTag,
};
use std::collections::BTreeMap;

fn main() {
    divan::main();
}

fn tags_from_rows<I, R, S>(rows: I) -> NostrTags
where
    I: IntoIterator<Item = R>,
    R: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut tags = NostrTags::new();
    for row in rows {
        tags.add_row(row.into_iter().map(Into::into));
    }
    tags
}

fn sample_note() -> NostrNote {
    NostrNote {
        id: Some("abc123def456".to_string()),
        pubkey: "deadbeef".repeat(8),
        created_at: 1_234_567_890,
        kind: 1,
        tags: tags_from_rows([["e", "event_id"], ["p", "pubkey"]]),
        content: "Hello Nostr! This is a test message.".to_string(),
        sig: Some("signature".repeat(16)),
    }
}

fn sample_subscription() -> NostrSubscription {
    NostrSubscription {
        authors: Some(vec!["author1".to_string(), "author2".to_string()]),
        ids: Some(vec!["id1".to_string(), "id2".to_string()]),
        kinds: Some(vec![1, 2, 3]),
        since: Some(1_234_567_890),
        until: Some(9_876_543_210),
        limit: Some(100),
        tags: {
            let mut tags = BTreeMap::new();
            tags.insert("e".to_string(), vec!["event1".to_string()]);
            tags.insert("p".to_string(), vec!["pubkey1".to_string()]);
            Some(tags)
        },
    }
}

// ── Client event serialization ────────────────────────────────────

#[divan::bench]
fn client_ser_send_note() -> String {
    let event: NostrClientEvent = black_box(sample_note()).into();
    bourne::to_string(&event).unwrap()
}

#[divan::bench]
fn client_ser_subscribe() -> String {
    let event: NostrClientEvent = black_box(sample_subscription()).into();
    bourne::to_string(&event).unwrap()
}

#[divan::bench]
fn client_ser_close() -> String {
    bourne::to_string(&NostrClientEvent::close_subscription(black_box("sub_id"))).unwrap()
}

// ── Relay event serialization ─────────────────────────────────────

#[divan::bench]
fn relay_ser_new_note() -> String {
    let event = NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        black_box(sample_note()),
    );
    bourne::to_string(&event).unwrap()
}

#[divan::bench]
fn relay_ser_sent_ok() -> String {
    let event = NostrRelayEvent::SentOk(
        RelayEventTag::Ok,
        "event_id".to_string(),
        true,
        "OK".to_string(),
    );
    bourne::to_string(black_box(&event)).unwrap()
}

#[divan::bench]
fn relay_ser_eose() -> String {
    let event = NostrRelayEvent::EndOfSubscription(RelayEventTag::Eose, "sub_id".to_string());
    bourne::to_string(black_box(&event)).unwrap()
}

#[divan::bench]
fn relay_ser_notice() -> String {
    let event = NostrRelayEvent::Notice(
        RelayEventTag::Notice,
        "This is a notice message".to_string(),
    );
    bourne::to_string(black_box(&event)).unwrap()
}

// ── Client event deserialization ──────────────────────────────────

#[divan::bench]
fn client_deser_send_note(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrClientEvent::from(sample_note())).unwrap();
    bencher.bench(|| bourne::parse_str::<NostrClientEvent>(black_box(&json)).unwrap());
}

#[divan::bench]
fn client_deser_subscribe(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrClientEvent::from(sample_subscription())).unwrap();
    bencher.bench(|| bourne::parse_str::<NostrClientEvent>(black_box(&json)).unwrap());
}

#[divan::bench]
fn client_deser_close(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrClientEvent::close_subscription("sub_id")).unwrap();
    bencher.bench(|| bourne::parse_str::<NostrClientEvent>(black_box(&json)).unwrap());
}

// ── Relay event deserialization ───────────────────────────────────

#[divan::bench]
fn relay_deser_new_note(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        sample_note(),
    ))
    .unwrap();
    bencher.bench(|| bourne::parse_str::<NostrRelayEvent>(black_box(&json)).unwrap());
}

#[divan::bench]
fn relay_deser_sent_ok(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrRelayEvent::SentOk(
        RelayEventTag::Ok,
        "event_id".to_string(),
        true,
        "OK".to_string(),
    ))
    .unwrap();
    bencher.bench(|| bourne::parse_str::<NostrRelayEvent>(black_box(&json)).unwrap());
}

#[divan::bench]
fn relay_deser_eose(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrRelayEvent::EndOfSubscription(
        RelayEventTag::Eose,
        "sub_id".to_string(),
    ))
    .unwrap();
    bencher.bench(|| bourne::parse_str::<NostrRelayEvent>(black_box(&json)).unwrap());
}

#[divan::bench]
fn relay_deser_notice(bencher: divan::Bencher) {
    let json = bourne::to_string(&NostrRelayEvent::Notice(
        RelayEventTag::Notice,
        "This is a notice message".to_string(),
    ))
    .unwrap();
    bencher.bench(|| bourne::parse_str::<NostrRelayEvent>(black_box(&json)).unwrap());
}

// ── Roundtrip ─────────────────────────────────────────────────────

#[divan::bench]
fn roundtrip_client_send_note() -> NostrClientEvent {
    let event: NostrClientEvent = black_box(sample_note()).into();
    let json = bourne::to_string(&event).unwrap();
    bourne::parse_str::<NostrClientEvent>(&json).unwrap()
}

#[divan::bench]
fn roundtrip_client_subscribe() -> NostrClientEvent {
    let event: NostrClientEvent = black_box(sample_subscription()).into();
    let json = bourne::to_string(&event).unwrap();
    bourne::parse_str::<NostrClientEvent>(&json).unwrap()
}

#[divan::bench]
fn roundtrip_relay_new_note() -> NostrRelayEvent {
    let event = NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        black_box(sample_note()),
    );
    let json = bourne::to_string(&event).unwrap();
    bourne::parse_str::<NostrRelayEvent>(&json).unwrap()
}

// ── Varying note sizes ────────────────────────────────────────────

const SIZES: &[usize] = &[10, 100, 1000, 5000];

#[divan::bench(args = SIZES)]
fn note_size_ser(bencher: divan::Bencher, size: usize) {
    let note = NostrNote {
        id: Some("abc123".to_string()),
        pubkey: "deadbeef".repeat(8),
        created_at: 1_234_567_890,
        kind: 1,
        tags: tags_from_rows([["e", "event_id"]]),
        content: "x".repeat(size),
        sig: Some("sig".repeat(16)),
    };
    bencher.bench(|| {
        let event: NostrClientEvent = black_box(note.clone()).into();
        bourne::to_string(&event).unwrap()
    });
}
