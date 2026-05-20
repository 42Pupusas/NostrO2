#![no_main]
use libfuzzer_sys::fuzz_target;
use nostro2::NostrNoteView;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = bourne::parse_str::<NostrNoteView<'_>>(s);
    }
});
