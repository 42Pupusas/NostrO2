#![warn(
    clippy::all,
    clippy::missing_errors_doc,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
//! # `NostrO2`
//!
//! Simple yet powerful Rust library for the Nostr protocol.
//!
//! `nostro2` provides the core data structures and types for working with Nostr,
//! as defined in NIP-01. It focuses on type safety, ergonomics, and performance
//! with full serde support and zero-copy operations where possible.
//!
//! ## Quick Start
//!
//! ### Creating Notes
//!
//! ```rust
//! use nostro2::NostrNote;
//!
//! // Simple text note
//! let note = NostrNote::text_note("Hello, Nostr!");
//!
//! // Using the builder
//! let note = NostrNote::builder()
//!     .content("Hello, Nostr!")
//!     .kind(1)
//!     .tag_pubkey("abc123...")
//!     .build();
//!
//! // Metadata note
//! let metadata = r#"{"name":"Alice","about":"Nostr user"}"#;
//! let note = NostrNote::metadata(metadata);
//! ```
//!
//! ### Creating Subscriptions
//!
//! ```rust
//! use nostro2::NostrSubscription;
//!
//! // Filter for text notes from specific authors
//! let filter = NostrSubscription::new()
//!     .kind(1)
//!     .author("pubkey1...")
//!     .author("pubkey2...")
//!     .limit(10);
//! ```
//!
//! ### Working with Tags
//!
//! ```rust
//! use nostro2::NostrTags;
//!
//! let tags = NostrTags::new()
//!     .with_pubkey("abc123...", None)
//!     .with_event("event123...")
//!     .with_tag("t", "nostr");
//!
//! // Tags behave like Vec
//! assert_eq!(tags.len(), 3);
//! ```
//!
//! ### Validation
//!
//! ```rust
//! use nostro2::validation;
//!
//! if validation::is_valid_pubkey("abc123...") {
//!     // Valid hex-encoded public key
//! }
//! ```
//!
//! ## Features
//!
//! - **NIP-01 Data Structures**: Complete implementation of core Nostr types
//! - **Serde Support**: Full serialization/deserialization with serde
//! - **Builder Patterns**: Ergonomic APIs for constructing notes and filters
//! - **Type Safety**: Strong typing with comprehensive error handling
//! - **WASM Compatible**: Works in browser environments
//! - **Zero-Copy Operations**: Non-allocating variants for performance
//!
//! ## Error Handling
//!
//! The crate uses a [`Result`](type.Result.html) type alias for convenience:
//!
//! ```rust
//! use nostro2::{NostrNote, Result};
//!
//! fn create_note() -> Result<NostrNote> {
//!     let mut note = NostrNote::text_note("Hello");
//!     note.serialize_id()?;
//!     Ok(note)
//! }
//! ```
pub mod errors;
mod note;
mod relay_events;
mod subscriptions;
mod tags;
pub mod validation;
pub mod view;

pub use note::{NostrNote, NostrNoteBuilder};
pub use view::{NostrNoteView, TagsView};
pub use relay_events::{NostrClientEvent, NostrRelayEvent, RelayEventTag};
pub use subscriptions::NostrSubscription;
pub use tags::{NostrTag, NostrTags};

/// Convenience type alias for Results with `NostrErrors`
pub type Result<T> = std::result::Result<T, errors::NostrErrors>;

/// Core signing interface for Nostr keypairs.
///
/// Implement this to plug any keypair type into the nostro2 ecosystem.
pub trait NostrSigner {
    /// Sign a Nostr note, setting `pubkey`, `id`, and `sig`.
    ///
    /// # Errors
    ///
    /// Returns an error if signing fails.
    fn sign_nostr_note(&self, note: &mut crate::note::NostrNote) -> Result<()>;

    /// Generate a new random keypair.
    fn generate() -> Self;

    /// Return the public key as a 64-character lowercase hex string.
    fn public_key(&self) -> String;
}

/// Extended keypair interface with key-export and ECDH.
///
/// Implement this on top of [`NostrSigner`] to expose the full keypair API
/// (secret key export, ECDH shared point, bech32 encoding).
pub trait NostrKeypair: NostrSigner {
    /// Return the raw 32-byte secret key, or `None` if this keypair was
    /// created without export permission.
    fn secret_key(&self) -> Option<String>;

    /// Derive the ECDH shared point with a peer's x-only public key
    /// (32-byte hex string).
    ///
    /// # Errors
    ///
    /// Returns an error if the peer public key is invalid.
    fn shared_point(&self, peer_pubkey: &str) -> Result<[u8; 32]>;

