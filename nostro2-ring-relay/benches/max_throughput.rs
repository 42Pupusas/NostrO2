use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nostro2::NostrRelayEvent;
use nostro2_ring_relay::{PoolMessage, create_pool};
use quetzalcoatl::capacity::Capacity;
use std::time::{Duration, Instant};

fn generate_event(id: usize) -> NostrRelayEvent {
    let note = nostro2::NostrNote {
        id: Some(format!("{:064x}", id)),
        pubkey: "test_pubkey".to_string(),
        created_at: 1234567890,
        kind: 1,
        tags: vec![].into(),
        content: format!("Test event {}", id),
        sig: Some("test_sig".to_string()),
    };
    NostrRelayEvent::NewNote(nostro2::RelayEventTag::Event, "test_sub".to_string(), note)
}

fn make_msg(thread_id: usize, i: usize, events_per_producer: usize) -> PoolMessage {
    PoolMessage::RelayEvent {
        relay_url: format!("test_{}", thread_id).into(),
        event: generate_event(thread_id * events_per_producer + i),
    }
}

fn bench_ring_relay_max_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("max_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for num_producers in [1, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(100_000));

        group.bench_with_input(
            BenchmarkId::new("ring_relay_producers", num_producers),
            num_producers,
            |b, &producers| {
                b.iter(|| {
                    let (mut consumer, producer) = create_pool(16384, 10_000);
                    let events_per_producer = 100_000 / producers;

                    let handles: Vec<_> = (0..producers)
                        .map(|thread_id| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for i in 0..events_per_producer {
                                    let msg = make_msg(thread_id, i, events_per_producer);
                                    while prod.push(msg.clone()).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    let mut received = 0;
                    let start = Instant::now();
                    while received < 100_000 {
                        if let Some(PoolMessage::RelayEvent { .. }) = consumer.try_recv() {
                            received += 1;
                        }
                    }
                    let elapsed = start.elapsed();

                    for handle in handles {
                        handle.join().unwrap();
                    }

                    println!(
                        "  Ring Relay: {} producers, 100K events in {:?} ({:.0} events/sec)",
                        producers,
                        elapsed,
                        100_000.0 / elapsed.as_secs_f64()
                    );
                });
            },
        );
    }
    group.finish();
}

fn bench_spsc_max_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("spsc_max_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(100_000));

    // SPSC with pop()
    group.bench_function("spsc_pop", |b| {
        b.iter(|| {
            let (producer, mut consumer) =
                quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384))
                    .split();

            std::thread::spawn(move || {
                for i in 0..100_000 {
                    let msg = make_msg(0, i, 100_000);
                    while producer.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });

            let mut received = 0;
            let start = Instant::now();
            while received < 100_000 {
                if consumer.pop().is_some() {
                    received += 1;
                }
            }
            let elapsed = start.elapsed();

            println!(
                "  SPSC pop: 100K events in {:?} ({:.0} events/sec)",
                elapsed,
                100_000.0 / elapsed.as_secs_f64()
            );
        });
    });

    // SPSC with pop_ref() (zero-copy)
    group.bench_function("spsc_pop_ref", |b| {
        b.iter(|| {
            let (producer, mut consumer) =
                quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384))
                    .split();

            std::thread::spawn(move || {
                for i in 0..100_000 {
                    let msg = make_msg(0, i, 100_000);
                    while producer.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });

            let mut received = 0;
            let start = Instant::now();
            while received < 100_000 {
                if consumer.pop_ref().is_some() {
                    received += 1;
                }
            }
            let elapsed = start.elapsed();

            println!(
                "  SPSC pop_ref: 100K events in {:?} ({:.0} events/sec)",
                elapsed,
                100_000.0 / elapsed.as_secs_f64()
            );
        });
    });

    // MPSC single-producer with pop_ref() for comparison
    group.bench_function("mpsc_1_pop_ref", |b| {
        b.iter(|| {
            let (producer, mut consumer) =
                quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384))
                    .split();

            std::thread::spawn(move || {
                for i in 0..100_000 {
                    let msg = make_msg(0, i, 100_000);
                    while producer.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });

            let mut received = 0;
            let start = Instant::now();
            while received < 100_000 {
                if consumer.pop_ref().is_some() {
                    received += 1;
                }
            }
            let elapsed = start.elapsed();

            println!(
                "  MPSC(1) pop_ref: 100K events in {:?} ({:.0} events/sec)",
                elapsed,
                100_000.0 / elapsed.as_secs_f64()
            );
        });
    });

    group.finish();
}

fn bench_multi_producer_zero_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("max_throughput_zero_copy");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for num_producers in [1, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(100_000));

        group.bench_with_input(
            BenchmarkId::new("mpsc_pop_ref_producers", num_producers),
            num_producers,
            |b, &producers| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            16384,
                        ))
                        .split();
                    let events_per_producer = 100_000 / producers;

                    let handles: Vec<_> = (0..producers)
                        .map(|thread_id| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for i in 0..events_per_producer {
                                    let msg = make_msg(thread_id, i, events_per_producer);
                                    while prod.push(msg.clone()).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    let mut received = 0;
                    let start = Instant::now();
                    while received < 100_000 {
                        if consumer.pop_ref().is_some() {
                            received += 1;
                        }
                    }
                    let elapsed = start.elapsed();

                    for handle in handles {
                        handle.join().unwrap();
                    }

                    println!(
                        "  Ring pop_ref: {} producers, 100K events in {:?} ({:.0} events/sec)",
                        producers,
                        elapsed,
                        100_000.0 / elapsed.as_secs_f64()
                    );
                });
            },
        );
    }
    group.finish();
}

fn bench_async_relay_max_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("max_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for num_senders in [1, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(100_000));

        group.bench_with_input(
            BenchmarkId::new("async_relay_senders", num_senders),
            num_senders,
            |b, &senders| {
                let rt = tokio::runtime::Runtime::new().unwrap();

                b.iter(|| {
                    rt.block_on(async {
                        let (tx, mut rx) =
                            tokio::sync::mpsc::unbounded_channel::<NostrRelayEvent>();
                        let events_per_sender = 100_000 / senders;

                        for thread_id in 0..senders {
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                for i in 0..events_per_sender {
                                    let event = generate_event(thread_id * events_per_sender + i);
                                    tx.send(event).unwrap();
                                }
                            });
                        }
                        drop(tx);

                        let dedup = nostro2_cache::Cache::new(10_000);
                        let mut received = 0;
                        let start = Instant::now();

                        while received < 100_000 {
                            if let Some(NostrRelayEvent::NewNote(_, _, note)) = rx.recv().await {
                                if let Some(ref id) = note.id {
                                    if dedup.insert(id.clone()) {
                                        received += 1;
                                    }
                                } else {
                                    received += 1;
                                }
                            }
                        }
                        let elapsed = start.elapsed();

                        println!(
                            "  Async Relay: {} senders, 100K events in {:?} ({:.0} events/sec)",
                            senders,
                            elapsed,
                            100_000.0 / elapsed.as_secs_f64()
                        );
                    });
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_ring_relay_max_throughput,
    bench_spsc_max_throughput,
    bench_multi_producer_zero_copy,
    bench_async_relay_max_throughput
);
criterion_main!(benches);
