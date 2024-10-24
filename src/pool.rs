use std::{collections::HashSet, sync::Arc};

use async_channel::{Receiver, Sender};
use tokio::sync::Mutex;

use crate::{
    notes::SignedNote,
    relays::{NostrRelay, NostrSubscription, RelayEvents},
};
#[cfg(not(target_arch = "wasm32"))]
use tokio::spawn as new_thread;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local as new_thread;

#[derive(Clone)]
pub struct RelayPool {
    note_channel: Receiver<SignedNote>,
    event_channel: Receiver<RelayEvents>,
    subscriptions: Vec<Sender<NostrSubscription>>,
    outgoing_channels: Vec<Sender<SignedNote>>,
    cancel_sub_channels: Vec<Sender<String>>,
    close_channels: Vec<Sender<()>>,
}

impl RelayPool {
    pub fn all_events(&self) -> Receiver<RelayEvents> {
        self.event_channel.clone()
    }
    pub fn pooled_notes(&self) -> Receiver<SignedNote> {
        self.note_channel.clone()
    }
    pub async fn broadcast_note(&self, signed_note: SignedNote) -> anyhow::Result<()> {
        for channel in &self.outgoing_channels {
            channel.send(signed_note.clone()).await?;
        }
        Ok(())
    }
    pub async fn subscribe(&self, sub: NostrSubscription) -> anyhow::Result<()> {
        for subscription in &self.subscriptions {
            subscription.send(sub.clone()).await?;
        }
        Ok(())
    }
    pub fn broadcaster(&self) -> Vec<Sender<SignedNote>> {
        self.outgoing_channels.clone()
    }
    pub async fn cancel_subscription(&self, sub_id: String) -> anyhow::Result<()> {
        for channel in &self.cancel_sub_channels {
            channel.send(sub_id.clone()).await?;
        }
        Ok(())
    }
    pub async fn close(&self) -> anyhow::Result<()> {
        self.note_channel.close();
        self.event_channel.close();
        for channel in &self.close_channels {
            channel.send(()).await?;
            channel.close();
        }
        for subscription in &self.subscriptions {
            subscription.close();
        }
        for channel in &self.outgoing_channels {
            channel.close();
        }
        for channel in &self.cancel_sub_channels {
            channel.close();
        }
        Ok(())
    }
    pub async fn new(urls: Vec<String>) -> anyhow::Result<Self> {
        let (note_tx, note_rx) = async_channel::unbounded();
        let (event_tx, event_rx) = async_channel::unbounded();
        let unique_notes = Arc::new(Mutex::new(HashSet::<SignedNote>::new()));
        let mut subscriptions = vec![];
        let mut outgoing_channels = vec![];
        let mut cancel_sub_channels = vec![];
        let mut close_channels = vec![];
        for relay_url in urls {
            let note_tx = note_tx.clone();
            let event_tx = event_tx.clone();
            let notes = unique_notes.clone();
            let (sub_tx, sub_rx) = async_channel::unbounded();
            let (outgoing_tx, outgoing_rx) = async_channel::unbounded();
            let (cancel_sub_tx, cancel_sub_rx) = async_channel::unbounded();
            let (close_tx, close_rx) = async_channel::unbounded();
            subscriptions.push(sub_tx);
            outgoing_channels.push(outgoing_tx);
            cancel_sub_channels.push(cancel_sub_tx);
            close_channels.push(close_tx);
            new_thread(async move {
                if let Ok(relay) = NostrRelay::new(&relay_url).await {
                    let event_reader = relay.relay_event_reader();

                    loop {
                        tokio::select! {
                            // Handle incoming events
                            event = event_reader.recv() => match event {
                                Ok(event) => {
                                    if let RelayEvents::EVENT(_, signed_note) = &event {
                                        let mut notes = notes.lock().await;
                                        if notes.insert(signed_note.clone()) {
                                            if let Err(_e) = note_tx.send(signed_note.clone()).await {
                                                break;
                                            }
                                        }
                                    }
                                    if let Err(_e) = event_tx.send(event).await {
                                        break;
                                    }
                                }
                                Err(_e) => break, // Handle relay disconnect
                            },
                            // Handle subscriptions
                            sub = sub_rx.recv() => {
                                if let Ok(sub) = sub {
                                    if let Err(_e) = relay.subscribe(&sub).await {
                                    }
                                }
                            },
                            // Handle outgoing notes
                            note = outgoing_rx.recv() => {
                                if let Ok(note) = note {
                                    let _ = relay.send_note(note).await;
                                }
                            },
                            // Handle subscription cancellations
                            cancel = cancel_sub_rx.recv() => {
                                if let Ok(cancel) = cancel {
                                    let _ = relay.unsubscribe(cancel).await;
                                }
                            },
                            // Handle close requests
                            _ = close_rx.recv() => {
                                break;
                            }
                        }
                    }
                    relay.cleanup().await;
                    let _ = relay.close().await;
                }
            });
        }
        Ok(Self {
            note_channel: note_rx,
            event_channel: event_rx,
            subscriptions,
            outgoing_channels,
            cancel_sub_channels,
            close_channels,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::relays::NostrFilter;

    #[tokio::test]
    async fn test_relay_pool() {
        let pool = RelayPool::new(vec![
            "wss://relay.arrakis.lat".to_string(),
            "wss://relay.illuminodes.com".to_string(),
            "wss://frens.nostr1.com".to_string(),
            "wss://bitcoiner.social".to_string(),
            "wss://bouncer.minibolt.info".to_string(),
            "wss://freespeech.casa".to_string(),
            "wss://junxingwang.org".to_string(),
            "wss://nostr.0x7e.xyz".to_string(),
        ])
        .await
        .expect("Failed to create pool");
        let filter = NostrFilter::default().new_kind(1).new_limit(1).subscribe();
        pool.subscribe(filter).await.expect("Failed to subscribe");

        let mut events = vec![];
        while let Ok(event) = pool.all_events().recv().await {
            match event {
                RelayEvents::EOSE(_) => {
                    events.push(event);
                    println!("EOSE");
                    if events.len() == 8 {
                        break;
                    }
                }
                _ => (),
            }
        }
        pool.close().await.expect("Failed to close pool");
        let filter = NostrFilter::default().new_kind(1).new_limit(1).subscribe();
        assert!(pool.subscribe(filter).await.is_err());
    }
}

#[cfg(target_arch = "wasm32")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::RelayEvents;
    use crate::relays::NostrFilter;
    use wasm_bindgen_test::console_log;
    use wasm_bindgen_test::wasm_bindgen_test;
    #[wasm_bindgen_test]
    async fn test_relay_pool() {
        let pool = RelayPool::new(vec![
            "wss://relay.arrakis.lat".to_string(),
            "wss://relay.illuminodes.com".to_string(),
            "wss://frens.nostr1.com".to_string(),
            "wss://bitcoiner.social".to_string(),
            "wss://bouncer.minibolt.info".to_string(),
            "wss://freespeech.casa".to_string(),
            "wss://junxingwang.org".to_string(),
            "wss://nostr.0x7e.xyz".to_string(),
        ])
        .await
        .expect("Failed to create pool");
        println!("Pool created");
        let filter = NostrFilter::default().new_kind(1).new_limit(1000).subscribe();
        pool.subscribe(filter).await.expect("Failed to subscribe");

        let mut events = vec![];
        while let Ok(event) = pool.all_events().recv().await {
            match event {
                RelayEvents::EOSE(_) => {
                    events.push(event);
                    console_log!("EOSE");
                    if events.len() >= 4 {
                        break;
                    }
                }
                _ => (),
            }
        }
        pool.close().await.expect("Failed to close pool");
        console_log!("{:?}", events);
        assert_eq!(events.len(), 8);
        console_log!("Pool closed");
        let filter = NostrFilter::default().new_kind(1).new_limit(1).subscribe();
        assert!(pool.subscribe(filter).await.is_err());
    }
}
