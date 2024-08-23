use std::fmt::Formatter;

use bip39::Language;
use secp256k1::{KeyPair, Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

use crate::nips::{
    nip_04::{nip_04_decrypt, nip_04_encrypt},
    nip_44::{nip_44_decrypt, nip_44_encrypt},
};

use super::notes::{Note, SignedNote};
use bech32::{Bech32, Hrp};

#[derive(Debug, PartialEq, Clone, Eq)]
pub struct UserKeys {
    keypair: KeyPair,
    extractable: bool,
}

#[derive(Debug)]
pub enum NostroError {
    DecryptionError(Box<dyn std::error::Error + Send>),
    EncryptionError(Box<dyn std::error::Error + Send>),
    DecodingError(Box<dyn std::error::Error + Send>),
    NsecError(Box<dyn std::error::Error + Send>),
    MnemonicError(Box<dyn std::error::Error + Send>),
    UnknownCommand,
}

impl std::error::Error for NostroError {
    fn description(&self) -> &str {
        match self {
            NostroError::DecryptionError(_) => "Failed to decrypt",
            NostroError::EncryptionError(_) => "Failed to encrypt",
            NostroError::DecodingError(_) => "Failed to decode",
            NostroError::NsecError(_) => "Failed to decode nsec",
            NostroError::MnemonicError(_) => "Failed to parse mnemonic",
            NostroError::UnknownCommand => "Unknown command",
        }
    }
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match self {
            NostroError::DecryptionError(e) => Some(e.as_ref()),
            NostroError::EncryptionError(e) => Some(e.as_ref()),
            NostroError::DecodingError(e) => Some(e.as_ref()),
            NostroError::NsecError(e) => Some(e.as_ref()),
            NostroError::MnemonicError(e) => Some(e.as_ref()),
            NostroError::UnknownCommand => None,
        }
    }
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NostroError::DecryptionError(e) => Some(e.as_ref()),
            NostroError::EncryptionError(e) => Some(e.as_ref()),
            NostroError::DecodingError(e) => Some(e.as_ref()),
            NostroError::NsecError(e) => Some(e.as_ref()),
            NostroError::MnemonicError(e) => Some(e.as_ref()),
            NostroError::UnknownCommand => None,
        }
    }
}

impl std::fmt::Display for NostroError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            NostroError::DecryptionError(e) => write!(f, "Failed to decrypt: {}", e),
            NostroError::EncryptionError(e) => write!(f, "Failed to encrypt: {}", e),
            NostroError::DecodingError(e) => write!(f, "Failed to decode: {}", e),
            NostroError::NsecError(e) => write!(f, "Failed to decode nsec: {}", e),
            NostroError::MnemonicError(e) => write!(f, "Failed to parse mnemonic: {}", e),
            NostroError::UnknownCommand => write!(f, "Unknown command"),
        }
    }
}

