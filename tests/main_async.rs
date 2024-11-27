
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen]
    extern "C" {
        fn setTimeout(closure: &Closure<dyn FnMut()>, millis: u32);
    }

    //     #[tokio::test]
    //     async fn send_note() {
    //         let relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();
    //
    //         let user_keys = hex::encode(&new_keys()[..]);
    //         let keypair = UserKeys::new(&user_keys).unwrap();
    //
    //         let note = Note::new(&keypair.get_public_key(), 1, "Hello, World!");
    //
    //         let signednote = keypair.sign_nostr_event(note);
    //
    //         relay_connection.send_note(signednote).await.unwrap();
    //         while let Ok(event) = relay_connection.relay_event_reader().recv().await {
    //             match event {
    //                 RelayEvents::OK(_id, success, _notice) => {
    //                     assert_eq!(success, true);
    //                     break;
    //                 }
    //                 _ => {}
    //             }
    //         }
    //     }
    //
    //     #[tokio::test]
    //     async fn fetch_events() {
    //         use nostro2::relays::NostrFilter;
    //
    //         let relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();
    //
    //         let mut counter = 0;
    //         let subscription = NostrFilter::default().new_limit(10).subscribe();
    //         let events = relay_connection
    //             .subscribe_until_eose(&subscription)
    //             .await
    //             .unwrap();
    //         for event in events {
    //             match event {
    //                 RelayEvents::EVENT(_id, _signed_note) => {
    //                     counter += 1;
    //                     println!("EVENT {}", _signed_note.get_kind());
    //                 }
    //                 _ => {}
    //             }
    //         }
    //         assert_eq!(counter, 10);
    //     }
    //
    //     #[tokio::test]
    //     async fn use_relay_on_threads() {
    //         use nostro2::relays::NostrFilter;
    //         use tokio::select;
    //
    //         let relay_connection = NostrRelay::new("wss://relay.arrakis.lat").await.unwrap();
    //
    //         let relay_clone2 = relay_connection.clone();
    //
    //         let handle2 = tokio::spawn(async move {
    //             let mut counter = 0;
    //             loop {
    //                 tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    //                 let test_keys = UserKeys::new(&hex::encode(&new_keys()[..])).unwrap();
    //                 let new_note = Note::new(&test_keys.get_public_key(), 20042, "Hello, World!");
    //                 let signed_note = test_keys.sign_nostr_event(new_note);
    //                 relay_clone2.send_note(signed_note).await.unwrap();
    //                 counter += 1;
    //                 println!("THREAD 2");
    //                 if counter == 12 {
    //                     break;
    //                 }
    //             }
    //         });
    //
    //         let relay_clone = relay_connection.clone();
    //
    //         let handle = tokio::spawn(async move {
    //             let mut counter = 0;
    //             println!("THREAD 1");
    //             let subscription = NostrFilter::default().new_kind(20042).subscribe();
    //             relay_clone.subscribe(&subscription).await.unwrap();
    //             while let Ok(event) = relay_clone.relay_event_reader().recv().await {
    //                 match event {
    //                     RelayEvents::EVENT(_id, _signed_note) => {
    //                         println!("EVENT 1 {}", _signed_note.get_kind());
    //                         counter += 1;
    //                         if counter == 3 {
    //                             break;
    //                         }
    //                     }
    //                     _ => {}
    //                 }
    //             }
    //         });
    //
    //         select! {
    //             _ = handle => {
    //                 assert!(true);
    //             }
    //             _ = handle2 => {
    //                 panic!("THREAD 2 DONE");
    //             }
    //         }
    //     }
}
