#![warn(
    clippy::all,
    clippy::missing_errors_doc,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]

pub mod pool;
pub mod relay;
pub extern crate nostro2;

#[cfg(test)]
mod tests {
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_wasm_connection() {
        let relay = crate::relay::NostrRelay::new("wss://relay.illuminodes.com");
        relay.is_open().await;
        assert_eq!(
            relay.relay_state().await,
            nostro2::relay_events::RelayStatus::OPEN
        );
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        relay.send(filter).await.expect("Failed to send filter");

        let mut received = false;
        while let Some(msg) = relay.read().await {
            let nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                received = true;
                continue;
            };

            break;
        }
        assert!(received);
        relay.disconnect().await;
        assert_eq!(
            relay.relay_state().await,
            nostro2::relay_events::RelayStatus::CLOSED
        );
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_wasm_count() {
        let relay = crate::relay::NostrRelay::new("wss://relay.arrakis.lat");
        relay.is_open().await;
        assert_eq!(
            relay.relay_state().await,
            nostro2::relay_events::RelayStatus::OPEN
        );
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        relay.send(filter).await.expect("Failed to send filter");

        let mut received = 0;
        while let Some(msg) = relay.read().await {
            match msg {
                nostro2::relay_events::NostrRelayEvent::NewNote(..) => {
                    received += 1;
                }
                nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) => {
                    break;
                }
                _ => (),
            }
        }
        assert!(received == 10);
        relay.disconnect().await;
        assert_eq!(
            relay.relay_state().await,
            nostro2::relay_events::RelayStatus::CLOSED
        );
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_closed_relay() {
        let relay = crate::relay::NostrRelay::new("wss://bouncer.minibolt.info");
        relay.is_open().await;
        assert_eq!(
            relay.relay_state().await,
            nostro2::relay_events::RelayStatus::CLOSED
        );
    }
    // #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_note_dedup() {
        let pool: crate::pool::RelayPool = [
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]
        .as_slice()
        .into();
        wasm_bindgen_test::console_log!("Created pool");
        wasm_bindgen_test::console_log!("Connected to pool");
        let new_keys = nostro2_signer::keypair::NostrKeypair::generate(false);
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![20004].into(),
            authors: vec![new_keys.public_key()].into(),
            ..Default::default()
        };
        let sub = pool.send(filter).await;
        assert!(matches!(
            sub,
            nostro2::relay_events::NostrClientEvent::Subscribe(..)
        ));
        wasm_bindgen_test::console_log!("Sent filter");
        let mut new_note = nostro2::note::NostrNote {
            content: "Test".to_string(),
            kind: 20004,
            pubkey: new_keys.public_key(),
            ..Default::default()
        };
        new_keys.sign_nostr_event(&mut new_note);
        pool.send(new_note).await;
        wasm_bindgen_test::console_log!("Sent note");

        loop {
            let Some(msg) = pool.read().await else {
                wasm_bindgen_test::console_log!("Failed to read from pool");
                continue;
            };
            if let nostro2::relay_events::NostrRelayEvent::NewNote(.., note) = msg {
                assert_eq!(note.kind, 20004);
                assert_eq!(note.pubkey, new_keys.public_key());
                wasm_bindgen_test::console_log!("Received note {:?}", note);
            }
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_relay_pool() {
        let pool: crate::pool::RelayPool = [
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]
        .as_slice()
        .into();
        wasm_bindgen_test::console_log!("Created pool");
        wasm_bindgen_test::console_log!("Connected to pool");
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        pool.send(filter).await;
        wasm_bindgen_test::console_log!("Sent filter");
        let mut count = 0;
        loop {
            let Some(msg) = pool.read().await else {
                wasm_bindgen_test::console_log!("Failed to read from pool");
                continue;
            };
            let nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                count += 1;
                if count > 5 {
                    break;
                } else {
                    continue;
                }
            };
            wasm_bindgen_test::console_log!("Received {:?}", msg);
        }
        assert!(count > 5);
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_message_count() {
        let pool: crate::pool::RelayPool =
            ["wss://relay.illuminodes.com", "wss://bitcoiner.social"]
                .as_slice()
                .into();
        wasm_bindgen_test::console_log!("Created pool");
        wasm_bindgen_test::console_log!("Connected to pool");
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        pool.send(filter).await;
        wasm_bindgen_test::console_log!("Sent filter");
        let mut count = 0;
        let mut eose = 0;
        loop {
            let Some(msg) = pool.read().await else {
                wasm_bindgen_test::console_log!("Failed to read from pool");
                continue;
            };
            match msg {
                nostro2::relay_events::NostrRelayEvent::NewNote(..) => {
                    count += 1;
                }
                nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) => {
                    wasm_bindgen_test::console_log!("Received {:?}", msg);
                    eose += 1;
                }
                _ => (),
            }
            if eose == 2 {
                break;
            }
        }
        wasm_bindgen_test::console_log!("Received {} messages", count);
        assert!(count == 20);
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _stress_test_relay_pool() {
        let pool: crate::pool::RelayPool = [
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]
        .as_slice()
        .into();

        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            ..Default::default()
        };
        pool.send(filter).await;
        let mut count = 0;
        loop {
            let Some(msg) = pool.read().await else {
                wasm_bindgen_test::console_log!("Failed to read from pool");
                continue;
            };
            if let nostro2::relay_events::NostrRelayEvent::NewNote(..) = msg {
                wasm_bindgen_test::console_log!("Received {}", count);
                count += 1;
            };
            if count > 10000 {
                break;
            }
        }
        assert!(count > 10000);
    }
}
