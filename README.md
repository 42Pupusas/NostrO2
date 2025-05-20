# NostrO2

Simple yet powerful Rust tools for interacting with the Nostr ecosystem. 
Built on top of `serde` and `tokio` for easy integration with other Rust libraries.
Supports WASM compilation for use in web applications.

## Features

The library provides redy to use types for the main data structures of the Nostr protocol,
as well as a simple interface for interacting with a relay, or a more complex interface for
creating a relay pool.

### `NostrNote`s

The main data structures of Nostr, as defined by [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md). 
`NostrNote`s can be created using default helpers like this:

```rust
    let note = NostrNote {
        content: "Hello World".to_string(),
        kind: 300,
        public_key: "0x1234567890abcdef".to_string(),
        ..Default::default()
    };
```

### NostrKeypair

Can be created from a private key `str` or a mnemonic phrase and will allow you to sign Nostr Notes.

```rust
    let new_user = "<64-bit hex string>".parse::<NostrKeypair>().expect("Failed to create user keys");
    let mut note = NostrNote {
        content: "Hello World".to_string(),
        kind: 300,
        ..Default::default()
    };
    user_key_pair.sign_nostr_event(&mut unsigned_note); // -> Modifies the note in place, adds pubkey, id and sig.
```

### Subscriptions

Create a new `NostrSubscription` using the default constructor and then add filters to it.
Filters correspond to the object described by [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md).
All fields are represented as options and skipped in serialization if `None`.

```rust
let subscription = 
    NostrFilter {
        kinds: vec![0, 1].into(),
        limit: Some(10),
        ..Default::default()
    };

let relay_event = pool.send(subscription)?;

println!("Subscribe to relay with id: {}", relay_event.1);
```

### NostrRelay

Ready-to-go connection to a relay. The `NostrRelay` has two main parts, a `reader` and a `writer`.
The `writer` is reference counted and can be cloned to send events to the relay across multiple threads.
The `reader` is a single stream that can be used to receive events from the relay. 
`NostrRelay` also holds its url and connection state internally.

```rust
let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
let filter = NostrSubscription {
    kinds: Some(vec![1]),
    limit: Some(3),
    ..Default::default()
}
.relay_subscription();
let id = relay.writer.subscribe(filter).await?;
_debug("Subscribed with id");

let mut finished = String::new();
let mut ws_stream = relay.relay_event_stream()?;
while let Some(event) = ws_stream.next().await {
    match event {
        RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, id)) => {
            _debug(&format!("End of subscription: {}", id));
            finished = id;
            break;
        }
        _ => (),
    }
}

```

### Relay Pool 

The `NostrRelayPool` is a more complex interface for managing multiple relays. It's created with a list of relay urls
and will connect to all relays concurrently. The pool will then manage the connections and distribute events 
to the relays in a round-robin fashion.
Unique `NostrNotes` are kep in a `library` and duplicate notes are filtered out. 
The pool holds a `RelayTable` that keeps the connection state of each relay.

```rust
let mut pool = NostrRelayPool::new(vec![
    "wss://relay.arrakis.lat".to_string(),
    "wss://relay.illuminodes.com".to_string(),
    "wss://frens.nostr1.com".to_string(),
    "wss://bitcoiner.social".to_string(),
    "wss://bouncer.minibolt.info".to_string(),
    "wss://freespeech.casa".to_string(),
    "wss://junxingwang.org".to_string(),
    "wss://nostr.0x7e.xyz".to_string(),
])
.await
.expect("Failed to create pool");
let filter = NostrSubscription {
    kinds: Some(vec![1]),
    limit: Some(10),
    ..Default::default()
}
.relay_subscription();
pool.subscribe(filter.clone())
    .await
    .expect("Failed to subscribe");
let mut events = vec![];
while let Some((_, event)) = pool.listener.recv().await {
    if let RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, ref subscription_id)) =
        event
    {
        events.push(subscription_id.clone());
        if events.len() == 5 {
            break;
        }
    }
    if let RelayEvent::NewNote(NoteEvent(_, _, _)) = event {}
}
```

### Nostr Authentication

The `NostrNotes` objects also provide a verification method for both content and signatures.

```rust
    assert_eq!(signed_note.verify(), true);
```

## Installation

Run `cargo add nostro2` to get the latest version.

You can also add `nostro2` to your `Cargo.toml` dependencies:

```toml
[dependencies]
nostro2 = "0.2.0"
```

