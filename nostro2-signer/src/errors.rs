#[derive(Debug, thiserror::Error)]
pub enum NostrKeypairError {
    #[error("Invalid key")]
    InvalidKey,
    #[error("Bech32 decode error {0}")]
    Bech32DecodeError(#[from] bech32::DecodeError),
    #[error("Bech32 encode error {0}")]
    Bech32EncodeError(#[from] bech32::EncodeError),
    #[error("Hex decode error {0}")]
    HexDecodeError(#[from] hex::FromHexError),
    #[error("Invalid hrp")]
    HrpParseError,
    #[error("Nostr error {0}")]
    Nip01Error(#[from] nostro2::errors::NostrErrors),
    #[error("Nip04 error {0}")]
    Nip04Error(#[from] nostro2_nips::Nip04Error),
    #[error("Nip44 error {0}")]
    Nip44Error(#[from] nostro2_nips::Nip44Error),
    #[error("Nip59 error {0}")]
    Nip59Error(#[from] nostro2_nips::Nip59Error),
    #[error("Secp256k1 error {0}")]
    Secp256k1Error(#[from] secp256k1::Error),
    #[error("Conversion error {0}")]
    ConversionError(#[from] std::convert::Infallible),
    #[error("Shared secret error")]
    SharedSecretError,
    #[error("Not extractable")]
    NotExtractable,
    #[error("BIP39 error {0}")]
    Bip39Error(#[from] bip39::Error),
}
