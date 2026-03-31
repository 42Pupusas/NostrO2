use nostro2_ring_relay::{PoolMessage, RelayPool};
use std::time::{Duration, Instant};

const TEST_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://relay.illuminodes.com",
    "wss://nos.lol",
    "wss://nostr.wine",
];

fn main() {
    println!("=== Ring Relay Memory/CPU Test ===");
    println!("Running for 30 seconds...");
    println!("Monitor with: ps aux | grep memory_test");
    println!("Or: top -p $(pgrep -f memory_test)\n");

    // Create relay pool with bidirectional messaging
    let mut pool = RelayPool::new(4096, 10_000, 64, TEST_RELAYS.len());

    // Spawn relay connections
    for url in TEST_RELAYS {
        pool.add_relay(url.to_string()).unwrap();
    }

    let start = Instant::now();
    let test_duration = Duration::from_secs(30);
    let mut event_count = 0;
    let mut last_report = Instant::now();

    println!("Started at: {:?}", start);
    println!("PID: {}\n", std::process::id());

    // Receive events for 30 seconds
    while start.elapsed() < test_duration {
        match pool.try_recv() {
            Some(PoolMessage::RelayEvent { .. }) => {
                event_count += 1;
            }
            Some(_) => {} // Ignore connection closed
            None => {
                std::thread::sleep(Duration::from_millis(1));
            }
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
