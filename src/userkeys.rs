use secp256k1::{KeyPair, Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

use super::notes::{Note, SignedNote};

pub struct UserKeys {
    keypair: KeyPair,
}

impl UserKeys {
    pub fn new(private_key: &str) -> Self {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(
          &hex::decode(private_key).unwrap()
        ).unwrap();
        let keypair = KeyPair::from_secret_key(&secp, &secret_key);
        UserKeys { keypair }
    }

    pub fn get_public_key(&self) -> String {
        return self.keypair.public_key().to_string()[2..].to_string();
    }

    pub fn sign_nostr_event(&self, note: Note) -> SignedNote {
        // Serialize the event as JSON
        let json_str = note.serialize_for_nostr();
        
        // Compute the SHA256 hash of the serialized JSON string
        let mut hasher = Sha256::new();
        hasher.update(json_str);
        
        // Hex Encod the hash
        let hash_result = hasher.finalize();
        let id = hex::encode(hash_result);
        
        // Create a byte representation of the hash.
        let secp = Secp256k1::new();
        let id_message = Message::from_slice(&hash_result).unwrap();
        
        // Sign it with the schnorr.
        let sig = secp
            .sign_schnorr_no_aux_rand(&id_message, &self.keypair)
            .to_string();
        
        let signed_note = SignedNote::new(
          id,
          self.get_public_key(),
          note.tags,
          note.kind,
          &*note.content,
          sig
        );
        signed_note
    }
}
