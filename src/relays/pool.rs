use super::relay_connection::{NostrWriter, RelayState};
use crate::{
    notes::NostrNote,
    relays::{NostrRelay, NoteEvent, RelayEvent, SubscribeEvent},
};
use futures_util::StreamExt;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    RwLock,
};
use tracing::error;

pub type RelayBroadcasterList = Arc<RwLock<Vec<NostrWriter>>>;
#[derive(Clone)]
pub struct PoolRelayBroadcaster(pub RelayBroadcasterList);
impl PoolRelayBroadcaster {
    pub async fn broadcast_note(&self, signed_note: NostrNote) -> anyhow::Result<()> {
        let mut writers = self.0.write().await;
        for writer in writers.iter_mut() {
            writer.send_note(signed_note.clone()).await?;
        }
        Ok(())
    }
    pub async fn subscribe(&self, sub: SubscribeEvent) -> anyhow::Result<()> {
        let mut writers = self.0.write().await;
        for writer in writers.iter_mut() {
            writer.subscribe(sub.clone()).await?;
        }
        Ok(())
    }
    pub async fn cancel_subscription(&self, sub_id: String) -> anyhow::Result<()> {
        let mut writers = self.0.write().await;
        for writer in writers.iter_mut() {
            writer.unsubscribe(sub_id.clone()).await?;
        }
        Ok(())
    }
}
pub type PoolRelayReceiver = UnboundedReceiver<(String, RelayEvent)>;
pub type PoolRelaySender = UnboundedSender<(String, RelayEvent)>;

pub type RelayTableMap = HashMap<String, RelayState>;
#[derive(Clone)]
pub struct RelayTable(pub Arc<RwLock<RelayTableMap>>);
impl RelayTable {
    pub async fn get(&self, url: &str) -> Option<RelayState> {
        self.0.read().await.get(url).cloned()
    }
    pub async fn insert(&self, url: String, state: RelayState) {
        self.0.write().await.insert(url, state);
    }
    pub async fn remove(&self, url: &str) {
        self.0.write().await.remove(url);
    }
}

pub type NostrNoteLibrary = HashSet<NostrNote>;
#[derive(Clone)]
pub struct NoteLibrary(pub Arc<RwLock<NostrNoteLibrary>>);
impl NoteLibrary {
    pub async fn insert(&self, note: NostrNote) -> bool {
        self.0.write().await.insert(note)
    }
}

pub struct NostrRelayPool {
    pub listener: PoolRelayReceiver,
    pub writer: PoolRelayBroadcaster,
    pub relay_states: RelayTable,
    pub library: NoteLibrary,
}

impl NostrRelayPool {
    pub async fn new(urls: Vec<String>) -> anyhow::Result<Self> {
        let (event_tx, listener) = tokio::sync::mpsc::unbounded_channel();
        let library = NoteLibrary(Arc::new(RwLock::new(HashSet::new())));
        let relay_states = RelayTable(Arc::new(RwLock::new(HashMap::new())));
        let writer = PoolRelayBroadcaster(Arc::new(RwLock::new(vec![])));
        let mut tasks = vec![];
        for relay_url in urls {
            if let Ok(relay) = NostrRelay::new(&relay_url).await {
                relay_states
                    .insert(relay_url.clone(), RelayState::Connected)
                    .await;
                let mut writer = writer.0.write().await;
                writer.push(relay.writer.clone());
                let future = Self::process_relay_events(
                    library.clone(),
                    relay_states.clone(),
                    relay,
                    event_tx.clone(),
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
            listener,
            writer,
            relay_states,
            library,
        })
    }
    async fn process_relay_events(
        notes: NoteLibrary,
        relay_table: RelayTable,
        mut relay: NostrRelay,
        event_writer: PoolRelaySender,
    ) -> anyhow::Result<()> {
        let mut reader = relay.relay_event_stream()?;

        loop {
            if let RelayState::Disconnected(e) = relay.state {
                error!("{:?}", e);
                relay_table
                    .insert(relay.url.url.to_string(), RelayState::Disconnected(e))
                    .await;
                break;
            }
            tokio::select! {
                Some(event) = reader.next() => {
                    match event {
                        RelayEvent::NewNote(NoteEvent(_, _, ref signed_note)) => {
                            if notes.insert(signed_note.clone()).await {
                                if let Err(e) = event_writer.send((relay.url.url.to_string(), event)) {
                                    error!("{:?}", e);
                                    drop(relay);
                                    break;
                                }
                            }
                        }
                        _ => {
                            if let Err(e) = event_writer.send((relay.url.url.to_string(), event)) {
                                error!("{:?}", e);
                                drop(relay);
                                break;
                            }
                        }
                    }
                }
                else => {
                    drop(relay);
                    break},
            }
        }
        Err(anyhow::anyhow!("Relay closed"))
    }
    pub fn close(mut self) -> anyhow::Result<()> {
        self.listener.close();
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::relays::{EndOfSubscriptionEvent, NostrSubscription};
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    //#[tokio::test]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_relay_pool() {
        let mut pool = NostrRelayPool::new(vec![
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
        pool.writer
            .subscribe(filter.clone())
            .await
            .expect("Failed to subscribe");
        let mut events = vec![];
        while let Some((_, event)) = pool.listener.recv().await {
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
