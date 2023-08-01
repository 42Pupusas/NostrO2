extern crate rustr;
use rustr::userkeys::{UserKeys};
use rustr::nostr::{Note, SignedNote};

const pk1: &str = "07947aa9d48d099604ea53e2d347203d90fb133d77a430de43373b8eabd6275d";
const pk2: &str = "4f6ddf3e79731d1b7039e28feb394e41e9117c93e383d31e8b88719095c6b17d";

#[cfg(test)]
mod tests {
  use super::*;

  // Test Private Public UserKeys Match
  #[test]
  fn test_user_key() {
    let uk = UserKeys::new(pk1);
    assert_eq!(pk2,uk.get_public_key());
  }
  // Test Private Public UserKeys Do Not Match
  #[test]
  fn test_not_user_key() {
    let uk = UserKeys::new(pk1);
    assert_ne!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",uk.get_public_key());
  }

  // Sign messages
  #[test]
  fn test_create_note() {
    let content = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
    let uk = UserKeys::new(pk1);
    let note = Note::new(
      uk.get_public_key().to_string(),
      [].to_vec(),
      300,
      content.to_string()
    );
    let en_test = NoteTest::new(
      uk.sign_nostr_event(note)
    )
    // Should be able to verify the signature against
    // the id the and the public key.
  }
  // Send messages to a relay.
  // Recieve messages to a relay.
}
