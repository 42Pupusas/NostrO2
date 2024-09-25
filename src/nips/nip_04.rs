use crate::utils::get_shared_point;
use base64::{engine::general_purpose, Engine as _};
use libaes::Cipher;
use secp256k1::KeyPair;

pub fn nip_04_encrypt(
    private_keypair: KeyPair,
    plaintext: String,
    public_key_string: String,
) -> anyhow::Result<String> {
    let shared_secret = get_shared_point(private_keypair, public_key_string)?;
    let iv = rand::random::<[u8; 16]>();
    let mut cipher = Cipher::new_256(&shared_secret);
    cipher.set_auto_padding(true);
    let cyphertext = cipher.cbc_encrypt(&iv, plaintext.as_bytes());
    let base_64_cyphertext = general_purpose::STANDARD.encode(&cyphertext);
    let base_64_iv = general_purpose::STANDARD.encode(&iv);
    Ok(format!("{}?iv={}", base_64_cyphertext, base_64_iv))
}

pub fn nip_04_decrypt(
    private_keypair: KeyPair,
    cyphertext: String,
    public_key_string: String,
) -> anyhow::Result<String> {
    let shared_secret = get_shared_point(private_keypair, public_key_string)?;
    let mut parts = cyphertext.split('?');
    let base_64_cyphertext = parts.next().ok_or(anyhow::anyhow!("No cyphertext"))?;
    let base_64_iv = &parts.next().ok_or(anyhow::anyhow!("No iv"))?[3..]; // skip "iv="
    let cyphertext = general_purpose::STANDARD.decode(base_64_cyphertext.as_bytes())?;
    let iv = general_purpose::STANDARD.decode(base_64_iv.as_bytes())?;
    let mut cipher = Cipher::new_256(&shared_secret);
    cipher.set_auto_padding(true);
    let plaintext = cipher.cbc_decrypt(&iv, &cyphertext);
    Ok(String::from_utf8(plaintext)?)
}

#[cfg(test)]
mod tests {
    use crate::{nips::nip_46::Nip46Request, notes::SignedNote, userkeys::UserKeys};

    use super::*;

    #[test]
    fn test_nip_04() {
        let secp = secp256k1::Secp256k1::new();
        let new_key = crate::utils::new_keys();
        let private_keypair = KeyPair::from_secret_key(&secp, &new_key);
        let plaintext = "Hello, world!".to_string();
        let public_key_string =
            hex::encode(new_key.keypair(&secp).x_only_public_key().0.serialize());
        let cyphertext = nip_04_encrypt(
            private_keypair,
            plaintext.clone(),
            public_key_string.clone(),
        )
        .unwrap();
        println!("cyphertext: {}", cyphertext);
        let decrypted = nip_04_decrypt(private_keypair, cyphertext, public_key_string).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn second_test() {
        let cyphertext = "PXvfOGMyeWnkWIuuUEEvM8VvliPmf6OGiBT7SFXoWPloW9Cm+DURd9hf0mUrc6puB4jMfMYonJ+gsIKJJ1xx3nTtf9DW8IGylCl9o1LDOjZi71G3rqoJELptQxaQTr4iVACOpOC8/lVyBQtMXwcg9FkONbbbLJXxVXXPzFmXcSQfByD/+iIak68AlKnxJp9abHJwLIlgOeR+D49VCObnVT6LRKeYbRBJ0i2e+RVA0fA=?iv=t+eLXPQHfnaFfslDoi7mzg==";
        let public_key = "62dfdb53ea2282ef478f7cdbf77938ec1add74b2bcbc8d862cfe1df24ac72cba";
        let my_keys = UserKeys::new_extractable(
            "341fe1a3b23d0f1660a70e0395fcd7d09a73ff041a4a2cf4d0760b721eb14c55",
        )
        .expect("");
        let secp = secp256k1::Secp256k1::new();
        let keypair = KeyPair::from_seckey_slice(&secp, &my_keys.get_secret_key()).expect("");
        let plaintext =
            nip_04_decrypt(keypair, cyphertext.to_string(), public_key.to_string()).unwrap();
        println!("plaintext: {}", plaintext);
    }

    #[test]
    fn third_test() {
        let note_str = r#"
        {
    "id": "a0bec1e5b029394436ed20382f22b549e88b12ea079a0db4cb7091a0f585cc30",
    "pubkey": "51fedac7279d0b2898b154a08504e887c04e5483da5837869a1a88733923a614",
    "created_at": 1714628274,
    "kind": 24133,
    "tags": [
        [
            "p",
            "f27bc411c93b863d6b3ee6b301a10acf447cd5587270bc65f0523d0f15a5a97e"
        ]
    ],
    "content": "00WrsKbtrlik9kHFU5ZW37QxXHSTQRNPk+79GjSYVQS7c8/Kqg5eBgcZpTW6W2K5PSoXwdTQfIKe7mOFL7d5y4F+NHiW6dhvgN1zmnu07UD892SC7Xe4tMjyZdyhMzBg+Mkcj403EnBoQZvQ1vVM500G4DC/w9jvtST2cxBbGjBEt2+yx8nE9VrTeMTY2ZKKQBTap/32E+VlyI5A0hkrSCT3JsCAALLUxYVvnzAyo4s=?iv=loq8CPmRkoZgEn91wqcSFQ==",
    "sig": "9a712ba5ac6d4069f7e6a0029e739ea6754d43b5ee4d19e35123e3e1e15e939ad767e025d6cf3643dfbb0787913092d81999544b2d6de29a12590219b5b190cb"
    }
        "#;
        let signed_note = serde_json::from_str::<SignedNote>(note_str).unwrap();
        let my_keys = UserKeys::new_extractable(
            "341fe1a3b23d0f1660a70e0395fcd7d09a73ff041a4a2cf4d0760b721eb14c55",
        )
        .expect("");
        let respnse = Nip46Request::get_request_command(&signed_note, &my_keys);
        println!("response: {:?}", respnse);
    }
}
