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
//! with zero-copy operations where possible.
//!
//! ## Quick Start
//!
//! ### Creating Notes
//!
//! ```rust
//! use nostro2::{NostrNote, NostrNoteBuilder};
//!
//! // Simple text note
//! let note = NostrNoteBuilder::text_note("Hello, Nostr!").build();
//!
//! // Using the builder
//! let note = NostrNoteBuilder::new()
//!     .content("Hello, Nostr!")
//!     .kind(1)
//!     .tag_pubkey("abc123...")
//!     .build();
//!
//! // Metadata note
//! let metadata = r#"{"name":"Alice","about":"Nostr user"}"#;
//! let note = NostrNoteBuilder::metadata(metadata).build();
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
//! let mut tags = NostrTags::new();
//! tags.add_pubkey_tag("abc123...", None);
//! tags.add_event_tag("event123...");
//! tags.add_custom_tag("t", "nostr");
//! assert_eq!(tags.len(), 3);
//! ```
//!
//! ### Validation
//!
//! ```rust
//! use nostro2::validation::NostrValidate;
//!
//! if "abc123...".is_valid_pubkey() {
//!     // Valid hex-encoded public key
//! }
//! ```
//!
//! ## Features
//!
//! - **NIP-01 Data Structures**: Complete implementation of core Nostr types
//! - **Fast JSON**: Type-driven serialization via bourne (no serde)
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
//! use nostro2::{NostrNote, NostrNoteBuilder, Result};
//!
//! fn create_note() -> Result<NostrNote> {
//! let mut note = NostrNoteBuilder::text_note("Hello").build();
//!     note.serialize_id()?;
//!     Ok(note)
//! }
//! ```
// The `k256` and `secp256k1` features pick the verification backend at
// compile time. Enabling both is a configuration error: every backend
// supports the same Schnorr scheme, so two impls would collide and there
// is no sensible "both" semantic. Enabling neither is allowed — the data
// types and zero-copy view stay available, but `NostrNote::verify` and
// `NostrNoteView::verify` are gated out (parse-only consumers like
// `nostro2-relay` don't need to verify locally).
#[cfg(all(feature = "k256", feature = "secp256k1"))]
compile_error!("features `k256` and `secp256k1` are mutually exclusive; pick exactly one");

pub mod errors;
pub mod event;
pub(crate) mod hash;
mod note;
mod relay_events;
mod subscriptions;
mod tags;
pub mod validation;
pub mod view;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use event::NostrEvent;
pub use note::{NostrNote, NostrNoteBuilder};
pub use relay_events::{NostrClientEvent, NostrRelayEvent, RelayEventTag};
pub use subscriptions::NostrSubscription;
pub use tags::NostrTags;
pub use view::{NostrClientEventView, NostrNoteView, NostrRelayEventView, NostrSubscriptionView, TagsView};

/// Re-export of the signer traits. Defined in `nostro2-traits` so protocol
/// crates (`nostro2-nips`) and signer impls (`nostro2-signer`) can share the
/// surface without depending on `nostro2`'s data structures.
pub use nostro2_traits::{NostrKeypair, NostrSigner, SignerError};

/// Convenience type alias for Results with `NostrErrors`
pub type Result<T> = std::result::Result<T, errors::NostrErrors>;

#[cfg(test)]
mod tests {
    const PUB: &str = "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d";

    use super::event::NostrEvent;
    use super::note::{NostrNote, NostrNoteBuilder};

    // An unsigned note — no `id`, no `sig` — must not verify, and the
    // failure must hold even if `id` is later filled in but `sig` isn't.
    // Both gates protect the same invariant: `verify()` only returns true
    // for fully signed notes.
    #[test]
    fn unsigned_note_does_not_verify() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let mut note = NostrNote {
            pubkey: PUB.into(),
            kind: 300,
            content: content_of_note.into(),
            ..Default::default()
        };
        assert!(!note.verify(), "unsigned note must not verify");

        // Even with a freshly computed id (but no sig), it still must not verify.
        note.serialize_id().expect("id serialization");
        assert!(note.id.is_some());
        assert!(note.sig.is_none());
        assert!(
            !note.verify(),
            "id-only note (no sig) must still fail verification"
        );
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

