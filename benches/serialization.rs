use divan::black_box;
use nostro2::{
    NostrClientEvent, NostrNote, NostrRelayEvent, NostrSubscription, NostrTags, RelayEventTag,
};
use std::collections::{BTreeMap, HashSet};

fn main() {
    divan::main();
}

#[cfg(feature = "bourne")]
fn to_json_string<T: json_bourne::ToJson + ?Sized>(v: &T) -> String {
    json_bourne::to_string(v).unwrap()
}
#[cfg(feature = "serde")]
fn to_json_string<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

#[cfg(feature = "bourne")]
fn from_json_str<T: for<'a> json_bourne::FromJson<'a>>(s: &str) -> T {
    json_bourne::parse_str(s).unwrap()
}
#[cfg(feature = "serde")]
fn from_json_str<T: serde::de::DeserializeOwned>(s: &str) -> T {
    serde_json::from_str(s).unwrap()
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
        authors: Some(HashSet::from([
            "author1".to_string(),
            "author2".to_string(),
        ])),
        ids: Some(HashSet::from(["id1".to_string(), "id2".to_string()])),
        kinds: Some(HashSet::from([1, 2, 3])),
        since: Some(1_234_567_890),
        until: Some(9_876_543_210),
        limit: Some(100),
        tags: {
            let mut tags = BTreeMap::new();
            tags.insert("e".to_string(), HashSet::from(["event1".to_string()]));
            tags.insert("p".to_string(), HashSet::from(["pubkey1".to_string()]));
            Some(tags)
        },
    }
}

// ── Client event serialization ────────────────────────────────────

#[divan::bench]
fn client_ser_send_note() -> String {
    let event: NostrClientEvent = black_box(sample_note()).into();
    to_json_string(&event)
}

#[divan::bench]
fn client_ser_subscribe() -> String {
    let event: NostrClientEvent = black_box(sample_subscription()).into();
    to_json_string(&event)
}

#[divan::bench]
fn client_ser_close() -> String {
    to_json_string(&NostrClientEvent::close_subscription(black_box("sub_id")))
}

// ── Relay event serialization ─────────────────────────────────────

#[divan::bench]
fn relay_ser_new_note() -> String {
    let event = NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        black_box(sample_note()),
    );
    to_json_string(&event)
}

#[divan::bench]
fn relay_ser_sent_ok() -> String {
    let event = NostrRelayEvent::SentOk(
        RelayEventTag::Ok,
        "event_id".to_string(),
        true,
        "OK".to_string(),
    );
    to_json_string(black_box(&event))
}

#[divan::bench]
fn relay_ser_eose() -> String {
    let event = NostrRelayEvent::EndOfSubscription(RelayEventTag::Eose, "sub_id".to_string());
    to_json_string(black_box(&event))
}

#[divan::bench]
fn relay_ser_notice() -> String {
    let event = NostrRelayEvent::Notice(
        RelayEventTag::Notice,
        "This is a notice message".to_string(),
    );
    to_json_string(black_box(&event))
}

// ── Client event deserialization ──────────────────────────────────

#[divan::bench]
fn client_deser_send_note(bencher: divan::Bencher) {
    let json = to_json_string(&NostrClientEvent::from(sample_note()));
    bencher.bench(|| from_json_str::<NostrClientEvent>(black_box(&json)));
}

#[divan::bench]
fn client_deser_subscribe(bencher: divan::Bencher) {
    let json = to_json_string(&NostrClientEvent::from(sample_subscription()));
    bencher.bench(|| from_json_str::<NostrClientEvent>(black_box(&json)));
}

#[divan::bench]
fn client_deser_close(bencher: divan::Bencher) {
    let json = to_json_string(&NostrClientEvent::close_subscription("sub_id"));
    bencher.bench(|| from_json_str::<NostrClientEvent>(black_box(&json)));
}

// ── Relay event deserialization ───────────────────────────────────

#[divan::bench]
fn relay_deser_new_note(bencher: divan::Bencher) {
    let json = to_json_string(&NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        sample_note(),
    ));
    bencher.bench(|| from_json_str::<NostrRelayEvent>(black_box(&json)));
}

#[divan::bench]
fn relay_deser_sent_ok(bencher: divan::Bencher) {
    let json = to_json_string(&NostrRelayEvent::SentOk(
        RelayEventTag::Ok,
        "event_id".to_string(),
        true,
        "OK".to_string(),
    ));
    bencher.bench(|| from_json_str::<NostrRelayEvent>(black_box(&json)));
}

#[divan::bench]
fn relay_deser_eose(bencher: divan::Bencher) {
    let json = to_json_string(&NostrRelayEvent::EndOfSubscription(
        RelayEventTag::Eose,
        "sub_id".to_string(),
    ));
    bencher.bench(|| from_json_str::<NostrRelayEvent>(black_box(&json)));
}

#[divan::bench]
fn relay_deser_notice(bencher: divan::Bencher) {
    let json = to_json_string(&NostrRelayEvent::Notice(
        RelayEventTag::Notice,
        "This is a notice message".to_string(),
    ));
    bencher.bench(|| from_json_str::<NostrRelayEvent>(black_box(&json)));
}

// ── Roundtrip ─────────────────────────────────────────────────────

#[divan::bench]
fn roundtrip_client_send_note() -> NostrClientEvent {
    let event: NostrClientEvent = black_box(sample_note()).into();
    let json = to_json_string(&event);
    from_json_str(&json)
}

#[divan::bench]
fn roundtrip_client_subscribe() -> NostrClientEvent {
    let event: NostrClientEvent = black_box(sample_subscription()).into();
    let json = to_json_string(&event);
    from_json_str(&json)
}

#[divan::bench]
fn roundtrip_relay_new_note() -> NostrRelayEvent {
    let event = NostrRelayEvent::NewNote(
        RelayEventTag::Event,
        "sub_id".to_string(),
        black_box(sample_note()),
    );
    let json = to_json_string(&event);
    from_json_str(&json)
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
        to_json_string(&event)
    });
}
