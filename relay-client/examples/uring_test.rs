//! Quick smoke test for kTLS + io_uring relay connection.
//!
//! Prerequisites: `sudo modprobe tls`
//! Run: `cargo run -p relay-client --example uring_test`

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

fn main() {
    use nostro2::NostrRelayEvent;
    use relay_client::PoolMessage;
    use std::time::{Duration, Instant};

    let mut pool = relay_client::RelayPool::new(4096, 10_000, 64, 12);

    for relay_url in TEST_RELAYS {
        println!("Connecting to {relay_url} via kTLS + io_uring...");
        match pool.add_relay(relay_url.to_string()) {
            Ok(()) => println!("Connected!"),
            Err(e) => {
                eprintln!("Connection failed: {e}");
                eprintln!("Hint: make sure kTLS module is loaded: sudo modprobe tls");
                std::process::exit(1);
            }
        }
    }

    // Send a subscription for kind 1 events
    let subscription = nostro2::NostrSubscription {
        kinds: vec![1].into(),
        limit: Some(1000),
        ..Default::default()
    };
    pool.sender().send(subscription).unwrap();

    // Read events for 10 seconds
    let start = Instant::now();
    let mut count = 0;

    println!("\nListening for events (10s)...\n");

    while start.elapsed() < Duration::from_secs(10) {
        match pool.try_recv() {
            Some(PoolMessage::RelayEvent { event, relay_url }) => {
                count += 1;
                match &event {
                    NostrRelayEvent::NewNote(_, sub_id, note) => {
                        let content = note.content.chars().take(60).collect::<String>();
                        println!(
                            "[{count:>4}] NewNote sub={sub_id} kind={} content=\"{content}...\"",
                            note.kind
                        );
                    }
                    NostrRelayEvent::EndOfSubscription(_, sub_id) => {
                        println!("[{count:>4}] EOSE sub={sub_id}");
                    }
                    NostrRelayEvent::SentOk(_, id, ok, msg) => {
                        println!("[{count:>4}] OK id={id} ok={ok} msg=\"{msg}\"");
                    }
                    NostrRelayEvent::Notice(_, msg) => {
                        println!("[{count:>4}] NOTICE: {msg}");
                    }
                    other => {
                        println!("[{count:>4}] {relay_url}: {other:?}");
                    }
                }
            }
            Some(PoolMessage::ConnectionClosed { relay_url, error }) => {
                println!("Connection closed: {relay_url} error={error:?}");
                break;
            }
            None => {
                std::thread::sleep(Duration::from_micros(100));
            }
        }
    }

    println!("\nReceived {count} events in {:?}", start.elapsed());
    println!("Active connections: {}", pool.active_connection_count());

    drop(pool);
    println!("Shutdown complete.");
}