    /// Round-trips a `NostrNote` through bourne serialize/parse so the
    /// entire field surface (escapes, unicode, extreme numerics, tag rows)
    /// actually exercises both directions.
    #[test]
    fn nostr_note_bourne_round_trip() {
        let mut note = NostrNote {
            pubkey: PUB.into(),
            kind: u32::MAX,
            created_at: i64::MIN,
            content: "every escape: \\ \" \n \t \0 — and unicode 🦀".into(),
            id: Some("a".repeat(64)),
            sig: Some("b".repeat(128)),
            ..Default::default()
        };
        note.tags.add_pubkey_tag(PUB, Some("wss://relay"));
        note.tags.add_event_tag(PUB);
        note.tags.add_custom_tag("x", "y");

        let json = bourne::to_string(&note).expect("serialize");
        let round_trip: NostrNote = bourne::parse_str(&json).expect("parse back");
        assert_eq!(note, round_trip);
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
        let note = NostrNoteBuilder::new()
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
        let note = NostrNoteBuilder::text_note("Hello, world!").build();
        assert_eq!(note.kind, 1);
        assert_eq!(note.content, "Hello, world!");
    }

    #[test]
    fn test_metadata_note() {
        let metadata = r#"{"name":"Alice"}"#;
        let note = NostrNoteBuilder::metadata(metadata).build();
        assert_eq!(note.kind, 0);
        assert_eq!(note.content, metadata);
    }

    #[test]
    fn test_with_kind() {
        let note = NostrNoteBuilder::new().kind(4).build();
        assert_eq!(note.kind, 4);
    }

    #[test]
    fn test_with_timestamp() {
        let note = NostrNoteBuilder::text_note("Hello").timestamp(1_234_567_890).build();
        assert_eq!(note.created_at, 1_234_567_890);
    }

    #[test]
    fn test_with_content() {
        let note = NostrNoteBuilder::new().kind(1).content("New content").build();
        assert_eq!(note.content, "New content");
    }

    #[test]
    fn test_note_now() {
        let timestamp = NostrNoteBuilder::new().build().created_at;
        assert!(timestamp > 0);
        // Should be recent (after 2020-01-01)
        assert!(timestamp > 1_577_836_800);
    }

    #[test]
    fn test_builder_chaining() {
        let note = NostrNoteBuilder::new()
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

    /// Locks the `sign_with` → `verify` round-trip from the *consumer* side.
    /// The per-backend tests in `nostro2-signer` would still pass if a
    /// refactor in `note.rs` changed the canonical prehash tuple in a way
    /// that's invisible to the signer crate but breaks downstream verifiers.
    /// This test is the canary for that case — it exercises the same path
    /// every external caller uses (`note.sign_with(&kp)` then `note.verify()`).
    ///
    /// Gated to `feature = "k256"` because `nostro2`'s dev-dependency on
    /// `nostro2-signer` is pinned to the k256 backend (see Cargo.toml).
    #[cfg(feature = "k256")]
    #[test]
    fn sign_with_then_verify_round_trips() {
        use nostro2_traits::NostrKeypair as _;
        let kp = nostro2_signer::NostrKeypair::generate();

        let mut note = NostrNoteBuilder::text_note("round trip").build();
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"a".repeat(64), None);

        note.sign_with(&kp).expect("sign");
        assert!(note.verify(), "freshly signed note must verify");
        // Mutating any field after signing must invalidate the note.
        note.content.push('!');
        assert!(!note.verify(), "tampered content must not verify");
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod proptests {
        use super::*;
        use proptest::prelude::*;
        use crate::event::NostrEvent;

        fn arb_note() -> impl Strategy<Value = NostrNote> {
            (
                "[a-zA-Z0-9]{0,64}",
                any::<i64>(),
                any::<u32>(),
                "[a-zA-Z0-9 ]{0,128}",
                proptest::collection::vec(("[a-zA-Z0-9]{1,4}", "[a-zA-Z0-9]{0,32}"), 0..8),
            )
                .prop_map(|(pubkey, created_at, kind, content, tag_pairs)| {
                    let mut note = NostrNote {
                        pubkey,
                        created_at,
                        kind,
                        content,
                        ..Default::default()
                    };
                    for (name, value) in tag_pairs {
                        note.tags.add_custom_tag(&name, &value);
                    }
                    note
                })
        }

        proptest! {
            #[test]
            fn json_round_trip(note in arb_note()) {
                let json = bourne::to_string(&note).unwrap();
                let back: NostrNote = bourne::parse_str(&json).unwrap();
                prop_assert_eq!(&note, &back);
            }

            #[test]
            fn serialize_id_is_deterministic(note in arb_note()) {
                let mut a = note.clone();
                let mut b = note;
                a.serialize_id().unwrap();
                b.serialize_id().unwrap();
                prop_assert_eq!(&a.id, &b.id);
            }

            #[test]
            fn view_id_matches_owned_id(note in arb_note()) {
                let mut owned = note;
                owned.serialize_id().unwrap();
                let json = bourne::to_string(&owned).unwrap();
                let view: crate::view::NostrNoteView<'_> =
                    bourne::parse_str(&json).unwrap();
                let view_id = view.compute_id_bytes();
                let owned_id = owned.id_bytes().unwrap();
                prop_assert_eq!(owned_id, view_id);
            }
        }
    }
}
