extern crate nostro2;
use nostro2::notes::Note;
use nostro2::relays::{NostrRelay, RelayEvents};
use nostro2::userkeys::UserKeys;
use nostro2::utils::get_unix_timestamp;
use serde_json::json;

use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

const URL: &str = "wss://relay.nostrss.re";
const PK1: &str = "07947aa9d48d099604ea53e2d347203d90fb133d77a430de43373b8eabd6275d";

#[tokio::test]
async fn connect_subscribe_and_read_note() {
     let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::TRACE)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");
    // Init Relay
    if let Ok(ws_connection) = NostrRelay::new(URL).await {
        ws_connection
            .subscribe(json!({"kinds":[1],"limit":1}))
            .await
            .expect("Failed to subscribe to relay!");

        loop {
            if let Some(Ok(relay_msg)) = ws_connection.read_from_relay().await {
                match relay_msg {
                    RelayEvents::EVENT(_event, _id, signed_note) => {
                        assert_eq!(signed_note.verify(), true);
                        break;
                    }
                    _ => {}
                }
            }
        }
    } else {
        panic!("Failed to connect to relay!");
    }
}

#[tokio::test]
async fn connect_subscribe_and_send_note() {
    let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
    if let Ok(ws_connection) = NostrRelay::new(URL).await {
        let user_key_pair = UserKeys::new(PK1).expect("Failed to create UserKeys!");
        let unsigned_note = Note::new(
            user_key_pair.get_public_key().to_string(),
            300,
            content_of_note,
        );
        let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
        ws_connection
            .send_note(signed_note)
            .await
            .expect("Failed to send note!");

        ws_connection
            .subscribe(json!({
              "kinds":[300],
              "limit":1,
              "since": get_unix_timestamp() - 100
            }))
            .await
            .expect("Not Subscribed");

        loop {
            if let Some(Ok(relay_msg)) = ws_connection.read_from_relay().await {
                match relay_msg {
                    RelayEvents::EVENT(_event, _id, signed_note) => {
                        assert_eq!(signed_note.verify(), true);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[tokio::test]
async fn check_filtered_tags() {
    let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
    if let Ok(ws_connection) = NostrRelay::new(URL).await {
        let user_key_pair = UserKeys::new(PK1).expect("Failed to create UserKeys!");
        let mut unsigned_note = Note::new(
            user_key_pair.get_public_key().to_string(),
            400,
            content_of_note,
        );
        unsigned_note.tag_note("l", "rust");
        let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
        let mut unsigned_note2 = Note::new(
            user_key_pair.get_public_key().to_string(),
            400,
            content_of_note,
        );
        unsigned_note2.tag_note("l", "python");
        let signed_note2 = user_key_pair.sign_nostr_event(unsigned_note2);
        ws_connection
            .send_note(signed_note)
            .await
            .expect("Failed to send note!");
        ws_connection
            .send_note(signed_note2)
            .await
            .expect("Failed to send note!");

        ws_connection
            .subscribe(json!({
              "kinds":[400],
              "limit":2,
              "#l":["rust"],
            }))
            .await
            .expect("Not Subscribed");


        loop {
            if let Some(Ok(relay_msg)) = ws_connection.read_from_relay().await {
                match relay_msg {
                    RelayEvents::EVENT(_event, _id, signed_note) => {
                        assert_eq!(signed_note.verify(), true);
                        assert_eq!(&*signed_note.get_tags_by_id("l").unwrap(), ["rust"]);
                    }
                    RelayEvents::EOSE(_, _) => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    } else {
    }
}

#[tokio::test]
async fn check_relay_can_run_on_threads() {
    tokio::spawn(async move {
        if let Ok(_ws_connection) = NostrRelay::new(URL).await {
            assert_eq!(true, true);
        } else {
            assert_eq!(true, false);
        }
    });
}
