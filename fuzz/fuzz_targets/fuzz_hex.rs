#![no_main]
use libfuzzer_sys::fuzz_target;
use nostro2_traits::hex::{FromHex, Hexable};

fuzz_target!(|data: &[u8]| {
    let hex = data.to_hex();
    let decoded = hex.decode_hex().expect("round-trip decode must succeed");
    assert_eq!(data, decoded.as_slice());

    if let Ok(s) = std::str::from_utf8(data) {
        let _ = s.decode_hex();
    }
});
