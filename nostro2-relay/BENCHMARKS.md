# NostrO2 Relay Benchmarks - Quick Start

Comprehensive Criterion benchmark suite for measuring and comparing relay performance.

> **Note:** Protocol-level benchmarks (serialization, subscription filtering) are in `../nostro2/benches/`

## Quick Commands

```bash
# Run all relay benchmarks
cargo bench

# Run specific suite
cargo bench --bench deduplication
cargo bench --bench relay_benchmarks

# Run protocol benchmarks (from ../nostro2)
cd ../nostro2 && cargo bench

# Run specific benchmark
cargo bench -- "channel_throughput"
cargo bench -- "sequential_insertions"

# Quick run (fewer samples, faster)
cargo bench -- --quick

# Save baseline for comparison
cargo bench -- --save-baseline my-implementation

# Compare against baseline
cargo bench -- --baseline my-implementation
```

## Benchmark Suites

### Protocol Benchmarks (in `../nostro2/benches/`)
- **Serialization** - JSON encode/decode for protocol messages
- **Subscription Filtering** - Event filtering performance

See `../nostro2/benches/README.md` for details.

### 1. Deduplication (`cargo bench --bench deduplication`)
Tests the HashSet-based deduplication strategy used in NostrPool.
- Sequential insertions (unique vs duplicates)
- Lookup performance (100 to 100K entries)
- Concurrent operations (2-16 tasks)
- Mixed read/write workloads

**Example Result:**
```
sequential_insertions/unique/10000      time: [1.2ms 1.3ms 1.4ms]
lookup_performance/contains_hit/10000   time: [45ns 48ns 52ns]
```

### 2. Relay Operations (`cargo bench --bench relay_benchmarks`)
Tests channel-based message passing and relay patterns.
- Channel throughput (unbounded vs bounded MPSC)
- Broadcast channels (1-8 subscribers)
- Full message pipeline (create→serialize→send→recv→parse)
- Concurrent clients (2-8 clients)
- Variable content sizes (100B to 50KB)

**Example Result:**
```
channel_throughput/unbounded_mpsc/1000  time: [125µs 132µs 140µs]
message_pipeline/full_pipeline/1000     time: [1.29ms 1.30ms 1.31ms]
```

## Viewing Results

Criterion generates HTML reports in `target/criterion/`:

```bash
# Open main report
xdg-open target/criterion/report/index.html

# Open specific benchmark
xdg-open target/criterion/channel_throughput/report/index.html
```

Reports include:
- Timing statistics (mean, median, std dev)
- Throughput measurements
- Violin plots and histograms
- Regression analysis (linear/quadratic/cubic fits)
- Comparison charts (when using baselines)

## Comparing Implementations

### Example: Testing a New Relay Strategy

1. **Baseline current implementation:**
```bash
cargo bench -- --save-baseline current
```

2. **Implement your new strategy** in a separate module

3. **Update benchmarks** to test both strategies:
```rust
// In benches/relay_benchmarks.rs
group.bench_function("strategy_current", |b| { /* test current */ });
group.bench_function("strategy_new", |b| { /* test new */ });
```

4. **Run and compare:**
```bash
cargo bench --bench relay_benchmarks
```

5. **Review HTML report** to see side-by-side comparison

### Example: Optimizing Deduplication

Test alternatives to `Arc<Mutex<HashSet>>`:

```rust
// Benchmark using DashMap
group.bench_function("dashmap", |b| { /* impl */ });

// Benchmark using RwLock
group.bench_function("rwlock", |b| { /* impl */ });

// Benchmark using Bloom filter
group.bench_function("bloom", |b| { /* impl */ });
```

## Performance Tips

1. **Consistent environment:**
```bash
# Disable CPU frequency scaling (Linux)
sudo cpupower frequency-set --governor performance

# Close other applications
# Disable background services
```

2. **Increase sample size for accuracy:**
```bash
cargo bench -- --sample-size 200
```

3. **Profile hot paths with perf:**
```bash
cargo bench --bench relay_benchmarks --no-run
perf record target/release/deps/relay_benchmarks-*
perf report
```

4. **Flamegraph for visualization:**
```bash
cargo install flamegraph
cargo flamegraph --bench relay_benchmarks
```

## Performance Targets

Based on Nostr relay requirements:

| Operation | Target | Current |
|-----------|--------|---------|
| Serialize typical message (< 1KB) | < 1µs | ~500ns ✓ |
| Deserialize message | < 2µs | ~1µs ✓ |
| Dedup lookup (100K seen) | < 100ns | ~50ns ✓ |
| Unbounded channel throughput | > 100K msg/s | ~200K msg/s ✓ |
| Concurrent clients (8x) | Linear scaling | TBD |
| Large message (10KB) | < 100µs | TBD |

## Understanding Criterion Output

```
channel_throughput/unbounded_mpsc/1000
                        time:   [132.45 µs 135.12 µs 138.91 µs]
                        change: [-5.2341% +2.1234% +8.7654%] (p = 0.42 > 0.05)
                        No change in performance detected.
```

- **time**: [lower bound, estimate, upper bound] (95% confidence)
- **change**: Performance delta vs previous run
- **p-value**: Statistical significance (< 0.05 = significant change)

## Common Benchmark Patterns

### Adding a New Benchmark

```rust
fn bench_my_feature(c: &mut Criterion) {
    let mut group = c.benchmark_group("my_feature");

    // Simple timing
    group.bench_function("operation", |b| {
        b.iter(|| {
            // Code to benchmark
            black_box(expensive_operation())
        });
    });

    // With setup (not timed)
    group.bench_function("with_setup", |b| {
        let data = prepare_data(); // Not timed
        b.iter(|| {
            process(black_box(&data)) // Timed
        });
    });

    // Async operation
    let runtime = tokio::runtime::Runtime::new().unwrap();
    group.bench_function("async_op", |b| {
        b.to_async(&runtime).iter(|| async {
            black_box(async_operation().await)
        });
    });

    // Parameterized (different input sizes)
    for size in [100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::new("param", size), size, |b, &size| {
            b.iter(|| {
                process_n_items(black_box(size))
            });
        });
    }

    group.finish();
}

criterion_group!(my_benches, bench_my_feature);
criterion_main!(my_benches);
```

### black_box() Usage

Always use `black_box()` to prevent compiler optimizations:

```rust
// Bad - compiler may optimize away
b.iter(|| expensive_calculation());

// Good - forces actual execution
b.iter(|| black_box(expensive_calculation()));
```

## Troubleshooting

**"gnuplot not found" warning:**
- Install gnuplot: `sudo apt install gnuplot` (Linux)
- Or use plotters backend (automatic fallback)

**Benchmarks take too long:**
```bash
cargo bench -- --quick          # Fewer samples
cargo bench -- --sample-size 10 # Custom sample count
```

**Noisy results:**
- Close other applications
- Run multiple times and compare
- Use larger sample size
- Check CPU governor settings

**Out of memory:**
- Reduce benchmark input sizes
- Run benchmarks individually
- Close memory-intensive applications

## Next Steps

1. **Establish baselines:** Run `cargo bench -- --save-baseline initial` now
2. **Implement new strategy:** Code your experimental relay implementation
3. **Compare:** Run benchmarks and check HTML reports
4. **Iterate:** Use profiling data to identify bottlenecks
5. **Document:** Record findings in benchmark comments

For detailed information, see `benches/README.md`.
