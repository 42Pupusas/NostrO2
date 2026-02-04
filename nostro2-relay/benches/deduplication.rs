use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Simulates the SeenNotes structure used in NostrPool
#[derive(Debug, Clone, Default)]
struct SeenNotes(Arc<Mutex<HashSet<Option<String>>>>);

impl SeenNotes {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(HashSet::new())))
    }

    async fn add(&self, id: Option<String>) -> bool {
        let mut seen = self.0.lock().await;
        seen.insert(id)
    }

    async fn contains(&self, id: &Option<String>) -> bool {
        let seen = self.0.lock().await;
        seen.contains(id)
    }

    async fn len(&self) -> usize {
        let seen = self.0.lock().await;
        seen.len()
    }
}

/// Generate unique event IDs for testing
fn generate_event_id(index: usize) -> Option<String> {
    Some(format!("event_{:016x}", index))
}

fn bench_sequential_insertions(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("sequential_insertions");

    for size in [100, 1000, 10_000, 100_000].iter() {
        group.bench_with_input(BenchmarkId::new("unique", size), size, |b, &size| {
            b.to_async(&runtime).iter(|| async {
                let seen = SeenNotes::new();
                for i in 0..size {
                    seen.add(generate_event_id(i)).await;
                }
                black_box(seen)
            });
        });

        group.bench_with_input(BenchmarkId::new("duplicates", size), size, |b, &size| {
            b.to_async(&runtime).iter(|| async {
                let seen = SeenNotes::new();
                // Insert half unique, then repeat
                for i in 0..(size / 2) {
                    seen.add(generate_event_id(i)).await;
                }
                for i in 0..(size / 2) {
                    seen.add(generate_event_id(i)).await;
                }
                black_box(seen)
            });
        });
    }

    group.finish();
}

fn bench_lookup_performance(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("lookup_performance");

    for size in [100, 1000, 10_000, 100_000].iter() {
        // Pre-populate the HashSet
        let seen = runtime.block_on(async {
            let seen = SeenNotes::new();
            for i in 0..*size {
                seen.add(generate_event_id(i)).await;
            }
            seen
        });

        group.bench_with_input(BenchmarkId::new("contains_hit", size), size, |b, &size| {
            b.to_async(&runtime).iter(|| {
                let seen = seen.clone();
                async move {
                    // Look up an event that exists (middle of range)
                    let id = generate_event_id(size / 2);
                    black_box(seen.contains(&id).await)
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("contains_miss", size), size, |b, &size| {
            b.to_async(&runtime).iter(|| {
                let seen = seen.clone();
                async move {
                    // Look up an event that doesn't exist
                    let id = generate_event_id(size + 1000);
                    black_box(seen.contains(&id).await)
                }
            });
        });
    }

    group.finish();
}

fn bench_concurrent_insertions(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_insertions");
    group.sample_size(20); // Reduce sample size for slower concurrent tests

    for num_tasks in [2, 4, 8, 16].iter() {
        group.bench_with_input(BenchmarkId::new("tasks", num_tasks), num_tasks, |b, &num_tasks| {
            b.to_async(&runtime).iter(|| async move {
                let seen = SeenNotes::new();
                let tasks: Vec<_> = (0..num_tasks)
                    .map(|task_id| {
                        let seen = seen.clone();
                        tokio::spawn(async move {
                            for i in 0..1000 {
                                let id = generate_event_id(task_id * 1000 + i);
                                seen.add(id).await;
                            }
                        })
                    })
                    .collect();

                for task in tasks {
                    task.await.unwrap();
                }

                black_box(seen)
            });
        });
    }

    group.finish();
}

fn bench_concurrent_mixed_operations(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_mixed");
    group.sample_size(20);

    for num_tasks in [2, 4, 8].iter() {
        // Pre-populate with some data
        let seen = runtime.block_on(async {
            let seen = SeenNotes::new();
            for i in 0..5000 {
                seen.add(generate_event_id(i)).await;
            }
            seen
        });

        group.bench_with_input(BenchmarkId::new("read_write", num_tasks), num_tasks, |b, &num_tasks| {
            b.to_async(&runtime).iter(|| {
                let seen = seen.clone();
                async move {
                    let tasks: Vec<_> = (0..num_tasks)
                        .map(|task_id| {
                            let seen = seen.clone();
                            tokio::spawn(async move {
                                for i in 0..500 {
                                    if i % 2 == 0 {
                                        // Write operation
                                        let id = generate_event_id(task_id * 1000 + i);
                                        seen.add(id).await;
                                    } else {
                                        // Read operation
                                        let id = generate_event_id(i);
                                        seen.contains(&id).await;
                                    }
                                }
                            })
                        })
                        .collect();

                    for task in tasks {
                        task.await.unwrap();
                    }

                    black_box(seen)
                }
            });
        });
    }

    group.finish();
}

fn bench_insertion_single_op(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("single_operation");

    // Benchmark the cost of a single insert operation at different HashSet sizes
    for size in [0, 100, 1000, 10_000, 100_000].iter() {
        let seen = runtime.block_on(async {
            let seen = SeenNotes::new();
            for i in 0..*size {
                seen.add(generate_event_id(i)).await;
            }
            seen
        });

        group.bench_with_input(BenchmarkId::new("insert_at_size", size), size, |b, _| {
            let mut counter = 0;
            b.to_async(&runtime).iter(|| {
                let seen = seen.clone();
                async move {
                    counter += 1;
                    let id = generate_event_id(1_000_000 + counter);
                    black_box(seen.add(id).await)
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("lookup_at_size", size), size, |b, &size| {
            b.to_async(&runtime).iter(|| {
                let seen = seen.clone();
                async move {
                    let id = generate_event_id(size / 2);
                    black_box(seen.contains(&id).await)
                }
            });
        });
    }

    group.finish();
}

fn bench_memory_overhead(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("memory_overhead");
    group.sample_size(10);

    // Benchmark memory allocation patterns
    for size in [10_000, 50_000, 100_000].iter() {
        group.bench_with_input(BenchmarkId::new("allocate", size), size, |b, &size| {
            b.to_async(&runtime).iter(|| async move {
                let seen = SeenNotes::new();
                for i in 0..size {
                    seen.add(generate_event_id(i)).await;
                }
                let len = seen.len().await;
                black_box(len)
            });
        });
    }

    group.finish();
}

criterion_group!(
    deduplication_benches,
    bench_sequential_insertions,
    bench_lookup_performance,
    bench_concurrent_insertions,
    bench_concurrent_mixed_operations,
    bench_insertion_single_op,
    bench_memory_overhead,
);
criterion_main!(deduplication_benches);
