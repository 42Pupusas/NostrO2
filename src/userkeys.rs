use base64::{engine::general_purpose, Engine as _};
use bip39::Language;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use secp256k1::{ecdh::shared_secret_point, KeyPair, Message, PublicKey, Secp256k1, SecretKey};
use secp256k1::{Parity, XOnlyPublicKey};
use sha2::{Digest, Sha256};

use super::notes::{Note, SignedNote};
use bech32::{Bech32, Hrp};

#[derive(Debug, PartialEq, Clone, Eq)]
pub struct UserKeys {
    keypair: KeyPair,
    extractable: bool,
}

#[derive(Debug)]
pub enum UserError {
    DecryptionError,
    DecodingError,
    NsecError,
    MnemonicError,
}

impl ToString for UserError {
    fn to_string(&self) -> String {
        match self {
            UserError::DecryptionError => "Failed to decrypt".to_string(),
            UserError::DecodingError => "Failed to decode".to_string(),
            UserError::NsecError => "Failed to decode nsec".to_string(),
            UserError::MnemonicError => "Failed to parse mnemonic".to_string(),
        }
    }
}

impl UserKeys {
    pub fn new(private_key: &str) -> Result<Self, UserError> {
        // Check if the private key starts with "nsec"
        if private_key.starts_with("nsec") {
            let bech32_decoded = bech32::decode(&private_key);
            if let Err(_) = bech32_decoded {
                return Err(UserError::NsecError);
            }
            let (hrp, data) = bech32_decoded.unwrap();
            if hrp.to_string() != "nsec" {
                return Err(UserError::NsecError);
            }
            let secret_key = SecretKey::from_slice(&data);
            if let Err(_) = secret_key {
                return Err(UserError::DecryptionError);
            }
            return Self::create_user_keys(secret_key.unwrap(), false);
        }

        // Decode the private key as hex
        let decoded_private_key = hex::decode(private_key);
        if let Err(_) = decoded_private_key {
            return Err(UserError::DecodingError);
        }
        let secret_key = SecretKey::from_slice(&decoded_private_key.unwrap());
        if let Err(_) = secret_key {
            return Err(UserError::DecryptionError);
        }

        // Create and return UserKeys
        Self::create_user_keys(secret_key.unwrap(), false)
    }

    fn create_user_keys(secret_key: SecretKey, extractable: bool) -> Result<UserKeys, UserError> {
        let secp = Secp256k1::new();
        let keypair = KeyPair::from_secret_key(&secp, &secret_key);
        Ok(UserKeys {
            keypair,
            extractable,
        })
    }

    pub fn new_extractable(private_key: &str) -> Result<Self, UserError> {
        // Check if the private key starts with "nsec"
        if private_key.starts_with("nsec") {
            let bech32_decoded = bech32::decode(&private_key);
            if let Err(_) = bech32_decoded {
                return Err(UserError::NsecError);
            }
            let (hrp, data) = bech32_decoded.unwrap();
            if hrp.to_string() != "nsec" {
                return Err(UserError::NsecError);
            }
            let secret_key = SecretKey::from_slice(&data);
            if let Err(_) = secret_key {
                return Err(UserError::DecryptionError);
            }
            return Self::create_user_keys(secret_key.unwrap(), true);
        }

        // Decode the private key as hex
        let decoded_private_key = hex::decode(private_key);
        if let Err(_) = decoded_private_key {
            return Err(UserError::DecodingError);
        }
        let secret_key = SecretKey::from_slice(&decoded_private_key.unwrap());
        if let Err(_) = secret_key {
            return Err(UserError::DecryptionError);
        }

        // Create and return UserKeys
        Self::create_user_keys(secret_key.unwrap(), true)
    }

    pub fn generate() -> Self {
        let new_secret_key = crate::utils::new_keys();
        Self::create_user_keys(new_secret_key, false).unwrap()
    }

    pub fn generate_extractable() -> Self {
        let new_secret_key = crate::utils::new_keys();
        Self::create_user_keys(new_secret_key, true).unwrap()
    }

    pub fn get_public_key(&self) -> String {
        return self.keypair.public_key().x_only_public_key().0.to_string();
    }

    pub fn get_raw_public_key(&self) -> [u8; 32] {
        return self.keypair.public_key().x_only_public_key().0.serialize();
    }

    pub fn get_secret_key(&self) -> [u8; 32] {
        if !self.extractable {
            return [0u8; 32];
        }
        self.keypair.secret_key().secret_bytes()
    }

    pub fn get_npub(&self) -> String {
        let hrp = Hrp::parse("npub").expect("valid hrp");
        let pk_data = self.keypair.public_key().x_only_public_key().0.serialize();
        let string = bech32::encode::<Bech32>(hrp, &pk_data).expect("failed to encode string");
        string
    }

