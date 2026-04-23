//! Pure-function bench for `protocol::parse`.
//!
//! Measures the per-frame parse cost for the three message shapes. The
//! current implementation round-trips through `serde_json::Value` and
//! clones sub-values into the typed structs — this bench is the
//! baseline for any direct-deserialize rewrite.

use criterion::{Criterion, criterion_group, criterion_main};
use nostro2::NostrNote;
use ring_relay_nostr::parse;
use std::hint::black_box;

fn make_event_frame() -> String {
    let mut note = NostrNote {
        pubkey: "a".repeat(64),
        created_at: 1_700_000_000,
        kind: 1,
        content: "This is a typical short nostr note body. \
                  It's not huge but it's not a single word either — \
                  aim for something close to what people actually post."
            .into(),
        id: Some("b".repeat(64)),
        sig: Some("c".repeat(128)),
        ..Default::default()
    };
    note.tags.add_custom_tag("t", "nostr");
    note.tags.add_pubkey_tag(&"d".repeat(64), None);
    note.tags.add_event_tag(&"e".repeat(64));
    format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
}

fn bench_parse_event(c: &mut Criterion) {
    let frame = make_event_frame();
    c.bench_function("parse/event", |b| {
        b.iter(|| parse(black_box(&frame)).expect("parse event"));
    });
}

fn bench_parse_req(c: &mut Criterion) {
    let frame = r#"["REQ","sub1",{"kinds":[1,7],"authors":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"limit":100}]"#;
    c.bench_function("parse/req_1_filter", |b| {
        b.iter(|| parse(black_box(frame)).expect("parse req"));
    });

    let frame_many = r#"["REQ","s",{"kinds":[1]},{"kinds":[7]},{"kinds":[30023]},{"kinds":[4]}]"#;
    c.bench_function("parse/req_4_filters", |b| {
        b.iter(|| parse(black_box(frame_many)).expect("parse req"));
    });
}

fn bench_parse_close(c: &mut Criterion) {
    let frame = r#"["CLOSE","sub1"]"#;
    c.bench_function("parse/close", |b| {
        b.iter(|| parse(black_box(frame)).expect("parse close"));
    });
}

/// Reference: parse using the same Value-roundtrip pattern the current
/// `protocol::parse` uses, so we can watch the delta narrow (or flip)
/// when we move to a direct deserializer.
fn bench_parse_reference(c: &mut Criterion) {
    let frame = make_event_frame();

    c.bench_function("parse/event_direct_serde", |b| {
        use serde::Deserialize;
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Frame(String, NostrNote);
        b.iter(|| {
            let _f: Frame = serde_json::from_str(black_box(&frame)).expect("direct parse");
        });
    });
}

criterion_group!(
    benches,
    bench_parse_event,
    bench_parse_req,
    bench_parse_close,
    bench_parse_reference
);
criterion_main!(benches);
