extern crate nostro2;
use nostro2::notes::Note;
use nostro2::userkeys::UserKeys;

const PRIV: &str = "07947aa9d48d099604ea53e2d347203d90fb133d77a430de43373b8eabd6275d";
const PUB: &str = "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d";

#[cfg(test)]
mod tests {
    use super::*;

    // Test Private Public UserKeys Match
    #[test]
    fn test_user_key() {
        let uk = UserKeys::new(PRIV).unwrap();
        assert_eq!(PUB, uk.get_public_key());
    }
    // Test Private Public UserKeys Do Not Match
    #[test]
    fn test_not_user_key() {
        let uk = UserKeys::new(PRIV).unwrap();
        assert_ne!(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            uk.get_public_key()
        );
    }

    // Created and verified the signature of a note.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_create_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let user_key_pair = UserKeys::new(PRIV).unwrap();
        let unsigned_note = Note::new(&user_key_pair.get_public_key(), 300, content_of_note);
        let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
        assert_eq!(signed_note.verify(), true);
    }


    #[test]
    fn test_create_tagged_note() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let user_key_pair = UserKeys::new(PRIV).expect("Failed to create UserKeys!");
        let mut unsigned_note = Note::new(&user_key_pair.get_public_key(), 300, content_of_note);
        unsigned_note.add_tag("t", "test");
        unsigned_note.add_event_tag("adsfasdfadsfadsfasdfadfs");
        unsigned_note.add_pubkey_tag("adsfasdfadsfadsfasdfadfs");
        let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
        let t_tags = signed_note.get_tags_by_id("t").expect("Failed to get tag!");
        let t_tag = t_tags.first().unwrap();
        assert_eq!(t_tag, "test");
        let p_tags = signed_note.get_tags_by_id("p").expect("Failed to get tag!");
        let p_tag = p_tags.first().unwrap();
        assert_eq!(p_tag, "adsfasdfadsfadsfasdfadfs");
        let e_tags = signed_note.get_tags_by_id("e").expect("Failed to get tag!");
        let e_tag = e_tags.first().unwrap();
        assert_eq!(e_tag, "adsfasdfadsfadsfasdfadfs");
    }

    #[test]
    fn test_try_p_and_e_tags() {
        let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
        let user_key_pair = UserKeys::new(PRIV).expect("Failed to create UserKeys!");
        let mut unsigned_note = Note::new(&user_key_pair.get_public_key(), 300, content_of_note);
        unsigned_note.add_tag("p", "test");
        unsigned_note.add_tag("e", "test2");
        let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
        assert_eq!(signed_note.get_tags_by_id("p"), None);
        assert_eq!(signed_note.get_tags_by_id("e"), None);
    }
}
