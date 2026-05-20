# nostro2-relay

WebSocket relay client and connection pool for the Nostr protocol.

[![Crates.io](https://img.shields.io/crates/v/nostro2-relay.svg)](https://crates.io/crates/nostro2-relay)
[![Documentation](https://docs.rs/nostro2-relay/badge.svg)](https://docs.rs/nostro2-relay)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- **Single Relay Connection** - Connect to individual Nostr relays
- **Connection Pool** - Manage multiple relay connections with automatic aggregation
- **Automatic Reconnection** - Exponential backoff reconnection when connections drop
- **Event Deduplication** - Built-in LRU cache to prevent duplicate events across relays
- **Configurable Crypto Backend** - Choose between Ring or AWS-LC for TLS/crypto operations
- **Async/Await** - Built on Tokio for efficient async I/O
- **Zero-Copy Message Passing** - Optimized internal architecture using channels

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
nostro2-relay = "0.3"
```

### Choosing a Crypto Backend

By default, `nostro2-relay` uses the Ring crypto library. You can switch to AWS-LC-RS:

```toml
[dependencies]
# Use Ring (default)
nostro2-relay = "0.3"

# Or use AWS-LC
nostro2-relay = { version = "0.3", default-features = false, features = ["rustls-aws-lc"] }
```

**Why choose one over the other?**

- **Ring** (default): Pure Rust, well-audited, works everywhere including WASM
- **AWS-LC**: AWS's cryptographic library, potentially faster on some platforms, FIPS-validated builds available

## Usage

### Single Relay Connection

Connect to a single relay and subscribe to events:

```rust
use nostro2_relay::NostrRelay;
use nostro2::NostrSubscription;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to a relay
    let relay = NostrRelay::new("wss://relay.example.com").await?;

    // Create a subscription filter
    let filter = NostrSubscription {
        kinds: vec![1].into(), // Text notes
        limit: Some(10),
        ..Default::default()
    };

    // Send the subscription
    relay.send(filter)?;

    // Receive events
    while let Some(event) = relay.recv().await {
        println!("Received: {:?}", event);
    }

    Ok(())
}
```

### Connection Pool with Multiple Relays

Manage multiple relays with automatic event deduplication:

```rust
use nostro2_relay::NostrPool;
use nostro2::NostrSubscription;

#[tokio::main]
async fn main() {
    // Create a pool with multiple relays
    let pool = NostrPool::new(&[
        "wss://relay.damus.io",
        "wss://relay.snort.social",
        "wss://nos.lol",
    ]);

    // Subscribe to events across all relays
    let filter = NostrSubscription {
        kinds: vec![1].into(),
        limit: Some(20),
        ..Default::default()
    };

    pool.send(&filter).expect("Failed to send subscription");

    // Receive deduplicated events from all relays
    while let Some(event) = pool.recv().await {
        match event {
            nostro2::NostrRelayEvent::NewNote(relay_url, sub_id, note) => {
                println!("Note from {}: {}", relay_url, note.content);
            }
            nostro2::NostrRelayEvent::EndOfSubscription(relay_url, sub_id) => {
                println!("EOSE from {}", relay_url);
            }
            _ => {}
        }
    }
}
```

### Custom Cache Configuration

Configure the deduplication cache size for the pool:

```rust
use nostro2_relay::NostrPool;

// Default cache: 10,000 events
let pool = NostrPool::new(&["wss://relay.example.com"]);

// Custom cache: 50,000 events (higher memory, fewer duplicates)
let pool = NostrPool::with_cache_size(
    &["wss://relay.example.com"],
    50_000
);
```

The cache uses an LRU (Least Recently Used) eviction strategy. When the cache is full, the oldest events are automatically evicted to make room for new ones. This prevents unbounded memory growth in long-running applications.

**Cache sizing guidelines:**
- **10,000 events** (~640 KB): Good for most applications
- **50,000 events** (~3.2 MB): Better for high-traffic pools with many relays
- **100,000 events** (~6.4 MB): Enterprise applications with extensive relay networks

### Automatic Reconnection

By default, relays automatically reconnect with exponential backoff when connections drop. This makes your application resilient to network issues.

```rust
use nostro2_relay::{NostrRelay, ReconnectConfig};
use std::time::Duration;

// Default: infinite retries with exponential backoff
let relay = NostrRelay::new("wss://relay.example.com").await?;

// Custom reconnection settings
let config = ReconnectConfig {
    max_retries: 10,              // Max reconnection attempts (0 = infinite)
    initial_delay: Duration::from_secs(1),   // Start with 1 second delay
    max_delay: Duration::from_secs(60),      // Cap at 60 seconds
    backoff_multiplier: 2.0,      // Double the delay each retry
};
let relay = NostrRelay::with_reconnect("wss://relay.example.com", config).await?;

// Disable reconnection entirely
let config = ReconnectConfig::disabled();
let relay = NostrRelay::with_reconnect("wss://relay.example.com", config).await?;
```

**Reconnection behavior:**
1. Connection drops or encounters an error
2. Wait `initial_delay` before first retry
3. Each subsequent retry doubles the delay (exponential backoff)
4. Delay is capped at `max_delay`
5. Stops after `max_retries` attempts (0 = never stop)
6. Successfully reconnected connections reset the retry counter

**Configure reconnection for pools:**

```rust
use nostro2_relay::{NostrPool, ReconnectConfig};
use std::time::Duration;

let config = ReconnectConfig {
    max_retries: 5,
    initial_delay: Duration::from_secs(2),
    max_delay: Duration::from_secs(30),
    backoff_multiplier: 1.5,
};

let pool = NostrPool::with_config(
    &["wss://relay1.example.com", "wss://relay2.example.com"],
    10_000,  // cache size
    config
);
```

### Publishing Events

```rust
use nostro2::NostrNote;

// Create and sign a note (requires nostro2-signer)
let mut note = NostrNote::text_note("Hello, Nostr!");
// ... sign the note with nostro2-signer ...

// Publish to a single relay
relay.send(note.clone())?;

// Or publish to all relays in a pool
pool.send(note)?;
```

## Architecture

### NostrRelay

- Single WebSocket connection to one relay
- Separate reader/writer tasks for concurrent I/O
- Unbounded channels for message passing
- Automatic reconnection with exponential backoff

### NostrPool

- Manages multiple `NostrRelay` instances
- Broadcast channel for sending to all relays
- Aggregated receiver for all relay events
- Built-in event deduplication using `nostro2-cache`
- Each relay runs in its own task

## Performance Considerations

- **Zero-copy message passing** using Arc and channels
- **LRU cache** with O(1) insert/lookup for deduplication
- **Parallel relay connections** spawn independent tasks
- **Efficient serialization** with pre-serialized JSON in writer tasks

## Error Handling

```rust
use nostro2_relay::errors::NostrRelayError;

match relay.send(subscription) {
    Ok(_) => println!("Subscription sent"),
    Err(NostrRelayError::SendError) => {
        eprintln!("Connection closed");
    }
    Err(e) => eprintln!("Error: {}", e),
}
```

## Compatibility

- **Rust**: 1.75+ (2021 edition)
- **Tokio**: Requires async runtime
- **Platform**: Linux, macOS, Windows
- **WASM**: Not yet supported (coming soon)

## Examples

See the `examples/` directory for more usage patterns:

```bash
cargo run --example single_relay
cargo run --example relay_pool
```

## Related Crates

- [`nostro2`](../nostro2) - Core Nostr protocol types and utilities
- [`nostro2-signer`](../nostro2-signer) - Key management and event signing
- [`nostro2-cache`](../nostro2-cache) - Standalone LRU deduplication cache
- [`nostro2-nips`](../nips) - Extended protocol implementations (NIPs)

## Contributing

Contributions are welcome! Please see the [main repository](https://github.com/42Pupusas/NostrO2) for guidelines.

## License

MIT License - see [LICENSE](../LICENSE) for details.
