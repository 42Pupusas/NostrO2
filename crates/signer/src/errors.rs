//! Error types for the nostro2-signer crate
//!
//! This module contains all error types that can be returned by keypair and signing operations.

/// Errors that can occur when working with Nostr keypairs and cryptographic operations.
///
/// `Display` is `transparent` for every wrapped error type — the user-facing
/// message is the leaf error's message, not "Nostr error: Signer error: …"
/// chained. The variant name is still useful for matching; `Debug` still
/// shows the chain.
#[derive(Debug, thiserror::Error)]
pub enum NostrKeypairError {
    #[error("invalid key")]
    InvalidKey,
    #[error(transparent)]
    Bech32DecodeError(#[from] bech32::DecodeError),
    #[error(transparent)]
    Bech32EncodeError(#[from] bech32::EncodeError),
    #[error(transparent)]
    HexDecodeError(#[from] hex::FromHexError),
    #[error("invalid hrp")]
    HrpParseError,
    #[error(transparent)]
    Nip01Error(#[from] nostro2::errors::NostrErrors),
    #[error(transparent)]
    Nip04Error(#[from] nostro2_nips::Nip04Error),
    #[error(transparent)]
    Nip44Error(#[from] nostro2_nips::Nip44Error),
    #[error(transparent)]
    Nip59Error(#[from] nostro2_nips::Nip59Error),
    #[cfg(feature = "k256")]
    #[error(transparent)]
    K256Error(#[from] k256::elliptic_curve::Error),
    #[cfg(feature = "secp256k1")]
    #[error(transparent)]
    Secp256k1Error(#[from] secp256k1::Error),
    #[error("shared secret error")]
    SharedSecretError,
    #[error("not extractable")]
    NotExtractable,
    #[error(transparent)]
    Bip39Error(#[from] bip39::Error),
}
