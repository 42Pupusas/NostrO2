#[derive(Debug, thiserror::Error)]
pub enum NostrErrors {
    #[error("Secp error: {0}")]
    SecpError(#[from] secp256k1::Error),
    #[error("Serde error: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("No ID found on note")]
    MissingId,
    #[error("No signature found on note")]
    MissingSignature,
    #[error("No pubkey found on note")]
    MissingPubkey,
}
