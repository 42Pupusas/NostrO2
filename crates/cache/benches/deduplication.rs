//! Benchmarks for `nostro2_cache::Cache` — the std-Mutex + LRU dedup cache.
//!
//! The earlier multi-strategy comparison (DashMap / parking_lot / std::Mutex)
//! lived here while the cache crate exported three competing implementations.
//! After the std::Mutex variant won the relay-pool shootout, the others were
//! deleted from the public surface; this bench was rewritten to match.
//!
//! Three benches:
//! - `single_thread_insert`: pure throughput, no contention.
//! - `multi_thread_insert`: sweep concurrent writers (2–20).
//! - `realistic_relay_pattern`: 10 writers, 20 % duplicate rate (relay pool).

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nostro2_cache::Cache;

fn generate_event_id(n: usize) -> String {
    format!("{n:064x}")
}

fn bench_single_thread_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_thread_insert");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("std_mutex_lru", |b| {
        let cache = Cache::new(10_000);
        let mut counter = 0_usize;
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

    for &num_threads in &[2_usize, 4, 8, 10, 20] {
        group.throughput(Throughput::Elements((1000 * num_threads) as u64));

        group.bench_with_input(
            BenchmarkId::new("std_mutex_lru", num_threads),
            &num_threads,
            |b, &threads| {
                b.iter(|| {
                    let cache = Cache::new(10_000);
                    let handles: Vec<_> = (0..threads)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            std::thread::spawn(move || {
                                for i in 0..1000 {
                                    let id = generate_event_id(thread_id * 1000 + i);
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

    // 10 concurrent writers, ~20 % duplicate rate — mirrors what the relay
    // pool sees during a typical fan-out from many connected relays.
    let num_threads = 10_usize;
    let events_per_thread = 1000_usize;

    group.bench_function("std_mutex_lru_20pct_dupes", |b| {
        b.iter(|| {
            let cache = Cache::new(10_000);
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
