use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nostro2::NostrRelayEvent;
use nostro2_ring_relay::{create_pool, PoolMessage};

fn bench_ring_buffer_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_throughput");

    for buffer_size in [128, 512, 1024, 4096].iter() {
        group.throughput(Throughput::Elements(1000));

        group.bench_with_input(
            BenchmarkId::from_parameter(buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (mut consumer, mut producer) = create_pool(size);

                    // Simulate producer thread
                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            let msg = PoolMessage::RelayEvent {
                                relay_url: "wss://test.relay".to_string(),
                                event: NostrRelayEvent::Ping,
                            };
                            while producer.push(black_box(msg.clone())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    // Consumer thread
                    for _ in 0..1000 {
                        black_box(consumer.recv());
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_multi_producer(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_producer");

    for num_producers in [2, 4, 8].iter() {
        group.throughput(Throughput::Elements(1000 * num_producers));

        group.bench_with_input(
            BenchmarkId::from_parameter(num_producers),
            num_producers,
            |b, &count| {
                b.iter(|| {
                    let (mut consumer, producer) = create_pool(4096);

                    // Spawn multiple producer threads
                    let handles: Vec<_> = (0..count)
                        .map(|i| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for _ in 0..1000 {
                                    let msg = PoolMessage::RelayEvent {
                                        relay_url: format!("wss://relay{}.test", i),
                                        event: NostrRelayEvent::Ping,
                                    };
                                    while prod.push(black_box(msg.clone())).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    // Consumer thread receives all messages
                    let total_messages = count * 1000;
                    for _ in 0..total_messages {
                        black_box(consumer.recv());
                    }

                    // Wait for producers to finish
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_ring_buffer_throughput, bench_multi_producer);
criterion_main!(benches);
