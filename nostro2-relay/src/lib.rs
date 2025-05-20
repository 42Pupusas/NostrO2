#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
pub mod errors;
mod pool;
mod relay;
pub extern crate nostro2;
pub use pool::NostrPool;
pub use relay::NostrRelay;


#[cfg(test)]
mod tests {
    use nostro2::NostrSigner;

    #[tokio::test]
    async fn test_relay() {
        let relay = super::relay::NostrRelay::new("wss://relay.illuminodes.com")
            .await
            .expect("Failed to create relay");
        let filter = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(100),
            ..Default::default()
        };
        relay.send(filter).await.expect("Failed to send filter");
        let msg = relay.recv().await;
        assert!(msg.is_some());
    }
    #[tokio::test]
    async fn notes_deduped_correctly() {
        let pool =
            super::pool::NostrPool::new(&["wss://relay.illuminodes.com", "wss://freespeech.casa"]);
        let mut seen = std::collections::HashSet::new();

        let pool_clone = pool.clone();
        let filter = nostro2::NostrSubscription {
            kinds: vec![24442].into(),
            ..Default::default()
        };
        pool.send(&filter).expect("Failed to send filter");

        let mut test_note = nostro2::NostrNote {
            content: "test".to_string(),
            kind: 24442,
            ..Default::default()
        };
        let test_key = nostro2_signer::keypair::NostrKeypair::generate(true);
        test_key
            .sign_note(&mut test_note)
            .expect("Failed to sign note");

        let mut test_note_2 = nostro2::NostrNote {
            content: "test-2".to_string(),
            kind: 24442,
            ..Default::default()
        };
        test_key
            .sign_note(&mut test_note_2)
            .expect("Failed to sign note");
        pool.send(test_note).expect("Failed to send note");
        pool.send(test_note_2).expect("Failed to send note");
        while let Some(msg) = pool_clone.recv().await {
            println!("{msg:#?}");
            if let nostro2::NostrRelayEvent::NewNote(.., ref note) = msg {
                assert!(seen.insert(note.id.clone()));
                if seen.len() == 2 {
                    break;
                }
            }
        }
    }
    #[tokio::test]
    async fn test_relay_pool_count() {
        let pool = super::pool::NostrPool::new(&[
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]);
        let filter = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        pool.send(&filter).expect("Failed to send filter");
        let mut count = 0;
        let mut eose = 0;
        while let Some(msg) = pool.recv().await {
            match msg {
                nostro2::NostrRelayEvent::NewNote(..) => {
                    println!("{msg:?}");
                    count += 1;
                }
                nostro2::NostrRelayEvent::EndOfSubscription(_, _) => {
                    println!("{msg:?}");
                    eose += 1;
                }
                _ => {}
            }
            if eose == 2 {
                break;
            }
        }
        assert!(count > 10);
    }

    #[tokio::test]
    async fn test_pool() {
        let time_spent = std::time::Instant::now();
        let pool = super::pool::NostrPool::new(&[
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]);
        println!("Connected in: {:?}", time_spent.elapsed());
        let filter = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(20),
            ..Default::default()
        };
        pool.send(&filter).expect("Failed to send filter");
        println!("Sent filter in: {:?}", time_spent.elapsed());
        let mut count = 0;
        while let Some(msg) = pool.recv().await {
            let nostro2::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                continue;
            };
            println!("{msg:?}");
            println!("Received in: {:?}", time_spent.elapsed());
            count += 1;
            if count > 3 {
                break;
            }
        }
        assert!(count > 3);
        println!("Done in: {:?}", time_spent.elapsed());
    }
}
