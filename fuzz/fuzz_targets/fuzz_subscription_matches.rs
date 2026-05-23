#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nostro2::{NostrNote, NostrSubscription};

#[derive(Debug, Arbitrary)]
struct Input {
    authors: Vec<String>,
    ids: Vec<String>,
    kinds: Vec<u32>,
    since: Option<u64>,
    until: Option<u64>,
    tag_filters: Vec<(String, String)>,
    note_pubkey: String,
    note_kind: u32,
    note_created_at: i64,
    note_id: Option<String>,
    note_tags: Vec<(String, String)>,
}

fuzz_target!(|input: Input| {
    let mut sub = NostrSubscription::new();
    if !input.authors.is_empty() {
        sub = sub.authors(input.authors);
    }
    if !input.ids.is_empty() {
        sub = sub.ids(input.ids);
    }
    if !input.kinds.is_empty() {
        sub = sub.kinds(input.kinds);
    }
    if let Some(s) = input.since {
        sub = sub.since(s);
    }
    if let Some(u) = input.until {
        sub = sub.until(u);
    }
    for (k, v) in &input.tag_filters {
        sub.add_tag(k, v);
    }

    let mut note = NostrNote {
        pubkey: input.note_pubkey,
        kind: input.note_kind,
        created_at: input.note_created_at,
        id: input.note_id,
        ..Default::default()
    };
    for (name, value) in &input.note_tags {
        note.tags.add_custom_tag(name, value);
    }

    let linear = sub.matches(&note);
    let compiled = sub.compile().matches(&note);
    assert_eq!(linear, compiled, "compiled and linear matchers must agree");
});
