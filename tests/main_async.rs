extern crate nostro2;
use nostro2::relays::{NostrRelay, RelayEvents};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::wasm_bindgen;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[cfg(test)]
mod tests {

    #[cfg(not(target_arch = "wasm32"))]
    use nostro2::{notes::Note, userkeys::UserKeys, utils::new_keys};

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_futures::spawn_local;
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    use super::*;

    #[cfg(target_arch = "wasm32")]
    use serde_json::json;

    #[cfg(target_arch = "wasm32")]
    use crate::tests::nostro2::utils::new_keys;
    #[cfg(target_arch = "wasm32")]
    use nostro2::notes::Note;
    #[cfg(target_arch = "wasm32")]
    use nostro2::userkeys::UserKeys;

    #[cfg(target_arch = "wasm32")]
    use tokio_tungstenite_wasm::Message;

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen]
    extern "C" {
        fn setTimeout(closure: &Closure<dyn FnMut()>, millis: u32);
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    fn pass_threads() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::sync::Mutex;

        let websocket_thread = async {
            let nostr_relay = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();

            let relay_arc = Arc::new(nostr_relay);

            let writer_half = relay_arc.clone();
            let write_thread = async move {
                let filter = json!({
                    "kinds": [20042],
                });
                writer_half.subscribe(filter).await.unwrap();
                let user_keys = hex::encode(&new_keys()[..]);
                let keypair = UserKeys::new(&user_keys).unwrap();
                for i in 0..100 {
                    let note = Note::new(&keypair.get_public_key(), 20042, "Hello, World!");
                    let signednote = keypair.sign_nostr_event(note);
                    writer_half.send_note(signednote).await.unwrap();
                }
            };
            spawn_local(write_thread);

            let reader_half = relay_arc.clone();
            let read_thread = async move {
                while let Ok(event) = reader_half.read_relay_events().await {
                    match event {
                        RelayEvents::EVENT(_event, _id, _signed_note) => {
                            console_log!("EVENT {}", _signed_note.get_kind());
                        }
                        _ => {}
                    }
                }
            };
            spawn_local(read_thread);
        };

        spawn_local(websocket_thread);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn send_note() {
        let relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();

        let user_keys = hex::encode(&new_keys()[..]);
        let keypair = UserKeys::new(&user_keys).unwrap();

        let note = Note::new(&keypair.get_public_key(), 1, "Hello, World!");

        let signednote = keypair.sign_nostr_event(note);

        relay_connection.send_note(signednote).await.unwrap();
        while let Ok(event) = relay_connection.relay_event_reader().recv().await {
            match event {
                RelayEvents::OK(_id, success, _notice) => {
                    assert_eq!(success, true);
                    break;
                }
                _ => {}
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn fetch_events() {
        use nostro2::relays::NostrFilter;

        let relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();

        let mut counter = 0;
        let subscription = NostrFilter::default().new_limit(10).subscribe();
        let events = relay_connection
            .subscribe_until_eose(&subscription)
            .await
            .unwrap();
        for event in events {
            match event {
                RelayEvents::EVENT(_id, _signed_note) => {
                    counter += 1;
                    println!("EVENT {}", _signed_note.get_kind());
                }
                _ => {}
            }
        }
        assert_eq!(counter, 10);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn use_relay_on_threads() {
        use nostro2::relays::NostrFilter;
        use tokio::select;

        let relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();

        let relay_clone2 = relay_connection.clone();

        let handle2 = tokio::spawn(async move {
            let mut counter = 0;
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let test_keys = UserKeys::new(&hex::encode(&new_keys()[..])).unwrap();
                let new_note = Note::new(&test_keys.get_public_key(), 20042, "Hello, World!");
                let signed_note = test_keys.sign_nostr_event(new_note);
                relay_clone2.send_note(signed_note).await.unwrap();
                counter += 1;
                println!("THREAD 2");
                if counter == 12 {
                    break;
                }
            }
        });

        let relay_clone = relay_connection.clone();

        let handle = tokio::spawn(async move {
            let mut counter = 0;
            println!("THREAD 1");
            let subscription = NostrFilter::default().new_kind(20042).subscribe();
            relay_clone.subscribe(&subscription).await.unwrap();
            while let Ok(event) = relay_clone.relay_event_reader().recv().await {
                match event {
                    RelayEvents::EVENT(_id, _signed_note) => {
                        println!("EVENT 1 {}", _signed_note.get_kind());
                        counter += 1;
                        if counter == 3 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        });

        select! {
            _ = handle => {
                assert!(true);
            }
            _ = handle2 => {
                panic!("THREAD 2 DONE");
            }
        }
    }
}
