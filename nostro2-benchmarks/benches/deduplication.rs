use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nostro2_cache::{DashMapCache, ParkingLotLruCache, StdMutexLruCache};

// Generate realistic event IDs (64 hex chars like Nostr event IDs)
fn generate_event_id(n: usize) -> String {
    format!("{:064x}", n)
}

fn bench_single_thread_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_thread_insert");
    group.throughput(Throughput::Elements(1000));

    let cache_size = 10_000;

    group.bench_function("dashmap", |b| {
        let cache = DashMapCache::new(cache_size);
        let mut counter = 0;
        b.iter(|| {
            for _ in 0..1000 {
                counter += 1;
                let id = generate_event_id(counter);
                black_box(cache.insert(id));
            }
        });
    });

    group.bench_function("parking_lot_lru", |b| {
        let cache = ParkingLotLruCache::new(cache_size);
        let mut counter = 0;
        b.iter(|| {
            for _ in 0..1000 {
                counter += 1;
                let id = generate_event_id(counter);
                black_box(cache.insert(id));
            }
        });
    });

    group.bench_function("std_mutex_lru", |b| {
        let cache = StdMutexLruCache::new(cache_size);
        let mut counter = 0;
        b.iter(|| {
            for _ in 0..1000 {
                counter += 1;
                let id = generate_event_id(counter);
                black_box(cache.insert(id));
            }
        });
    });

    group.finish();
}

fn bench_multi_thread_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_thread_insert");

    for num_threads in [2, 4, 8, 10, 20].iter() {
        group.throughput(Throughput::Elements(1000 * num_threads));

        group.bench_with_input(
            BenchmarkId::new("dashmap", num_threads),
            num_threads,
            |b, &threads| {
                b.iter(|| {
                    let cache = DashMapCache::new(10_000);
                    let handles: Vec<_> = (0..threads)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            std::thread::spawn(move || {
                                for i in 0..1000 {
                                    let id = generate_event_id(thread_id as usize * 1000 + i);
                                    black_box(cache.insert(id));
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parking_lot_lru", num_threads),
            num_threads,
            |b, &threads| {
                b.iter(|| {
                    let cache = ParkingLotLruCache::new(10_000);
                    let handles: Vec<_> = (0..threads)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            std::thread::spawn(move || {
                                for i in 0..1000 {
                                    let id = generate_event_id(thread_id as usize * 1000 + i);
                                    black_box(cache.insert(id));
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("std_mutex_lru", num_threads),
            num_threads,
            |b, &threads| {
                b.iter(|| {
                    let cache = StdMutexLruCache::new(10_000);
                    let handles: Vec<_> = (0..threads)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            std::thread::spawn(move || {
                                for i in 0..1000 {
                                    let id = generate_event_id(thread_id as usize * 1000 + i);
                                    black_box(cache.insert(id));
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_realistic_relay_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_relay_pattern");
    group.throughput(Throughput::Elements(10_000));

    // Simulate realistic relay pool: 10 threads (relays) with 20% duplicate rate
    let num_threads = 10;
    let events_per_thread = 1000;

    group.bench_function("dashmap_20pct_dupes", |b| {
        b.iter(|| {
            let cache = DashMapCache::new(10_000);
            let handles: Vec<_> = (0..num_threads)
                .map(|thread_id| {
                    let cache = cache.clone();
                    std::thread::spawn(move || {
                        for i in 0..events_per_thread {
                            // 20% duplicates: reuse every 5th ID
                            let id_num = if i % 5 == 0 && i > 0 {
                                thread_id as usize * events_per_thread + i - 1
                            } else {
                                thread_id as usize * events_per_thread + i
                            };
                            let id = generate_event_id(id_num);
                            black_box(cache.insert(id));
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.bench_function("parking_lot_lru_20pct_dupes", |b| {
        b.iter(|| {
            let cache = ParkingLotLruCache::new(10_000);
            let handles: Vec<_> = (0..num_threads)
                .map(|thread_id| {
                    let cache = cache.clone();
                    std::thread::spawn(move || {
                        for i in 0..events_per_thread {
                            let id_num = if i % 5 == 0 && i > 0 {
                                thread_id * events_per_thread + i - 1
                            } else {
                                thread_id * events_per_thread + i
                            };
                            let id = generate_event_id(id_num);
                            black_box(cache.insert(id));
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.bench_function("std_mutex_lru_20pct_dupes", |b| {
        b.iter(|| {
            let cache = StdMutexLruCache::new(10_000);
            let handles: Vec<_> = (0..num_threads)
                .map(|thread_id| {
                    let cache = cache.clone();
                    std::thread::spawn(move || {
                        for i in 0..events_per_thread {
                            let id_num = if i % 5 == 0 && i > 0 {
                                thread_id * events_per_thread + i - 1
                            } else {
                                thread_id * events_per_thread + i
                            };
                            let id = generate_event_id(id_num);
                            black_box(cache.insert(id));
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_single_thread_insert,
    bench_multi_thread_insert,
    bench_realistic_relay_pattern
);
criterion_main!(benches);
