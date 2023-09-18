# NostrO2

This crate is our first approach at building simple Rust tools for interacting with the Nostr ecosystem.

## Features

- **User Key Creation**: Generate user keys with ease.
- **Object Handling**: Create and sign objects to ensure data integrity.
- **Relay Connectivity**: Connect to Nostr relays to send and receive messages seamlessly.
- **Nostr Types**: Static typing for Nostr data structures, from Notes to Relay messages.

## Installation
Add `nostro2` to your `Cargo.toml` dependencies:

```toml
[dependencies]
nostro2 = "0.1.4"
```

## Example

Using this library is straightforward. Connect to a relay, send a subscription event, and await the received messages.

Rust and serde allow for performant parsing of the notes into objects, which can then be pattern matched with ease.

```rust
let nostr_relay = NostrRelay::new("wss://nostr.bongbong.com").await;

let filter: Value = json!({
    "authors": vec!(pubkey),
    "kinds": [1]
});

nostr_relay.subscribe(filter).await;

while let Some(Ok(result)) = nostr_relay.read_from_relay().await {
    match result {
        RelayNotice::EVENT(_notice, _id, note) => {
            info!("SignedNote: {:?}", note.get_content());
        }
        _ => {
            nostr_relay.close().await.expect("Error");
            return Err("Error".to_string());
        }
    }
}
```


