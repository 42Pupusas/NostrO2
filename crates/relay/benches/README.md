# NostrO2 Relay Infrastructure Benchmarks

Benchmarks for relay-specific infrastructure: channels, deduplication, and message passing.

> **Note:** Protocol-level benchmarks (serialization, subscription filtering) are in `nostro2/benches/`

## Running Benchmarks

### Run all relay benchmarks
```bash
cargo bench
```

### Run specific benchmark suite
```bash
cargo bench --bench deduplication
cargo bench --bench relay_benchmarks
```

### Run specific benchmark within a suite
```bash
cargo bench --bench deduplication -- "sequential_insertions"
cargo bench --bench relay_benchmarks -- "channel_throughput"
```

### Save baseline for comparison
```bash
# Must specify --bench <name> to pass Criterion options
cargo bench --bench deduplication -- --save-baseline my-baseline
cargo bench --bench relay_benchmarks -- --save-baseline my-baseline
```

### Compare against baseline
```bash
# Compare current run against saved baseline
cargo bench --bench deduplication -- --baseline my-baseline
cargo bench --bench relay_benchmarks -- --baseline my-baseline
```

## Benchmark Suites

### 1. Deduplication Benchmarks (`deduplication.rs`)

Tests the `SeenNotes` HashSet-based deduplication strategy used in `NostrPool`.

**Benchmarks:**
- `sequential_insertions` - Insert unique vs duplicate event IDs (100 - 100K entries)
- `lookup_performance` - HashSet lookup speed with varying sizes (contains hit/miss)
- `concurrent_insertions` - Multi-task concurrent insert performance (2-16 tasks)
- `concurrent_mixed` - Mixed read/write workload under concurrency
- `single_operation` - Cost of individual insert/lookup at different HashSet sizes
- `memory_overhead` - Allocation patterns for large HashSets (10K - 100K entries)

**Key Insights:**
- Shows how deduplication performance degrades with HashSet size
- Identifies contention points under concurrent access
- Measures mutex overhead for the `Arc<Mutex<HashSet>>` pattern
- Helps evaluate alternative deduplication strategies

**Example:**
```bash
cargo bench --bench deduplication -- "concurrent_insertions"
```

### 2. Relay Benchmarks (`relay_benchmarks.rs`)

Tests channel-based message passing and relay operation patterns.

**Benchmarks:**
- `channel_throughput` - Unbounded vs bounded MPSC channel throughput (100 - 10K messages)
- `broadcast_channel` - Broadcast performance with multiple receivers (1-8 subscribers)
- `message_pipeline` - Full pipeline: create→serialize→send→receive→deserialize
- `concurrent_simulation` - Multiple concurrent clients sending to one relay (2-8 clients)
- `note_sizes` - Pipeline performance with varying content sizes (100B - 50KB)

**Key Insights:**
- Compares unbounded vs bounded channel strategies
- Shows broadcast overhead for multi-relay pools
- Measures complete message processing latency
- Identifies bottlenecks in subscription filtering logic

**Example:**
```bash
cargo bench --bench relay_benchmarks -- "channel_throughput"
```

## Interpreting Results

Criterion produces HTML reports in `target/criterion/`. Open `target/criterion/report/index.html` in a browser for:
- Detailed timing statistics
- Throughput measurements
- Regression analysis
- Performance plots

### Key Metrics
- **Time** - Lower is better
- **Throughput** - Higher is better (messages/second)
- **Slope** - How performance scales with input size
- **R²** - Statistical confidence (closer to 1.0 is better)

## Comparing Strategies

### Example: Testing a New Relay Implementation

1. **Baseline current implementation:**
```bash
# Save baseline for all relay benchmarks
cargo bench --bench deduplication -- --save-baseline current
cargo bench --bench relay_benchmarks -- --save-baseline current
```

2. **Implement your new strategy** (e.g., different channel type, caching, etc.)

3. **Run benchmarks and compare:**
```bash
# Compare against saved baseline
cargo bench --bench deduplication -- --baseline current
cargo bench --bench relay_benchmarks -- --baseline current
```

4. **Check HTML report:**
```bash
open target/criterion/report/index.html
```

### Example: Evaluating Deduplication Alternatives

Test different deduplication strategies:
- Current: `Arc<Mutex<HashSet>>` (async)
- Alternative: `DashMap` (lock-free concurrent HashMap)
- Alternative: `RwLock` instead of `Mutex`
- Alternative: Bloom filter for approximate deduplication

Modify `deduplication.rs` to test each strategy and compare results.

## Performance Targets

Based on Nostr relay requirements:

- **Serialization:** < 1µs for typical messages (< 1KB)
- **Deduplication lookup:** < 100ns for 100K seen events
- **Channel throughput:** > 100K messages/sec for unbounded
- **Concurrent clients:** Linear scaling up to 8 clients
- **Large messages:** < 100µs for 10KB content

## Tips

1. **Disable CPU throttling** for consistent results:
   ```bash
   # Linux
   sudo cpupower frequency-set --governor performance
   ```

2. **Run with release optimizations** (automatically done by `cargo bench`)

3. **Close other applications** to reduce noise

4. **Run multiple times** to ensure statistical significance:
   ```bash
   cargo bench --bench relay_benchmarks -- --sample-size 100
   ```

5. **Profile with perf** for deeper insights:
   ```bash
   cargo bench --bench relay_benchmarks --no-run
   perf record target/release/deps/relay_benchmarks-* --bench
   perf report
   ```

## Future Benchmarks

Consider adding:
- **WebSocket I/O benchmarks** - Actual network round-trip times
- **Integration benchmarks** - Full `NostrRelay::new()` to connection close
- **Memory usage tracking** - Heap allocations per operation
- **Stress tests** - Sustained high load over minutes
- **Pool vs Single** - Direct comparison of `NostrPool` vs multiple `NostrRelay` instances
