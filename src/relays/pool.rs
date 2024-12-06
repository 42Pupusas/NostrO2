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

pub type PoolRelayBroadcaster = Vec<UnboundedSender<WebSocketMessage>>;
pub type PoolRelayReceiver = UnboundedReceiver<(String, RelayEvent)>;
pub type PoolRelaySender = UnboundedSender<(String, RelayEvent)>;

pub struct RelayPool {
    pub incoming_channel: PoolRelayReceiver,
    pub outgoing_channels: PoolRelayBroadcaster,
}

impl RelayPool {
    pub async fn new(urls: Vec<String>) -> anyhow::Result<Self> {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let unique_notes = Arc::new(Mutex::new(HashSet::<SignedNote>::new()));
        let mut outgoing_channels = vec![];
        let mut tasks = vec![];
        for relay_url in urls {
            if let Ok(relay) = NostrRelay::new(&relay_url).await {
                let outgoing_chan = tokio::sync::mpsc::unbounded_channel();
                outgoing_channels.push(outgoing_chan.0);
                let future = Self::process_relay_events(
                    unique_notes.clone(),
                    relay,
                    event_tx.clone(),
                    outgoing_chan.1,
                );
                tasks.push(Box::pin(future));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        use tokio::task::spawn as new_task;
        #[cfg(target_arch = "wasm32")]
        use wasm_bindgen_futures::spawn_local as new_task;
        new_task(async move {
            if let Err(e) = futures_util::future::select_ok(tasks.iter_mut()).await {
                error!("{:?}", e);
            }
        });
        Ok(Self {
            outgoing_channels,
            incoming_channel: event_rx,
        })
    }
    async fn process_relay_events(
        notes: Arc<Mutex<HashSet<SignedNote>>>,
        mut relay: NostrRelay,
        event_writer: PoolRelaySender,
        mut outgoing_chan: UnboundedReceiver<WebSocketMessage>,
    ) -> Result<(), ()> {
        loop {
            tokio::select! {
                Some(Ok(event)) = relay.reader.next() => {
                    if let WebSocketMessage::Close(_) = event {
                        error!("Relay closed connection");
                        drop(relay);
                        break;
                    }
                    if let WebSocketMessage::Text(text_event) = event {
                        if let Ok(event) = RelayEvent::try_from(text_event) {
                            match event {
                                RelayEvent::NewNote(NoteEvent(_, _, ref signed_note)) => {
                                    let mut notes = notes.lock().await;
                                    if notes.insert(signed_note.clone()) {
                                        if let Err(e) = event_writer.send((relay.url.clone().to_string(), event)) {
                                            error!("{:?}", e);
                                            drop(relay);
                                            break;
                                        }
                                    }
                                }
                                _ => {
                                    if let Err(e) = event_writer.send((relay.url.clone().to_string(), event)) {
                                        error!("{:?}", e);
                                        drop(relay);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Some(out) = outgoing_chan.recv() => {
                    if let Err(e) = relay.writer.send(out).await {
                        error!("{:?}", e);
                        break;
                    }
                }
                else => {
                    drop(relay);
                    break},
            }
        }
        Err(())
    }
    pub fn broadcaster(&self) -> PoolRelayBroadcaster {
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
        self.incoming_channel.close();
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {

    use super::*;
    use crate::relays::{EndOfSubscriptionEvent, NostrSubscription};
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[tokio::test]
    //#[wasm_bindgen_test::wasm_bindgen_test]
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
            limit: Some(10),
            ..Default::default()
        }
        .relay_subscription();
        pool.subscribe(filter).expect("Failed to subscribe");
        let mut events = vec![];
        while let Some((_, event)) = pool.incoming_channel.recv().await {
            if let RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, ref subscription_id)) =
                event
            {
                events.push(subscription_id.clone());
                if events.len() == 5 {
                    break;
                }
            }
            if let RelayEvent::NewNote(NoteEvent(_, _, _)) = event {}
        }
        assert_eq!(events.len(), 5);
    }
}
