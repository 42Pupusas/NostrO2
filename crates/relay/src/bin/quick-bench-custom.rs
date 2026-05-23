use std::env;
use std::time::Instant;

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

async fn run_custom_benchmark(
    cache_size: usize,
    num_events: usize,
    num_tasks: usize,
    read_write_ratio: f64,
) {
    println!("\n=== Custom Benchmark ===");
    println!("Cache size: {}", cache_size);
    println!("Events: {}", num_events);
    println!("Concurrent tasks: {}", num_tasks);
    println!("Read/Write ratio: {:.0}% reads", read_write_ratio * 100.0);
    println!("------------------------");

    let seen = SeenNotes::new(cache_size);
    let events_per_task = num_events / num_tasks;

    let start = Instant::now();

    let tasks: Vec<_> = (0..num_tasks)
        .map(|task_id| {
            let seen = seen.clone();
            tokio::spawn(async move {
                for i in 0..events_per_task {
                    let id = generate_event_id(task_id * events_per_task + i);

                    // Decide if this is a read or write based on ratio
                    let is_read = (i as f64 / events_per_task as f64) < read_write_ratio;

                    if is_read && i > 0 {
                        // Read from previously inserted data
                        let read_id = generate_event_id(task_id * events_per_task + (i / 2));
                        seen.contains(&read_id);
                    } else {
                        // Write operation
                        seen.insert(id);
                    }
                }
            })
        })
        .collect();

    for task in tasks {
        task.await.unwrap();
    }

    let elapsed = start.elapsed();
    let total_ops = num_events;
    let per_op = elapsed.as_nanos() / total_ops as u128;
    let throughput = (total_ops as f64) / elapsed.as_secs_f64();

    println!("Total time: {}", format_duration(elapsed.as_nanos()));
    println!("Per operation: {}", format_duration(per_op));
    println!("Throughput: {:.2} ops/sec", throughput);
}

fn print_usage() {
    println!("Usage: quick-bench-custom [CACHE_SIZE] [NUM_EVENTS] [NUM_TASKS] [READ_RATIO]");
    println!();
    println!("Arguments:");
    println!("  CACHE_SIZE   - LRU cache capacity (default: 10000)");
    println!("  NUM_EVENTS   - Total number of operations (default: 100000)");
    println!("  NUM_TASKS    - Number of concurrent tasks (default: 4)");
    println!("  READ_RATIO   - Fraction of reads 0.0-1.0 (default: 0.5)");
    println!();
    println!("Examples:");
    println!("  quick-bench-custom                    # Use all defaults");
    println!("  quick-bench-custom 50000              # 50K cache");
    println!("  quick-bench-custom 50000 1000000      # 50K cache, 1M events");
    println!(
        "  quick-bench-custom 10000 100000 8 0.8 # 10K cache, 100K events, 8 tasks, 80% reads"
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 && (args[1] == "-h" || args[1] == "--help") {
        print_usage();
        return;
    }

    let cache_size = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10_000);

    let num_events = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100_000);

    let num_tasks = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);

    let read_write_ratio: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.5);

    if cache_size == 0 || num_events == 0 || num_tasks == 0 {
        eprintln!("Error: cache_size, num_events, and num_tasks must be > 0");
        print_usage();
        return;
    }

    if !(0.0..=1.0).contains(&read_write_ratio) {
        eprintln!("Error: read_ratio must be between 0.0 and 1.0");
        print_usage();
        return;
    }

    run_custom_benchmark(cache_size, num_events, num_tasks, read_write_ratio).await;
}
