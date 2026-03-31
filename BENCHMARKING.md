# Quick Benchmarking Guide

This guide explains how to use the fast iteration benchmarks for nostro2-relay.

## Why Quick Benchmarks?

Criterion benchmarks are comprehensive but slow (several minutes). These quick benchmarks run in under 1 second, making them ideal for rapid iteration during development.

## Available Benchmarks

### 1. HashMap vs LRU Comparison

Compare the old HashMap implementation with the new LRU cache:

```bash
cargo run --release --bin compare-bench
```

This directly compares both approaches across multiple scenarios and shows the memory growth differences.

**Key findings:**
- LRU is within 2-4% performance of HashMap for most operations
- LRU is actually **faster** for lookups (~18% faster) and some concurrent scenarios
- **Critical advantage:** With 500K events, LRU uses 48% less time and caps memory at 100K events vs HashMap's unbounded 500K
- Trade-off: Small performance cost for massive memory savings in long-running relays

### 2. Full Benchmark Suite

Runs all standard benchmarks with predefined scenarios:

```bash
cargo run --release --bin quick-bench
```

This tests:
- Sequential insertions (1K, 10K, 100K events)
- Duplicate detection
- Lookup performance (cache hits/misses)
- Concurrent insertions (4 and 16 tasks)
- Mixed read/write workloads
- LRU eviction performance

**Runtime:** ~0.9 seconds

### 3. Custom Benchmark

Test specific scenarios with custom parameters:

```bash
cargo run --release --bin quick-bench-custom [CACHE_SIZE] [NUM_EVENTS] [NUM_TASKS] [READ_RATIO]
```

**Parameters:**
- `CACHE_SIZE` - LRU cache capacity (default: 10000)
- `NUM_EVENTS` - Total operations to perform (default: 100000)
- `NUM_TASKS` - Concurrent tasks (default: 4)
- `READ_RATIO` - Fraction of reads, 0.0-1.0 (default: 0.5)

**Examples:**

```bash
# Test with 50K cache
cargo run --release --bin quick-bench-custom 50000

# Stress test: 1M events with 16 concurrent tasks
cargo run --release --bin quick-bench-custom 100000 1000000 16

# Read-heavy workload (80% reads, 20% writes)
cargo run --release --bin quick-bench-custom 10000 100000 8 0.8

# Write-heavy workload (20% reads, 80% writes)
cargo run --release --bin quick-bench-custom 10000 100000 8 0.2

# Single-threaded performance
cargo run --release --bin quick-bench-custom 10000 100000 1
```

## Iteration Workflow

When optimizing code:

1. **Establish baseline:**
   ```bash
   cargo run --release --bin quick-bench > baseline.txt
   ```

2. **Make your changes** to the code

3. **Compare performance:**
   ```bash
   cargo run --release --bin quick-bench > after.txt
   diff baseline.txt after.txt
   ```

4. **Test specific scenarios:**
   ```bash
   # If you're optimizing concurrent writes
   cargo run --release --bin quick-bench-custom 10000 500000 16 0.1
   ```

5. **Once satisfied, run full Criterion suite:**
   ```bash
   cargo bench --bench deduplication
   ```

## HashMap vs LRU: The Numbers

Based on the comparison benchmark, here's the detailed breakdown:

**Performance Impact:**
- Sequential insertions: LRU is ~2-4% slower
- Duplicate detection: LRU is ~3% **faster**
- Lookups: LRU is ~18% **faster**
- Concurrent insertions (4 tasks): LRU is ~4% slower
- Concurrent insertions (16 tasks): LRU is ~7% **faster**

**Memory Impact:**
- 10K events: Similar memory usage
- 100K events: LRU starts showing optimization (~15% faster)
- 500K events: **LRU is 48% faster and uses 80% less memory**

**Conclusion:** The LRU cache provides nearly identical performance for typical workloads while preventing unbounded memory growth. In high-load scenarios (500K+ events), the LRU actually becomes significantly faster due to its bounded nature.

## Performance Targets

Based on current implementation (LRU cache with tokio::sync::Mutex):

- **Sequential insert:** ~250-300 ns per operation
- **Duplicate detection:** ~100-150 ns per operation
- **Lookup (hit/miss):** ~95-100 ns per operation
- **Concurrent insert (4 tasks):** ~400-500 ns per operation
- **Concurrent insert (16 tasks):** ~450-550 ns per operation
- **LRU eviction:** ~140-260 ns per operation

## Tips

- Always use `--release` mode for meaningful benchmarks
- Run benchmarks multiple times to account for variance
- Close other applications to reduce system noise
- Consider cache size relative to your workload
- Higher concurrency (16+ tasks) shows contention effects

## Comparing with Criterion

After optimizing with quick benchmarks, validate with Criterion:

```bash
# Full benchmark suite (takes ~5-10 minutes)
cargo bench --bench deduplication

# Specific benchmark group
cargo bench --bench deduplication -- sequential_insertions

# Save baseline for comparison
cargo bench --bench deduplication -- --save-baseline main

# Compare against baseline
cargo bench --bench deduplication -- --baseline main
```
