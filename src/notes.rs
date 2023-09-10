use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use secp256k1::schnorr::Signature;
use secp256k1::{Message, XOnlyPublicKey};
use serde_json::{json, to_value};
use super::utils::get_unix_timestamp;

#[derive(Serialize, Deserialize, Debug)]
pub struct Note {
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl Note {
    pub fn new(
      pubkey: String,
      tags: Vec<Vec<String>>,
      kind: u32,
      content: String
    ) -> Self {
        Note {
            pubkey,
            created_at: get_unix_timestamp(),
            kind,
            tags,
            content,
        }
    }
    pub fn serialize_for_nostr(&self) -> String {
        let value = to_value(self).unwrap();

        let json_str = json!([
            0,
            value["pubkey"],
            value["created_at"],
            value["kind"],
            value["tags"],
            value["content"]
        ]);
        json_str.to_string()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignedNote {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl SignedNote {
    pub fn prepare_ws_message(&self) -> WsMessage {
      let event_string = json!(["EVENT", self]).to_string();
      let event_ws_message = WsMessage::Text(event_string);
      event_ws_message
    }

    pub fn verify_note(signed_note: SignedNote) -> bool {
      let signature_of_signed_note = Signature::from_slice(
        &hex::decode(signed_note.sig)
        .expect("Failed to decode signed_note signature.")
      ).expect("Failed to instantiate Signature from byte array.");
      let message_of_signed_note = Message::from_slice(
        &hex::decode(signed_note.id)
        .expect("Failed to decode signed_note id.")
      ).expect("Failed to instantiate Message from byte array.");
      let public_key_of_signed_note = XOnlyPublicKey::from_slice(
        &hex::decode(signed_note.pubkey)
        .expect("Failed to decode signed_note public")
      ).expect("Failed to instantiate XOnlyPublicKey from byte array.");

      match signature_of_signed_note.verify(
        &message_of_signed_note,
        &public_key_of_signed_note
      ) {
        Ok(()) => return true,
        _ => return false
      };
    }
}
