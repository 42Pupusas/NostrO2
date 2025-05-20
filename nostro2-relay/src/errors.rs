#[derive(Debug)]
pub enum NostrRelayError {
    Tungstenite(Box<tokio_tungstenite::tungstenite::Error>),
    Serde(serde_json::Error),
    TokioSend(Box<tokio::sync::broadcast::error::SendError<nostro2::NostrClientEvent>>),
}
impl std::fmt::Display for NostrRelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NostrRelayError: {self:#?}")
    }
}
impl From<tokio_tungstenite::tungstenite::Error> for NostrRelayError {
    fn from(value: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Tungstenite(Box::new(value))
    }
}
impl From<tokio::sync::broadcast::error::SendError<nostro2::NostrClientEvent>> for NostrRelayError {
    fn from(value: tokio::sync::broadcast::error::SendError<nostro2::NostrClientEvent>) -> Self {
        Self::TokioSend(Box::new(value))
    }
}


impl std::error::Error for NostrRelayError {}
