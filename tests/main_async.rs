extern crate nostro2;
use nostro2::notes::{Note, SignedNote};
use nostro2::relays::NostrRelay;
use nostro2::userkeys::UserKeys;
use nostro2::utils::get_unix_timestamp;
use serde_json::{from_str, json};

const URL: &str = "wss://relay.roadrunner.lat";
const PK1: &str = "07947aa9d48d099604ea53e2d347203d90fb133d77a430de43373b8eabd6275d";

#[tokio::test]
async fn connect_subscribe_and_read_note() {
    // Init Relay
    let ws_connection = NostrRelay::new(URL).await;

    ws_connection
        .subscribe(json!({"kinds":[1],"limit":1}))
        .await
        .expect("Failed to subscribe to relay!");

    loop {
        match ws_connection.read_notes().await {
            Some(Ok(message)) => {
                if let Ok((_type, _id, note)) = from_str::<(String, String, SignedNote)>(&message) {
                    assert_eq!(SignedNote::verify_note(note), true);
                    break;
                }
            }
            _ => println!("None"),
        }
    }
}

#[tokio::test]
async fn connect_subscribe_and_send_note() {
    let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
    let ws_connection = NostrRelay::new(URL).await;
    println!("Subscribed to relay!");
    let user_key_pair = UserKeys::new(PK1);
    println!("Created UserKeys!");
    let unsigned_note = Note::new(
        user_key_pair.get_public_key().to_string(),
        [].to_vec(),
        300,
        content_of_note,
    );
    let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
    println!("Signed Note!");
    ws_connection.send_note(signed_note).await;
    println!("Sent Note!");

    ws_connection
        .subscribe(json!({
          "kinds":[300],
          "limit":1,
          "since": get_unix_timestamp() - 100
        }))
        .await
        .expect("Not Subscribed");

    println!("Subscribed to relay!");

    loop {
        match ws_connection.read_notes().await {
            Some(Ok(message)) => {
                println!("Message: {}", message);
                match from_str::<(String, String, SignedNote)>(&message) {
                    Ok((_type, _id, note)) => {
                        println!("Received Note!");
                        assert_eq!(note.kind, 300);
                        break;
                    }
                    Err(e) => {
                        println!("Deserialization error: {:?}", e);
                    }
                }
            }
            _ => println!("None"),
        }
    }
}
