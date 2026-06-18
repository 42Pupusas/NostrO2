//! Validation helpers for Nostr data — exposed as an extension trait on `str`.

use nostro2_traits::hex::FromHex as _;

/// Extension trait for hex-validated Nostr identifiers.
pub trait NostrValidate {
    /// Check if this is a valid hex-encoded Nostr public key (64 hex chars).
    fn is_valid_pubkey(&self) -> bool;

    /// Check if this is a valid hex-encoded Nostr event ID (64 hex chars).
    fn is_valid_event_id(&self) -> bool;

    /// Check if this is a valid hex-encoded Nostr signature (128 hex chars).
    fn is_valid_signature(&self) -> bool;
}

impl NostrValidate for str {
    fn is_valid_pubkey(&self) -> bool {
        self.len() == 64 && self.decode_hex().is_ok()
    }

    fn is_valid_event_id(&self) -> bool {
        self.len() == 64 && self.decode_hex().is_ok()
    }

    fn is_valid_signature(&self) -> bool {
        self.len() == 128 && self.decode_hex().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_pubkey() {
        assert!("4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d"
            .is_valid_pubkey());
        assert!(!"invalid".is_valid_pubkey());
        assert!(!"4f6d".is_valid_pubkey()); // too short
    }

    #[test]
    fn test_valid_event_id() {
        assert!("a123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .is_valid_event_id());
        assert!(!"not_hex".is_valid_event_id());
    }

    #[test]
    fn test_valid_signature() {
        let valid_sig = "a".repeat(128);
        assert!(valid_sig.is_valid_signature());
        assert!(!"short".is_valid_signature());
    }
}
