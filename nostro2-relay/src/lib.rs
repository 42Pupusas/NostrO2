#![warn(
    clippy::all,
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

    #[tokio::test]
    async fn test_relay() {
        let relay = super::relay::NostrRelay::new("wss://relay.illuminodes.com")
            .await
            .expect("Failed to create relay");
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(100),
            ..Default::default()
        };
        relay.send(filter).await.expect("Failed to send filter");
        while let Some(msg) = relay.recv().await {
            println!("{:?}", msg);
            break;
        }
    }
    #[tokio::test]
    async fn test_relay_pool_count() {
        let pool = super::pool::NostrPool::new(&vec![
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
        ])
        .await;
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        pool.send(&filter).await.expect("Failed to send filter");
        let mut count = 0;
        let mut eose = 0;
        while let Some(msg) = pool.recv().await {
            match msg {
                nostro2::relay_events::NostrRelayEvent::NewNote(..) => {
                    println!("{:?}", msg);
                    count += 1;
                }
                nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) => {
                    println!("{:?}", msg);
                    eose += 1;
                }
                _ => {}
            }
            if eose == 2 {
                break;
            }
        }
        assert!(count == 20);
    }

    #[tokio::test]
    async fn test_pool() {
        let time_spent = std::time::Instant::now();
        let pool = super::pool::NostrPool::new(&vec![
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ])
        .await;
        println!("Connected in: {:?}", time_spent.elapsed());
        let filter = nostro2::subscriptions::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(2000),
            ..Default::default()
        };
        pool.send(&filter).await.expect("Failed to send filter");
        println!("Sent filter in: {:?}", time_spent.elapsed());
        let mut count = 0;
        while let Some(msg) = pool.recv().await {
            let nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                continue;
            };
            println!("{:?}", msg);
            println!("Received in: {:?}", time_spent.elapsed());
            count += 1;
            if count > 5 {
                break;
            }
        }
        assert!(count > 3);
        println!("Done in: {:?}", time_spent.elapsed());
    }
    #[tokio::test]
    async fn send_note() {
        let signer = nostro2_signer::keypair::NostrKeypair::generate(false);
        let mut note = nostro2::note::NostrNote {
            kind: 300,
            content: "Hello, World!".to_string(),
            ..Default::default()
        };
        signer.sign_nostr_event(&mut note);
        let pool = super::pool::NostrPool::new(&[
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ])
        .await;
        pool.send(&note).await.expect("Failed to send note");
        let mut count = 0;
        while let Some(msg) = pool.recv().await {
            let nostro2::relay_events::NostrRelayEvent::SentOk(_, _, did_send, _) = msg else {
                continue;
            };
            if did_send {
                println!("{:?}", msg);
                count += 1;
                if count > 3 {
                    break;
                }
            }
        }
    }
}
