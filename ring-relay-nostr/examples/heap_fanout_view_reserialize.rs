//! Heap profile of the EVENT fan-out path *without* the RawValue splice —
//! serializes the view back to JSON per event, which is what the shard
//! dispatcher did before the RawValue wiring landed.
//!
//! Same workload as `heap_fanout_view`; the diff is the per-event
//! `serialize_note_view` call that this bin makes. Pair the two dhat
//! outputs to read off what the verbatim-splice optimization cost in
//! allocations.
//!
//! Run:
//!   cargo run --release --example heap_fanout_view_reserialize

use nostro2::NostrNote;
use ring_relay_nostr::{ClientMessageView, event_from_serialized, parse_view, serialize_note_view};
use std::hint::black_box;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const FRAMES: usize = 10_000;
const SUBSCRIBERS_PER_FRAME: usize = 50;

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
    note.tags.add_custom_tag("t", "relay-bench");
    note.tags.add_custom_tag("t", "view");
    format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
}

fn main() {
    let profiler = dhat::Profiler::new_heap();

    let frame = make_event_frame();
    let sub_ids: Vec<String> = (0..SUBSCRIBERS_PER_FRAME)
        .map(|i| format!("sub-{i:04}"))
        .collect();

    let mut produced = 0usize;
    for _ in 0..FRAMES {
        let msg = parse_view(black_box(&frame)).expect("parse view");
        let ClientMessageView::Event { note, raw: _ } = msg else {
            panic!("expected Event");
        };
        black_box(note.pubkey.len());

        // Reserialize path: walk the view and emit JSON. This allocates
        // one `String` per inbound event (amortized across subscribers
        // inside this inner loop).
        let note_json = serialize_note_view(&note);
        for sub_id in &sub_ids {
            let out = event_from_serialized(sub_id, &note_json);
            produced += 1;
            black_box(out.len());
        }
    }

    drop(profiler);

    eprintln!(
        "heap_fanout_view_reserialize: {FRAMES} frames × {SUBSCRIBERS_PER_FRAME} subs = \
         {produced} out frames — wrote dhat-heap.json"
    );
}
