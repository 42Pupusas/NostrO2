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

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_bourne_err() -> bourne::Error {
        bourne::Error::new(
            bourne::ErrorKind::UnexpectedEof,
            bourne::Position { offset: 0 },
        )
    }

    #[test]
    fn display_covers_all_variants() {
        let cases: Vec<NostrErrors> = vec![
            NostrErrors::JsonError(dummy_bourne_err()),
            NostrErrors::MissingId,
            NostrErrors::MissingSignature,
            NostrErrors::MissingPubkey,
            NostrErrors::InvalidPublicKey,
            NostrErrors::InvalidSignature,
            NostrErrors::Signer(nostro2_traits::SignerError::MissingId),
        ];
        for err in &cases {
            let msg = format!("{err}");
            assert!(!msg.is_empty(), "Display must produce non-empty output");
        }
    }

    #[test]
    fn source_delegates_correctly() {
        use std::error::Error;

        let json_err = NostrErrors::JsonError(dummy_bourne_err());
        assert!(json_err.source().is_some());

        let signer_err = NostrErrors::Signer(nostro2_traits::SignerError::MissingId);
        assert!(signer_err.source().is_some());

        assert!(NostrErrors::MissingId.source().is_none());
        assert!(NostrErrors::MissingSignature.source().is_none());
        assert!(NostrErrors::MissingPubkey.source().is_none());
        assert!(NostrErrors::InvalidPublicKey.source().is_none());
        assert!(NostrErrors::InvalidSignature.source().is_none());
    }
}
