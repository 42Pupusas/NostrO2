//! Smoke test against a local ring-relay-nostr instance.
//!
//! Prereq: `cargo run --release --example relay -p ring-relay-nostr` in another terminal.
//! Run:    `cargo run -p nostro2-relay --example local_relay`

use nostro2::{NostrRelayEvent, NostrSubscription};
use nostro2_relay::NostrRelay;
use nostro2_signer::NostrKeypair;
use nostro2::NostrKeypair as _;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let url = "ws://127.0.0.1:4848";
    println!("Connecting to {url}...");

    let relay = NostrRelay::new(url).await.expect("connect failed");
    println!("Connected.");

    // Subscribe so we can observe our own event come back.
    let sub = NostrSubscription {
        kinds: Some(vec![1].into_iter().collect()),
        limit: Some(10),
        ..Default::default()
    };
    relay.send(sub).expect("send REQ");

    // Sign and send a test note.
    let keypair = NostrKeypair::generate();
    let mut note = nostro2::NostrNote {
        content: "hello from nostro2-relay".to_string(),
        kind: 1,
        ..Default::default()
    };
    note.sign_with(&keypair).expect("sign");
    println!("Publishing note id={:?}", note.id);
    relay.send(note).expect("send EVENT");

    // Read for up to 5 seconds.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            println!("Timeout reached.");
            break;
        }
        match tokio::time::timeout(remaining, relay.recv()).await {
            Ok(Some(NostrRelayEvent::SentOk(_, id, ok, msg))) => {
                println!("OK id={id} ok={ok} msg=\"{msg}\"");
            }
            Ok(Some(NostrRelayEvent::NewNote(_, sub_id, n))) => {
                println!(
                    "NewNote sub={sub_id} id={:?} content=\"{}\"",
                    n.id, n.content
                );
            }
            Ok(Some(NostrRelayEvent::EndOfSubscription(_, sub_id))) => {
                println!("EOSE sub={sub_id}");
            }
            Ok(Some(NostrRelayEvent::Notice(_, msg))) => {
                println!("NOTICE: {msg}");
            }
            Ok(Some(other)) => println!("{other:?}"),
            Ok(None) => {
                println!("Channel closed.");
                break;
            }
            Err(_) => {
                println!("Timeout reached.");
                break;
            }
        }
    }
}
