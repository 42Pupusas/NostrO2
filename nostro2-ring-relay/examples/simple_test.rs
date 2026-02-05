use nostro2_ring_relay::{create_pool, PoolMessage, RelayConnection};
use std::time::Instant;

fn main() {
    println!("=== Ring Buffer Relay - Race to 3000 Events ===\n");

    let start_time = Instant::now();

    // Create ring buffer
    let (mut consumer, producer) = create_pool(4096);

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

    // Spawn connection threads
    let _connections: Vec<_> = relays
        .into_iter()
        .map(|url| RelayConnection::spawn(url.to_string(), producer.clone()))
        .collect();

    let mut event_count = 0;
    let mut relay_counts = std::collections::HashMap::new();
    let target = 3000;

    println!("Racing to {} events...\n", target);

    // Race to 3000 events
    while event_count < target {
        match consumer.recv() {
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
    println!("Events/sec: {:.1}", event_count as f64 / total_time.as_secs_f64());
    println!("\nDistribution:");
    for (relay, count) in relay_counts {
        println!("  {}: {} ({:.1}%)", relay, count, (count as f64 / event_count as f64) * 100.0);
    }
}
