use super::relay_connection::WebsocketStatus;
use crate::{
    notes::NostrNote,
    relays::{NostrRelay, RelayEvent},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{
    select,
    sync::{
        broadcast::Sender,
        mpsc::{UnboundedReceiver, UnboundedSender},
        RwLock,
    },
};

pub type PoolRelayReceiver = UnboundedReceiver<(String, RelayEvent)>;
pub type PoolRelaySender = UnboundedSender<(String, RelayEvent)>;

pub type RelayTableMap = HashMap<String, WebsocketStatus>;
pub type NostrNoteLibrary = HashSet<NostrNote>;

#[derive(Clone)]
pub struct NoteLibrary(pub Arc<RwLock<NostrNoteLibrary>>);
impl NoteLibrary {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashSet::new())))
    }
    pub async fn insert(&self, note: NostrNote) -> bool {
        let mut library = self.0.write().await;
        library.insert(note)
    }
}

pub struct NostrRelayPool {
    pub relays: Vec<NostrRelay>,
    pub reader: PoolRelayReceiver,
    pub broadcaster: Sender<crate::relays::WebSocketMessage>,
}

impl NostrRelayPool {
    pub async fn new(urls: Vec<String>) -> anyhow::Result<Self> {
        let library = NoteLibrary::new();
        let relays = urls
            .into_iter()
            .filter_map(|url| NostrRelay::new(&url).ok())
            .collect::<Vec<_>>();
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);

        let broadcast_tx_clone = broadcast_tx.clone();
        let relay_tasks = relays
            .iter()
            .map(move |relay| {
                Box::pin(NostrRelayPool::process_relay_events(
                    library.clone(),
                    relay.clone(),
                    in_tx.clone(),
                    broadcast_tx_clone.subscribe(),
                ))
            })
            .collect::<Vec<_>>();
        crate::relays::spawn_thread(async move {
            let _ = futures_util::future::select_ok(relay_tasks).await;
        });
        Ok(Self {
            relays,
            reader: in_rx,
            broadcaster: broadcast_tx,
        })
    }
    async fn process_relay_events(
        notes: NoteLibrary,
        relay: NostrRelay,
        event_writer: PoolRelaySender,
        mut broadcast_rx: tokio::sync::broadcast::Receiver<crate::relays::WebSocketMessage>,
    ) -> anyhow::Result<()> {
        loop {
            if let WebsocketStatus::Closed(e) = relay.state().await {
                tracing::error!("Relay disconnected: {}", e);
                break;
            }
            select! {
                event = relay.next_relay_event() => {
                    match event {
                        Some(event) => {
                            match event {
                                RelayEvent::NewNote((_, _, ref note)) => {
                                    if notes.insert(note.clone()).await {
                                        if let Err(e) = event_writer.send((relay.url.clone(), event)) {
                                            tracing::error!("Failed to send event: {:?}", e);
                                            break;
                                        }
                                    }
                                }
                                _ => {
                                    if let Err(e) = event_writer.send((relay.url.clone(), event)) {
                                        tracing::error!("Failed to send event: {:?}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }
                note = broadcast_rx.recv() => {
                    if let Ok(note) = note {
                        if let Err(e) = relay.send_to_relay(note).await {
                            tracing::error!("Failed to send note to relay {}: {:?}", relay.url, e);
                            break;
                        }
                    }
                }
                else => {
                    break;
                }
            }
        }
        relay.close().await;
        Err(anyhow::anyhow!("Relay closed"))
    }
    pub async fn send_to_relay(
        &self,
        signed_note: crate::relays::WebSocketMessage,
    ) -> anyhow::Result<()> {
        if let Err(e) = self.broadcaster.send(signed_note) {
            tracing::error!("Failed to send note to relay pool: {:?}", e);
        }
        Ok(())
    }
    pub async fn close(mut self) -> anyhow::Result<()> {
        for relay in &self.relays {
            relay.clone().close().await;
        }
        self.reader.close();
        drop(self);
        Ok(())
    }
}

impl Drop for NostrRelayPool {
    fn drop(&mut self) {
        // Ensure all resources are cleaned up
        self.reader.close();
        for relay in &self.relays {
            let relay = relay.clone();
            crate::relays::spawn_thread(async move {
                relay.close().await;
            });
        }
    }
}
#[cfg(test)]
mod tests {

    use super::*;
    use crate::relays::{NostrSubscription, SubscribeEvent};
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    //#[tokio::test]
    //#[tracing_test::traced_test]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _relay_pool_stress() {
        // let time = tokio::time::Instant::now();
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
        //println!("Time to create pool: {:?}", time.elapsed());
        let filter: SubscribeEvent = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(5000),
            ..Default::default()
        }
        .into();
        pool.send_to_relay(filter.into())
            .await
            .expect("Failed to subscribe");
        let mut events = vec![];
        //println!("Time to subscribe: {:?}", time.elapsed());
        while let Some((_, event)) = pool.reader.recv().await {
            tracing::info!("Received event: {:?}", event);
            if let RelayEvent::NewNote((_, _, _)) = event {
                tracing::info!("Received note");
                events.push(event);
                if events.len() == 1 {
                    //          println!("Time to get first event: {:?}", time.elapsed());
                }
                if events.len() == 1000 {
                    //        println!("Time to get 1000 events: {:?}", time.elapsed());
                    wasm_bindgen_test::console_log!("Events: {:?}", events.len());
                }
                if events.len() == 5000 {
                    //      println!("Time to get 5000 events: {:?}", time.elapsed());
                    wasm_bindgen_test::console_log!("Events: {:?}", events.len());
                    break;
                }
            }
        }
        // println!("Time to get all events: {:?}", time.elapsed());
        assert_eq!(events.len(), 5000);
        pool.close().await.expect("Failed to close pool");
        wasm_bindgen_test::console_log!("Pool closed");
    }
    //#[tokio::test]
    //#[tracing_test::traced_test]
    // #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_relay_pool() {
        tracing::info!("Starting test");
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
        tracing::info!("Pool created");
        let filter: SubscribeEvent = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(10),
            ..Default::default()
        }
        .into();
        pool.send_to_relay(filter.into())
            .await
            .expect("Failed to subscribe");
        tracing::info!("Subscribed");
        let mut events = vec![];
        pool.send_to_relay(NostrNote::default().into())
            .await
            .expect("Failed to subscribe");
        loop {
            if pool.reader.is_closed() {
                println!("Closed");
                break;
            }
            match pool.reader.recv().await {
                Some((relay_url, event)) => {
                    if let RelayEvent::EndOfSubscription((_, ref subscription_id)) = event {
                        events.push(subscription_id.clone());
                        tracing::info!("Events: {:?}", events.len());
                        tracing::info!("End of subscription: {}", subscription_id);
                        if events.len() > 3 {
                            wasm_bindgen_test::console_log!("Events: {:?}", events.len());
                            break;
                        }
                    }
                    if let RelayEvent::NewNote((_, _, ref note)) = event {
                        tracing::info!("Received note: {:?} from {}", note.id, relay_url);
                    }
                }
                None => {
                    println!("No events");
                }
            }
        }
        assert!(events.len() >= 3);
        pool.close().await.expect("Failed to close pool");
    }
}
