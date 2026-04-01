use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nostro2::NostrRelayEvent;
use ring_relay_client::PoolMessage;
use quetzalcoatl::capacity::Capacity;
use std::time::{Duration, Instant};

const TOTAL_EVENTS: usize = 10_000;
// Target ~20KB per NostrNote: 64 (id) + 64 (pubkey) + 8 (created_at) + 8 (kind)
// + tags overhead + content + 128 (sig) ~ content needs to be ~19.5KB
const CONTENT_SIZE: usize = 20_000;

fn generate_large_event(id: usize) -> NostrRelayEvent {
    let base = format!("Large event payload {id}: ");
    let padding = "X".repeat(CONTENT_SIZE - base.len());

    let note = nostro2::NostrNote {
        id: Some(format!("{:064x}", id)),
        pubkey: "deadbeef".repeat(8),
        created_at: 1234567890,
        kind: 1,
        tags: vec![
            vec!["e".into(), format!("{:064x}", id.wrapping_add(1))],
            vec!["p".into(), "cafebabe".repeat(8)],
        ]
        .into(),
        content: format!("{base}{padding}"),
        sig: Some("a]".repeat(64)),
    };
    NostrRelayEvent::NewNote(nostro2::RelayEventTag::Event, "test_sub".to_string(), note)
}

fn make_large_msg(thread_id: usize, i: usize, events_per_producer: usize) -> PoolMessage {
    PoolMessage::RelayEvent {
        relay_url: format!("test_{}", thread_id).into(),
        event: generate_large_event(thread_id * events_per_producer + i),
    }
}

fn print_stats(label: &str, producers: usize, elapsed: Duration) {
    let throughput_mb =
        (TOTAL_EVENTS as f64 * CONTENT_SIZE as f64) / elapsed.as_secs_f64() / 1_048_576.0;
    println!(
        "  {} (~20KB): {} producers, {}K events in {:?} ({:.0} events/sec, {:.0} MB/sec)",
        label,
        producers,
        TOTAL_EVENTS / 1000,
        elapsed,
        TOTAL_EVENTS as f64 / elapsed.as_secs_f64(),
        throughput_mb,
    );
}

fn bench_ring_relay_large_payload(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_payload");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for num_producers in [1, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(TOTAL_EVENTS as u64));

        group.bench_with_input(
            BenchmarkId::new("ring_relay_producers", num_producers),
            num_producers,
            |b, &producers| {
                b.iter(|| {
                    let (producer, mut consumer) = quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(4096)).split();
                    let events_per_producer = TOTAL_EVENTS / producers;

                    let handles: Vec<_> = (0..producers)
                        .map(|thread_id| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for i in 0..events_per_producer {
                                    let msg = make_large_msg(thread_id, i, events_per_producer);
                                    while prod.push(msg.clone()).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    let mut received = 0;
                    let start = Instant::now();
                    while received < TOTAL_EVENTS {
                        if let Some(PoolMessage::RelayEvent { .. }) = consumer.pop() {
                            received += 1;
                        }
                    }
                    let elapsed = start.elapsed();

                    for handle in handles {
                        handle.join().unwrap();
                    }

                    print_stats("Ring", producers, elapsed);
                });
            },
        );
    }
    group.finish();
}

