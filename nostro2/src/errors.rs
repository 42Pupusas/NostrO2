#[derive(Debug)]
pub enum NostrErrors {
    StdError(Box<dyn std::error::Error + 'static>),
    NotFound(Box<dyn std::error::Error + 'static>),
    Bech32Error(Box<dyn std::error::Error + 'static>),
    SecpError(secp256k1::Error),
    SerdeError(serde_json::Error),
    IoError(std::io::Error),
    SignatureError(String),
}
impl From<Box<dyn std::error::Error>> for NostrErrors {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Self::StdError(e)
    }
}
impl From<std::io::Error> for NostrErrors {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}
impl From<&'static str> for NostrErrors {
    fn from(e: &'static str) -> Self {
        Self::NotFound(e.into())
    }
}
impl From<secp256k1::Error> for NostrErrors {
    fn from(e: secp256k1::Error) -> Self {
        Self::SecpError(e)
    }
}
impl From<serde_json::Error> for NostrErrors {
    fn from(e: serde_json::Error) -> Self {
        Self::SerdeError(e)
    }
}
impl From<bech32::EncodeIoError> for NostrErrors {
    fn from(e: bech32::EncodeIoError) -> Self {
        Self::Bech32Error(e.into())
    }
}
impl From<bech32::DecodeError> for NostrErrors {
    fn from(e: bech32::DecodeError) -> Self {
        Self::Bech32Error(e.into())
    }
}
impl From<bech32::EncodeError> for NostrErrors {
    fn from(e: bech32::EncodeError) -> Self {
        Self::Bech32Error(e.into())
    }
}
impl From<bech32::primitives::hrp::Error> for NostrErrors {
    fn from(e: bech32::primitives::hrp::Error) -> Self {
        Self::Bech32Error(e.into())
    }
}

impl core::error::Error for NostrErrors {}
impl core::fmt::Display for NostrErrors {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "NostrErrors: {self:#?}")
    }
}
