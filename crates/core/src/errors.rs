//! Error types for the nostro2 crate

#[derive(Debug)]
pub enum NostrErrors {
    JsonError(bourne::Error),
    MissingId,
    MissingSignature,
    MissingPubkey,
    InvalidPublicKey,
    InvalidSignature,
    Signer(nostro2_traits::SignerError),
}

impl std::fmt::Display for NostrErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JsonError(e) => write!(f, "{e}"),
            Self::MissingId => f.write_str("no id found on note"),
            Self::MissingSignature => f.write_str("no signature found on note"),
            Self::MissingPubkey => f.write_str("no pubkey found on note"),
            Self::InvalidPublicKey => f.write_str("invalid public key"),
            Self::InvalidSignature => f.write_str("invalid signature"),
            Self::Signer(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NostrErrors {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::JsonError(e) => Some(e),
            Self::Signer(e) => Some(e),
            _ => None,
        }
    }
}

impl From<bourne::Error> for NostrErrors {
    fn from(e: bourne::Error) -> Self {
        Self::JsonError(e)
    }
}

impl From<nostro2_traits::SignerError> for NostrErrors {
    fn from(e: nostro2_traits::SignerError) -> Self {
        Self::Signer(e)
    }
}
