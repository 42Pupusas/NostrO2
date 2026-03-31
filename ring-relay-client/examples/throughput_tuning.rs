use ring_relay_client::{PoolMessage, RelayPool};
use std::time::{Duration, Instant};

const TEST_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://relay.illuminodes.com",
    "wss://nos.lol",
    "wss://nostr.wine",
];

fn test_config(ring_size: usize, cache_size: usize, use_sleep: bool) -> (usize, f64, usize) {
    println!(
        "Testing: ring={}, cache={}, sleep={}",
        ring_size, cache_size, use_sleep
    );

    let mut pool = RelayPool::new(ring_size, cache_size, 64, TEST_RELAYS.len());

    for url in TEST_RELAYS {
        pool.add_relay(url.to_string()).unwrap();
    }

    let start = Instant::now();
    let test_duration = Duration::from_secs(10);
    let mut event_count = 0;
    let mut empty_reads = 0;

    while start.elapsed() < test_duration {
        match pool.try_recv() {
            Some(PoolMessage::RelayEvent { .. }) => {
                event_count += 1;
            }
            Some(_) => {}
            None => {
                empty_reads += 1;
                if use_sleep {
                    std::thread::sleep(Duration::from_micros(100));
                } else {
                    std::hint::spin_loop();
                }
            }
        }
    }

    let rate = event_count as f64 / test_duration.as_secs_f64();
    println!(
        "  -> {} events, {:.1} events/sec, {} empty reads\n",
        event_count, rate, empty_reads
    );

    (event_count, rate, empty_reads)
}

fn main() {
    println!("=== Ring Relay Throughput Tuning ===\n");

    let configs = vec![
        // (ring_size, cache_size, use_sleep)
        (1024, 10_000, true),
        (4096, 10_000, true), // Current default
        (8192, 10_000, true),
        (16384, 10_000, true),
        (4096, 50_000, true),  // Bigger cache
        (4096, 10_000, false), // No sleep (spin)
        (8192, 10_000, false), // Bigger buffer + no sleep
    ];

    let mut results = Vec::new();

    for (ring_size, cache_size, use_sleep) in configs {
        let result = test_config(ring_size, cache_size, use_sleep);
        results.push((ring_size, cache_size, use_sleep, result));

        // Wait a bit between tests
        std::thread::sleep(Duration::from_secs(2));
    }

    println!("\n=== Summary ===");
    println!("Ring Size | Cache Size | Sleep | Events | Rate (ev/s) | Empty Reads");
    println!("----------|------------|-------|--------|-------------|-------------");

    for (ring_size, cache_size, use_sleep, (events, rate, empty)) in results {
        println!(
            "{:>9} | {:>10} | {:>5} | {:>6} | {:>11.1} | {:>11}",
            ring_size, cache_size, use_sleep, events, rate, empty
        );
    }

    println!("\nTips:");
    println!("  - Higher ring size = less producer blocking");
    println!("  - No sleep = lower latency but higher CPU");
    println!("  - Larger cache = better long-term dedup");
}
