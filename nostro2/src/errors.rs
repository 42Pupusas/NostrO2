//! Error types for the nostro2 crate
//!
//! This module contains all error types that can be returned by nostro2 operations.

/// Errors that can occur when working with Nostr notes and protocol operations
#[derive(Debug, thiserror::Error)]
pub enum NostrErrors {
    #[error("Serde error: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("No ID found on note")]
    MissingId,
    #[error("No signature found on note")]
    MissingSignature,
    #[error("No pubkey found on note")]
    MissingPubkey,
    #[error("Invalid public key")]
    InvalidPublicKey,
    #[error("Invalid signature")]
    InvalidSignature,
}
