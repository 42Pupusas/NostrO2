use crate::notes::SignedNote;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use url::Url;

use super::{CloseEvent, RelayEventTag, SendNoteEvent, SubscribeEvent};

#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
#[cfg(target_arch = "wasm32")]
use tokio_tungstenite_wasm::Message as WebSocketMessage;

#[cfg(not(target_arch = "wasm32"))]
pub type NostrWebsocketReader = SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;
#[cfg(target_arch = "wasm32")]
pub type NostrWebsocketReader = SplitStream<tokio_tungstenite_wasm::WebSocketStream>;

#[cfg(not(target_arch = "wasm32"))]
pub type NostrWebsocketWriter = SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WebSocketMessage,
>;
#[cfg(target_arch = "wasm32")]
pub type NostrWebsocketWriter =
    SplitSink<tokio_tungstenite_wasm::WebSocketStream, WebSocketMessage>;

pub struct NostrRelay {
    pub url: Url,
    pub writer: NostrWebsocketWriter,
    pub reader: NostrWebsocketReader,
}
impl NostrRelay {
    pub async fn new(relay_string: &str) -> anyhow::Result<Self> {
        let relay_url = Url::parse(relay_string)?;

        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) =
            tokio_tungstenite::connect_async(relay_url.to_string()).await?;
        #[cfg(target_arch = "wasm32")]
        let websocket = tokio_tungstenite_wasm::connect(relay_url.clone()).await?;

        let (websocket_writer, websocket_reader) = websocket.split();

        Ok(NostrRelay {
            url: relay_url,
            reader: websocket_reader,
            writer: websocket_writer,
        })
    }
    pub async fn subscribe(&mut self, filter: SubscribeEvent) -> anyhow::Result<String> {
        let id = filter.1.clone();
        self.writer
            .send(WebSocketMessage::Text(filter.into()))
            .await?;
        Ok(id)
    }
    pub async fn unsubscribe(&mut self, id: String) -> anyhow::Result<()> {
        let subscription: String = CloseEvent(RelayEventTag::CLOSE, id).into();
        let message = WebSocketMessage::Text(subscription);
        self.writer.send(message).await?;
        Ok(())
    }
    pub async fn send_note(&mut self, note: SignedNote) -> anyhow::Result<()> {
        let note: String = SendNoteEvent(RelayEventTag::EVENT, note).into();
        let message = WebSocketMessage::Text(note);
        self.writer.send(message).await?;
        Ok(())
    }
    pub async fn close(self) {
        drop(self.reader);
        drop(self.writer);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {

    use tracing::{debug, error};
    use tracing_test::traced_test;

    use crate::{
        notes::Note,
        relays::{EndOfSubscriptionEvent, NostrSubscription, NoteEvent, RelayEvent},
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
        debug!("Subscribed with id");

        let mut finished = String::new();
        while let Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::NewNote(NoteEvent(_, _, _))) => {
                    debug!("New note");
                }
                Ok(RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, id))) => {
                    debug!("End of subscription: {}", id);
                    finished = id;
                    break;
                }
                Err(e) => {
                    error!("{:?}", e);
                    // break;
                }
                _ => (),
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
        relay.send_note(signed_note.clone()).await?;
        relay.subscribe(filter).await?;
        let mut collected_notes = vec![];
        while let Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::NewNote(NoteEvent(_, _, signed_note))) => {
                    collected_notes.push(signed_note);
                }
                Ok(RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, _))) => {
                    break;
                }
                Err(e) => {
                    error!("{:?}", e);
                    // break;
                }
                _ => (),
            }
        }
        assert_eq!(collected_notes.len(), 1);
        assert_eq!(collected_notes[0].get_id(), note_id);
        Ok(())
    }
}
#[cfg(target_arch = "wasm32")]
#[cfg(test)]
mod tests {

    use crate::{
        notes::Note,
        relays::{EndOfSubscriptionEvent, NostrSubscription, NoteEvent, RelayEvent},
    };

    use wasm_bindgen_test::wasm_bindgen_test;
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    #[wasm_bindgen_test]
    async fn test_single_relay() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(3),
            ..Default::default()
        }
        .relay_subscription();
        let id = filter.1.clone();
        relay
            .writer
            .send(WebSocketMessage::Text(filter.into()))
            .await?;

        let mut finished = String::new();
        while let Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, id))) => {
                    finished = id;
                    break;
                }
                _ => (),
            }
        }
        assert_eq!(id, finished);
        Ok(())
    }
    #[wasm_bindgen_test]
    async fn test_relay_send_note() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
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
        relay.send_note(signed_note.clone()).await?;
        relay.subscribe(filter).await?;
        let mut collected_notes = vec![];
        while let Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::NewNote(NoteEvent(_, _, signed_note))) => {
                    collected_notes.push(signed_note);
                }
                Ok(RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, _))) => {
                    break;
                }
                Err(_e) => {
                    // break;
                }
                _ => (),
            }
        }
        assert_eq!(collected_notes.len(), 1);
        assert_eq!(collected_notes[0].get_id(), note_id);
        Ok(())
    }
}
