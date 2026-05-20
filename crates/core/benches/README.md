# NostrO2 Core Protocol Benchmarks

Benchmarks for the core Nostr protocol types and operations.

## Running Benchmarks

```bash
# From nostro2 directory
cargo bench

# Run specific suite
cargo bench --bench serialization
cargo bench --bench subscription

# Run specific benchmark
cargo bench -- "filter_by_author"
cargo bench -- "client_event_serialization"

# Save baseline (must specify --bench)
cargo bench --bench serialization -- --save-baseline my-baseline
cargo bench --bench subscription -- --save-baseline my-baseline

# Compare against baseline
cargo bench --bench serialization -- --baseline my-baseline
cargo bench --bench subscription -- --baseline my-baseline
```

## Benchmark Suites

### 1. Serialization (`serialization.rs`)

Tests JSON serialization/deserialization performance for Nostr protocol messages.

**What it measures:**
- `NostrClientEvent` serialization (SendNote, Subscribe, Close)
- `NostrRelayEvent` serialization (NewNote, SentOk, EOSE, Notice)
- Deserialization performance for all event types
- Roundtrip (serialize + deserialize) cycles
- Impact of message content size (10B - 5KB)

**Why it matters:** JSON encoding/decoding is a fundamental operation for every message. Faster serialization = higher relay throughput.

**Example results:**
```
client_event_serialization/send_note    time: [551 ns]
client_event_deserialization/send_note  time: [946 ns]
roundtrip/client_send_note              time: [1.53 µs]
```

### 2. Subscription Filtering (`subscription.rs`)

Tests `NostrSubscription` filter performance against collections of events.

**What it measures:**
- Filter by author pubkey
- Filter by event kind
- Filter by timestamp (since/until)
- Filter by event IDs
- Multi-filter combinations
- Filter with result limits
- Empty filter (match all)

**Why it matters:** Relays must filter thousands of events per subscription request. Efficient filtering directly impacts query response time.

**Example results:**
```
filter_by_timestamp     time: [1.05 µs]  (fastest)
filter_by_kind          time: [3.26 µs]
filter_by_author        time: [4.44 µs]
filter_multi            time: [9.54 µs]
```

**Optimization opportunities:**
- Pre-hash author pubkeys in subscriptions
- Use bloom filters for ID lookups
- Index events by kind/timestamp for faster filtering

## Understanding Results

### Serialization
- **Target:** < 1µs for typical messages
- **Current:** ~550ns (excellent!)
- **Bottleneck:** JSON parsing for complex structures

### Subscription Filtering
- **Target:** < 10µs for multi-filter on 1K events
- **Current:** ~9.5µs (excellent!)
- **Bottleneck:** String comparisons for author filtering

## Performance Targets

| Operation | Target | Typical |
|-----------|--------|---------|
| Serialize simple event | < 500 ns | 550 ns |
| Deserialize event | < 1 µs | 946 ns |
| Filter 1K events by author | < 5 µs | 4.4 µs |
| Filter 1K events by time | < 2 µs | 1.05 µs |
| Multi-filter 1K events | < 10 µs | 9.5 µs |

## Optimization Ideas

### Serialization
1. **simd-json**: 2-3x faster JSON parsing
2. **sonic-rs**: Alternative fast JSON library
3. **Binary encoding**: Protocol Buffers or MessagePack (non-standard)
4. **Pre-serialized templates**: Cache common message structures

### Subscription Filtering
1. **Hash-based author lookup**: O(1) instead of O(n) string compare
2. **Indexed event storage**: B-tree by timestamp, hash map by kind
3. **Bloom filters**: Probabilistic filtering for large ID sets
4. **SIMD string matching**: Vectorized pubkey comparisons

## Adding New Benchmarks

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nostro2::{NostrNote, NostrSubscription};

fn bench_my_feature(c: &mut Criterion) {
    c.bench_function("my_feature", |b| {
        b.iter(|| {
            // Code to benchmark
            black_box(my_operation())
        });
    });
}

criterion_group!(my_benches, bench_my_feature);
criterion_main!(my_benches);
```

## See Also

- `nostro2-relay/benches/` - Relay-specific benchmarks (channels, deduplication)
- `BENCHMARKS.md` - Overall benchmarking guide
- Criterion docs: https://bheisler.github.io/criterion.rs/
