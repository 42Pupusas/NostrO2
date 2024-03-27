extern crate nostro2;
use nostro2::relays::{NostrRelay, RelayEvents};
use serde_json::json;

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
    use std::sync::Arc;
    #[cfg(not(target_arch = "wasm32"))]
    use nostro2::{notes::Note, userkeys::UserKeys, utils::new_keys};
    #[cfg(not(target_arch = "wasm32"))]
    use tokio::sync::Mutex;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_futures::spawn_local;
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    use super::*;

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    fn pass() {
        let websocket_thread = async {
            let mut counter = 0;
            let mut nostr_relay = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();
            let filter = json!({
                "kinds": [1],
                "limit": 10,
            });
            nostr_relay.subscribe(filter).await.unwrap();
            while let Ok(event) = nostr_relay.read_relay_events().await {
                match event {
                    RelayEvents::EVENT(_event, _id, _signed_note) => {
                        #[cfg(target_arch = "wasm32")]
                        console_log!("EVENT 1 {}", _signed_note.get_kind());
                        counter += 1;
                    }
                    RelayEvents::EOSE(_, _) => {
                        #[cfg(target_arch = "wasm32")]
                        console_log!("End of THREAD 1");
                        
                        break;

                    }
                    _ => {}
                }
            }
            assert_eq!(counter, 10);
        };
        spawn_local(websocket_thread);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn send_note() {
        let mut relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();

        let user_keys = hex::encode(&new_keys()[..]);
        let keypair = UserKeys::new(&user_keys).unwrap();

        let note = Note::new(&keypair.get_public_key(), 1, "Hello, World!");

        let signednote = keypair.sign_nostr_event(note);

        relay_connection.send_note(signednote).await.unwrap();
        while let Ok(event) = relay_connection.read_relay_events().await {
            match event {
                RelayEvents::OK(_event, _id, success, _notice) => {
                    assert_eq!(success, true);
                    break;
                }
                _ => {}
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn use_relay_on_threads() {
        let relay_connection = Arc::new(Mutex::new(
            NostrRelay::new("wss://relay.arrakis.lat").await.unwrap(),
        ));

        let relay_clone2 = relay_connection.clone();

        #[cfg(not(target_arch = "wasm32"))]
        let handle2 = tokio::spawn(async move {
            println!("THREAD 2");
            relay_clone2
                .lock()
                .await
                .subscribe(json!({
                    "kinds": [3],
                    "limit": 10,
                }))
                .await
                .unwrap();
            while let Ok(event) = relay_clone2.lock().await.read_relay_events().await {
                match event {
                    RelayEvents::EVENT(_event, _id, _signed_note) => {
                        println!("EVENT 2 {}", _signed_note.get_kind());
                    }
                    RelayEvents::EOSE(_, _) => {
                        println!("End of THREAD 2");
                        break;
                    }
                    _ => {}
                }
            }
        });

        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            println!("THREAD 2");
            relay_clone2
                .lock()
                .await
                .subscribe(json!({
                    "kinds": [3],
                    "limit": 10,
                }))
                .await
                .unwrap();
            while let Ok(event) = relay_clone2.lock().await.read_relay_events().await {
                match event {
                    RelayEvents::EVENT(_event, _id, _signed_note) => {
                        println!("EVENT 2 {}", _signed_note.get_kind());
                    }
                    RelayEvents::EOSE(_, _) => {
                        println!("End of THREAD 2");
                        break;
                    }
                    _ => {}
                }
            }
        });

        let relay_clone = relay_connection.clone();

        #[cfg(not(target_arch = "wasm32"))]
        let handle = tokio::spawn(async move {
            println!("THREAD 1");
            relay_clone
                .lock()
                .await
                .subscribe(json!({
                    "kinds": [1],
                    "limit": 10,
                }))
                .await
                .unwrap();
            while let Ok(event) = relay_clone.lock().await.read_relay_events().await {
                match event {
                    RelayEvents::EVENT(_event, _id, _signed_note) => {
                        println!("EVENT 1 {}", _signed_note.get_kind());
                    }
                    RelayEvents::EOSE(_, _) => {
                        println!("End of THREAD 1");
                        break;
                    }
                    _ => {}
                }
            }
        });

        #[cfg(not(target_arch = "wasm32"))]
        handle.await.unwrap();

        #[cfg(not(target_arch = "wasm32"))]
        handle2.await.unwrap();

        assert!(true);
    }
}
