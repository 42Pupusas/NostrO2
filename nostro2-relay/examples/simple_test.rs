use std::time::Instant;

#[tokio::main]
async fn main() {
    println!("=== Async Relay - Race to 3000 Events ===\n");

    let start_time = Instant::now();

    // List of relays from nostr.watch
    let relays = vec![
        "wss://relay.damus.io",
        "wss://relay.primal.net",
        "wss://relay.illuminodes.com",
        "wss://nos.lol",
        "wss://nostr.wine",
        "wss://relay.nostr.band",
        "wss://nostr.mom",
        "wss://relay.snort.social",
        "wss://nostr-pub.wellorder.net",
        "wss://relay.current.fyi",
        "wss://purplepag.es",
        "wss://relay.orangepill.dev",
        "wss://relay.mostr.pub",
        "wss://nostr.zebedee.cloud",
        "wss://relay.nostrati.com",
        "wss://relay.nostr.wirednet.jp",
        "wss://nostr.fmt.wiz.biz",
        "wss://relay.arcade.city",
        "wss://nostr.einundzwanzig.space",
        "wss://relay.nostr.info",
    ];

    println!("Connecting to {} relays...", relays.len());

    // Create pool with relays
    let pool = nostro2_relay::NostrPool::new(&relays);

    // Subscribe to kind 1 events with limit 3000 per relay
    let subscription = nostro2::NostrSubscription {
        kinds: vec![1].into(),
        limit: Some(1000),
        ..Default::default()
    };

    pool.send(subscription).unwrap();

    let mut event_count = 0;
    let target = 3000;

    println!("Racing to {} events...\n", target);

    // Race to 3000 events
    while event_count < target {
        match pool.recv().await {
            Some(nostro2::NostrRelayEvent::NewNote(..)) => {
                event_count += 1;
                if event_count % 500 == 0 {
                    println!("Progress: {}/{} events", event_count, target);
                }
            }
            Some(_) => {
                // Ignore other event types (EOSE, Notice, etc.)
            }
            None => {
                println!("Channel closed at {} events", event_count);
                break;
            }
        }
    }

    let total_time = start_time.elapsed();

    // Print final results
    println!("\n=== RESULTS ===");
    println!("Total events: {}", event_count);
    println!("Total time: {:?}", total_time);
    println!("Events/sec: {:.1}", event_count as f64 / total_time.as_secs_f64());
    println!("\nNote: Async pool doesn't track per-relay distribution");
}
