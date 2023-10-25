# NostrO2

This crate is our first approach at building simple Rust tools for interacting with the Nostr ecosystem.

## Features

The library provides class-based functionality through 3 basic types: UserKeys, Notes, and Relays.

### Notes

The main data structures of Nostr, as defined by [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md). 
Implementations are split between Notes and SignedNotes, 
to allow for easy interoperability with external applications like NIP-07. Both structures have full `serde` 
serialization features and provide ready-to-send outputs for relay messages.

### UserKeys

Can be created from a private key `str` and will allow you to sign Nostr Notes.

```rust
    let new_user = UserKeys::new("<64-bit hex string>");
    let mut unsigned_note = Note::new(
        user_key_pair.get_public_key().to_string(),
        1,
        "Hello World"
    );
    unsigned_note.tag_note("t", "test");
    let signed_note = user_key_pair.sign_nostr_event(unsigned_note); // -> SignedNote
    // A note object can also be parsed by a NIP 07 client
```

### NostrRelay

Ready-to-go connection to a relay. WebSocket protocols are handled across reference
counted threads to allow you to handle multiple relays with ease. `RelayEvents` provide 
easy pattern-matching for relay/client communication and error-handling.

### Subscriptions

You can pass any JSON filter to the `subscribe` function within a `NostrRelay`, 
following the filter protocol in NIP-01.

```rust
    // Open a connection
    let ws_connection = NostrRelay::new("relay.roadrunner.lat").await.expect("Failed to connect");

    // Subscribe to a filter
    ws_connection
        .subscribe(json!({"kinds":[1],"limit":1}))
        .await
        .expect("Failed to subscribe to relay!");

    // Send notes in an async manner
    ws_connection.send_note(signed_note).await.expect("Unable to send note");

    // Read the responses from the relay
    loop {
        if let Some(Ok(relay_msg)) = ws_connection.read_from_relay().await {
            match relay_msg {
                RelayEvents::EVENT(_event, _id, signed_note) => {
                    println!("Message received: {:?}", &signed_note);

                    // Extract the signed note info
                    let content = signed_note.get_content();
                    let specific_tags = signed_note.get_tags_by_id("a"); 
                },
                RelayEvents::OK(_event, id, success, _msg) => {
                    println!("Message received: {} {}", id, success);
                },
                RelayEvents::EOSE(_event, _sub) => println!("No more events"),
                RelayEvents::NOTICE(_event, notice) => println!("Relay says: {}", notice),
            }
        }
    }
```

### Nostr Authentication

The `SignedNotes` objects also provide verification methods for both content and signatures.

```rust
    assert_eq!(signed_note.verify_content(), true);
    assert_eq!(signed_note.verify_signature(), true);
```

## Installation

Run `cargo add nostro2` to get the latest version.

You can also add `nostro2` to your `Cargo.toml` dependencies:

```toml
[dependencies]
nostro2 = "0.1.7"
```

