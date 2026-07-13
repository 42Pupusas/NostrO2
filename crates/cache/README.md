# nostro2-cache

Event-ID deduplication cache for Nostr relay clients.

## `Cache`

A `std::sync::Mutex<lru::LruCache<String, ()>>` behind an `Arc`, cheap to
`clone` and share across relay connections. It is **not** lock-free — the
mutex is held for the duration of each operation — but it was the fastest
strategy in benchmarks under realistic multi-threaded relay pool scenarios
(10-20 concurrent connections), beating sharded and lock-free alternatives.

- Automatic LRU eviction, bounded memory
- Zero external dependencies beyond the `lru` crate
- Simple, predictable behavior

## Usage

```rust
use nostro2_cache::Cache;

let cache = Cache::new(10_000);

// `insert` returns true for a new id, false for a duplicate.
if cache.insert(event_id) {
    println!("New event!");
} else {
    println!("Duplicate event, skip");
}
```

`Cache::new(0)` panics — an LRU cache must hold at least one entry.

## Benchmarks

```bash
cargo bench --package nostro2-cache
```
