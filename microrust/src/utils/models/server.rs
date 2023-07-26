use secp256k1::{KeyPair, Message, PublicKey, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

use super::{nostr::{Note, SignedNote}, secrets::PRIV_KEY};

pub struct ServerBot {
    pub id: String,
    keypair: KeyPair,
}

impl ServerBot {
    pub fn new() -> Self {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&hex::decode(PRIV_KEY).unwrap()).unwrap();
        let keypair = KeyPair::from_secret_key(&secp, &secret_key);
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);
        let id = hex::encode(Sha256::digest(&public_key.serialize()[..]));
        ServerBot { id, keypair }
    }

    pub fn get_public_key(&self) -> String {
        return self.keypair.public_key().to_string()[2..].to_string();
    }
    
    pub fn sign_nostr_event(&self, event: Note) -> SignedNote {
        // Serialize the event as JSON
        let json_str = event.serialize_for_nostr();
        // Compute the SHA256 hash of the serialized JSON string
        let mut hasher = Sha256::new();
        hasher.update(json_str);
        let hash_result = hasher.finalize();
        let id = hex::encode(hash_result);
        let secp = Secp256k1::new();

        let id_message = Message::from_slice(&hash_result).unwrap();
        
        let sig = secp
            .sign_schnorr_no_aux_rand(&id_message, &self.keypair)
            .to_string();
        let signed_event = SignedNote {
            id,
            pubkey: self.get_public_key(),
            created_at: event.created_at,
            kind: event.kind,
            tags: event.tags,
            content: event.content,
            sig,
        };
        signed_event
    }
}
