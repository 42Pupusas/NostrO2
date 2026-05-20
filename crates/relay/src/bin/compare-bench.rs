use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// Old implementation: unbounded HashSet
#[derive(Debug, Clone)]
struct SeenNotesHashSet(Arc<Mutex<HashSet<String>>>);

impl SeenNotesHashSet {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(HashSet::new())))
    }

    async fn insert(&self, id: String) -> bool {
        let mut seen = self.0.lock().await;
        seen.insert(id)
    }

    async fn contains(&self, id: &str) -> bool {
        let seen = self.0.lock().await;
        seen.contains(id)
    }

    async fn len(&self) -> usize {
        let seen = self.0.lock().await;
        seen.len()
    }
}

/// New implementation: bounded LRU cache
type SeenNotesLRU = nostro2_cache::Cache;

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

async fn bench_sequential_insertions(num_events: usize) {
    println!("\n=== Sequential Insertions ({} events) ===", num_events);

    // Benchmark HashSet
    let seen_hash = SeenNotesHashSet::new();
    let start = Instant::now();
    for i in 0..num_events {
        seen_hash.insert(generate_event_id(i)).await;
    }
    let hash_time = start.elapsed();
    let hash_size = seen_hash.len().await;

    // Benchmark LRU (with enough capacity to not evict)
    let seen_lru = SeenNotesLRU::new(num_events * 2);
    let start = Instant::now();
    for i in 0..num_events {
        seen_lru.insert(generate_event_id(i));
    }
    let lru_time = start.elapsed();
    let lru_size = seen_lru.len();

    println!(
        "  HashSet: {} ({} events stored)",
        format_duration(hash_time.as_nanos()),
        hash_size
    );
    println!(
        "  LRU:     {} ({} events stored)",
        format_duration(lru_time.as_nanos()),
        lru_size
    );
    println!(
        "  Difference: {:.2}%",
        ((lru_time.as_nanos() as f64 / hash_time.as_nanos() as f64) - 1.0) * 100.0
    );
}

async fn bench_duplicate_detection(num_events: usize) {
    println!("\n=== Duplicate Detection ({} events) ===", num_events);

    // Pre-populate both
    let seen_hash = SeenNotesHashSet::new();
    for i in 0..num_events {
        seen_hash.insert(generate_event_id(i)).await;
    }

    let seen_lru = SeenNotesLRU::new(num_events * 2);
    for i in 0..num_events {
        seen_lru.insert(generate_event_id(i));
    }

    // Benchmark duplicate insertions
    let start = Instant::now();
    for i in 0..num_events {
        seen_hash.insert(generate_event_id(i)).await;
    }
    let hash_time = start.elapsed();

    let start = Instant::now();
    for i in 0..num_events {
        seen_lru.insert(generate_event_id(i));
    }
    let lru_time = start.elapsed();

    println!("  HashSet: {}", format_duration(hash_time.as_nanos()));
    println!("  LRU:     {}", format_duration(lru_time.as_nanos()));
    println!(
        "  Difference: {:.2}%",
        ((lru_time.as_nanos() as f64 / hash_time.as_nanos() as f64) - 1.0) * 100.0
    );
}

async fn bench_lookups(num_events: usize, num_lookups: usize) {
    println!(
        "\n=== Lookup Performance ({} events, {} lookups) ===",
        num_events, num_lookups
    );

    // Pre-populate both
    let seen_hash = SeenNotesHashSet::new();
    for i in 0..num_events {
        seen_hash.insert(generate_event_id(i)).await;
    }

    let seen_lru = SeenNotesLRU::new(num_events * 2);
    for i in 0..num_events {
        seen_lru.insert(generate_event_id(i));
    }

    // Benchmark cache hits
    let id = generate_event_id(num_events / 2);

    let start = Instant::now();
    for _ in 0..num_lookups {
        seen_hash.contains(&id).await;
    }
    let hash_time = start.elapsed();

    let start = Instant::now();
    for _ in 0..num_lookups {
        seen_lru.contains(&id);
    }
    let lru_time = start.elapsed();

    println!(
        "  HashSet: {} per lookup",
        format_duration(hash_time.as_nanos() / num_lookups as u128)
    );
    println!(
        "  LRU:     {} per lookup",
        format_duration(lru_time.as_nanos() / num_lookups as u128)
    );
    println!(
        "  Difference: {:.2}%",
        ((lru_time.as_nanos() as f64 / hash_time.as_nanos() as f64) - 1.0) * 100.0
    );
}

