use crate::notes::SignedNote;
use futures_util::{SinkExt, StreamExt};

use tracing::{error, warn};
use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use tokio::spawn as new_thread;
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;


#[cfg(target_arch = "wasm32")]
use tokio_tungstenite_wasm::Message as WebSocketMessage;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local as new_thread;

use super::{CloseEvent, RelayEvent, RelayEventTag, SendNoteEvent, SubscribeEvent};

#[derive(Debug)]
pub struct NostrRelay {
    pub url: Url,
    pub reader: tokio::sync::mpsc::UnboundedReceiver<RelayEvent>,
    pub writer: tokio::sync::mpsc::UnboundedSender<WebSocketMessage>,
}

impl NostrRelay {
    pub async fn new(relay_string: &str) -> anyhow::Result<Self> {
        let relay_url = Url::parse(relay_string)?;

        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) =
            tokio_tungstenite::connect_async(relay_url.to_string()).await?;

        #[cfg(target_arch = "wasm32")]
        let websocket = tokio_tungstenite_wasm::connect(relay_url.clone()).await?;

        let (mut websocket_writer, mut websocket_reader) = websocket.split();

        let (writer, mut writer_rx) = tokio::sync::mpsc::unbounded_channel::<WebSocketMessage>();
        let (reader_tx, reader) = tokio::sync::mpsc::unbounded_channel::<RelayEvent>();
        new_thread(async move {
            loop {
                tokio::select! {
                    reader = websocket_reader.next() => {
                        match reader {
                            None => break,
                            Some(Err(e)) => {
                                error!("{:?}", e);
                                break
                            },
                            Some(Ok(WebSocketMessage::Text(message))) => {
                                let message = message.to_string();
                                match RelayEvent::try_from(message) {
                                    Ok(event) => {
                                    let _ = reader_tx.send(event);
                                    },
                                    Err(e) => {
                                        error!("{:?}", e);
                                        break;
                                    }
                                }
                            },
                            Some(Ok(_)) => (),
                        }
                    },
                    writer = writer_rx.recv() => {
                        match writer {
                            Some(message) => {
                                if let Err(e) = websocket_writer.send(message).await {
                                    error!("{:?}", e);
                                    break;
                                }
                            },
                            None => break,
                        }
                    }
                    else => break,
                }
            }
            let _ = websocket_writer.close();
            drop(websocket_writer);
            warn!("Relay connection closed");
        });
        Ok(NostrRelay {
            url: relay_url,
            reader,
            writer,
        })
    }
    pub async fn subscribe(&self, filter: SubscribeEvent) -> anyhow::Result<String> {
        let id = filter.1.clone();
        self.writer.send(WebSocketMessage::Text(filter.into()))?;
        Ok(id)
    }
    pub async fn unsubscribe(&self, id: String) -> anyhow::Result<()> {
        let subscription: String = CloseEvent(RelayEventTag::CLOSE, id).into();
        let message = WebSocketMessage::Text(subscription);
        self.writer.send(message)?;
        Ok(())
    }
    pub async fn send_note(&self, note: SignedNote) -> anyhow::Result<()> {
        let note: String = SendNoteEvent(RelayEventTag::EVENT, note).into();
        let message = WebSocketMessage::Text(note.to_string());
        self.writer.send(message)?;
        Ok(())
    }
    pub async fn close(mut self) {
        self.reader.close();
        drop(self.writer);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {

    use tracing::debug;
    use tracing_test::traced_test;

    use crate::{
        notes::Note,
        relays::{EndOfSubscriptionEvent, NostrSubscription, NoteEvent},
    };

    #[tokio::test]
    #[traced_test]
    async fn test_single_relay() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        debug!("{:?}", relay.url);
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(3),
            ..Default::default()
        }
        .relay_subscription();
        let id = relay.subscribe(filter).await?;
        debug!("Subscribed with id: {}", id);

        let mut finished = String::new();
        loop {
            if relay.reader.is_closed() {
                error!("Reader is closed");
                break;
            }
            if let Some(event) = relay.reader.recv().await {
                match event {
                    RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, id)) => {
                        debug!("End of subscription: {}", id);
                        finished = id;
                        break;
                    }
                    _ => (),
                }
            }
        }
        assert_eq!(id, finished);
        Ok(())
    }
    #[tokio::test]
    #[traced_test]
    async fn test_relay_send_note() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        debug!("{:?}", relay.url);
        let user_keys = crate::userkeys::UserKeys::generate();
        let note = Note::new(&user_keys.get_public_key(), 1, "Hello, world!");
        let signed_note = user_keys.sign_nostr_event(note);
        let note_id = signed_note.get_id();
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            authors: Some(vec![user_keys.get_public_key()]),
            limit: Some(1),
            ..Default::default()
        }
        .relay_subscription();
        relay.send_note(signed_note).await?;
        relay.subscribe(filter).await?;
        let mut collected_notes = vec![];
        loop {
            if relay.reader.is_closed() {
                error!("Reader is closed");
                break;
            }
            if let Some(event) = relay.reader.recv().await {
                match event {
                    RelayEvent::NewNote(NoteEvent(_, _, signed_note)) => {
                        collected_notes.push(signed_note);
                    }
                    RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, _)) => {
                        break;
                    }
                    _ => (),
                }
            }
        }
        assert_eq!(collected_notes.len(), 1);
        assert_eq!(collected_notes[0].get_id(), note_id);
        Ok(())
    }
}
