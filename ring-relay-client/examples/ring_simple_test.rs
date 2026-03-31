use ring_relay_client::{PoolMessage, RelayPool};
use std::time::Instant;

fn main() {
    println!("=== Ring Buffer Relay - Bidirectional Test ===\n");

    let start_time = Instant::now();

    // Create relay pool with bidirectional messaging
    // ring_capacity=4096, cache_size=10K, broadcast_capacity=64, max_relays=20
    let mut pool = RelayPool::new(4096, 10_000, 64, 20);

    // Get a sender handle (cloneable for multi-threaded use)
    let _sender = pool.sender();

    // List of relays
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

    // Spawn relay connections — each gets a broadcast consumer clone
    for url in &relays {
        pool.add_relay(url.to_string()).unwrap();
    }

    let mut event_count = 0;
    let mut relay_counts = std::collections::HashMap::new();
    let target = 3000;

    println!("Racing to {} events...\n", target);

    // Race to 3000 events
    while event_count < target {
        match pool.recv() {
            PoolMessage::RelayEvent { relay_url, .. } => {
                event_count += 1;
                *relay_counts.entry(relay_url).or_insert(0) += 1;
                if event_count % 500 == 0 {
                    println!("Progress: {}/{} events", event_count, target);
                }
            }
            PoolMessage::ConnectionClosed { relay_url, error } => {
                if let Some(err) = error {
                    eprintln!("Connection error from {}: {}", relay_url, err);
                }
            }
        }
    }

    let total_time = start_time.elapsed();

    // Print final results
    println!("\n=== RESULTS ===");
    println!("Total events: {}", event_count);
    println!("Total time: {:?}", total_time);
    println!(
        "Events/sec: {:.1}",
        event_count as f64 / total_time.as_secs_f64()
    );
    println!("\nDistribution:");
    for (relay, count) in relay_counts {
        println!(
            "  {}: {} ({:.1}%)",
            relay,
            count,
            (count as f64 / event_count as f64) * 100.0
        );
    }
}
