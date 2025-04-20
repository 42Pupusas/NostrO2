#[derive(Debug)]
pub enum NostrKeypairError {
    InvalidKey,
    Bech32DecodeError(bech32::DecodeError),
    Bech32EncodeError(bech32::EncodeError),
    HexDecodeError(hex::FromHexError),
    HrpParseError,
    Nip01Error(nostro2::errors::NostrErrors),
    Nip04Error(nips::Nip04Error),
    Nip44Error(nips::Nip44Error),
    Nip59Error(nips::Nip59Error),
    Secp256k1Error(secp256k1::Error),
    ConversionError(std::convert::Infallible),
    SharedSecretError,
    NotExtractable,
    Bip39Error(bip39::Error),
    StdErrors(Box<dyn std::error::Error>),
}
impl std::fmt::Display for NostrKeypairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey => write!(f, "Invalid key"),
            Self::Bech32DecodeError(err) => write!(f, "Bech32 decode error: {err}"),
            Self::Bech32EncodeError(err) => write!(f, "Bech32 encode error: {err}"),
            Self::HexDecodeError(err) => write!(f, "Hex decode error: {err}"),
            Self::HrpParseError => write!(f, "Invalid hrp"),
            Self::Nip04Error(err) => write!(f, "Nip04 error: {err}"),
            Self::Nip44Error(err) => write!(f, "Nip44 error: {err}"),
            Self::Secp256k1Error(err) => write!(f, "Secp256k1 error: {err}"),
            Self::ConversionError(err) => write!(f, "Conversion error: {err}"),
            Self::SharedSecretError => write!(f, "Shared secret error"),
            Self::NotExtractable => write!(f, "Keypair is not extractable"),
            Self::Bip39Error(err) => write!(f, "BIP39 error: {err}"),
            Self::Nip59Error(err) => write!(f, "Nip59 error: {err}"),
            Self::StdErrors(err) => write!(f, "Standard error: {err}"),
            Self::Nip01Error(err) => write!(f, "Nip01 error: {err}"),
        }
    }
}
impl std::error::Error for NostrKeypairError {}
impl From<nostro2::errors::NostrErrors> for NostrKeypairError {
    fn from(err: nostro2::errors::NostrErrors) -> Self {
        Self::Nip01Error(err)
    }
}
impl From<std::io::Error> for NostrKeypairError {
    fn from(err: std::io::Error) -> Self {
        Self::StdErrors(Box::new(err))
    }
}
impl From<Box<dyn std::error::Error>> for NostrKeypairError {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        Self::StdErrors(err)
    }
}
impl From<secp256k1::Error> for NostrKeypairError {
    fn from(err: secp256k1::Error) -> Self {
        Self::Secp256k1Error(err)
    }
}
impl From<bech32::DecodeError> for NostrKeypairError {
    fn from(err: bech32::DecodeError) -> Self {
        Self::Bech32DecodeError(err)
    }
}
impl From<bech32::EncodeError> for NostrKeypairError {
    fn from(err: bech32::EncodeError) -> Self {
        Self::Bech32EncodeError(err)
    }
}
impl From<hex::FromHexError> for NostrKeypairError {
    fn from(err: hex::FromHexError) -> Self {
        Self::HexDecodeError(err)
    }
}
impl From<nips::Nip04Error> for NostrKeypairError {
    fn from(err: nips::Nip04Error) -> Self {
        Self::Nip04Error(err)
    }
}
impl From<nips::Nip44Error> for NostrKeypairError {
    fn from(err: nips::Nip44Error) -> Self {
        Self::Nip44Error(err)
    }
}
impl From<nips::Nip59Error> for NostrKeypairError {
    fn from(err: nips::Nip59Error) -> Self {
        Self::Nip59Error(err)
    }
}
impl From<std::convert::Infallible> for NostrKeypairError {
    fn from(err: std::convert::Infallible) -> Self {
        Self::ConversionError(err)
    }
}
impl From<bip39::Error> for NostrKeypairError {
    fn from(err: bip39::Error) -> Self {
        Self::Bip39Error(err)
    }
}
impl From<NostrKeypairError> for nips::Nip04Error {
    fn from(err: NostrKeypairError) -> Self {
        Self::CustomError(err.to_string())
    }
}
impl From<NostrKeypairError> for nips::Nip44Error {
    fn from(err: NostrKeypairError) -> Self {
        Self::CustomError(err.to_string())
    }
}
