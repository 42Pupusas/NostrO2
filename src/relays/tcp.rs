use futures_util::stream::{SplitSink, SplitStream};

#[cfg(not(target_arch = "wasm32"))]
pub use tokio::task::spawn as spawn_thread;
#[cfg(target_arch = "wasm32")]
pub use wasm_bindgen_futures::spawn_local as spawn_thread;

#[cfg(not(target_arch = "wasm32"))]
pub use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
#[cfg(target_arch = "wasm32")]
pub use tokio_tungstenite_wasm::Message as WebSocketMessage;

#[cfg(not(target_arch = "wasm32"))]
pub type NostrRelayStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
#[cfg(target_arch = "wasm32")]
pub type NostrRelayStream = tokio_tungstenite_wasm::WebSocketStream;

pub type NostrWebsocketReader = Option<SplitStream<NostrRelayStream>>;
pub type NostrWebsocketWriter = Option<SplitSink<NostrRelayStream, WebSocketMessage>>;

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
