use criterion::{Criterion, criterion_group, criterion_main};
use std::time::{Duration, Instant};

// Test relays - using a subset for faster benchmarks
const TEST_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://relay.illuminodes.com",
];

fn bench_ring_relay_fixed_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("fixed_time_throughput");
    group.sample_size(10);

    // Measure how many events we can receive in 5 seconds
    group.bench_function("ring_relay_5sec", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = Duration::ZERO;

            for _ in 0..iters {
                let mut pool =
                    relay_client::RelayPool::new(4096, 10_000, 64, TEST_RELAYS.len());

                for url in TEST_RELAYS {
                    pool.add_relay(url.to_string()).unwrap();
                }

                let start = Instant::now();
                let test_duration = Duration::from_secs(5);
                let mut event_count = 0;

                // Receive events for 5 seconds
                while start.elapsed() < test_duration {
                    match pool.try_recv() {
                        Some(relay_client::PoolMessage::RelayEvent { .. }) => {
                            event_count += 1;
                        }
                        Some(_) => {} // Ignore connection closed
                        None => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                }

                let elapsed = start.elapsed();
                println!(
                    "Ring Relay: {} events in {:?} ({:.1} events/sec)",
                    event_count,
                    elapsed,
                    event_count as f64 / elapsed.as_secs_f64()
                );
                total_duration += elapsed;
            }

            total_duration
        });
    });

    group.finish();
}

fn bench_async_relay_fixed_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("fixed_time_throughput");
    group.sample_size(10);

    // Measure how many events we can receive in 5 seconds
    group.bench_function("async_relay_5sec", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total_duration = Duration::ZERO;

            for _ in 0..iters {
                let elapsed = rt.block_on(async {
                    // Create async pool
                    let pool = nostro2_relay::NostrPool::new(TEST_RELAYS);

                    // Subscribe
                    let subscription = nostro2::NostrSubscription {
                        kinds: vec![1].into(),
                        limit: Some(1000),
                        ..Default::default()
                    };
                    pool.send(subscription).unwrap();

                    let start = Instant::now();
                    let test_duration = Duration::from_secs(5);
                    let mut event_count = 0;

                    // Receive events for 5 seconds
                    while start.elapsed() < test_duration {
                        if let Some(nostro2::NostrRelayEvent::NewNote(..)) = pool.recv().await {
                            event_count += 1;
                        }
                    }

                    let elapsed = start.elapsed();
                    println!(
                        "Async Relay: {} events in {:?} ({:.1} events/sec)",
                        event_count,
                        elapsed,
                        event_count as f64 / elapsed.as_secs_f64()
                    );
                    elapsed
                });

                total_duration += elapsed;
            }

            total_duration
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_ring_relay_fixed_time,
    bench_async_relay_fixed_time
);
criterion_main!(benches);
