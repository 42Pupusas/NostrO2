//! Heap profile of the owned `NostrNote` parse path via dhat.
//!
//! Baseline for the `NostrNoteView<'_>` comparison. Both this bin and
//! `heap_parse_ingest_view` parse the *inner note JSON* only (not the
//! `["EVENT", ...]` outer frame) so the delta reflects pure note-deserialize
//! cost and nothing else.
//!
//! Run:
//!   cargo run --release --example heap_parse_ingest
//! Then open the generated `dhat-heap.json` in `dh_view.html`.
//!
//! Note: dhat's allocator wrapper adds overhead — this bin is for *allocation
//! counts and bytes*, not wall-clock. Use criterion for timing.

use nostro2::NostrNote;
use std::hint::black_box;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const ITERATIONS: usize = 50_000;

fn make_note_json(tag_count: usize) -> String {
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
    serde_json::to_string(&note).unwrap()
}

fn main() {
    let profiler = dhat::Profiler::new_heap();

    let note_json = make_note_json(5);

    let mut accepted = 0usize;
    for _ in 0..ITERATIONS {
        let note: NostrNote =
            serde_json::from_str(black_box(&note_json)).expect("parse note");
        accepted += 1;
        black_box(note.pubkey.len());
        black_box(note.tags.len());
    }

    drop(profiler);

    eprintln!(
        "heap_parse_ingest: {ITERATIONS} iters, {accepted} accepted — wrote dhat-heap.json"
    );
}
