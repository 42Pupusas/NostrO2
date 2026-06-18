#[derive(Debug)]
pub enum NostrRelayError {
    Tungstenite(Box<tokio_tungstenite::tungstenite::Error>),
    Serde(bourne::Error),
    TokioSend(Box<tokio::sync::broadcast::error::SendError<nostro2::NostrClientEvent>>),
    SendError,
}

impl std::fmt::Display for NostrRelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tungstenite(e) => write!(f, "tungstenite error: {e}"),
            Self::Serde(e) => write!(f, "serialization error: {e}"),
            Self::TokioSend(e) => write!(f, "broadcast send error: {e}"),
            Self::SendError => f.write_str("mpsc send error"),
        }
    }
}

impl std::error::Error for NostrRelayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Tungstenite(e) => Some(e.as_ref()),
            Self::Serde(e) => Some(e),
            Self::TokioSend(e) => Some(e.as_ref()),
            Self::SendError => None,
        }
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
