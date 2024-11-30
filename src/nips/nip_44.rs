use base64::{engine::general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;

use crate::userkeys::UserKeys;

pub struct Nip44 {
    private_key: UserKeys,
    peer_pubkey: String,
}
impl Nip44 {
    pub fn new(private_key: UserKeys, peer_pubkey: String) -> Self {
        Nip44 {
            private_key,
            peer_pubkey,
        }
    }
    pub fn nip_44_encrypt(&self, plaintext: String) -> anyhow::Result<String> {
        let shared_secret = self.private_key.get_shared_point(&self.peer_pubkey)?;
        let conversation_key = Self::derive_conversation_key(&shared_secret, b"nip44-v2")?;
        let nonce = Self::generate_nonce();
        let cypher_text = Self::encrypt(plaintext.as_bytes(), &conversation_key, &nonce)?;
        let mac = Self::calculate_mac(&cypher_text, &conversation_key)?;
        let encoded_params = Self::base64_encode_params(b"1", &nonce, &cypher_text, &mac);
        Ok(encoded_params)
    }
    pub fn nip_44_decrypt(&self, cyphertext: String) -> anyhow::Result<String> {
        let shared_secret = self.private_key.get_shared_point(&self.peer_pubkey)?;
        let conversation_key = Self::derive_conversation_key(&shared_secret, b"nip44-v2")?;
        let decoded = general_purpose::STANDARD.decode(cyphertext.as_bytes())?;
        let (_version, nonce, ciphertext, _mac) = Self::extract_components(&decoded)?;
        let decrypted = Self::decrypt(&ciphertext, &conversation_key, &nonce)?;
        Ok(String::from_utf8(decrypted)?)
    }
    fn encrypt(content: &[u8], key: &[u8], nonce: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut cipher = ChaCha20::new(key.into(), nonce.into());
        let mut padded_content = Self::pad_string(content).map_err(|e| anyhow::anyhow!(e))?;
        cipher.apply_keystream(&mut padded_content);

        Ok(padded_content)
    }
    fn decrypt(ciphertext: &[u8], key: &[u8], nonce: &[u8]) -> anyhow::Result<Vec<u8>> {
        if key.len() != 32 || nonce.len() != 12 {
            Err(anyhow::anyhow!("Invalid key or nonce length"))?;
        }
        let mut cipher = ChaCha20::new_from_slices(key, nonce).map_err(|e| anyhow::anyhow!(e))?;
        let mut decrypted = ciphertext.to_vec();
        cipher.apply_keystream(&mut decrypted);
        // Extract the plaintext length
        if decrypted.len() < 2 {
            Err(anyhow::anyhow!("Invalid decrypted length"))?;
        }
        let plaintext_length = u16::from_be_bytes([decrypted[0], decrypted[1]]) as usize;
        // Validate and extract the plaintext
        if plaintext_length > decrypted.len() - 2 {
            Err(anyhow::anyhow!("Invalid plaintext length"))?;
        }
        Ok(decrypted[2..2 + plaintext_length].to_vec())
    }
    fn derive_conversation_key(shared_secret: &[u8], salt: &[u8]) -> anyhow::Result<[u8; 32]> {
        let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret);
        let mut okm = [0u8; 32]; // Output Keying Material (OKM)
        hkdf.expand(&[], &mut okm).map_err(|e| anyhow::anyhow!(e))?;
        Ok(okm)
    }
    fn extract_components(
        decoded: &[u8],
    ) -> anyhow::Result<(&[u8], &[u8], &[u8], &[u8])> {
        const VERSION_SIZE: usize = 1;
        const NONCE_SIZE: usize = 12;
        const MAC_SIZE: usize = 32;
        // Ensure the length of the decoded data is sufficient
        if decoded.len() < VERSION_SIZE + NONCE_SIZE + MAC_SIZE {
            Err(anyhow::anyhow!("Decoded data too short"))?;
        }
        let version = &decoded[0..VERSION_SIZE];
        let nonce = &decoded[VERSION_SIZE..VERSION_SIZE + NONCE_SIZE];
        let mac = &decoded[decoded.len() - MAC_SIZE..];
        let ciphertext = &decoded[VERSION_SIZE + NONCE_SIZE..decoded.len() - MAC_SIZE];

        Ok((version, nonce, ciphertext, mac))
    }

    fn generate_nonce() -> [u8; 12] {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        nonce
    }

    fn calculate_mac(data: &[u8], key: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|e| anyhow::anyhow!(e))?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }
    fn base64_encode_params(version: &[u8], nonce: &[u8], ciphertext: &[u8], mac: &[u8]) -> String {
        let mut encoded_data =
            Vec::with_capacity(version.len() + nonce.len() + ciphertext.len() + mac.len());
        encoded_data.extend_from_slice(version);
        encoded_data.extend_from_slice(nonce);
        encoded_data.extend_from_slice(ciphertext);
        encoded_data.extend_from_slice(mac);

        general_purpose::STANDARD.encode(&encoded_data)
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
    fn test_nip_44() {
        let user_keys_1 = crate::userkeys::UserKeys::generate_extractable();
        let user_keys_2 = crate::userkeys::UserKeys::generate_extractable();
        let nip_44_1 = Nip44 {
            private_key: user_keys_1.clone(),
            peer_pubkey: user_keys_2.get_public_key(),
        };
        let nip_44_2 = Nip44 {
            private_key: user_keys_2,
            peer_pubkey: user_keys_1.get_public_key(),
        };
        let plaintext = "Hello, World!".to_string();
        let cyphertext = nip_44_1.nip_44_encrypt(plaintext.clone()).unwrap();
        let decrypted = nip_44_2.nip_44_decrypt(cyphertext).unwrap();

        assert_eq!(decrypted, plaintext);
    }
}