fn bench_spsc_large_payload(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_payload_spsc");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(TOTAL_EVENTS as u64));

    // SPSC with pop() - single producer, large payloads
    group.bench_function("spsc_pop", |b| {
        b.iter(|| {
            let (producer, mut consumer) =
                quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(4096))
                    .split();

            std::thread::spawn(move || {
                for i in 0..TOTAL_EVENTS {
                    let msg = make_large_msg(0, i, TOTAL_EVENTS);
                    while producer.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });

            let mut received = 0;
            let start = Instant::now();
            while received < TOTAL_EVENTS {
                if consumer.pop().is_some() {
                    received += 1;
                }
            }
            let elapsed = start.elapsed();
            print_stats("SPSC pop", 1, elapsed);
        });
    });

    // SPSC with pop_ref() - zero-copy avoids moving ~20KB per event
    group.bench_function("spsc_pop_ref", |b| {
        b.iter(|| {
            let (producer, mut consumer) =
                quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(4096))
                    .split();

            std::thread::spawn(move || {
                for i in 0..TOTAL_EVENTS {
                    let msg = make_large_msg(0, i, TOTAL_EVENTS);
                    while producer.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });

            let mut received = 0;
            let start = Instant::now();
            while received < TOTAL_EVENTS {
                if consumer.pop_ref().is_some() {
                    received += 1;
                }
            }
            let elapsed = start.elapsed();
            print_stats("SPSC pop_ref", 1, elapsed);
        });
    });

    // MPSC single-producer with pop_ref() for comparison
    group.bench_function("mpsc_1_pop_ref", |b| {
        b.iter(|| {
            let (producer, mut consumer) =
                quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(4096))
                    .split();

            std::thread::spawn(move || {
                for i in 0..TOTAL_EVENTS {
                    let msg = make_large_msg(0, i, TOTAL_EVENTS);
                    while producer.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });

            let mut received = 0;
            let start = Instant::now();
            while received < TOTAL_EVENTS {
                if consumer.pop_ref().is_some() {
                    received += 1;
                }
            }
            let elapsed = start.elapsed();
            print_stats("MPSC(1) pop_ref", 1, elapsed);
        });
    });

    group.finish();
}

fn bench_large_payload_zero_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_payload_zero_copy");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for num_producers in [1, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(TOTAL_EVENTS as u64));

        group.bench_with_input(
            BenchmarkId::new("mpsc_pop_ref_producers", num_producers),
            num_producers,
            |b, &producers| {
                b.iter(|| {
                    let (producer, mut consumer) =
                        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(
                            4096,
                        ))
                        .split();
                    let events_per_producer = TOTAL_EVENTS / producers;

                    let handles: Vec<_> = (0..producers)
                        .map(|thread_id| {
                            let prod = producer.clone();
                            std::thread::spawn(move || {
                                for i in 0..events_per_producer {
                                    let msg = make_large_msg(thread_id, i, events_per_producer);
                                    while prod.push(msg.clone()).is_err() {
                                        std::hint::spin_loop();
                                    }
                                }
                            })
                        })
                        .collect();

                    let mut received = 0;
                    let start = Instant::now();
                    while received < TOTAL_EVENTS {
                        if consumer.pop_ref().is_some() {
                            received += 1;
                        }
                    }
                    let elapsed = start.elapsed();

                    for handle in handles {
                        handle.join().unwrap();
                    }

                    print_stats("Ring pop_ref", producers, elapsed);
                });
            },
        );
    }
    group.finish();
}

fn bench_async_relay_large_payload(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_payload");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for num_senders in [1, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(TOTAL_EVENTS as u64));

        group.bench_with_input(
            BenchmarkId::new("async_relay_senders", num_senders),
            num_senders,
            |b, &senders| {
                let rt = tokio::runtime::Runtime::new().unwrap();

                b.iter(|| {
                    rt.block_on(async {
                        let (tx, mut rx) =
                            tokio::sync::mpsc::unbounded_channel::<NostrRelayEvent>();
                        let events_per_sender = TOTAL_EVENTS / senders;

                        for thread_id in 0..senders {
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                for i in 0..events_per_sender {
                                    let event =
                                        generate_large_event(thread_id * events_per_sender + i);
                                    tx.send(event).unwrap();
                                }
                            });
                        }
                        drop(tx);

                        let dedup = nostro2_cache::Cache::new(10_000);
                        let mut received = 0;
                        let start = Instant::now();

                        while received < TOTAL_EVENTS {
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

                        print_stats("Async", senders, elapsed);
                    });
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_ring_relay_large_payload,
    bench_spsc_large_payload,
    bench_large_payload_zero_copy,
    bench_async_relay_large_payload
);
criterion_main!(benches);
