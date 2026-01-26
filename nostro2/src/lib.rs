#![warn(
    clippy::all,
    clippy::missing_errors_doc,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]

/// # `NostrO2`
///
/// `nostr_o2` is a library for interacting with the `Nostr` protocol.
///
/// It contains the data structures described in NIP-01, with full serde support,
/// and type conversion between common formats.
pub mod errors;
mod note;
mod relay_events;
mod subscriptions;
mod tags;
pub mod validation;

pub use note::{NostrNote, NostrNoteBuilder};
pub use relay_events::{NostrClientEvent, NostrRelayEvent};
pub use subscriptions::NostrSubscription;
pub use tags::NostrTag;

/// Convenience type alias for Results with NostrErrors
pub type Result<T> = std::result::Result<T, errors::NostrErrors>;

pub trait NostrSigner {
    /// Sign a Nostr note
    ///
    /// # Errors
    ///
    /// Returns an error if the note cannot be signed
    /// or if the keypair is invalid
    fn sign_nostr_note(&self, note: &mut crate::note::NostrNote) -> Result<()>;
    fn generate(extractable: bool) -> Self;
    fn public_key(&self) -> String;
    fn secret_key(&self) -> String;
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
            .with_timestamp(1234567890);
        assert_eq!(note.created_at, 1234567890);
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
        assert!(timestamp > 1577836800);
    }

    #[test]
    fn test_builder_chaining() {
        let note = NostrNote::builder()
            .kind(1)
            .content("Test")
            .timestamp(1234567890)
            .tag_pubkey("pubkey1")
            .tag_event("event1")
            .tag_parameter("param1")
            .tag("custom", "value")
            .tag_relay("wss://relay.example.com")
            .build();

        assert_eq!(note.kind, 1);
        assert_eq!(note.content, "Test");
        assert_eq!(note.created_at, 1234567890);
        assert_eq!(note.tags.len(), 5);
    }
}
