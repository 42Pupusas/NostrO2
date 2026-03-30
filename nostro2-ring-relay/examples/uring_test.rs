//! Quick smoke test for io_uring + kTLS relay connection.
//!
//! Prerequisites: `sudo modprobe tls`
//! Run: `cargo run -p nostro2-ring-relay --example uring_test --features uring`

#[cfg(feature = "uring")]
fn main() {
    use nostro2::NostrRelayEvent;
    use nostro2_ring_relay::uring::UringRelayConnection;
    use nostro2_ring_relay::PoolMessage;
    use quetzalcoatl::broadcast;
    use quetzalcoatl::capacity::Capacity;
    use quetzalcoatl::mpsc::RingBuffer;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let relay_url = "wss://relay.illuminodes.com";
    println!("Connecting to {relay_url} via io_uring + kTLS...");

    // Create MPSC ring for inbound events
    let (mpsc_producer, mut mpsc_consumer) =
        RingBuffer::<PoolMessage>::new(Capacity::at_least(4096)).split();

    // Create broadcast ring for outbound messages
    let (bc_producer, bc_consumer) =
        broadcast::RingBuffer::<String>::new(Capacity::at_least(64), 2).split();

    let shutdown = Arc::new(AtomicBool::new(false));

    // Spawn uring connection
    let conn = match UringRelayConnection::spawn(
        relay_url.to_string(),
        mpsc_producer,
        bc_consumer,
        Arc::clone(&shutdown),
    ) {
        Ok(c) => {
            println!("Connected!");
            c
        }
        Err(e) => {
            eprintln!("Connection failed: {e}");
            eprintln!("Hint: make sure kTLS module is loaded: sudo modprobe tls");
            std::process::exit(1);
        }
    };

    // Send a subscription for kind 1 events
    let subscription = nostro2::NostrSubscription {
        kinds: vec![1].into(),
        limit: Some(10),
        ..Default::default()
    };
    let client_event: nostro2::NostrClientEvent = subscription.into();
    let json = serde_json::to_string(&client_event).unwrap();
    println!("Sending subscription: {}", &json[..80.min(json.len())]);
    bc_producer.push(json).unwrap();

    // Read events for 10 seconds
    let start = Instant::now();
    let mut count = 0;

    println!("\nListening for events (10s)...\n");

    while start.elapsed() < Duration::from_secs(10) {
        if let Some(msg) = mpsc_consumer.pop() {
            match msg {
                PoolMessage::RelayEvent { event, relay_url } => {
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
                PoolMessage::ConnectionClosed { relay_url, error } => {
                    println!("Connection closed: {relay_url} error={error:?}");
                    break;
                }
            }
        } else {
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    println!("\nReceived {count} events in {:?}", start.elapsed());
    println!("Connection finished: {}", conn.is_finished());

    drop(conn);
    println!("Shutdown complete.");
}

#[cfg(not(feature = "uring"))]
fn main() {
    eprintln!("This example requires the 'uring' feature.");
    eprintln!("Run: cargo run -p nostro2-ring-relay --example uring_test --features uring");
    std::process::exit(1);
}
