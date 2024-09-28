# NostrO2

This crate is our first approach at building simple Rust tools for interacting with the Nostr ecosystem.

## Features

The library provides class-based functionality through 3 basic types: UserKeys, Notes, and Relays.

### Notes

The main data structures of Nostr, as defined by [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md). 
Implementations are split between Notes and SignedNotes, to allow for easy interoperability with external 
applications like NIP-07. Both structures have full `serde`  serialization features and provide 
ready-to-send outputs for relay messages.

### UserKeys

Can be created from a private key `str` and will allow you to sign Nostr Notes.

```rust
    let new_user = UserKeys::new("<64-bit hex string>").expect("Failed to create user keys");
    let mut unsigned_note = Note::new(
        &user_key_pair.get_public_key(),
        1,
        "Hello World"
    );
    unsigned_note.tag_note("t", "test");
    let signed_note = user_key_pair.sign_nostr_event(unsigned_note); // -> SignedNote
```

### Subscriptions

Create a new `NostrFilter` using the default constructor and then add filters to it.
Filters correspond to the object described by [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md).
Using the subscribe() method, you can create a new `NostrSubscription` that can be sent to a relay.

```rust
let subscription = 
    NostrFilter::default().new_kinds(
        vec![0, 1]
    )
    .subscribe();

println!("Subscribe to relay with id: {}", subscription.id());
```

### NostrRelay

Ready-to-go connection to a relay. The `NostrRelay` object can be cloned across thread safely.
The relay_event_reader() method returns a `Receiver<RelayEvent>` that can be used to listen to relay events.
THis can also be cloned across threads. `RelayEvents` provide 
easy pattern-matching for relay/client communication and error-handling.

```rust
let relay = NostrRelay::new("wss://relay.illuminodes.com").await.expect("Failed to create relay");

let subscription = 
    NostrFilter::default().new_kinds(
        vec![0, 1, 4]
    )
    .subscribe();
relay.subscribe(&subscription).await.expect("Failed to subscribe to relay");
while let Ok(event) = reader_relay.relay_event_reader().recv().await {
    match event {
        RelayEvent::EVENT(sub_id, signed_note) => {
            println!("Received note: {:?}", signed_note);
        },
        RelayEvent::EOSE(sub_id) => {
            println!("End of events for subscription: {:?}", sub_id);
        },
        _ => {
            println!("Other Relay events {:?}", event);
        }
    }
}

```

### Nostr Authentication

The `SignedNotes` objects also provide a verification method for both content and signatures.

```rust
    assert_eq!(signed_note.verify(), true);
```

## Installation

Run `cargo add nostro2` to get the latest version.

You can also add `nostro2` to your `Cargo.toml` dependencies:

```toml
[dependencies]
nostro2 = "0.1.27"
```

