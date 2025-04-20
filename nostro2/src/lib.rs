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
pub mod note;
pub mod relay_events;
pub mod subscriptions;
pub mod tags;

pub trait NostrSigner {
    /// Sign a Nostr note
    ///
    /// # Errors
    ///
    /// Returns an error if the note cannot be signed
    /// or if the keypair is invalid
    fn sign_nostr_note(&self, note: &mut crate::note::NostrNote)
        -> Result<(), errors::NostrErrors>;
    fn generate(extractable: bool) -> Self;
    fn public_key(&self) -> String;
}

#[cfg(test)]
mod tests {
    const PUB: &str = "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d";

    use super::note::NostrNote;
    use super::tags::NostrTag;

    // Created and verified the signature of a note.
    #[test]
    fn test_create_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let unsigned_note = NostrNote {
            pubkey: PUB.to_string(),
            kind: 300,
            content: content_of_note.to_string(),
            ..Default::default()
        };
        assert_eq!(unsigned_note.verify(), false);
    }

    #[test]
    fn test_create_tagged_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let mut signed_note = NostrNote {
            pubkey: PUB.to_string(),
            kind: 300,
            content: content_of_note.to_string(),
            ..Default::default()
        };
        signed_note.tags.add_custom_tag("t", "test");
        signed_note.tags.add_event_tag("adsfasdfadsfadsfasdfadfs");
        signed_note
            .tags
            .add_pubkey_tag("adsfasdfadsfadsfasdfadfs", None);
        let t_tags = signed_note.tags.find_tags(&NostrTag::Custom("t"));
        let t_tag = t_tags.first().expect("Failed to get tag!");
        assert_eq!(t_tag, "test");
        let p_tag = signed_note
            .tags
            .find_first_tagged_pubkey()
            .expect("Failed to get tag!");
        assert_eq!(p_tag, "adsfasdfadsfadsfasdfadfs");
        let e_tag = signed_note
            .tags
            .find_first_tagged_event()
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
            signed_note.tags.find_first_tagged_pubkey(),
            Some(PUB.to_string())
        );
    }
}
