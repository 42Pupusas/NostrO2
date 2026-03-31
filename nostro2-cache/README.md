# nostro2-cache

Event ID deduplication cache strategies for Nostr relay implementations.

## Strategies

### 1. DashMapCache (Lock-Free)
- **Implementation**: `dashmap::DashMap` (concurrent hashmap)
- **Pros**: Lock-free, excellent concurrent performance
- **Cons**: No LRU eviction, simple clear-when-full strategy
- **Best for**: High-throughput, many concurrent writers

### 2. ParkingLotLruCache
- **Implementation**: `parking_lot::Mutex<lru::LruCache>`
- **Pros**: Fast mutex, automatic LRU eviction, bounded memory
- **Cons**: Mutex contention under very high concurrency
- **Best for**: Moderate concurrency with memory constraints

### 3. StdMutexLruCache
- **Implementation**: `std::sync::Mutex<lru::LruCache>`
- **Pros**: No external deps, automatic LRU eviction
- **Cons**: Slower than parking_lot under contention
- **Best for**: Simple use cases, minimal dependencies

## Benchmarks

Run benchmarks to compare strategies:

```bash
# Run all benchmarks
cargo bench --package nostro2-cache

# Run specific benchmark
cargo bench --package nostro2-cache -- single_thread
cargo bench --package nostro2-cache -- multi_thread
cargo bench --package nostro2-cache -- realistic

# View HTML reports
open target/criterion/report/index.html
```

### Benchmark Scenarios

1. **Single Thread Insert**: Pure insertion performance
2. **Multi Thread Insert**: Concurrent insert with 2, 4, 8, 10, 20 threads
3. **Realistic Relay Pattern**: 10 threads with 20% duplicate rate

## Usage

```rust
use nostro2_cache::{DashMapCache, ParkingLotLruCache, StdMutexLruCache};

// Choose your strategy
let cache = DashMapCache::new(10_000);

// Check for duplicates
if cache.insert(event_id) {
    println!("New event!");
} else {
    println!("Duplicate event, skip");
}
```

## Recommendations

- **ring-relay-client**: Use `DashMapCache` for lock-free consistency
- **nostro2-relay (async)**: Use `ParkingLotLruCache` for bounded memory
- **Low concurrency**: Use `StdMutexLruCache` to minimize dependencies
