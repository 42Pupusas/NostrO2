use std::sync::Arc;

use crate::notes::NostrNote;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use tokio::sync::RwLock;
use tracing::error;

use super::{CloseEvent, RelayEvent, RelayEventTag, SendNoteEvent, SubscribeEvent};

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

pub struct Url {
    pub url: String,
}
impl Url {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        if url.starts_with("wss://") {
            Ok(Url {
                url: url.to_string(),
            })
        } else {
            Err(anyhow::anyhow!("Invalid url, must start with wss://"))
        }
    }
}

#[derive(Clone)]
pub enum RelayState {
    Connected,
    Disconnected(String),
}

#[derive(Clone)]
pub struct NostrWriter(pub Arc<RwLock<NostrWebsocketWriter>>);
impl NostrWriter {
    pub async fn send(&mut self, message: WebSocketMessage) -> anyhow::Result<()> {
        self.0.write().await.send(message).await?;
        Ok(())
    }
    pub async fn subscribe(&mut self, filter: SubscribeEvent) -> anyhow::Result<String> {
        let id = filter.1.clone();
        self.send(WebSocketMessage::Text(filter.into())).await?;
        Ok(id)
    }
    pub async fn unsubscribe(&mut self, id: String) -> anyhow::Result<()> {
        let subscription: String = CloseEvent(RelayEventTag::CLOSE, id).into();
        let message = WebSocketMessage::Text(subscription);
        self.send(message).await?;
        Ok(())
    }
    pub async fn send_note(&mut self, note: NostrNote) -> anyhow::Result<()> {
        let note: String = SendNoteEvent(RelayEventTag::EVENT, note).into();
        let message = WebSocketMessage::Text(note);
        self.send(message).await?;
        Ok(())
    }
}

pub struct NostrRelay {
    pub url: Url,
    pub state: RelayState,
    pub writer: NostrWriter,
    pub reader: Option<NostrWebsocketReader>,
}
impl NostrRelay {
    pub async fn new(relay_string: &str) -> anyhow::Result<Self> {
        let relay_url = Url::new(relay_string)?;

        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) =
            tokio_tungstenite::connect_async(relay_url.url.to_string()).await?;
        #[cfg(target_arch = "wasm32")]
        let websocket = tokio_tungstenite_wasm::connect(relay_url.url.to_string()).await?;

        let (websocket_writer, websocket_reader) = websocket.split();

        Ok(NostrRelay {
            url: relay_url,
            state: RelayState::Connected,
            reader: Some(websocket_reader),
            writer: NostrWriter(Arc::new(RwLock::new(websocket_writer))),
        })
    }
    async fn parse_event(ws_message: WebSocketMessage) -> Option<RelayEvent> {
        match ws_message {
            WebSocketMessage::Text(text) => RelayEvent::try_from(text).ok(),
            WebSocketMessage::Close(e) => RelayEvent::Close(e.unwrap().to_string()).into(),
            _ => RelayEvent::Ping.into(),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn relay_event_stream(
        &mut self,
    ) -> anyhow::Result<impl futures_util::stream::Stream<Item = RelayEvent>> {
        let reader = self.reader.take().ok_or(anyhow::anyhow!("Reader was taken already"))?;
        Ok(reader
            .filter_map(|message| async {
                match message {
                    Ok(message) => Self::parse_event(message).await,
                    Err(e) => {
                        error!("{:?}", e);
                        None
                    }
                }
            })
            .boxed())
    }
    #[cfg(target_arch = "wasm32")]
    pub fn relay_event_stream(
        &mut self,
    ) -> anyhow::Result<impl futures_util::stream::Stream<Item = RelayEvent>> {
        let reader = self.reader.take().ok_or(anyhow::anyhow!("Reader was taken already"))?;
        Ok(reader
            .filter_map(|message| async {
                match message {
                    Ok(message) => Self::parse_event(message).await,
                    Err(e) => {
                        error!("{:?}", e);
                        None
                    }
                }
            })
            .boxed_local())
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

    use crate::relays::{EndOfSubscriptionEvent, NostrSubscription, OkEvent};

    //#[tokio::test]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_single_relay() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        let filter = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(3),
            ..Default::default()
        }
        .relay_subscription();
        let id = relay.writer.subscribe(filter).await?;
        _debug("Subscribed with id");

        let mut finished = String::new();
        let mut ws_stream = relay.relay_event_stream()?;
        while let Some(event) = ws_stream.next().await {
            match event {
                RelayEvent::EndOfSubscription(EndOfSubscriptionEvent(_, id)) => {
                    _debug(&format!("End of subscription: {}", id));
                    finished = id;
                    break;
                }
                _ => (),
            }
        }
        assert_eq!(id, finished);
        Ok(())
    }
    //#[tokio::test]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_relay_send_note() -> Result<(), anyhow::Error> {
        use super::*;
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com").await?;
        _debug(relay.url.url.as_str());
        let user_keys = crate::keypair::NostrKeypair::generate(false);
        let mut note = NostrNote {
            pubkey: user_keys.public_key(),
            content: "Hello, world!".to_string(),
            ..Default::default()
        };
        user_keys.sign_nostr_event(&mut note);
        relay.writer.send_note(note).await?;
        let mut sent = false;
        while let Some(event) = relay.relay_event_stream()?.next().await {
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