async fn bench_concurrent_insertions(num_tasks: usize, ops_per_task: usize) {
    println!(
        "\n=== Concurrent Insertions ({} tasks, {} ops each) ===",
        num_tasks, ops_per_task
    );

    // Benchmark HashSet
    let seen_hash = SeenNotesHashSet::new();
    let start = Instant::now();

    let tasks: Vec<_> = (0..num_tasks)
        .map(|task_id| {
            let seen = seen_hash.clone();
            tokio::spawn(async move {
                for i in 0..ops_per_task {
                    let id = generate_event_id(task_id * ops_per_task + i);
                    seen.insert(id).await;
                }
            })
        })
        .collect();

    for task in tasks {
        task.await.unwrap();
    }
    let hash_time = start.elapsed();

    // Benchmark LRU
    let seen_lru = SeenNotesLRU::new(num_tasks * ops_per_task * 2);
    let start = Instant::now();

    let tasks: Vec<_> = (0..num_tasks)
        .map(|task_id| {
            let seen = seen_lru.clone();
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
    let lru_time = start.elapsed();

    println!("  HashSet: {}", format_duration(hash_time.as_nanos()));
    println!("  LRU:     {}", format_duration(lru_time.as_nanos()));
    println!(
        "  Difference: {:.2}%",
        ((lru_time.as_nanos() as f64 / hash_time.as_nanos() as f64) - 1.0) * 100.0
    );
}

async fn bench_memory_growth() {
    println!("\n=== Memory Growth Test (Unbounded vs Bounded) ===");

    let sizes = [10_000, 50_000, 100_000, 500_000];

    for size in sizes {
        println!("\n  Inserting {} events:", size);

        // HashSet - grows unbounded
        let seen_hash = SeenNotesHashSet::new();
        let start = Instant::now();
        for i in 0..size {
            seen_hash.insert(generate_event_id(i)).await;
        }
        let hash_time = start.elapsed();
        let hash_len = seen_hash.len().await;

        // LRU - bounded to 100K capacity
        let seen_lru = SeenNotesLRU::new(100_000);
        let start = Instant::now();
        for i in 0..size {
            seen_lru.insert(generate_event_id(i));
        }
        let lru_time = start.elapsed();
        let lru_len = seen_lru.len();

        println!(
            "    HashSet: {} (stores {} events)",
            format_duration(hash_time.as_nanos()),
            hash_len
        );
        println!(
            "    LRU:     {} (stores {} events, capped at 100K)",
            format_duration(lru_time.as_nanos()),
            lru_len
        );

        if size > 100_000 {
            println!(
                "    Memory saved by LRU: ~{} events not stored",
                hash_len - lru_len
            );
        }
    }
}

#[tokio::main]
async fn main() {
    println!("==============================================");
    println!("HashMap vs LRU Cache Comparison Benchmark");
    println!("==============================================");

    bench_sequential_insertions(10_000).await;
    bench_sequential_insertions(100_000).await;

    bench_duplicate_detection(10_000).await;

    bench_lookups(10_000, 10_000).await;

    bench_concurrent_insertions(4, 10_000).await;
    bench_concurrent_insertions(16, 2_500).await;

    bench_memory_growth().await;

    println!("\n==============================================");
    println!("Summary");
    println!("==============================================");
    println!("LRU Cache advantages:");
    println!("  ✓ Bounded memory usage (prevents OOM)");
    println!("  ✓ Automatic eviction of old entries");
    println!("  ✓ Configurable capacity");
    println!();
    println!("HashMap advantages:");
    println!("  ✓ Slightly faster for small datasets");
    println!("  ✓ No eviction overhead");
    println!();
    println!("Trade-off: ~10-50% slower on some operations,");
    println!("but prevents unbounded memory growth in long-running relays.");
}
