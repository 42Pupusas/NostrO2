/// Why a relay operation failed.
#[derive(Debug)]
pub enum NostrRelayError {
    /// The relay address is not a valid WebSocket URL.
    Url(crate::url::RelayUrlError),
    /// The first connection attempt did not succeed.
    Connect(String),
    /// TLS setup failed.
    Tls(crate::tls::RelayTlsError),
    /// A message did not serialize.
    #[cfg(feature = "bourne")]
    Serde(json_bourne::Error),
    /// A message did not serialize.
    #[cfg(feature = "serde")]
    Serde(serde_json::Error),
    /// The outbound ring is full, or the connection has stopped.
    SendError,
}

impl std::fmt::Display for NostrRelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(e) => write!(f, "invalid relay url: {e}"),
            Self::Connect(reason) => write!(f, "could not connect: {reason}"),
            Self::Tls(e) => write!(f, "{e}"),
            Self::Serde(e) => write!(f, "serialization error: {e}"),
            Self::SendError => f.write_str("the relay connection is not accepting messages"),
        }
    }
}

impl std::error::Error for NostrRelayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Url(e) => Some(e),
            Self::Tls(e) => Some(e),
            Self::Serde(e) => Some(e),
            Self::Connect(_) | Self::SendError => None,
        }
    }
}

impl From<crate::url::RelayUrlError> for NostrRelayError {
    fn from(value: crate::url::RelayUrlError) -> Self {
        Self::Url(value)
    }
}

impl From<crate::tls::RelayTlsError> for NostrRelayError {
    fn from(value: crate::tls::RelayTlsError) -> Self {
        Self::Tls(value)
    }
}

#[cfg(feature = "bourne")]
impl From<json_bourne::Error> for NostrRelayError {
    fn from(value: json_bourne::Error) -> Self {
        Self::Serde(value)
    }
}

#[cfg(feature = "serde")]
impl From<serde_json::Error> for NostrRelayError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}