    pub fn get_nsec(&self) -> String {
        if !self.extractable {
            return String::from("Not extractable");
        }
        let secret_key = self.keypair.secret_key().secret_bytes();
        let hrp = Hrp::parse("nsec").expect("valid hrp");
        let string = bech32::encode::<Bech32>(hrp, &secret_key).expect("failed to encode string");
        string
    }

    pub fn get_mnemonic_phrase(&self) -> String {
        if !self.extractable {
            return String::from("Not extractable");
        }
        let secret_key = self.keypair.secret_key().secret_bytes();
        let mnemonic = bip39::Mnemonic::from_entropy(&secret_key).unwrap();
        mnemonic.word_iter().collect::<Vec<&str>>().join(" ")
    }

    pub fn get_mnemonic_spanish(&self) -> String {
        if !self.extractable {
            return String::from("Not extractable");
        }
        let secret_key = self.keypair.secret_key().secret_bytes();
        let mnemonic = bip39::Mnemonic::from_entropy_in(Language::Spanish, &secret_key).unwrap();
        mnemonic.word_iter().collect::<Vec<&str>>().join(" ")
    }

    pub fn parse_mnemonic(mnemonic: &str) -> Result<Self, UserError> {
        let english_parse = bip39::Mnemonic::parse_in(Language::English, mnemonic);
        let spanish_parse = bip39::Mnemonic::parse_in(Language::Spanish, mnemonic);

        if english_parse.is_ok() {
            let mnemonic = english_parse.unwrap();
            let secret_key = mnemonic.to_entropy();
            let secret_key = SecretKey::from_slice(&secret_key).unwrap();
            let keys = Self::create_user_keys(secret_key, false).unwrap();
            Ok(keys)
        } else if spanish_parse.is_ok() {
            let mnemonic = spanish_parse.unwrap();
            let secret_key_bytes = mnemonic.to_entropy();
            let secret_key = SecretKey::from_slice(&secret_key_bytes).unwrap();
            let keys = Self::create_user_keys(secret_key, false).unwrap();
            Ok(keys)
        } else {
            Err(UserError::MnemonicError)
        }
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

        let signed_note = SignedNote::new(note, id, sig);
        signed_note
    }

    pub fn sign_encrypted_nostr_event(&self, mut note: Note, pubkey: String) -> SignedNote {
        note.add_pubkey_tag(&pubkey);
        let encrypted_content = self.encrypt_content(note.content.to_string(), pubkey);
        note.content = encrypted_content;
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

        let signed_note = SignedNote::new(note, id, sig);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_keys() {
        let user_keys =
            UserKeys::new("a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8")
                .unwrap();
        let public_key = user_keys.get_public_key();
        assert_eq!(
            public_key,
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );
        let npub = user_keys.get_npub();
        assert_eq!(
            npub,
            "npub1dz2q85uqsf6g383hrnl98skh36c9wsafvnxxp5aju4vzf687ws9q7zr8df"
        );
        let nsec_key =
            UserKeys::new("nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53")
                .unwrap();
        let nsec_pubkey = nsec_key.get_public_key();
        let nsec_npub = nsec_key.get_npub();
        assert_eq!(nsec_pubkey, public_key);
        assert_eq!(nsec_npub, npub);
    }

    #[test]
    fn test_mnemonic() {
        let user_keys = UserKeys::generate_extractable();
        let mnemonic = user_keys.get_mnemonic_phrase();
        let spanish_mnemonic = user_keys.get_mnemonic_spanish();
        assert_eq!(
            UserKeys::parse_mnemonic(&mnemonic)
                .unwrap()
                .get_public_key(),
            user_keys.get_public_key()
        );
        assert_eq!(
            UserKeys::parse_mnemonic(&spanish_mnemonic)
                .unwrap()
                .get_public_key(),
            user_keys.get_public_key()
        );
    }

    #[test]
    fn test_extractable() {
        let user_keys = UserKeys::generate_extractable();
        let safe_user_keys = UserKeys::generate();
        let public_key = user_keys.get_public_key();
        let nsec = user_keys.get_nsec();
        let mnemonic = user_keys.get_mnemonic_phrase();
        let spanish_mnemonic = user_keys.get_mnemonic_spanish();
        assert_eq!(
            UserKeys::new_extractable(&nsec).unwrap().get_public_key(),
            public_key
        );
        assert_eq!(safe_user_keys.get_nsec(), "Not extractable".to_string());
        assert_eq!(
            safe_user_keys.get_mnemonic_phrase(),
            "Not extractable".to_string()
        );
        assert_eq!(
            UserKeys::parse_mnemonic(&mnemonic)
                .unwrap()
                .get_public_key(),
            public_key
        );
        assert_eq!(
            UserKeys::parse_mnemonic(&spanish_mnemonic)
                .unwrap()
                .get_public_key(),
            public_key
        );
    }
}
