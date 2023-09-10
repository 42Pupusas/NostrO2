# NostrO2

This crate is a first approach at building simple Rust tools for interacting with the Nostr ecosystem.

## Features

- **User Key Creation**: Generate user keys with ease.
- **Object Handling**: Create and sign objects to ensure data integrity.
- **Relay Connectivity**: Connect to Nostr relays to send and receive messages seamlessly.

## Installation
Add `nostro2` to your `Cargo.toml` dependencies:

```toml
[dependencies]
nostro2 = "0.1.1"
```

## Example

Using this library is very simple. Connect to a relay, send a subscription event, and await the received messages.

Rust and `serde` allow for performant parsing of the notes into objects, which can then be pattern amtch with ease.

```rust
let nostr_relay = NostrRelay::new("wss://nostr.bongbong.com").await.unwrap();
let filter: Value = json!({
    "authors": vec!(pubkey),
    "kinds": [1]
});

nostr_relay.subscribe(filter).await.unwrap();

let mut found_note: Option<SignedNote> = None;

while let Some(result) = nostr_relay.read_notes().await {
    match result {
        Ok(note) => {
            info!("Nota: {}", note.to_string());
            if let Ok((_notice, _id, signed_note)) = from_str::<(String, String, SignedNote)>(&note) {
                    found_note = Some(signed_note);
                    break;
            }
        }
        _ => {
            nostr_relay.close().await.expect("Error al cerrar la conexion");
            return Err("Error".to_string());
        }
    }
}
```

