//! Heap profile of the client→relay `parse` path via dhat.
//!
//! Runs a fixed workload of `EVENT` frames through `protocol::parse`, which
//! is the allocation-heavy half of the relay's hot path today (owned
//! `NostrNote` with `String` fields + `Vec<Vec<String>>` tags).
//!
//! Run:
//!   cargo run --release --example heap_parse_ingest
//! Then open the generated `dhat-heap.json` in `dh_view.html`.
//!
//! Keep the workload identical across runs so the `NostrNoteView<'_>`
//! prototype can be compared apples-to-apples against this baseline.
//!
//! Note: dhat's allocator wrapper adds overhead — this bin is for *allocation
//! counts and bytes*, not wall-clock. Use criterion for timing.

use nostro2::NostrNote;
use ring_relay_nostr::{ClientMessage, parse};
use std::hint::black_box;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const ITERATIONS: usize = 50_000;

fn make_event_frame(tag_count: usize) -> String {
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
    for i in 0..tag_count {
        match i % 3 {
            0 => note.tags.add_custom_tag("t", "nostr"),
            1 => note.tags.add_pubkey_tag(&"d".repeat(64), None),
            _ => note.tags.add_event_tag(&"e".repeat(64)),
        }
    }
    format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
}

fn main() {
    let profiler = dhat::Profiler::new_heap();

    // Representative shape: short content, 5 tags. Mirrors `protocol_parse.rs`.
    let frame = make_event_frame(5);

    let mut accepted = 0usize;
    for _ in 0..ITERATIONS {
        match parse(black_box(&frame)).expect("parse event") {
            ClientMessage::Event(note) => {
                accepted += 1;
                // Touch a field so the whole struct is actually materialized
                // and not DCE'd out from under the profiler.
                black_box(note.pubkey.len());
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    drop(profiler);

    eprintln!(
        "heap_parse_ingest: {ITERATIONS} iters, {accepted} accepted — wrote dhat-heap.json"
    );
}
