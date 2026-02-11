use std::time::Instant;

/// Simulates the SeenNotes structure used in NostrPool
type SeenNotes = nostro2_cache::Cache;

fn generate_event_id(index: usize) -> String {
    format!("event_{:016x}", index)
}

fn format_duration(ns: u128) -> String {
    if ns < 1_000 {
        format!("{} ns", ns)
    } else if ns < 1_000_000 {
        format!("{:.2} µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.2} ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2} s", ns as f64 / 1_000_000_000.0)
    }
}

fn bench_sequential_insertions() {
    println!("\n=== Sequential Insertions (Unique Events) ===");

    for size in [1_000, 10_000, 100_000] {
        let seen = SeenNotes::new(size * 2);
        let start = Instant::now();

        for i in 0..size {
            seen.insert(generate_event_id(i));
        }

        let elapsed = start.elapsed().as_nanos();
        let per_op = elapsed / size as u128;

        println!(
            "  {} events: {} total, {} per insert",
            size,
            format_duration(elapsed),
            format_duration(per_op)
        );
    }
}

fn bench_duplicate_detection() {
    println!("\n=== Duplicate Detection ===");

    for size in [1_000, 10_000, 100_000] {
        let seen = SeenNotes::new(size * 2);

        // Pre-populate
        for i in 0..size {
            seen.insert(generate_event_id(i));
        }

        // Test duplicate insertions
        let start = Instant::now();
        for i in 0..size {
            seen.insert(generate_event_id(i));
        }
        let elapsed = start.elapsed().as_nanos();
        let per_op = elapsed / size as u128;

        println!(
            "  {} duplicates: {} total, {} per duplicate check",
            size,
            format_duration(elapsed),
            format_duration(per_op)
        );
    }
}

fn bench_lookups(iterations: usize) {
    println!("\n=== Lookup Performance ===");

    for size in [1_000, 10_000, 100_000] {
        let seen = SeenNotes::new(size * 2);

        // Pre-populate
        for i in 0..size {
            seen.insert(generate_event_id(i));
        }

        // Test cache hits
        let start = Instant::now();
        for _ in 0..iterations {
            seen.contains(&generate_event_id(size / 2));
        }
        let elapsed = start.elapsed().as_nanos();
        let per_op = elapsed / iterations as u128;

        println!(
            "  {} events (cache hit): {} per lookup",
            size,
            format_duration(per_op)
        );

        // Test cache misses
        let start = Instant::now();
        for _ in 0..iterations {
            seen.contains(&generate_event_id(size + 1000));
        }
        let elapsed = start.elapsed().as_nanos();
        let per_op = elapsed / iterations as u128;

        println!(
            "  {} events (cache miss): {} per lookup",
            size,
            format_duration(per_op)
        );
    }
}

async fn bench_concurrent_insertions(num_tasks: usize, ops_per_task: usize) {
    println!("\n=== Concurrent Insertions ({} tasks, {} ops each) ===", num_tasks, ops_per_task);

    for cache_size in [10_000, 100_000] {
        let seen = SeenNotes::new(cache_size);
        let start = Instant::now();

        let tasks: Vec<_> = (0..num_tasks)
            .map(|task_id| {
                let seen = seen.clone();
                tokio::spawn(async move {
                    for i in 0..ops_per_task {
                        let id = generate_event_id(task_id * ops_per_task + i);
                        seen.insert(id);
                    }
                })
            })
            .collect();

        for task in tasks {
            task.await.unwrap();
        }

        let elapsed = start.elapsed().as_nanos();
        let total_ops = num_tasks * ops_per_task;
        let per_op = elapsed / total_ops as u128;

        println!(
            "  Cache size {}: {} total, {} per insert",
            cache_size,
            format_duration(elapsed),
            format_duration(per_op)
        );
    }
}

async fn bench_mixed_workload(num_tasks: usize, ops_per_task: usize) {
    println!("\n=== Mixed Workload ({} tasks, {} ops each) ===", num_tasks, ops_per_task);

    let seen = SeenNotes::new(100_000);

    // Pre-populate with 50% capacity
    for i in 0..50_000 {
        seen.insert(generate_event_id(i));
    }

    let start = Instant::now();

    let tasks: Vec<_> = (0..num_tasks)
        .map(|task_id| {
            let seen = seen.clone();
            tokio::spawn(async move {
                for i in 0..ops_per_task {
                    if i % 2 == 0 {
                        // Write operation
                        let id = generate_event_id(task_id * ops_per_task + i + 100_000);
                        seen.insert(id);
                    } else {
                        // Read operation
                        let id = generate_event_id(i);
                        seen.contains(&id);
                    }
                }
            })
        })
        .collect();

    for task in tasks {
        task.await.unwrap();
    }

    let elapsed = start.elapsed().as_nanos();
    let total_ops = num_tasks * ops_per_task;
    let per_op = elapsed / total_ops as u128;

    println!(
        "  50/50 read/write: {} total, {} per operation",
        format_duration(elapsed),
        format_duration(per_op)
    );
}

fn bench_lru_eviction() {
    println!("\n=== LRU Eviction Performance ===");

    for capacity in [1_000, 10_000, 50_000] {
        let seen = SeenNotes::new(capacity);

        // Fill to capacity
        for i in 0..capacity {
            seen.insert(generate_event_id(i));
        }

        // Measure eviction cost
        let start = Instant::now();
        for i in capacity..(capacity * 2) {
            seen.insert(generate_event_id(i));
        }
        let elapsed = start.elapsed().as_nanos();
        let per_op = elapsed / capacity as u128;

        println!(
            "  Capacity {}: {} per insert (with eviction)",
            capacity,
            format_duration(per_op)
        );
    }
}

#[tokio::main]
async fn main() {
    println!("Quick Benchmarks - NostrO2 Relay Deduplication");
    println!("==============================================");

    let lookup_iterations = 10_000;

    bench_sequential_insertions();
    bench_duplicate_detection();
    bench_lookups(lookup_iterations);
    bench_concurrent_insertions(4, 10_000).await;
    bench_concurrent_insertions(16, 2_500).await;
    bench_mixed_workload(4, 5_000).await;
    bench_lru_eviction();

    println!("\n==============================================");
    println!("Done! Run with `cargo run --release --bin quick-bench`");
}
