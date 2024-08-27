use std::fmt::Formatter;

#[derive(Debug)]
pub enum NostroError {
    DecryptionError(Box<dyn std::error::Error + Send>),
    EncryptionError(Box<dyn std::error::Error + Send>),
    DecodingError(Box<dyn std::error::Error + Send>),
    NsecError(Box<dyn std::error::Error + Send>),
    MnemonicError(Box<dyn std::error::Error + Send>),
    ConnectionError(Box<dyn std::error::Error + Send>),
    ParseError,
    SubscriptionError(String),
    SendError(String),
    ReadError(String),
    UnknownCommand,
}

impl std::error::Error for NostroError {
    fn description(&self) -> &str {
        match self {
            NostroError::DecryptionError(_) => "Failed to decrypt",
            NostroError::EncryptionError(_) => "Failed to encrypt",
            NostroError::DecodingError(_) => "Failed to decode",
            NostroError::NsecError(_) => "Failed to decode nsec",
            NostroError::MnemonicError(_) => "Failed to parse mnemonic",
            NostroError::UnknownCommand => "Unknown command",
            NostroError::ConnectionError(_) => "Could not connect to relay",
            NostroError::ParseError => "Could not parse message",
            NostroError::SubscriptionError(_) => "Could not subscribe",
            NostroError::SendError(_) => "Could not send note",
            NostroError::ReadError(_) => "Could not read message",
        }
    }
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match self {
            NostroError::DecryptionError(e) => Some(e.as_ref()),
            NostroError::EncryptionError(e) => Some(e.as_ref()),
            NostroError::DecodingError(e) => Some(e.as_ref()),
            NostroError::NsecError(e) => Some(e.as_ref()),
            NostroError::MnemonicError(e) => Some(e.as_ref()),
            NostroError::ConnectionError(e) => Some(e.as_ref()),
            _ => None,
        }
    }
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NostroError::DecryptionError(e) => Some(e.as_ref()),
            NostroError::EncryptionError(e) => Some(e.as_ref()),
            NostroError::DecodingError(e) => Some(e.as_ref()),
            NostroError::NsecError(e) => Some(e.as_ref()),
            NostroError::MnemonicError(e) => Some(e.as_ref()),
            NostroError::ConnectionError(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl std::fmt::Display for NostroError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            NostroError::DecryptionError(e) => write!(f, "Failed to decrypt: {}", e),
            NostroError::EncryptionError(e) => write!(f, "Failed to encrypt: {}", e),
            NostroError::DecodingError(e) => write!(f, "Failed to decode: {}", e),
            NostroError::NsecError(e) => write!(f, "Failed to decode nsec: {}", e),
            NostroError::MnemonicError(e) => write!(f, "Failed to parse mnemonic: {}", e),
            NostroError::UnknownCommand => write!(f, "Unknown command"),
            NostroError::ConnectionError(_) => write!(f, "Could not connect to relay"),
            NostroError::ParseError => write!(f, "Could not parse message"),
            NostroError::SubscriptionError(s) => write!(f, "Could not subscribe: {}", s),
            NostroError::SendError(s) => write!(f, "Could not send note: {}", s),
            NostroError::ReadError(s) => write!(f, "Could not read message: {}", s),
        }
    }
}
