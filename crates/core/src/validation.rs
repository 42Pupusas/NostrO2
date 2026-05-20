//! Validation helpers for Nostr data

use nostro2_traits::hex::FromHex as _;

/// Check if a string is a valid hex-encoded public key (64 hex characters)
#[must_use]
pub fn is_valid_pubkey(s: &str) -> bool {
    s.len() == 64 && s.decode_hex().is_ok()
}

/// Check if a string is a valid hex-encoded event ID (64 hex characters)
#[must_use]
pub fn is_valid_event_id(s: &str) -> bool {
    s.len() == 64 && s.decode_hex().is_ok()
}

/// Check if a string is a valid hex-encoded signature (128 hex characters)
#[must_use]
pub fn is_valid_signature(s: &str) -> bool {
    s.len() == 128 && s.decode_hex().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_pubkey() {
        assert!(is_valid_pubkey(
            "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d"
        ));
        assert!(!is_valid_pubkey("invalid"));
        assert!(!is_valid_pubkey("4f6d")); // too short
    }

    #[test]
    fn test_valid_event_id() {
        assert!(is_valid_event_id(
            "a123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_valid_event_id("not_hex"));
    }

    #[test]
    fn test_valid_signature() {
        let valid_sig = "a".repeat(128);
        assert!(is_valid_signature(&valid_sig));
        assert!(!is_valid_signature("short"));
    }
}
