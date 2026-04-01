use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nostro2::NostrRelayEvent;
use ring_relay_client::PoolMessage;
use quetzalcoatl::capacity::Capacity;

fn make_msg() -> PoolMessage {
    PoolMessage::RelayEvent {
        relay_url: "wss://test.relay".into(),
        event: NostrRelayEvent::Ping,
    }
}

fn bench_ring_buffer_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_throughput");

    for buffer_size in [128, 512, 1024, 4096].iter() {
        group.throughput(Throughput::Elements(1000));

        group.bench_with_input(
            BenchmarkId::from_parameter(buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) = quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(size)).split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(msg) = consumer.pop() {
                                black_box(msg);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_spsc_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("spsc_throughput");

    for buffer_size in [128, 512, 1024, 4096].iter() {
        group.throughput(Throughput::Elements(1000));

        // SPSC with pop()
        group.bench_with_input(
            BenchmarkId::new("pop", buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            size,
                        ))
                        .split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(msg) = consumer.pop() {
                                black_box(msg);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                });
            },
        );

        // SPSC with pop_ref() (zero-copy)
        group.bench_with_input(
            BenchmarkId::new("pop_ref", buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            size,
                        ))
                        .split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(reader) = consumer.pop_ref() {
                                black_box(&*reader);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_zero_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("zero_copy_single_producer");

    for buffer_size in [512, 1024, 4096].iter() {
        group.throughput(Throughput::Elements(1000));

        // MPSC pop() baseline
        group.bench_with_input(
            BenchmarkId::new("mpsc_pop", buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            size,
                        ))
                        .split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(msg) = consumer.pop() {
                                black_box(msg);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                });
            },
        );

        // MPSC pop_ref() zero-copy
        group.bench_with_input(
            BenchmarkId::new("mpsc_pop_ref", buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            size,
                        ))
                        .split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(reader) = consumer.pop_ref() {
                                black_box(&*reader);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                });
            },
        );

        // SPSC pop() baseline
        group.bench_with_input(
            BenchmarkId::new("spsc_pop", buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            size,
                        ))
                        .split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(msg) = consumer.pop() {
                                black_box(msg);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                });
            },
        );

        // SPSC pop_ref() zero-copy
        group.bench_with_input(
            BenchmarkId::new("spsc_pop_ref", buffer_size),
            buffer_size,
            |b, &size| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            size,
                        ))
                        .split();

                    std::thread::spawn(move || {
                        for _ in 0..1000 {
                            while producer.push(black_box(make_msg())).is_err() {
                                std::hint::spin_loop();
                            }
                        }
                    });

                    for _ in 0..1000 {
                        loop {
                            if let Some(reader) = consumer.pop_ref() {
                                black_box(&*reader);
                                break;
                            }
                            std::hint::spin_loop();
                        }
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
                    let (producer, mut consumer) = quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(4096)).split();

                    let handles: Vec<_> = (0..count)
                        .map(|i| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for _ in 0..1000 {
                                    let msg = PoolMessage::RelayEvent {
                                        relay_url: format!("wss://relay{}.test", i).into(),
                                        event: NostrRelayEvent::Ping,
                                    };
                                    while prod.push(black_box(msg.clone())).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    let total_messages = count * 1000;
                    for _ in 0..total_messages {
                        loop {
                            if let Some(msg) = consumer.pop() {
                                black_box(msg);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_multi_producer_zero_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_producer_zero_copy");

    for num_producers in [2, 4, 8].iter() {
        let total: u64 = 1000 * num_producers;
        group.throughput(Throughput::Elements(total));

        group.bench_with_input(
            BenchmarkId::from_parameter(num_producers),
            num_producers,
            |b, &count| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            4096,
                        ))
                        .split();

                    let handles: Vec<_> = (0..count)
                        .map(|i| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for _ in 0..1000 {
                                    let msg = PoolMessage::RelayEvent {
                                        relay_url: format!("wss://relay{}.test", i).into(),
                                        event: NostrRelayEvent::Ping,
                                    };
                                    while prod.push(black_box(msg.clone())).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    let total_messages = count as usize * 1000;
                    for _ in 0..total_messages {
                        loop {
                            if let Some(reader) = consumer.pop_ref() {
                                black_box(&*reader);
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_ring_buffer_throughput,
    bench_spsc_throughput,
    bench_zero_copy,
    bench_multi_producer,
    bench_multi_producer_zero_copy
);
criterion_main!(benches);
