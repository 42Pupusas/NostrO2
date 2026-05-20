//! Error types for the nostro2-signer crate

#[derive(Debug)]
pub enum NostrKeypairError {
    InvalidKey,
    Bech32DecodeError(bech32::DecodeError),
    Bech32EncodeError(bech32::EncodeError),
    HexDecodeError(nostro2_traits::hex::HexError),
    HrpParseError,
    Nip01Error(nostro2::errors::NostrErrors),
    Nip44Error(nostro2_nips::Nip44Error),
    Nip59Error(nostro2_nips::Nip59Error),
    #[cfg(feature = "k256")]
    K256Error(k256::elliptic_curve::Error),
    #[cfg(feature = "secp256k1")]
    Secp256k1Error(secp256k1::Error),
    SharedSecretError,
    NotExtractable,
    Bip39Error(xinachtli::Error),
}

impl std::fmt::Display for NostrKeypairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey => f.write_str("invalid key"),
            Self::Bech32DecodeError(e) => write!(f, "{e}"),
            Self::Bech32EncodeError(e) => write!(f, "{e}"),
            Self::HexDecodeError(e) => write!(f, "{e}"),
            Self::HrpParseError => f.write_str("invalid hrp"),
            Self::Nip01Error(e) => write!(f, "{e}"),
            Self::Nip44Error(e) => write!(f, "{e}"),
            Self::Nip59Error(e) => write!(f, "{e}"),
            #[cfg(feature = "k256")]
            Self::K256Error(e) => write!(f, "{e}"),
            #[cfg(feature = "secp256k1")]
            Self::Secp256k1Error(e) => write!(f, "{e}"),
            Self::SharedSecretError => f.write_str("shared secret error"),
            Self::NotExtractable => f.write_str("not extractable"),
            Self::Bip39Error(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NostrKeypairError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bech32DecodeError(e) => Some(e),
            Self::Bech32EncodeError(e) => Some(e),
            Self::HexDecodeError(e) => Some(e),
            Self::Nip01Error(e) => Some(e),
            Self::Nip44Error(e) => Some(e),
            Self::Nip59Error(e) => Some(e),
            #[cfg(feature = "k256")]
            Self::K256Error(e) => Some(e),
            #[cfg(feature = "secp256k1")]
            Self::Secp256k1Error(e) => Some(e),
            Self::Bip39Error(e) => Some(e),
            _ => None,
        }
    }
}

impl From<bech32::DecodeError> for NostrKeypairError {
    fn from(e: bech32::DecodeError) -> Self { Self::Bech32DecodeError(e) }
}
impl From<bech32::EncodeError> for NostrKeypairError {
    fn from(e: bech32::EncodeError) -> Self { Self::Bech32EncodeError(e) }
}
impl From<nostro2_traits::hex::HexError> for NostrKeypairError {
    fn from(e: nostro2_traits::hex::HexError) -> Self { Self::HexDecodeError(e) }
}
impl From<nostro2::errors::NostrErrors> for NostrKeypairError {
    fn from(e: nostro2::errors::NostrErrors) -> Self { Self::Nip01Error(e) }
}
impl From<nostro2_nips::Nip44Error> for NostrKeypairError {
    fn from(e: nostro2_nips::Nip44Error) -> Self { Self::Nip44Error(e) }
}
impl From<nostro2_nips::Nip59Error> for NostrKeypairError {
    fn from(e: nostro2_nips::Nip59Error) -> Self { Self::Nip59Error(e) }
}
#[cfg(feature = "k256")]
impl From<k256::elliptic_curve::Error> for NostrKeypairError {
    fn from(e: k256::elliptic_curve::Error) -> Self { Self::K256Error(e) }
}
#[cfg(feature = "secp256k1")]
impl From<secp256k1::Error> for NostrKeypairError {
    fn from(e: secp256k1::Error) -> Self { Self::Secp256k1Error(e) }
}
impl From<xinachtli::Error> for NostrKeypairError {
    fn from(e: xinachtli::Error) -> Self { Self::Bip39Error(e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_covers_all_variants() {
        let cases: Vec<NostrKeypairError> = vec![
            NostrKeypairError::InvalidKey,
            NostrKeypairError::HrpParseError,
            NostrKeypairError::SharedSecretError,
            NostrKeypairError::NotExtractable,
            NostrKeypairError::HexDecodeError(nostro2_traits::hex::HexError::OddLength),
            NostrKeypairError::Nip01Error(nostro2::errors::NostrErrors::MissingId),
            NostrKeypairError::Nip44Error(nostro2_nips::Nip44Error::SharedSecretError),
            NostrKeypairError::Nip59Error(nostro2_nips::Nip59Error::SigningError),
            NostrKeypairError::Bip39Error(xinachtli::Error::InvalidChecksum),
        ];
        for err in &cases {
            let msg = format!("{err}");
            assert!(!msg.is_empty(), "Display must produce non-empty output for {err:?}");
        }
    }

    #[test]
    fn source_delegates_correctly() {
        use std::error::Error;

        assert!(NostrKeypairError::InvalidKey.source().is_none());
        assert!(NostrKeypairError::HrpParseError.source().is_none());
        assert!(NostrKeypairError::SharedSecretError.source().is_none());
        assert!(NostrKeypairError::NotExtractable.source().is_none());

        let hex_err = NostrKeypairError::HexDecodeError(nostro2_traits::hex::HexError::OddLength);
        assert!(hex_err.source().is_some());

        let nip01_err = NostrKeypairError::Nip01Error(nostro2::errors::NostrErrors::MissingId);
        assert!(nip01_err.source().is_some());

        let nip44_err = NostrKeypairError::Nip44Error(nostro2_nips::Nip44Error::SharedSecretError);
        assert!(nip44_err.source().is_some());

        let nip59_err = NostrKeypairError::Nip59Error(nostro2_nips::Nip59Error::SigningError);
        assert!(nip59_err.source().is_some());

        let bip39_err = NostrKeypairError::Bip39Error(xinachtli::Error::InvalidChecksum);
        assert!(bip39_err.source().is_some());
    }
}
