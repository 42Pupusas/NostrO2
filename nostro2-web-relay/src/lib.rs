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
        let relay = crate::relay::NostrRelay::new("wss://relay.illuminodes.com").unwrap();
        relay.is_open().await;
        assert_eq!(relay.state(), nostro2::relay_events::RelayStatus::OPEN);
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        relay.send(&filter.into()).expect("Failed to send filter");
        let mut reader = relay.reader.resubscribe();

        let mut received = false;
        while let Ok(msg) = reader.recv().await {
            let nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                received = true;
                continue;
            };

            break;
        }
        assert!(received);
        relay.close().expect("Failed to close relay");
        assert_eq!(relay.state(), nostro2::relay_events::RelayStatus::CLOSING);
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
        pool.connect().await.unwrap();
        wasm_bindgen_test::console_log!("Connected to pool");
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        pool.send(&(filter.into())).expect("Failed to send filter");
        wasm_bindgen_test::console_log!("Sent filter");
        let mut count = 0;
        loop {
            let Ok(msg) = pool.read().await else {
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

        pool.connect().await.unwrap();
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            ..Default::default()
        };
        pool.send(&filter.into()).expect("Failed to send filter");
        let mut count = 0;
        loop {
            let Ok(msg) = pool.read().await else {
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
