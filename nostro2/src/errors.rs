//! Error types for the nostro2 crate
//!
//! This module contains all error types that can be returned by nostro2 operations.

/// Errors that can occur when working with Nostr notes and protocol operations.
///
/// Wrapper variants (`SerdeError`, `Signer`) use `#[error(transparent)]` so
/// that `Display` shows the leaf error's message directly, rather than the
/// "Nostr error: Signer error: signing backend error: …" chain you get when
/// every layer prefixes itself. `Debug` still prints the full chain, and
/// `source()` still walks the error tree the standard way.
#[derive(Debug, thiserror::Error)]
pub enum NostrErrors {
    #[error(transparent)]
    JsonError(#[from] bourne::Error),
    #[error("no id found on note")]
    MissingId,
    #[error("no signature found on note")]
    MissingSignature,
    #[error("no pubkey found on note")]
    MissingPubkey,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error(transparent)]
    Signer(#[from] nostro2_traits::SignerError),
}
