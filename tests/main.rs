extern crate nostro2;
use nostro2::keypair::NostrKeypair;

const PRIV: &str = "07947aa9d48d099604ea53e2d347203d90fb133d77a430de43373b8eabd6275d";
const PUB: &str = "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d";

#[cfg(test)]
mod tests {
    use nostro2::notes::{NostrNote, NostrTag};

    use super::*;

    // Test Private Public NostrKeypair Match
    #[test]
    fn test_user_key() {
        let uk = NostrKeypair::new(PRIV).unwrap();
        assert_eq!(PUB, uk.public_key());
    }
    // Test Private Public NostrKeypair Do Not Match
    #[test]
    fn test_not_user_key() {
        let uk = NostrKeypair::new(PRIV).unwrap();
        assert_ne!(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            uk.public_key()
        );
    }

    // Created and verified the signature of a note.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_create_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let user_key_pair = NostrKeypair::new(PRIV).unwrap();
        let mut unsigned_note = NostrNote {
            pubkey: user_key_pair.public_key(),
            kind: 300,
            content: content_of_note.to_string(),
            ..Default::default()
        };
        user_key_pair.sign_nostr_event(&mut unsigned_note);
        assert_eq!(unsigned_note.verify(), true);
    }

    #[test]
    fn test_create_tagged_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let user_key_pair = NostrKeypair::new(PRIV).expect("Failed to create NostrKeypair!");
        let mut signed_note = NostrNote {
            pubkey: user_key_pair.public_key(),
            kind: 300,
            content: content_of_note.to_string(),
            ..Default::default()
        };
        signed_note.tags.add_custom_tag(NostrTag::Custom("t"), "test");
        signed_note.tags.add_event_tag("adsfasdfadsfadsfasdfadfs");
        signed_note.tags.add_pubkey_tag("adsfasdfadsfadsfasdfadfs");
        user_key_pair.sign_nostr_event(&mut signed_note);
        let t_tags = signed_note.tags.find_tags(NostrTag::Custom("t"));
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
        let user_key_pair = NostrKeypair::new(PRIV).expect("Failed to create NostrKeypair!");
        let mut signed_note = NostrNote {
            pubkey: user_key_pair.public_key(),
            kind: 300,
            content: content_of_note.to_string(),
            ..Default::default()
        };
        user_key_pair.sign_nostr_event(&mut signed_note);
        signed_note.tags.add_pubkey_tag(&user_key_pair.public_key());
        signed_note
            .tags
            .add_event_tag(signed_note.id.as_ref().expect("Failed to get id!").as_str());
        assert_eq!(
            signed_note.tags.find_first_tagged_pubkey(),
            Some(user_key_pair.public_key())
        );
        assert_eq!(signed_note.tags.find_first_tagged_event(), signed_note.id);
    }
}
