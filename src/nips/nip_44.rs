use base64::{engine::general_purpose, Engine as _};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use secp256k1::Keypair;
use sha2::Sha256;

use crate::utils::get_shared_point;

pub fn nip_44_encrypt(
    private_key: Keypair,
    plaintext: String,
    public_key_string: String,
) -> anyhow::Result<String> {
    let shared_secret = get_shared_point(private_key, public_key_string)?;
    let conversation_key = derive_conversation_key(&shared_secret, b"nip44-v2")?;
    let nonce = generate_nonce();
    let cypher_text = encrypt(plaintext.as_bytes(), &conversation_key, &nonce)?;
    let mac = calculate_mac(&cypher_text, &conversation_key)?;
    let encoded_params = base64_encode_params(b"1", &nonce, &cypher_text, &mac);
    Ok(encoded_params)
}

pub fn nip_44_decrypt(
    private_key: Keypair,
    cyphertext: String,
    public_key_string: String,
) -> anyhow::Result<String> {
    let shared_secret = get_shared_point(private_key, public_key_string)?;
    let conversation_key = derive_conversation_key(&shared_secret, b"nip44-v2")?;
    let decoded = general_purpose::STANDARD.decode(cyphertext.as_bytes())?;
    let (_version, nonce, ciphertext, _mac) = extract_components(&decoded)?;
    let decrypted = decrypt(&ciphertext, &conversation_key, &nonce)?;
    Ok(String::from_utf8(decrypted)?)
}

fn encrypt(content: &[u8], key: &[u8], nonce: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut cipher = ChaCha20::new(key.into(), nonce.into());
    let mut padded_content = pad_string(content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    cipher.apply_keystream(&mut padded_content);

    Ok(padded_content)
}

fn decrypt(ciphertext: &[u8], key: &[u8], nonce: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    if key.len() != 32 || nonce.len() != 12 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid key or nonce length",
        ));
    }

    let mut cipher = ChaCha20::new_from_slices(key, nonce)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut decrypted = ciphertext.to_vec();
    cipher.apply_keystream(&mut decrypted);
    // Extract the plaintext length
    if decrypted.len() < 2 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid ciphertext length",
        ));
    }
    let plaintext_length = u16::from_be_bytes([decrypted[0], decrypted[1]]) as usize;

    // Validate and extract the plaintext
    if plaintext_length > decrypted.len() - 2 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid plaintext length",
        ));
    }
    Ok(decrypted[2..2 + plaintext_length].to_vec())
}

fn derive_conversation_key(shared_secret: &[u8], salt: &[u8]) -> anyhow::Result<Vec<u8>> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret);
    let mut okm = [0u8; 32]; // Output Keying Material (OKM)
    hkdf.expand(&[], &mut okm).map_err(|e| anyhow::anyhow!(e))?;
    Ok(okm.to_vec())
}

fn extract_components(
    decoded: &[u8],
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>), std::io::Error> {
    // Define the sizes of the components
    const VERSION_SIZE: usize = 1; // Size of version in bytes
    const NONCE_SIZE: usize = 12; // Size of nonce in bytes
    const MAC_SIZE: usize = 32; // Size of MAC in bytes

    // Calculate minimum size and check if the decoded data is long enough
    if decoded.len() < VERSION_SIZE + NONCE_SIZE + MAC_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Decoded data too short",
        ));
    }

    let version = decoded[0..VERSION_SIZE].to_vec();
    let nonce = decoded[VERSION_SIZE..VERSION_SIZE + NONCE_SIZE].to_vec();
    let mac_start = decoded.len() - MAC_SIZE; // MAC is the last 16 bytes
    let mac = decoded[mac_start..].to_vec();
    let ciphertext = decoded[VERSION_SIZE + NONCE_SIZE..mac_start].to_vec();

    Ok((version, nonce, ciphertext, mac))
}

fn generate_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

fn calculate_mac(data: &[u8], key: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::new_keys;
    use secp256k1::Secp256k1;

    #[test]
    fn test_nip_44() {
        let secp = Secp256k1::new();
        let new_key = new_keys();
        let private_keypair = Keypair::from_secret_key(&secp, &new_key);
        let plaintext = "Hello, world!".to_string();
        let public_key_string =
            hex::encode(new_key.keypair(&secp).x_only_public_key().0.serialize());
        let cyphertext = nip_44_encrypt(
            private_keypair,
            plaintext.clone(),
            public_key_string.clone(),
        )
        .expect("Encryption failed");
        let decrypted = nip_44_decrypt(private_keypair, cyphertext, public_key_string)
            .expect("Decryption failed");
        assert_eq!(decrypted, plaintext);
    }
}
