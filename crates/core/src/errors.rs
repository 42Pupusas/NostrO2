//! Error types for the nostro2 crate

#[derive(Debug)]
pub enum NostrErrors {
    #[cfg(feature = "bourne")]
    JsonError(json_bourne::Error),
    #[cfg(feature = "serde")]
    JsonError(serde_json::Error),
    MissingId,
    MissingSignature,
    MissingPubkey,
    InvalidPublicKey,
    InvalidSignature,
    Signer(nostro2_traits::SignerError),
    #[cfg(feature = "k256")]
    Ecdsa(k256::ecdsa::Error),
    #[cfg(feature = "secp256k1")]
    Ecdsa(secp256k1::Error),
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
            #[cfg(any(feature = "k256", feature = "secp256k1"))]
            Self::Ecdsa(e) => write!(f, "{e}"),
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

#[cfg(feature = "bourne")]
impl From<json_bourne::Error> for NostrErrors {
    fn from(e: json_bourne::Error) -> Self {
        Self::JsonError(e)
    }
}

#[cfg(feature = "serde")]
impl From<serde_json::Error> for NostrErrors {
    fn from(e: serde_json::Error) -> Self {
        Self::JsonError(e)
    }
}

impl From<nostro2_traits::SignerError> for NostrErrors {
    fn from(e: nostro2_traits::SignerError) -> Self {
        Self::Signer(e)
    }
}

#[cfg(feature = "k256")]
impl From<k256::ecdsa::Error> for NostrErrors {
    fn from(e: k256::ecdsa::Error) -> Self {
        Self::Ecdsa(e)
    }
}

#[cfg(feature = "secp256k1")]
impl From<secp256k1::Error> for NostrErrors {
    fn from(e: secp256k1::Error) -> Self {
        Self::Ecdsa(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "bourne")]
    fn dummy_json_err() -> json_bourne::Error {
        json_bourne::Error::new(
            json_bourne::ErrorKind::UnexpectedEof,
            json_bourne::Position { offset: 0 },
        )
    }
    #[cfg(feature = "serde")]
    fn dummy_json_err() -> serde_json::Error {
        serde_json::from_str::<()>("!!!").unwrap_err()
    }

    #[test]
    fn display_covers_all_variants() {
        let cases: Vec<NostrErrors> = vec![
            NostrErrors::JsonError(dummy_json_err()),
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

        let json_err = NostrErrors::JsonError(dummy_json_err());
        assert!(json_err.source().is_some());

        let signer_err = NostrErrors::Signer(nostro2_traits::SignerError::MissingId);
        assert!(signer_err.source().is_some());

        assert!(NostrErrors::MissingId.source().is_none());
        assert!(NostrErrors::MissingSignature.source().is_none());
        assert!(NostrErrors::MissingPubkey.source().is_none());
        assert!(NostrErrors::InvalidPublicKey.source().is_none());
        assert!(NostrErrors::InvalidSignature.source().is_none());
    }

    #[cfg(feature = "k256")]
    #[test]
    fn ecdsa_variant_display_and_source() {
        use std::error::Error;
        let err = NostrErrors::Ecdsa(k256::ecdsa::Error::new());
        assert!(!format!("{err}").is_empty());
        assert!(err.source().is_none());
    }
}
