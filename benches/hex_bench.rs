use divan::black_box;
use nostro2_traits::hex::{FromHex, Hexable};

fn main() {
    divan::main();
}

const SIZES: &[usize] = &[32, 64, 128, 256];

#[divan::bench(args = SIZES)]
fn encode_trait(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    bencher.bench(|| black_box(&bytes).to_hex());
}

#[divan::bench(args = SIZES)]
fn encode_hex_crate(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    bencher.bench(|| hex::encode(black_box(&bytes)));
}

#[divan::bench(args = SIZES)]
fn decode_trait(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    let hex_str = bytes.to_hex();
    bencher.bench(|| black_box(&hex_str).as_str().decode_hex().unwrap());
}

#[divan::bench(args = SIZES)]
fn decode_hex_crate(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    let hex_str = hex::encode(&bytes);
    bencher.bench(|| hex::decode(black_box(&hex_str)).unwrap());
}

#[divan::bench(args = SIZES)]
fn decode_to_slice_trait(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    let hex_str = bytes.to_hex();
    bencher.bench(|| {
        let mut out = vec![0u8; size];
        black_box(&hex_str)
            .as_str()
            .decode_hex_to_slice(&mut out)
            .unwrap();
        out
    });
}

#[divan::bench(args = SIZES)]
fn roundtrip_trait(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    bencher.bench(|| {
        let hex = black_box(&bytes).to_hex();
        hex.as_str().decode_hex().unwrap()
    });
}

#[divan::bench(args = SIZES)]
fn roundtrip_hex_crate(bencher: divan::Bencher, size: usize) {
    let bytes: Vec<u8> = (0..size).map(|i| i as u8).collect();
    bencher.bench(|| {
        let hex = hex::encode(black_box(&bytes));
        hex::decode(&hex).unwrap()
    });
}
