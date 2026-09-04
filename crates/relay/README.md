# nostro2-relay

WebSocket relay client and connection pool for the Nostr protocol.

[![Crates.io](https://img.shields.io/crates/v/nostro2-relay.svg)](https://crates.io/crates/nostro2-relay)
[![Documentation](https://docs.rs/nostro2-relay/badge.svg)](https://docs.rs/nostro2-relay)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- **Single Relay Connection** - Connect to individual Nostr relays
- **Connection Pool** - Manage multiple relay connections with automatic aggregation
- **Automatic Reconnection** - Exponential backoff reconnection when connections drop
- **Signature Verification** - Inbound notes are checked before you see them, so a relay cannot forge events
- **Event Deduplication** - Built-in LRU cache to prevent duplicate events across relays
- **Configurable Crypto Backend** - Choose between Ring or AWS-LC for TLS/crypto operations
- **No Runtime** - Each connection runs on a plain thread. The crate depends on no async runtime, its futures run on whichever executor you already have, and every operation has a blocking twin so you need no executor at all
- **Lock-Free Message Passing** - Connections talk to your code through lock-free rings, with no mutex on the data path

## Built for long-lived services

The intended user is a daemon that holds a pool open for weeks and reconnects
through every network fault. Such a service fails quietly, so the crate states
these guarantees and tests each one in `tests/liveness.rs`:

- **A dead connection is detected.** TCP never reports a peer that stops
  answering, so a quiet connection is pinged and a silent one is dropped.
  Without this a half-open socket stalls a reader forever and no reconnect
  ever starts.
- **A stalled write never freezes the connection.** A relay that accepts the
  socket but stops reading it fills both receive windows. One thread owns the
  socket, so an unbounded write would stop reads too.
- **A reconnect restores your subscriptions.** A subscription lives on the
  relay, which forgets it when the connection drops. The driver replays the
  open filters, so a service does not go silent while looking connected.
- **A reader is always released.** A spent retry budget, an explicit close, or
  even a panic on the IO thread ends the stream rather than parking a reader
  forever.
- **Reconnecting leaks nothing.** Sockets and threads stay flat across
  thousands of reconnects.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
nostro2-relay = "0.7"
```

### Choosing a Crypto Backend

By default, `nostro2-relay` uses the Ring crypto library. You can switch to AWS-LC-RS:

```toml
[dependencies]
# Use Ring (default)
nostro2-relay = "0.7"

# Or use AWS-LC. `default-features = false` also drops the default `serde`
# JSON backend, so name one explicitly.
nostro2-relay = { version = "0.7", default-features = false, features = ["rustls-aws-lc", "serde"] }
```

**Why choose one over the other?**

- **Ring** (default): Pure Rust, well-audited, works everywhere including WASM
- **AWS-LC**: AWS's cryptographic library, potentially faster on some platforms, FIPS-validated builds available

### Bringing Your Own Provider

Enable `rustls-custom-provider` to link no provider at all and supply your
own, such as [`rustls-rustcrypto`](https://crates.io/crates/rustls-rustcrypto):

```toml
[dependencies]
nostro2-relay = { version = "0.7", default-features = false, features = [
    "rustls-custom-provider",
    "serde",
    "k256",
] }
rustls-rustcrypto = "0.0.2-alpha"
```

Pass the provider to `RelayTls`, then hand that to the driver:

```rust,ignore
use nostro2_relay::{DriverConfig, NostrRelay, RelayTls, RelayUrl};

let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
let tls = RelayTls::with_provider(provider)?;

let url = RelayUrl::parse("wss://relay.example.com")?;
let relay = NostrRelay::connect_blocking_config(DriverConfig::new(url).with_tls(tls))?;
```

For a custom root store, client certificates, or specific protocol versions,
build the `rustls::ClientConfig` yourself and wrap it with
`RelayTls::from_config`. Build it against `nostro2_relay::rustls`, the
re-export of the exact `rustls` this crate links, so the types agree:

```rust,ignore
use nostro2_relay::{rustls, DriverConfig, NostrPool, RelayTls};

let config = rustls::ClientConfig::builder_with_provider(provider)
    .with_safe_default_protocol_versions()?
    .with_root_certificates(my_roots)
    .with_no_client_auth();

// One configuration for every relay: cloning shares the root store.
let tls = RelayTls::from_config(config);
let pool = NostrPool::with_driver_config(&["wss://relay.example.com"], 10_000, &|url| {
    DriverConfig::new(url).with_tls(tls.clone())
});
```

A build with `rustls-custom-provider` that supplies nothing gets
`RelayTlsError::NoProvider` on the first connection, naming the three ways
to fix it, rather than a panic from inside `rustls`.

## Usage

### Single Relay Connection

Connect to a single relay and subscribe to events:

```rust
use nostro2_relay::NostrRelay;
use nostro2::NostrSubscription;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to a relay. `recv` takes `&mut self`, so bind it mutably.
    let mut relay = NostrRelay::new("wss://relay.example.com").await?;

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
    let mut pool = NostrPool::new(&[
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

    pool.send(filter).expect("Failed to send subscription");

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
    &config
);
```

### Liveness tuning

`DriverConfig` carries every timing policy. The defaults suit a public relay;
lower them for a link you control.

```rust
use nostro2_relay::{DriverConfig, HeartbeatConfig, NostrPool, NostrRelay, RelayUrl};
use std::time::Duration;

let config = DriverConfig::new(RelayUrl::parse("wss://relay.example.com")?)
    // Probe after 15s of silence, give up 5s later. Default: 45s / 20s.
    .with_heartbeat(HeartbeatConfig {
        idle_timeout: Duration::from_secs(15),
        reply_timeout: Duration::from_secs(5),
    })
    // Bound one socket write. Default: 20s.
    .with_write_timeout(Duration::from_secs(10));

let relay = NostrRelay::with_driver_config(config)?;

// The same knobs for every relay in a pool.
let pool = NostrPool::with_driver_config(&["wss://relay.example.com"], 10_000, &|url| {
    DriverConfig::new(url).with_write_timeout(Duration::from_secs(10))
});
```

### Reacting to a reconnect

`recv` yields only relay messages. Use the `_event` twins when the service has
to see the connection lifecycle itself:

```rust
use nostro2_relay::DriverEvent;

while let Some(event) = relay.recv_event().await {
    match event {
        DriverEvent::Connected => println!("connected; subscriptions restored"),
        DriverEvent::Disconnected(reason) => eprintln!("dropped: {reason:?}"),
        DriverEvent::Exhausted => break,
        DriverEvent::Message(event) => println!("{event:?}"),
    }
}
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
- One thread owns the socket, the frame codec, and the retry budget, so
  nothing on the connection needs a lock
- Frames move over lock-free rings: outbound is many-producer, inbound is
  many-consumer
- Automatic reconnection with exponential backoff
- Parses relay frames on the connection thread, so a pool parses in parallel

### NostrPool

- Manages multiple `NostrRelay` instances
- Sends to every relay, and merges their streams into one
- Built-in event deduplication using `nostro2-cache`
- Each relay runs on its own thread

### Handles are `Send`, not `Sync`

A handle owns a private cursor into the ring, so it moves between threads but
is not shared by reference. Clone it to read or write from somewhere else:

```rust
let reader = relay.clone();   // reads the same stream
let writer = relay.clone();   // writes to the same socket
```

Readers **compete**: each message reaches exactly one handle. Clone to spread
work over several readers, not to give each reader a copy of the stream.

### Blocking callers

**Every** operation has a blocking twin, so a service built on threads never
has to poll a future. Connecting is not an asynchronous act, so it does not
require an executor either:

```rust
use nostro2_relay::{NostrPool, NostrRelay, PoolEvent};

// A whole session, with no runtime anywhere.
let mut relay = NostrRelay::connect_blocking("wss://relay.example.com")?;
relay.send(filter)?;

while let Some(event) = relay.recv_blocking() {
    println!("{event:?}");
}
```

| Async | Blocking |
|---|---|
| `NostrRelay::new` | `NostrRelay::connect_blocking` |
| `NostrRelay::with_reconnect` | `NostrRelay::connect_blocking_with` |
| `NostrRelay::with_driver_config` + await | `NostrRelay::connect_blocking_config` |
| `NostrRelay::recv` | `NostrRelay::recv_blocking` |
| `NostrRelay::recv_event` | `NostrRelay::recv_event_blocking` |
| `NostrRelay::send_all` | `NostrRelay::send_all_blocking` |
| `NostrPool::recv` | `NostrPool::recv_blocking` |
| `NostrPool::recv_event` | `NostrPool::recv_event_blocking` |

`send`, `close`, and every `NostrPool` constructor are already synchronous:
sending only pushes to a ring, so it never blocks.

A pool reacting to its own lifecycle, still with no executor:

```rust
let mut pool = NostrPool::new(&["wss://relay.example.com"]);
pool.send(filter)?;

while let Some(event) = pool.recv_event_blocking() {
    match event {
        // Every variant names its relay, so a drop is actionable and a
        // note is attributable. `event.url()` works on any of them.
        PoolEvent::Disconnected(url, reason) => eprintln!("{url} dropped: {reason:?}"),
        PoolEvent::Connected(url) => println!("{url} is back, subscriptions restored"),
        PoolEvent::Message(url, event) => println!("{url} served {event:?}"),
        PoolEvent::Exhausted(url) => eprintln!("{url} gave up"),
    }
}
```

The origin is an `Arc<RelayUrl>`, so attributing a message costs a refcount
bump rather than a copy of the address.

When only the message and its origin matter, `recv_from` pairs them without
the lifecycle events:

```rust
while let Some((url, event)) = pool.recv_from_blocking() {
    println!("{url} served {event:?}");
}
```

| Async | Blocking |
|---|---|
| `NostrPool::recv_from` | `NostrPool::recv_from_blocking` |

`tests/blocking_only.rs` exercises the crate without writing `.await` once,
so this parity cannot quietly lapse.

The dependency list reflects it too: the only async crate the library links
is `futures-core`, which has no dependencies of its own and supplies the
`Stream` trait for `send_all`. `cargo tree -p nostro2-relay --edges normal`
shows no runtime and no proc-macro chain behind it.

## Performance Considerations

- **Lock-free rings** on the data path, with no mutex between your code and the socket
- **LRU cache** with O(1) insert/lookup for deduplication
- **Parallel relay connections** each own a thread and parse their own frames
- **Efficient serialization** with JSON serialized before it reaches the ring

## Error Handling

```rust
use nostro2_relay::errors::NostrRelayError;

match relay.send(subscription) {
    Ok(_) => println!("Subscription sent"),
    Err(NostrRelayError::SendError) => {
        eprintln!("Connection closed, or the outbound queue is full");
    }
    Err(e) => eprintln!("Error: {}", e),
}
```

A pool does not stop at the first relay that refuses a message, because a pool
exists so one dead relay cannot silence the set. The message reaches every
relay that accepts it, and the error names the shortfall:

```rust
match pool.send(subscription) {
    Ok(_) => println!("every relay accepted it"),
    Err(NostrRelayError::PartialSend { delivered, total }) => {
        eprintln!("reduced coverage: {delivered} of {total} relays");
    }
    Err(NostrRelayError::SendError) => eprintln!("no relay accepted it"),
    Err(e) => eprintln!("Error: {}", e),
}
```

## Compatibility

- **Rust**: 1.87+ (2024 edition)
- **Runtime**: none required. The async methods run on any executor; the
  `*_blocking` twins need no executor at all
- **Platform**: Linux, macOS, Windows
- **WASM**: Not yet supported (coming soon)

## Examples

See the `examples/` directory for more usage patterns:

```bash
cargo run --example local_relay
cargo run --example simple_test
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
