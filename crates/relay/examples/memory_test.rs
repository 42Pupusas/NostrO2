use std::time::{Duration, Instant};

const TEST_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://relay.illuminodes.com",
    "wss://nos.lol",
    "wss://nostr.wine",
];

#[tokio::main]
async fn main() {
    println!("=== Async Relay Memory/CPU Test ===");
    println!("Running for 30 seconds...");
    println!("Monitor with: ps aux | grep memory_test");
    println!("Or: top -p $(pgrep -f memory_test)\n");

    // Create async pool
    let pool = nostro2_relay::NostrPool::new(TEST_RELAYS);

    // Subscribe
    let subscription = nostro2::NostrSubscription {
        kinds: Some(vec![1].into_iter().collect()),
        limit: Some(1000),
        ..Default::default()
    };
    pool.send(subscription).unwrap();

    let start = Instant::now();
    let test_duration = Duration::from_secs(30);
    let mut event_count = 0;
    let mut last_report = Instant::now();

    println!("Started at: {:?}", start);
    println!("PID: {}\n", std::process::id());

    // Receive events for 30 seconds
    while start.elapsed() < test_duration {
        if let Some(nostro2::NostrRelayEvent::NewNote(..)) = pool.recv().await {
            event_count += 1;
        }

        // Report every 5 seconds
        if last_report.elapsed() >= Duration::from_secs(5) {
            let elapsed = start.elapsed();
            let rate = event_count as f64 / elapsed.as_secs_f64();
            println!(
                "[{:>2}s] Events: {} ({:.1} events/sec)",
                elapsed.as_secs(),
                event_count,
                rate
            );
            last_report = Instant::now();
        }
    }

    let elapsed = start.elapsed();
    println!("\n=== Final Results ===");
    println!("Total events: {}", event_count);
    println!("Total time: {:?}", elapsed);
    println!(
        "Average rate: {:.1} events/sec",
        event_count as f64 / elapsed.as_secs_f64()
    );
    println!("\nCheck 'top' or 'ps' output above for memory/CPU usage");
}
