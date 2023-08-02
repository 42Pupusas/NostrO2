extern crate rustr;
use rustr::userkeys::{UserKeys};
use rustr::nostr::{Note, SignedNote};
use secp256k1::schnorr::{Signature};
use secp256k1::{Message, XOnlyPublicKey};

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

  // Created and verified the signature of a note.
  #[test]
  fn test_create_note() {
    let content_of_note = "- .... .. ... / .. ... / .- / -- . ... ... .- --. .";
    let user_key_pair = UserKeys::new(pk1);
    let unsigned_note = Note::new(
      user_key_pair.get_public_key().to_string(),
      [].to_vec(),
      300,
      content_of_note.to_string()
    );
    let signed_note = user_key_pair.sign_nostr_event(unsigned_note);
    let signature_of_signed_note = Signature::from_slice(
      &hex::decode(signed_note.sig).unwrap()
    ).unwrap();
    let message_of_signed_note = Message::from_slice(
      &hex::decode(signed_note.id).unwrap()
    ).unwrap();
    let public_key_of_signed_note = XOnlyPublicKey::from_slice(
      &hex::decode(signed_note.pubkey).unwrap()
    ).unwrap();
    signature_of_signed_note.verify(
      &message_of_signed_note,
      &public_key_of_signed_note
    );
  }
  // Send a kind 1 message to relay and recieve it.
  // Open relay to url
  // Create a kind 1 subscription
  // Send a kind 1 message
  // Loop over recieving message to retrieve a kind 1
}
