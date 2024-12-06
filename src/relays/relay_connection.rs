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

#[cfg(test)]
mod tests {

    fn _debug(s: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            println!("{}", s);
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_test::console_log!("{}", s);
        }
    }
    fn _error(s: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            eprintln!("{}", s);
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_test::console_error!("{}", s);
        }
    }
    // use wasm_bindgen_test::wasm_bindgen_test;

    use crate::{
        notes::Note,
        relays::{EndOfSubscriptionEvent, NostrSubscription, NoteEvent, OkEvent, RelayEvent},
    };

    #[tokio::test]
    //#[wasm_bindgen_test]
    async fn _test_single_relay() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(3),
            ..Default::default()
        }
        .relay_subscription();
        let id = relay.subscribe(filter).await?;
        _debug("Subscribed with id");

        let mut finished = String::new();
        while let Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::NewNote(NoteEvent(_, _, _))) => {
                    _debug("New note");
                }
                Ok(RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, id))) => {
                    _debug(&format!("End of subscription: {}", id));
                    finished = id;
                    break;
                }
                Err(e) => {
                    _error(&format!("{:?}", e));
                    // break;
                }
                _ => (),
            }
        }
        assert_eq!(id, finished);
        Ok(())
    }
    #[tokio::test]
    //#[wasm_bindgen_test]
    async fn _test_relay_send_note() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        _debug(relay.url.as_str());
        let user_keys = crate::userkeys::UserKeys::generate();
        let note = Note::new(&user_keys.get_public_key(), 1, "Hello, world!");
        let signed_note = user_keys.sign_nostr_event(note);
        relay.send_note(signed_note.clone()).await?;
        let mut sent = false;
        while let Some(Ok(WebSocketMessage::Text(event))) = relay.reader.next().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::SentOk(OkEvent(_, _, did_sent, _))) => {
                    _debug(&format!("Sent Ok: {}", did_sent));
                    sent = did_sent;
                    break;
                }
                Err(e) => {
                    _error(&format!("{:?}", e));
                    // break;
                }
                _ => (),
            }
        }
        assert!(sent);
        Ok(())
    }
}
