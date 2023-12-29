use std::sync::Arc;

use base64::{engine::general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use secp256k1::{ecdh::SharedSecret, KeyPair, Message, PublicKey, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

use super::notes::{Note, SignedNote};

pub struct UserKeys {
    keypair: KeyPair,
}

#[derive(Debug)]
pub enum UserError {
    CouldNotCreateKeys,
}

impl UserKeys {
    pub fn new(private_key: &str) -> Result<Self, UserError> {
        let secp = Secp256k1::new();
        if let Ok(secret_key) = SecretKey::from_slice(&hex::decode(private_key).unwrap()) {
            let keypair = KeyPair::from_secret_key(&secp, &secret_key);
            Ok(UserKeys { keypair })
        } else {
            Err(UserError::CouldNotCreateKeys)
        }
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
            sig,
        );
        signed_note
    }

    pub fn sign_encrypted_nostr_event(&self, mut note: Note, public_key: PublicKey) -> SignedNote {
        let encrypted_content = self.encrypt_content(note.content.to_string(), public_key);
        note.content = Arc::from(encrypted_content.to_string());
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
            sig,
        );
        signed_note
    }

    pub fn decrypt_note_content(&self, signed_note: &SignedNote) -> String {
        let modified_public_key = format!("02{}", signed_note.get_pubkey());
        let public_key_bytes = hex::decode(modified_public_key).expect("Invalid hex in public key");
        let public_key = PublicKey::from_slice(&public_key_bytes).unwrap();
        let shared_secret =
            UserKeys::derive_shared_secret(self.keypair.secret_key(), public_key).unwrap();
        let conversation_key =
            UserKeys::derive_conversation_key(&shared_secret, b"nip44-v2").unwrap();
        let decoded_params = general_purpose::STANDARD
            .decode(signed_note.get_content().to_string())
            .unwrap();
        let (_version, nonce, ciphertext, _mac) =
            Self::extract_components(&decoded_params).unwrap();
        let decrypted_data =
            Self::decrypt(&ciphertext, &conversation_key, &nonce).expect("Failed to decrypt");
        let decrypted_string = String::from_utf8(decrypted_data).unwrap();
        decrypted_string
    }

    fn derive_shared_secret(
        private_key: SecretKey,
        public_key: PublicKey,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let shared_secret = SharedSecret::new(&public_key, &private_key);
        Ok(shared_secret.as_ref().to_vec())
    }

    fn derive_conversation_key(shared_secret: &[u8], salt: &[u8]) -> Result<Vec<u8>, String> {
        let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret);
        let mut okm = [0u8; 32]; // Output Keying Material (OKM)
        let conversation_key = hkdf.expand(&[], &mut okm);

        match conversation_key {
            Ok(_) => Ok(okm.to_vec()),
            Err(_e) => Err("Failed to derive conversation key.".to_string()),
        }
    }

    fn extract_components(
        decoded: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>), &'static str> {
        // Define the sizes of the components
        const VERSION_SIZE: usize = 1; // Size of version in bytes
        const NONCE_SIZE: usize = 12; // Size of nonce in bytes
        const MAC_SIZE: usize = 32; // Size of MAC in bytes

        // Calculate minimum size and check if the decoded data is long enough
        if decoded.len() < VERSION_SIZE + NONCE_SIZE + MAC_SIZE {
            return Err("Decoded data is too short");
        }

        let version = decoded[0..VERSION_SIZE].to_vec();
        let nonce = decoded[VERSION_SIZE..VERSION_SIZE + NONCE_SIZE].to_vec();
        let mac_start = decoded.len() - MAC_SIZE; // MAC is the last 16 bytes
        let mac = decoded[mac_start..].to_vec();
        let ciphertext = decoded[VERSION_SIZE + NONCE_SIZE..mac_start].to_vec();

        Ok((version, nonce, ciphertext, mac))
    }

    fn decrypt(ciphertext: &[u8], key: &[u8], nonce: &[u8]) -> Result<Vec<u8>, &'static str> {
        if key.len() != 32 || nonce.len() != 12 {
            return Err("Invalid key or nonce size");
        }

        let mut cipher =
            ChaCha20::new_from_slices(key, nonce).map_err(|_| "Failed to create cipher")?;
        let mut decrypted = ciphertext.to_vec();
        cipher.apply_keystream(&mut decrypted);

        Ok(decrypted)
    }

    fn generate_nonce() -> [u8; 12] {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        nonce
    }

    fn encrypt(
        content: &[u8],
        key: &[u8],
        nonce: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        let mut encrypted_content = content.to_vec();
        cipher.apply_keystream(&mut encrypted_content);
        Ok(encrypted_content)
    }

    fn calculate_mac(data: &[u8], key: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn base64_encode_params(version: &[u8], nonce: &[u8], ciphertext: &[u8], mac: &[u8]) -> String {
        let mut data = Vec::new();
        data.extend_from_slice(version);
        data.extend_from_slice(nonce);
        data.extend_from_slice(ciphertext);
        data.extend_from_slice(mac);
        let encoded: String = general_purpose::STANDARD.encode(data);
        encoded
    }

    fn encrypt_content(&self, plaintext: String, public_key: PublicKey) -> String {
        let shared_secret =
            Self::derive_shared_secret(self.keypair.secret_key(), public_key).unwrap();
        let conversation_key = Self::derive_conversation_key(&shared_secret, b"nip44-v2").unwrap();
        let nonce = Self::generate_nonce();
        let cypher_text = Self::encrypt(plaintext.as_bytes(), &conversation_key, &nonce).unwrap();
        let mac = Self::calculate_mac(&cypher_text, &conversation_key).unwrap();
        let encoded_params = Self::base64_encode_params(b"1", &nonce, &cypher_text, &mac);
        encoded_params
    }
}

mod tests {

    #[test]
    fn test_nip_44() {
        let user_keys = crate::userkeys::UserKeys::new(
            "931CB0E58332505609D13BCAE00498353961467579C95CE0DF6B0301393BCAEB",
        )
        .unwrap();
        let note = crate::notes::Note::new(user_keys.get_public_key(), 4, "Testing Encryption");
        let encrypted_note =
            user_keys.sign_encrypted_nostr_event(note, user_keys.keypair.public_key());
        let decrypted_content = user_keys.decrypt_note_content(&encrypted_note);
        assert_eq!(decrypted_content, "Testing Encryption");
    }
}
