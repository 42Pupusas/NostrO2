//! Ephemeral Nostr relay.
//!
//! Run: cargo run --release --example relay -p ring-relay-nostr
//! Test: `nak req -s ws://127.0.0.1:4848` in one terminal,
//!       `nak event -s ws://127.0.0.1:4848` in another.

use ring_relay_nostr::{NostrRelay, RelayConfig};

fn main() {
    let port = 4848;
    println!("Starting ephemeral Nostr relay on 0.0.0.0:{port}");

    let mut relay = NostrRelay::bind([0, 0, 0, 0], port, RelayConfig::default())
        .expect("failed to start relay");

    relay.run();
}