    /// Return the public key in bech32 `npub1…` encoding.
    ///
    /// # Errors
    ///
    /// Returns an error if bech32 encoding fails.
    fn npub(&self) -> Result<String>;

    /// Return the secret key in bech32 `nsec1…` encoding, or an error if the
    /// keypair was created without export permission.
    ///
    /// # Errors
    ///
    /// Returns an error if the keypair is not extractable or bech32 fails.
    fn nsec(&self) -> Result<String>;
}

#[cfg(test)]
mod tests {
    const PUB: &str = "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d";

    use super::note::NostrNote;

    // Created and verified the signature of a note.
    #[test]
    fn test_create_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let unsigned_note = NostrNote {
            pubkey: PUB.into(),
            kind: 300,
            content: content_of_note.into(),
            ..Default::default()
        };
        assert!(!unsigned_note.verify());
    }

    #[test]
    fn test_create_tagged_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let mut signed_note = NostrNote {
            pubkey: PUB.into(),
            kind: 300,
            content: content_of_note.into(),
            ..Default::default()
        };
        signed_note.tags.add_custom_tag("t", "test");
        signed_note.tags.add_event_tag("adsfasdfadsfadsfasdfadfs");
        signed_note
            .tags
            .add_pubkey_tag("adsfasdfadsfadsfasdfadfs", None);
        let t_tags = signed_note.tags.find_tags("t");
        let t_tag = t_tags.first().expect("Failed to get tag!");
        assert_eq!(t_tag, "test");
        let p_tag = signed_note
            .tags
            .first_tagged_pubkey()
            .expect("Failed to get tag!");
        assert_eq!(p_tag, "adsfasdfadsfadsfasdfadfs");
        let e_tag = signed_note
            .tags
            .first_tagged_event()
            .expect("Failed to get tag!");
        assert_eq!(e_tag, "adsfasdfadsfadsfasdfadfs");
    }

    #[test]
    fn test_try_p_and_e_tags() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let mut signed_note = NostrNote {
            pubkey: PUB.to_string(),
            kind: 300,
            content: content_of_note.to_string(),
            ..Default::default()
        };
        signed_note.tags.add_pubkey_tag(PUB, None);
        assert_eq!(
            signed_note.tags.first_tagged_pubkey(),
            Some(PUB.to_string())
        );
    }

    #[test]
    fn test_note_builder() {
        let note = NostrNote::builder()
            .content("Hello, Nostr!")
            .kind(1)
            .tag_pubkey("abc123")
            .tag_event("event123")
            .tag("t", "nostr")
            .build();

        assert_eq!(note.content, "Hello, Nostr!");
        assert_eq!(note.kind, 1);
        assert_eq!(note.tags.len(), 3);
    }

    #[test]
    fn test_text_note() {
        let note = NostrNote::text_note("Hello, world!");
        assert_eq!(note.kind, 1);
        assert_eq!(note.content, "Hello, world!");
    }

    #[test]
    fn test_metadata_note() {
        let metadata = r#"{"name":"Alice"}"#;
        let note = NostrNote::metadata(metadata);
        assert_eq!(note.kind, 0);
        assert_eq!(note.content, metadata);
    }

    #[test]
    fn test_with_kind() {
        let note = NostrNote::with_kind(4);
        assert_eq!(note.kind, 4);
    }

    #[test]
    fn test_with_timestamp() {
        let note = NostrNote::text_note("Hello")
            .with_timestamp(1_234_567_890);
        assert_eq!(note.created_at, 1_234_567_890);
    }

    #[test]
    fn test_with_content() {
        let note = NostrNote::with_kind(1)
            .with_content("New content");
        assert_eq!(note.content, "New content");
    }

    #[test]
    fn test_note_now() {
        let timestamp = NostrNote::now();
        assert!(timestamp > 0);
        // Should be recent (after 2020-01-01)
        assert!(timestamp > 1_577_836_800);
    }

    #[test]
    fn test_builder_chaining() {
        let note = NostrNote::builder()
            .kind(1)
            .content("Test")
            .timestamp(1_234_567_890)
            .tag_pubkey("pubkey1")
            .tag_event("event1")
            .tag_parameter("param1")
            .tag("custom", "value")
            .tag_relay("wss://relay.example.com")
            .build();

        assert_eq!(note.kind, 1);
        assert_eq!(note.content, "Test");
        assert_eq!(note.created_at, 1_234_567_890);
        assert_eq!(note.tags.len(), 5);
    }
}
