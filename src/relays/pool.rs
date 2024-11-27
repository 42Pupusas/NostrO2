use std::{collections::HashSet, sync::Arc};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    Mutex,
};
use tracing::error;

use crate::{
    notes::SignedNote,
    relays::{
        CloseEvent, NostrRelay, NoteEvent, RelayEvent, RelayEventTag, SendNoteEvent, SubscribeEvent,
    },
};

#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
#[cfg(target_arch = "wasm32")]
use tokio_tungstenite_wasm::Message as WebSocketMessage;

pub struct RelayPool {
    pub note_channel: UnboundedReceiver<SignedNote>,
    pub event_channel: UnboundedReceiver<RelayEvent>,
    pub outgoing_channels: Vec<UnboundedSender<WebSocketMessage>>,
}

impl RelayPool {
    async fn process_relay_events(
        notes: Arc<Mutex<HashSet<SignedNote>>>,
        mut relay: NostrRelay,
        note_writer: UnboundedSender<SignedNote>,
        event_writer: UnboundedSender<RelayEvent>,
        mut outgoing_chan: UnboundedReceiver<WebSocketMessage>,
    ) {
        loop {
            tokio::select! {
                Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next() => {
                    match RelayEvent::try_from(event) {
                        Ok(RelayEvent::NewNote(NoteEvent(_, _, signed_note))) => {
                            let mut notes = notes.lock().await;
                            if notes.insert(signed_note.clone()) {
                                if let Err(e) = note_writer.send(signed_note.clone()) {
                                    error!("{:?}", e);
                                    break;
                                }
                            }
                        }
                        Ok(event) => {
                            if let Err(e) = event_writer.send(event) {
                                error!("{:?}", e);
                                break;
                            }
                        }
                        _ => (),
                    }
                }
                Some(out) = outgoing_chan.recv() => {
                    if let Err(e) = relay.writer.send(out).await {
                        error!("{:?}", e);
                        break;
                    }
                }
                else => break,
            }
        }
    }
    pub async fn new(urls: Vec<String>) -> anyhow::Result<Self> {
        let (note_tx, note_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let unique_notes = Arc::new(Mutex::new(HashSet::<SignedNote>::new()));
        let mut outgoing_channels = vec![];
        for relay_url in urls {
            if let Ok(relay) = NostrRelay::new(&relay_url).await {
                let outgoing_chan = tokio::sync::mpsc::unbounded_channel();
                outgoing_channels.push(outgoing_chan.0);
                let future = Self::process_relay_events(
                    unique_notes.clone(),
                    relay,
                    note_tx.clone(),
                    event_tx.clone(),
                    outgoing_chan.1,
                );
                #[cfg(not(target_arch = "wasm32"))]
                tokio::task::spawn(future);
                #[cfg(target_arch = "wasm32")]
                wasm_bindgen_futures::spawn_local(future);
            }
        }
        Ok(Self {
            note_channel: note_rx,
            event_channel: event_rx,
            outgoing_channels,
        })
    }
    pub fn broadcaster(&self) -> Vec<UnboundedSender<WebSocketMessage>> {
        self.outgoing_channels.clone()
    }
    pub fn broadcast_note(&mut self, signed_note: SignedNote) -> anyhow::Result<()> {
        let note: String = SendNoteEvent(RelayEventTag::EVENT, signed_note).into();
        let message = WebSocketMessage::Text(note.to_string());
        self.outgoing_channels
            .retain_mut(|c| c.send(message.clone()).is_ok());
        if self.outgoing_channels.is_empty() {
            return Err(anyhow::anyhow!("No relays available"));
        }
        Ok(())
    }
    pub fn subscribe(&mut self, sub: SubscribeEvent) -> anyhow::Result<()> {
        let message = WebSocketMessage::Text(sub.into());
        self.outgoing_channels
            .retain_mut(|c| c.send(message.clone()).is_ok());
        if self.outgoing_channels.is_empty() {
            return Err(anyhow::anyhow!("No relays available"));
        }
        Ok(())
    }
    pub fn cancel_subscription(&mut self, sub_id: String) -> anyhow::Result<()> {
        let cancel = CloseEvent(RelayEventTag::CLOSE, sub_id);
        let message = WebSocketMessage::Text(cancel.into());
        self.outgoing_channels
            .retain_mut(|c| c.send(message.clone()).is_ok());
        if self.outgoing_channels.is_empty() {
            return Err(anyhow::anyhow!("No relays available"));
        }
        Ok(())
    }
    pub fn close(mut self) -> anyhow::Result<()> {
        self.note_channel.close();
        self.event_channel.close();
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::relays::{EndOfSubscriptionEvent, NostrSubscription};

    #[tokio::test]
    async fn test_relay_pool() {
        let mut pool = RelayPool::new(vec![
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
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(100),
            ..Default::default()
        }
        .relay_subscription();
        pool.subscribe(filter).expect("Failed to subscribe");
        let mut events = vec![];
        while let Some(event) = pool.event_channel.recv().await {
            if let RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, subscription_id)) = event
            {
                events.push(subscription_id);
                println!("EOSE");
                if events.len() == 4 {
                    break;
                }
            }
        }
        assert_eq!(events.len(), 4);
    }
}

#[cfg(target_arch = "wasm32")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::relays::{EndOfSubscriptionEvent, NostrSubscription};
    use wasm_bindgen_test::console_log;
    use wasm_bindgen_test::wasm_bindgen_test;
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    #[wasm_bindgen_test]
    async fn test_relay_pool() {
        let mut pool = RelayPool::new(vec![
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
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(100),
            ..Default::default()
        }
        .relay_subscription();
        pool.subscribe(filter).expect("Failed to subscribe");
        let mut events = vec![];
        while let Some(event) = pool.event_channel.recv().await {
            match event {
                RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, subscription_id)) => {
                    events.push(subscription_id);
                    console_log!("EOSE");
                    if events.len() == 4 {
                        break;
                    }
                }
                _ => (),
            }
        }
        assert_eq!(events.len(), 4);
    }
}
