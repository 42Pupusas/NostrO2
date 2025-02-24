use bech32::{Bech32, Hrp};
use bip39::Language;

use secp256k1::{rand::rngs::OsRng, Keypair, Secp256k1};

use crate::{
    nips::{Nip04, Nip44},
    notes::NostrNote,
};

#[derive(Debug, PartialEq, Clone, Eq)]
pub struct NostrKeypair {
    keypair: Keypair,
    extractable: bool,
}

impl NostrKeypair {
    pub fn generate(extractable: bool) -> Self {
        let keypair = Keypair::new(&Secp256k1::signing_only(), &mut OsRng);
        Self {
            keypair,
            extractable,
        }
    }
    pub fn make_extractable(&mut self) {
        self.extractable = true;
    }
    pub fn public_key(&self) -> String {
        return self.keypair.public_key().x_only_public_key().0.to_string();
    }
    pub fn public_key_slice(&self) -> [u8; 32] {
        return self.keypair.public_key().x_only_public_key().0.serialize();
    }
    pub fn npub(&self) -> String {
        let hrp = Hrp::parse("npub").expect("valid hrp");
        let pk_data = self.keypair.public_key().x_only_public_key().0.serialize();
        let string = bech32::encode::<Bech32>(hrp, &pk_data).expect("failed to encode string");
        string
    }
    pub fn sign_nostr_event(&self, note: &mut NostrNote) {
        if note.serialize_id().is_ok() {
            let secp = Secp256k1::signing_only();
            let sig = secp
                .sign_schnorr_no_aux_rand(note.id_bytes().as_ref().unwrap(), &self.keypair)
                .to_string();
            note.sig = Some(sig);
        }
    }
    pub fn get_shared_point(&self, public_key_string: &String) -> anyhow::Result<[u8; 32]> {
        let hex_pk = Self::hex_decode(public_key_string);
        let x_only_public_key = secp256k1::XOnlyPublicKey::from_slice(hex_pk.as_slice())?;
        let public_key = secp256k1::PublicKey::from_x_only_public_key(
            x_only_public_key,
            secp256k1::Parity::Even,
        );
        let mut ssp = secp256k1::ecdh::shared_secret_point(&public_key, &self.keypair.secret_key())
            .as_slice()
            .to_owned();
        ssp.resize(32, 0); // toss the Y part
        Ok(ssp.try_into().unwrap())
    }
    pub fn encrypt_nip_04_plaintext(
        &self,
        plaintext: String,
        pubkey: String,
    ) -> anyhow::Result<String> {
        let nip_04 = Nip04::new(self.clone(), pubkey);
        nip_04.encrypt(plaintext)
    }
    pub fn decrypt_nip_04_plaintext(
        &self,
        cyphertext: String,
        pubkey: String,
    ) -> anyhow::Result<String> {
        let nip_04 = Nip04::new(self.clone(), pubkey);
        nip_04.decrypt(cyphertext)
    }
    pub fn encrypt_nip_44_plaintext(
        &self,
        plaintext: String,
        pubkey: String,
    ) -> anyhow::Result<String> {
        let nip_44 = Nip44::new(self.clone(), pubkey);
        nip_44.nip_44_encrypt(plaintext)
    }
    pub fn decrypt_nip_44_plaintext(
        &self,
        cyphertext: String,
        pubkey: String,
    ) -> anyhow::Result<String> {
        let nip_44 = Nip44::new(self.clone(), pubkey);
        nip_44.nip_44_decrypt(cyphertext)
    }
    pub fn sign_nip_04_encrypted(
        &self,
        note: &mut NostrNote,
        pubkey: String,
    ) -> anyhow::Result<()> {
        note.tags.add_pubkey_tag(&pubkey);
        let encrypted_content = self.encrypt_nip_04_plaintext(note.content.to_string(), pubkey)?;
        note.content = encrypted_content;
        self.sign_nostr_event(note);
        Ok(())
    }
    pub fn decrypt_nip_04_content(&self, signed_note: &NostrNote) -> anyhow::Result<String> {
        let cyphertext = signed_note.content.to_string();
        let public_key_string = signed_note.pubkey.to_string();

        let plaintext = self.decrypt_nip_04_plaintext(cyphertext, public_key_string)?;
        Ok(plaintext)
    }
    pub fn sign_nip_44_encrypted(
        &self,
        note: &mut NostrNote,
        pubkey: String,
    ) -> anyhow::Result<()> {
        note.tags.add_pubkey_tag(&pubkey);
        let encrypted_content = self.encrypt_nip_44_plaintext(note.content.to_string(), pubkey)?;
        note.content = encrypted_content;
        self.sign_nostr_event(note);
        Ok(())
    }
    pub fn decrypt_nip_44_content(&self, signed_note: &NostrNote) -> anyhow::Result<String> {
        let cyphertext = signed_note.content.to_string();
        let public_key_string = signed_note.pubkey.to_string();
        let plaintext = self.decrypt_nip_44_plaintext(cyphertext, public_key_string)?;
        Ok(plaintext)
    }
    pub fn get_secret_key(&self) -> [u8; 32] {
        if !self.extractable {
            return [0u8; 32];
        }
        self.keypair.secret_key().secret_bytes()
    }
    pub fn get_nsec(&self) -> anyhow::Result<String> {
        if !self.extractable {
            return Err(anyhow::anyhow!("Not extractable"));
        }
        let secret_key = self.keypair.secret_key().secret_bytes();
        let hrp = Hrp::parse("nsec").expect("valid hrp");
        let string = bech32::encode::<Bech32>(hrp, &secret_key).expect("failed to encode string");
        Ok(string)
    }
    pub fn get_mnemonic(&self, language: Language) -> anyhow::Result<String> {
        if !self.extractable {
            return Err(anyhow::anyhow!("Not extractable"));
        }
        let secret_key = self.keypair.secret_key().secret_bytes();
        let mnemonic = bip39::Mnemonic::from_entropy_in(language, &secret_key).unwrap();
        Ok(mnemonic.words().collect::<Vec<&str>>().join(" "))
    }
    pub fn parse_mnemonic(
        mnemonic: &str,
        language: Language,
        extractable: bool,
    ) -> anyhow::Result<Self> {
        let mnemonic = bip39::Mnemonic::parse_in(language, mnemonic)?;
        let keypair: Self = mnemonic.try_into()?;
        match extractable {
            true => {
                let mut keypair = keypair;
                keypair.make_extractable();
                Ok(keypair)
            }
            false => Ok(keypair),
        }
    }
    fn hex_decode(hex_string: &str) -> Vec<u8> {
        hex_string
            .as_bytes()
            .chunks(2)
            .filter_map(|b| u8::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok())
            .collect()
    }
}
impl TryFrom<&[u8]> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let keypair = Keypair::from_seckey_slice(&Secp256k1::signing_only(), value)
            .map_err(|_| anyhow::anyhow!("Invalid private key"))?;
        Ok(Self {
            keypair,
            extractable: false,
        })
    }
}
impl From<[u8; 64]> for NostrKeypair {
    fn from(value: [u8; 64]) -> Self {
        let keypair = Keypair::from_seckey_slice(&Secp256k1::signing_only(), &value).unwrap();
        Self {
            keypair,
            extractable: false,
        }
    }
}
impl From<&[u8; 64]> for NostrKeypair {
    fn from(value: &[u8; 64]) -> Self {
        let keypair = Keypair::from_seckey_slice(&Secp256k1::signing_only(), value).unwrap();
        Self {
            keypair,
            extractable: false,
        }
    }
}
impl TryFrom<&str> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.starts_with("nsec") {
            true => {
                let (hrp, data) = bech32::decode(&value)?;
                if hrp.to_string() != "nsec" {
                    anyhow::bail!("Invalid nsec prefix");
                }
                Self::try_from(data.as_slice())
            }
            false => {
                let hex = Self::hex_decode(value);
                Self::try_from(hex.as_slice())
            }
        }
    }
}
impl TryFrom<bip39::Mnemonic> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: bip39::Mnemonic) -> Result<Self, Self::Error> {
        Self::try_from(value.to_entropy().as_slice())
    }
}
impl TryFrom<String> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        NostrKeypair::try_from(value.as_str())
    }
}
impl TryFrom<&String> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: &String) -> Result<Self, Self::Error> {
        NostrKeypair::try_from(value.as_str())
    }
}
#[cfg(feature = "wasm")]
impl TryFrom<web_sys::js_sys::ArrayBuffer> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: web_sys::js_sys::ArrayBuffer) -> Result<Self, Self::Error> {
        let uint8_array = web_sys::js_sys::Uint8Array::new(&value);
        NostrKeypair::try_from(uint8_array)
    }
}
#[cfg(feature = "wasm")]
impl TryFrom<web_sys::js_sys::Uint8Array> for NostrKeypair {
    type Error = anyhow::Error;
    fn try_from(value: web_sys::js_sys::Uint8Array) -> Result<Self, Self::Error> {
        value.to_vec().as_slice().try_into()
    }
}
#[cfg(feature = "wasm")]
impl Into<web_sys::js_sys::Object> for NostrKeypair {
    fn into(self) -> web_sys::js_sys::Object {
        let array: web_sys::js_sys::Uint8Array = self.into();
        let key_object: web_sys::js_sys::Object = array.buffer().into();
        key_object
    }
}
#[cfg(feature = "wasm")]
impl Into<web_sys::js_sys::Uint8Array> for NostrKeypair {
    fn into(self) -> web_sys::js_sys::Uint8Array {
        let key = self.keypair.secret_key().secret_bytes();
        web_sys::js_sys::Uint8Array::from(&key[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_keys() {
        let user_keys = NostrKeypair::try_from(
            "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8",
        )
        .expect("Failed to create NostrKeypair!");
        let public_key = user_keys.public_key();
        assert_eq!(
            public_key,
            "689403d3808274889e371cfe53c2d78eb05743a964cc60d3b2e55824e8fe740a"
        );
        let npub = user_keys.npub();
        assert_eq!(
            npub,
            "npub1dz2q85uqsf6g383hrnl98skh36c9wsafvnxxp5aju4vzf687ws9q7zr8df"
        );
        let nsec_key = NostrKeypair::try_from(
            "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53",
        )
        .unwrap();
        let nsec_pubkey = nsec_key.public_key();
        let nsec_npub = nsec_key.npub();
        assert_eq!(nsec_pubkey, public_key);
        assert_eq!(nsec_npub, npub);
    }

    #[test]
    fn test_mnemonic() {
        let user_keys = NostrKeypair::generate(true);
        let mnemonic = user_keys.get_mnemonic(Language::English).unwrap();
        let spanish_mnemonic = user_keys.get_mnemonic(Language::Spanish).unwrap();
        assert_eq!(
            NostrKeypair::parse_mnemonic(&mnemonic, Language::English, false)
                .unwrap()
                .public_key(),
            user_keys.public_key()
        );
        assert_eq!(
            NostrKeypair::parse_mnemonic(&spanish_mnemonic, Language::Spanish, false)
                .unwrap()
                .public_key(),
            user_keys.public_key()
        );
    }

    #[test]
    fn test_extractable() {}

    #[test]
    fn test_encryption() {
        let user_keys = NostrKeypair::generate(false);
        let client_keys = NostrKeypair::generate(false);
        let mut note_request = NostrNote {
            pubkey: user_keys.public_key(),
            kind: 24133,
            content: "test".to_string(),
            ..Default::default()
        };
        user_keys
            .sign_nip_04_encrypted(&mut note_request, client_keys.public_key())
            .unwrap();
        let decrypted = client_keys.decrypt_nip_04_content(&note_request).unwrap();
        assert_eq!(decrypted, "test");

        let mut nip_44_note_request = NostrNote {
            pubkey: user_keys.public_key(),
            kind: 24133,
            content: "test".to_string(),
            ..Default::default()
        };
        user_keys
            .sign_nip_44_encrypted(&mut nip_44_note_request, client_keys.public_key())
            .expect("");
        let decrypted_nip_44 = client_keys
            .decrypt_nip_44_content(&nip_44_note_request)
            .expect("");
        assert_eq!(decrypted_nip_44, "test");
    }
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test;
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    fn test_encryption_wasm() {
        let user_keys = NostrKeypair::generate(false);
        let client_keys = NostrKeypair::generate(false);
        let mut note_request = NostrNote {
            pubkey: user_keys.public_key(),
            kind: 24133,
            content: "test".to_string(),
            ..Default::default()
        };
        user_keys
            .sign_nip_04_encrypted(&mut note_request, client_keys.public_key())
            .unwrap();
        let decrypted = client_keys.decrypt_nip_04_content(&note_request).unwrap();
        assert_eq!(decrypted, "test");

        let mut nip_44_note_request = NostrNote {
            pubkey: user_keys.public_key(),
            kind: 24133,
            content: "test".to_string(),
            ..Default::default()
        };
        user_keys
            .sign_nip_44_encrypted(&mut nip_44_note_request, client_keys.public_key())
            .expect("");
        let decrypted_nip_44 = client_keys
            .decrypt_nip_44_content(&nip_44_note_request)
            .expect("");
        assert_eq!(decrypted_nip_44, "test");
    }
}
