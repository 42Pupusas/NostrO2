//! Criterion benchmark: ring relay vs async relay on local servers.
//!
//! Prerequisites (run in separate terminals):
//!   1. cargo run -p ring-relay-client --example local_server --release
//!   2. caddy run --config ring-relay-client/examples/Caddyfile
//!   3. sudo modprobe tls
//!
//! Run: cargo bench -p ring-relay-client --bench local_throughput

use criterion::{Criterion, criterion_group, criterion_main};
use nostro2::NostrRelayEvent;
use std::time::Duration;

const NUM_RELAYS: usize = 24;
const BASE_PORT: u16 = 10900;

fn relay_urls() -> Vec<String> {
    (0..NUM_RELAYS)
        .map(|i| format!("wss://localhost:{}", BASE_PORT + i as u16))
        .collect()
}

fn bench_ring_relay(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("ring_relay", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let urls = relay_urls();
                let mut pool =
                    ring_relay_client::RelayPool::new(1_048_576, 2_000_000, 1024, urls.len());
                let sender = pool.sender();

                for url in &urls {
                    let _ = pool.add_relay(url.clone());
                }

                std::thread::sleep(Duration::from_millis(200));

                let subscription = nostro2::NostrSubscription {
                    kinds: vec![1].into(),
                    ..Default::default()
                };
                sender.send(subscription).unwrap();

                let start = std::time::Instant::now();
                let mut eose = 0;

                loop {
                    match pool.try_recv() {
                        Some(ring_relay_client::PoolMessage::RelayEvent { event, .. }) => {
                            if matches!(event, NostrRelayEvent::EndOfSubscription(..)) {
                                eose += 1;
                                if eose >= NUM_RELAYS {
                                    break;
                                }
                            }
                        }
                        Some(_) => {}
                        None => std::hint::spin_loop(),
                    }
                }

                total += start.elapsed();
                std::thread::spawn(move || drop(pool));
                std::thread::sleep(Duration::from_millis(500));
            }

            total
        });
    });

    group.finish();
}

fn bench_async_relay(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("async_relay", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let elapsed = rt.block_on(async {
                    let urls = relay_urls();
                    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
                    let pool = nostro2_relay::NostrPool::new(&url_refs);

                    let subscription = nostro2::NostrSubscription {
                        kinds: vec![1].into(),
                        ..Default::default()
                    };

                    tokio::time::sleep(Duration::from_millis(200)).await;

                    if pool.send(subscription).is_err() {
                        // Tasks failed to connect — skip this iteration
                        drop(pool);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return Duration::from_nanos(1);
                    }

                    let start = std::time::Instant::now();
                    let mut eose = 0;

                    loop {
                        match tokio::time::timeout(
                            Duration::from_secs(30),
                            pool.recv(),
                        )
                        .await
                        {
                            Ok(Some(NostrRelayEvent::EndOfSubscription(..))) => {
                                eose += 1;
                                if eose >= urls.len() {
                                    break;
                                }
                            }
                            Ok(Some(_)) => {}
                            _ => break, // timeout or channel closed
                        }
                    }

                    let elapsed = start.elapsed();
                    drop(pool);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    elapsed
                });

                total += elapsed;
            }

            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_async_relay, bench_ring_relay);
criterion_main!(benches);
