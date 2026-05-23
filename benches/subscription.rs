use divan::black_box;
use nostro2::{NostrNote, NostrSubscription};

fn main() {
    divan::main();
}

fn test_notes() -> Vec<NostrNote> {
    (0..1000)
        .map(|i| {
            let mut note = NostrNote {
                id: Some(format!("event_{i:016x}")),
                pubkey: format!("pubkey_{}", i % 10),
                created_at: 1_234_567_890 + i as i64,
                kind: if i % 3 == 0 { 1 } else { 2 },
                content: format!("Message {i}"),
                sig: Some("sig".repeat(16)),
                ..Default::default()
            };
            note.tags.add_event_tag(&format!("ref_{i}"));
            if i % 5 == 0 {
                note.tags.add_custom_tag("t", "nostr");
            }
            note
        })
        .collect()
}

#[divan::bench]
fn filter_by_author(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::new().author("pubkey_5");
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_by_kind(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::new().kind(1);
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_by_timestamp(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::new()
        .since(1_234_567_890 + 500)
        .until(1_234_567_890 + 700);
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_by_ids(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::new()
        .id(format!("event_{:016x}", 100))
        .id(format!("event_{:016x}", 200))
        .id(format!("event_{:016x}", 300));
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_multi(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::new()
        .author("pubkey_3")
        .author("pubkey_7")
        .kind(1)
        .since(1_234_567_890 + 100);
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_with_tag(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::new().kind(1).tag("#t", "nostr");
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_empty_matches_per_note(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::default();
    bencher.bench(|| black_box(notes.iter().filter(|n| f.matches(n)).count()));
}

#[divan::bench]
fn filter_empty_wildcard_skip(bencher: divan::Bencher) {
    let notes = test_notes();
    let f = NostrSubscription::default();
    bencher.bench(|| {
        black_box(if f.is_wildcard() {
            notes.len()
        } else {
            notes.iter().filter(|n| f.matches(n)).count()
        })
    });
}
