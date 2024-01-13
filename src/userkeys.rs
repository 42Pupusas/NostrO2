use std::sync::Arc;

use base64::{engine::general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use secp256k1::{ecdh::shared_secret_point, KeyPair, Message, PublicKey, Secp256k1, SecretKey};
use secp256k1::{Parity, XOnlyPublicKey};
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

    pub fn get_raw_public_key(&self) -> PublicKey {
        return self.keypair.public_key();
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

    pub fn sign_encrypted_nostr_event(&self, mut note: Note, pubkey: String) -> SignedNote {
        note.tag_for_private_message(&pubkey);
        let encrypted_content = self.encrypt_content(note.content.to_string(), pubkey);
        note.content = Arc::from(encrypted_content.to_string());
        note.kind = 4;
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
        let shared_secret = UserKeys::get_shared_point(&self, signed_note.get_pubkey().to_string());
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

    fn get_shared_point(&self, public_key_string: String) -> [u8; 32] {
        let x_only_public_key =
            XOnlyPublicKey::from_slice(hex::decode(public_key_string).unwrap().as_slice()).unwrap();
        let public_key = PublicKey::from_x_only_public_key(x_only_public_key, Parity::Even);
        let mut ssp = shared_secret_point(&public_key, &self.keypair.secret_key())
            .as_slice()
            .to_owned();
        ssp.resize(32, 0); // toss the Y part
        ssp.try_into().unwrap()
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

    fn decrypt(ciphertext: &[u8], key: &[u8], nonce: &[u8]) -> Result<Vec<u8>, String> {
        if key.len() != 32 || nonce.len() != 12 {
            return Err("Invalid key or nonce size".to_string());
        }

        let mut cipher =
            ChaCha20::new_from_slices(key, nonce).map_err(|_| "Failed to create cipher")?;
        let mut decrypted = ciphertext.to_vec();
        cipher.apply_keystream(&mut decrypted);
        // Extract the plaintext length
        if decrypted.len() < 2 {
            return Err("Decrypted data too short for length prefix".to_string());
        }
        let plaintext_length = u16::from_be_bytes([decrypted[0], decrypted[1]]) as usize;

        // Validate and extract the plaintext
        if plaintext_length > decrypted.len() - 2 {
            return Err("Invalid plaintext length".to_string());
        }
        Ok(decrypted[2..2 + plaintext_length].to_vec())
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
        let mut padded_content = Self::pad_string(content)?;
        cipher.apply_keystream(&mut padded_content);

        Ok(padded_content)
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

    fn encrypt_content(&self, plaintext: String, public_key_string: String) -> String {
        let shared_secret = Self::get_shared_point(&self, public_key_string);
        let conversation_key = Self::derive_conversation_key(&shared_secret, b"nip44-v2").unwrap();
        let nonce = Self::generate_nonce();
        let cypher_text = Self::encrypt(plaintext.as_bytes(), &conversation_key, &nonce).unwrap();
        let mac = Self::calculate_mac(&cypher_text, &conversation_key).unwrap();
        let encoded_params = Self::base64_encode_params(b"1", &nonce, &cypher_text, &mac);
        encoded_params
    }

    fn pad_string(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        if plaintext.is_empty() || plaintext.len() > 65535 {
            return Err("Plaintext length must be between 1 and 65535 bytes".to_string());
        }

        let plaintext_length_with_prefix = plaintext.len() + 2; // +2 for the length prefix
        let mut total_length = 32;
        while total_length < plaintext_length_with_prefix {
            total_length *= 2;
        }

        let mut padded_message = Vec::with_capacity(total_length);
        padded_message.extend_from_slice(&(plaintext.len() as u16).to_be_bytes()); // length prefix
        padded_message.extend_from_slice(plaintext);
        padded_message.resize(total_length, 0); // add zero bytes for padding

        Ok(padded_message)
    }
}

mod tests {

    #[test]
    fn test_nip_44() {
        let pk_list = [
            "CCCC227A28C49EE57628DA97570AF4FCBACA5776C659C488AB145258E005C68F",
            "D053D85592C48B73B6DED93681B3743D72234077E325858F0E5FE31B0793A63B",
            "65336E6F1527392E8CAC9B2F53CDC8083A74435B0A06045B713D893734571069",
            "BBD39EB40490B8070168959D10C99EB31815D4ED5DDF8916B483101F3B9D4E4F",
            "2A3AE39CE5404B3C7A7C74DFA3EA5E93DD30584094F9E6419BCD5B8F9BD95F66",
            "AA6F2CDDEA668B65C882635EB5773F83C0BD0D0F82B8DBC7A3C61DF4F61E4AC1",
        ];

        use rand::Rng;
        for keys in pk_list.iter() {
            let user_keys = crate::userkeys::UserKeys::new(keys).unwrap();
            let random_length = rand::thread_rng().gen_range(1..32);
            let note = crate::notes::Note::new(
                user_keys.get_public_key(),
                4,
                &user_keys.get_public_key()[..random_length],
            );
            let encrypted_note =
                user_keys.sign_encrypted_nostr_event(note, user_keys.get_public_key());
            let decrypted_content = user_keys.decrypt_note_content(&encrypted_note);
            assert_eq!(
                decrypted_content,
                &user_keys.get_public_key()[..random_length]
            );
            assert_eq!(encrypted_note.get_kind(), 4);
            assert_eq!(encrypted_note.get_tags_by_id("p"), Some(vec![user_keys.get_public_key()]));
        }
    }

    #[test]
    fn test_encrypting_to_other() {
        let pk_list = [
            "CCCC227A28C49EE57628DA97570AF4FCBACA5776C659C488AB145258E005C68F",
            "D053D85592C48B73B6DED93681B3743D72234077E325858F0E5FE31B0793A63B",
            "65336E6F1527392E8CAC9B2F53CDC8083A74435B0A06045B713D893734571069",
            "BBD39EB40490B8070168959D10C99EB31815D4ED5DDF8916B483101F3B9D4E4F",
            "2A3AE39CE5404B3C7A7C74DFA3EA5E93DD30584094F9E6419BCD5B8F9BD95F66",
            "AA6F2CDDEA668B65C882635EB5773F83C0BD0D0F82B8DBC7A3C61DF4F61E4AC1",
        ];

        let user_keys = crate::userkeys::UserKeys::new(&pk_list[0]).unwrap();
        let user_keys2 = crate::userkeys::UserKeys::new(&pk_list[1]).unwrap();
        let note = crate::notes::Note::new(user_keys.get_public_key(), 4, "PEchan es GAY");
        let encrypted_note =
            user_keys.sign_encrypted_nostr_event(note, user_keys2.get_public_key());
        println!("Encrypted Note: {:?}", encrypted_note.get_content());
        let decrypted_content = user_keys2.decrypt_note_content(&encrypted_note);
        println!("Decrypted Content: {:?}", decrypted_content);
        assert_eq!(decrypted_content, "PEchan es GAY");
    }
}
