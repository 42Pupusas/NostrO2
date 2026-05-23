//! Smoke test for the async tokio relay pool.
//!
//! Connects to popular relays, subscribes to kind 1 notes,
//! and prints events for 10 seconds.
//!
//! Run: `cargo run -p nostro2-relay --example relay_test`

const TEST_RELAYS: &[&str] = &[
    "wss://relay.illuminodes.com",
    "wss://nos.lol",
    "wss://nostr.mom",
    "wss://relay.snort.social",
    "wss://nostr-pub.wellorder.net",
    "wss://relay.jerseyplebs.com",
    "wss://relay.primal.net",
    "wss://relay.bostr.shop",
    "wss://relay.albylabs.com",
    "wss://relay.bitcoindistrict.org",
    "wss://relay.nsite.run",
    "wss://git.shakespeare.diy",
];

#[tokio::main]
async fn main() {
    use nostro2::NostrRelayEvent;
    use std::time::{Duration, Instant};

    println!(
        "Connecting to {} relays via tokio-tungstenite...",
        TEST_RELAYS.len()
    );

    let pool = nostro2_relay::NostrPool::new(TEST_RELAYS);

    // Send a subscription for kind 1 events
    let subscription = nostro2::NostrSubscription {
        kinds: vec![1].into(),
        limit: Some(1000),
        ..Default::default()
    };
    pool.send(subscription).unwrap();

    // Read events for 10 seconds
    let start = Instant::now();
    let mut count = 0;

    println!("\nListening for events (10s)...\n");

    while start.elapsed() < Duration::from_secs(10) {
        let msg = tokio::time::timeout(Duration::from_millis(100), pool.recv()).await;

        match msg {
            Ok(Some(NostrRelayEvent::NewNote(_, sub_id, note))) => {
                count += 1;
                let content = note.content.chars().take(60).collect::<String>();
                println!(
                    "[{count:>4}] NewNote sub={sub_id} kind={} content=\"{content}...\"",
                    note.kind
                );
            }
            Ok(Some(NostrRelayEvent::EndOfSubscription(_, sub_id))) => {
                count += 1;
                println!("[{count:>4}] EOSE sub={sub_id}");
            }
            Ok(Some(NostrRelayEvent::SentOk(_, id, ok, msg))) => {
                count += 1;
                println!("[{count:>4}] OK id={id} ok={ok} msg=\"{msg}\"");
            }
            Ok(Some(NostrRelayEvent::Notice(_, msg))) => {
                count += 1;
                println!("[{count:>4}] NOTICE: {msg}");
            }
            Ok(Some(other)) => {
                count += 1;
                println!("[{count:>4}] {other:?}");
            }
            Ok(None) => {
                println!("Channel closed.");
                break;
            }
            Err(_) => {
                // timeout, loop again
            }
        }
    }

    println!("\nReceived {count} events in {:?}", start.elapsed());
    println!(
        "Rate: {:.1} events/sec",
        count as f64 / start.elapsed().as_secs_f64()
    );
}
