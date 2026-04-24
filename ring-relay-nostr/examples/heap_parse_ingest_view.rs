//! Heap profile of the zero-copy `NostrNoteView<'_>` parse path.
//!
//! Mirrors `heap_parse_ingest.rs` (owned `NostrNote` via `protocol::parse`)
//! but deserializes straight into the borrowed view. Use this to measure
//! the allocation delta between the two code paths under an identical
//! workload.
//!
//! Frame shape: 50k `EVENT` frames carrying a 5-tag note (same as the
//! baseline).
//!
//! Run:
//!   cargo run --release --example heap_parse_ingest_view
//! Then diff the resulting `dhat-heap.json` against the owned baseline in
//! `dh_view.html`.
//!
//! To keep the comparison clean we parse just the inner note JSON here; the
//! `["EVENT", <note>]` outer frame adds one array-level parse either way,
//! and isolating the note path makes the per-event allocation count easier
//! to interpret.

use nostro2::{NostrNote, NostrNoteView};
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
        let view: NostrNoteView<'_> =
            serde_json::from_str(black_box(&note_json)).expect("parse view");
        accepted += 1;
        // Touch fields so nothing gets DCE'd out from under the profiler.
        black_box(view.pubkey.len());
        black_box(view.tags.len());
    }

    drop(profiler);

    eprintln!(
        "heap_parse_ingest_view: {ITERATIONS} iters, {accepted} accepted — wrote dhat-heap.json"
    );
}