impl UserKeys {
    pub fn new(private_key: &str) -> Result<Self, NostroError> {
        // Check if the private key starts with "nsec"
        if private_key.starts_with("nsec") {
            let (hrp, data) = bech32::decode(&private_key)
                .map_err(|e| NostroError::DecodingError(Box::new(e)))?;
            if hrp.to_string() != "nsec" {
                return Err(NostroError::NsecError(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid nsec prefix",
                ))));
            }
            let secret_key = SecretKey::from_slice(&data)
                .map_err(|e| NostroError::DecryptionError(Box::new(e)))?;
            return Self::create_user_keys(secret_key, false);
        }

        // Decode the private key as hex
        let decoded_private_key =
            hex::decode(private_key).map_err(|e| NostroError::DecodingError(Box::new(e)))?;
        let secret_key = SecretKey::from_slice(&decoded_private_key)
            .map_err(|e| NostroError::DecryptionError(Box::new(e)))?;
        // Create and return UserKeys
        Self::create_user_keys(secret_key, false)
    }

    fn create_user_keys(secret_key: SecretKey, extractable: bool) -> Result<UserKeys, NostroError> {
        let secp = Secp256k1::new();
        let keypair = KeyPair::from_secret_key(&secp, &secret_key);
        Ok(UserKeys {
            keypair,
            extractable,
        })
    }

    pub fn new_extractable(private_key: &str) -> Result<Self, NostroError> {
        // Check if the private key starts with "nsec"
        if private_key.starts_with("nsec") {
            let (hrp, data) = bech32::decode(&private_key)
                .map_err(|e| NostroError::DecodingError(Box::new(e)))?;
            if hrp.to_string() != "nsec" {
                return Err(NostroError::NsecError(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid nsec prefix",
                ))));
            }
            let secret_key = SecretKey::from_slice(&data)
                .map_err(|e| NostroError::DecryptionError(Box::new(e)))?;
            return Self::create_user_keys(secret_key, true);
        }

        // Decode the private key as hex
        let decoded_private_key =
            hex::decode(private_key).map_err(|e| NostroError::DecodingError(Box::new(e)))?;
        let secret_key = SecretKey::from_slice(&decoded_private_key)
            .map_err(|e| NostroError::DecryptionError(Box::new(e)))?;
        // Create and return UserKeys
        Self::create_user_keys(secret_key, true)
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

    pub fn get_npub(&self) -> String {
        let hrp = Hrp::parse("npub").expect("valid hrp");
        let pk_data = self.keypair.public_key().x_only_public_key().0.serialize();
        let string = bech32::encode::<Bech32>(hrp, &pk_data).expect("failed to encode string");
        string
    }

    fn hash_id_and_sign(&self, note: &Note) -> (String, String) {
        let note_hash = note.serialize_for_nostr();
        let mut hasher = Sha256::new();
        hasher.update(note_hash);
        let hash_result = hasher.finalize();
        let id_message = Message::from_slice(&hash_result).unwrap();
        let id = hex::encode(hash_result);
        let secp = Secp256k1::new();
        let sig = secp
            .sign_schnorr_no_aux_rand(&id_message, &self.keypair)
            .to_string();
        (id, sig)
    }

    pub fn sign_nostr_event(&self, note: Note) -> SignedNote {
        // Serialize the event as JSON
        let (id, sig) = self.hash_id_and_sign(&note);
        let signed_note = SignedNote::new(note, id, sig);
        signed_note
    }

    pub fn encrypt_nip_04_plaintext(
        &self,
        plaintext: String,
        pubkey: String,
    ) -> Result<String, NostroError> {
        nip_04_encrypt(self.keypair, plaintext, pubkey)
    }

    pub fn decrypt_nip_04_plaintext(
        &self,
        cyphertext: String,
        pubkey: String,
    ) -> Result<String, NostroError> {
        nip_04_decrypt(self.keypair, cyphertext, pubkey)
    }

    pub fn encrypt_nip_44_plaintext(
        &self,
        plaintext: String,
        pubkey: String,
    ) -> Result<String, NostroError> {
        nip_44_encrypt(self.keypair, plaintext, pubkey)
    }

    pub fn decrypt_nip_44_plaintext(
        &self,
        cyphertext: String,
        pubkey: String,
    ) -> Result<String, NostroError> {
        nip_44_decrypt(self.keypair, cyphertext, pubkey)
    }

    pub fn sign_nip_04_encrypted(
        &self,
        mut note: Note,
        pubkey: String,
    ) -> Result<SignedNote, NostroError> {
        note.add_pubkey_tag(&pubkey);
        let encrypted_content = nip_04_encrypt(self.keypair, note.content.to_string(), pubkey)?;
        note.content = encrypted_content;
        let (id, sig) = self.hash_id_and_sign(&note);
        let signed_note = SignedNote::new(note, id, sig);
        Ok(signed_note)
    }

    pub fn decrypt_nip_04_content(&self, signed_note: &SignedNote) -> Result<String, NostroError> {
        let cyphertext = signed_note.get_content().to_string();
        let public_key_string = signed_note.get_pubkey().to_string();

        let plaintext = nip_04_decrypt(self.keypair, cyphertext, public_key_string)?;
        Ok(plaintext)
    }

    pub fn sign_nip_44_encrypted(
        &self,
        mut note: Note,
        pubkey: String,
    ) -> Result<SignedNote, NostroError> {
        note.add_pubkey_tag(&pubkey);
        let encrypted_content = nip_44_encrypt(self.keypair, note.content.to_string(), pubkey)?;
        note.content = encrypted_content;
        let (id, sig) = self.hash_id_and_sign(&note);
        let signed_note = SignedNote::new(note, id, sig);
        Ok(signed_note)
    }

    pub fn decrypt_nip_44_content(&self, signed_note: &SignedNote) -> Result<String, NostroError> {
        let cyphertext = signed_note.get_content().to_string();
        let public_key_string = signed_note.get_pubkey().to_string();
        let plaintext = nip_44_decrypt(self.keypair, cyphertext, public_key_string)?;
        Ok(plaintext)
    }

    pub fn get_secret_key(&self) -> [u8; 32] {
        if !self.extractable {
            return [0u8; 32];
        }
        self.keypair.secret_key().secret_bytes()
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

    pub fn parse_mnemonic(mnemonic: &str, extractable: bool) -> Result<Self, NostroError> {
        let english_parse = bip39::Mnemonic::parse_in(Language::English, mnemonic);
        let spanish_parse = bip39::Mnemonic::parse_in(Language::Spanish, mnemonic);
        if english_parse.is_err() && spanish_parse.is_err() {
            return Err(NostroError::MnemonicError(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid mnemonic",
            ))));
        }
        let mnemonic = if english_parse.is_ok() {
            english_parse.unwrap()
        } else {
            spanish_parse.unwrap()
        };
        let secret_key = mnemonic
            .to_entropy()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        match extractable {
            true => Self::create_user_keys(
                SecretKey::from_slice(&hex::decode(secret_key).unwrap()).unwrap(),
                true,
            ),
            false => Self::create_user_keys(
                SecretKey::from_slice(&hex::decode(secret_key).unwrap()).unwrap(),
                false,
            ),
        }
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
            UserKeys::parse_mnemonic(&mnemonic, false)
                .unwrap()
                .get_public_key(),
            user_keys.get_public_key()
        );
        assert_eq!(
            UserKeys::parse_mnemonic(&spanish_mnemonic, false)
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
            UserKeys::parse_mnemonic(&mnemonic, true)
                .unwrap()
                .get_public_key(),
            public_key
        );
        assert_eq!(
            UserKeys::parse_mnemonic(&spanish_mnemonic, true)
                .unwrap()
                .get_public_key(),
            public_key
        );
    }

    #[test]
    fn test_encryption() {
        let user_keys = UserKeys::generate();
        let client_keys = UserKeys::generate();
        let note_request = Note::new(&user_keys.get_public_key(), 24133, "test");
        let signed_note = user_keys
            .sign_nip_04_encrypted(note_request, client_keys.get_public_key())
            .unwrap();
        let decrypted = client_keys.decrypt_nip_04_content(&signed_note).unwrap();
        assert_eq!(decrypted, "test");

        let nip_44_note_request = Note::new(&user_keys.get_public_key(), 24133, "test");
        let signed_nip_44_note = user_keys
            .sign_nip_44_encrypted(nip_44_note_request, client_keys.get_public_key())
            .expect("");
        let decrypted_nip_44 = client_keys
            .decrypt_nip_44_content(&signed_nip_44_note)
            .expect("");
        assert_eq!(decrypted_nip_44, "test");
    }
}
