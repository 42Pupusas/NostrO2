#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nostro2::NostrNote;

#[derive(Debug, Arbitrary)]
struct NoteInput {
    pubkey: String,
    created_at: i64,
    kind: u32,
    content: String,
    tag_names: Vec<(String, String)>,
}

fuzz_target!(|input: NoteInput| {
    let mut note = NostrNote {
        pubkey: input.pubkey,
        created_at: input.created_at,
        kind: input.kind,
        content: input.content,
        ..Default::default()
    };
    for (name, value) in &input.tag_names {
        note.tags.add_custom_tag(name, value);
    }

    let _ = note.serialize_id();

    if note.id.is_some() {
        let mut note2 = note.clone();
        let _ = note2.serialize_id();
        assert_eq!(note.id, note2.id, "serialize_id must be deterministic");
    }

    if let Ok(json) = note.serialize() {
        let parsed: NostrNote = json.parse().expect("round-trip parse must succeed");
        assert_eq!(note, parsed);
    }
});
