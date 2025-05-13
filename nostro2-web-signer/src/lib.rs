#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
#![allow(clippy::future_not_send)]
// mod nips;
// pub use nips::{Nip44, Nip59};
//
// #[derive(Debug)]
// pub enum NostrSignerError {
//     NotAvailable,
//     NotReady,
//     NotNostr,
//     WindowObjectError(wasm_bindgen::JsValue),
//     BindgenError(serde_wasm_bindgen::Error),
// }
// #[allow(async_fn_in_trait)]
// pub trait NostrBrowserSigner: Sized {
//     /// Sign a Nostr note
//     ///
//     /// # Errors
//     ///
//     /// Returns an error if the note cannot be signed
//     /// or if the keypair is invalid
//     async fn sign_nostr_note(
//         &self,
//         note: nostro2::note::NostrNote,
//     ) -> Result<nostro2::note::NostrNote, NostrSignerError>;
//     async fn generate_new(extractable: bool) -> Result<Self, NostrSignerError>;
//     async fn public_key(&self) -> Result<String, NostrSignerError>;
//     async fn secret_key(&self) -> Result<String, NostrSignerError>;
// }

use wasm_bindgen::prelude::*;

#[derive(Debug)]
pub enum NostrWindowObjectError {
    NotAvailable,
    NotReady,
    NotNostr,
    JsError(wasm_bindgen::JsValue),
    SerdeError(serde_wasm_bindgen::Error),
}
impl std::fmt::Display for NostrWindowObjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format!("{self:?}").fmt(f)
    }
}
impl From<wasm_bindgen::JsValue> for NostrWindowObjectError {
    fn from(value: wasm_bindgen::JsValue) -> Self {
        if value.is_null() {
            Self::NotAvailable
        } else if value.is_undefined() {
            Self::NotReady
        } else if value.is_object() {
            Self::NotNostr
        } else {
            Self::NotAvailable
        }
    }
}
impl std::error::Error for NostrWindowObjectError {}

#[wasm_bindgen]
extern "C" {
    /// The `nostr` object is a global object that is injected by the Nostr client.
    /// It provides access to the Nostr protocol and its features.
    /// must be accessed through window.nostr.
    /// This object is available in the browser environment.

    #[derive(Debug, Clone)]
    type NostrWindowObject;

    #[wasm_bindgen(method, js_name = getPublicKey)]
    #[wasm_bindgen(catch)]
    async fn get_public_key(this: &NostrWindowObject) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, js_name = signEvent)]
    #[wasm_bindgen(catch)]
    async fn sign_event(this: &NostrWindowObject, event: JsValue) -> Result<JsValue, JsValue>;

    type NostrWindowNip44;

    #[wasm_bindgen(method, js_name = encrypt)]
    #[wasm_bindgen(catch)]
    async fn encrypt(
        this: &NostrWindowNip44,
        pubkey: JsValue,
        plaintext: JsValue,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, js_name = decrypt)]
    #[wasm_bindgen(catch)]
    async fn decrypt(
        this: &NostrWindowNip44,
        pubkey: JsValue,
        ciphertext: JsValue,
    ) -> Result<JsValue, JsValue>;

}
impl NostrWindowObject {
    async fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let document = window.document()?;
        if document.ready_state() != "completed" {
            let (sender, receiver) = futures::channel::oneshot::channel();
            let closure = wasm_bindgen::prelude::Closure::once_into_js(
                move |nostr: wasm_bindgen::JsValue| {
                    let _ = sender.send(nostr);
                },
            );
            if window
                .add_event_listener_with_callback("load", closure.as_ref().unchecked_ref())
                .is_ok()
            {
                let _ = receiver.await;
            }
        }
        window.get("nostr").map(JsCast::unchecked_into::<Self>)
    }
    fn nip_44(&self) -> Result<NostrWindowNip44, NostrWindowObjectError> {
        let nip_44 = web_sys::js_sys::Reflect::get(self, &JsValue::from_str("nip44"))
            .map_err(NostrWindowObjectError::JsError)?;
        wasm_bindgen_test::console_error!("Nip44: {nip_44:?}");
        if nip_44.is_null() || nip_44.is_undefined() {
            return Err(NostrWindowObjectError::NotAvailable);
        }
        let nip_44 = nip_44.unchecked_into::<NostrWindowNip44>();
        Ok(nip_44)
    }
    async fn encrypt(
        &self,
        pubkey: &str,
        plaintext: &str,
    ) -> Result<String, NostrWindowObjectError> {
        wasm_bindgen_test::console_error!("Encrypting before");
        let nip_44 = self.nip_44()?;
        wasm_bindgen_test::console_error!("Encrypting");
        let pubkey = JsValue::from_str(pubkey);
        let plaintext = JsValue::from_str(plaintext);
        let ciphertext = nip_44
            .encrypt(pubkey, plaintext)
            .await
            .map_err(NostrWindowObjectError::JsError)?;
        wasm_bindgen_test::console_error!("Encrypted");
        if ciphertext.is_null() || ciphertext.is_undefined() {
            return Err(NostrWindowObjectError::NotAvailable);
        }
        let ciphertext = ciphertext
            .as_string()
            .ok_or(NostrWindowObjectError::NotAvailable)?;
        wasm_bindgen_test::console_error!("Ciphertext: {ciphertext}");
        Ok(ciphertext)
    }
    async fn decrypt(
        &self,
        pubkey: &str,
        ciphertext: &str,
    ) -> Result<String, NostrWindowObjectError> {
        let nip_44 = self.nip_44()?;
        let pubkey = JsValue::from_str(pubkey);
        let ciphertext = JsValue::from_str(ciphertext);
        let plaintext = nip_44
            .decrypt(pubkey, ciphertext)
            .await
            .map_err(NostrWindowObjectError::JsError)?;
        if plaintext.is_null() || plaintext.is_undefined() {
            return Err(NostrWindowObjectError::NotAvailable);
        }
        let plaintext = plaintext
            .as_string()
            .ok_or(NostrWindowObjectError::NotAvailable)?;
        Ok(plaintext)
    }
}

pub struct NostrWindowSigner {
    nostr: NostrWindowObject,
}
impl NostrWindowSigner {
    /// Create a new `NostrWindowSigner`
    ///
    /// # Errors
    ///
    /// Returns an error if the `NostrWindowObject` cannot be created
    /// or if the Nostr client is not available.
    pub async fn new() -> Result<Self, NostrWindowObjectError> {
        let nostr = NostrWindowObject::new()
            .await
            .ok_or(NostrWindowObjectError::NotAvailable)?;
        Ok(Self { nostr })
    }
    /// Get the public key of the Nostr client
    ///
    /// # Errors
    ///
    /// Returns an error if the public key cannot be retrieved
    /// or if the keypair is invalid. If the extension signer is not
    /// available, it will return an error.
    pub async fn pubkey(&self) -> Result<String, NostrWindowObjectError> {
        self.nostr
            .get_public_key()
            .await
            .map_err(|_| NostrWindowObjectError::NotNostr)
            .map(|v| v.as_string().ok_or(NostrWindowObjectError::NotNostr))?
    }
    /// Sign a Nostr note
    ///
    /// # Errors
    ///
    /// Returns an error if the note cannot be signed
    /// or if the keypair is invalid. If the extension signer is not
    /// available, it will return an error.
    pub async fn sign_note(
        &self,
        note: nostro2::note::NostrNote,
    ) -> Result<nostro2::note::NostrNote, NostrWindowObjectError> {
        let signed = self
            .nostr
            .sign_event(
                serde_wasm_bindgen::to_value(&note).map_err(NostrWindowObjectError::SerdeError)?,
            )
            .await
            .map_err(NostrWindowObjectError::JsError)?;
        serde_wasm_bindgen::from_value(signed).map_err(NostrWindowObjectError::SerdeError)
    }
    /// Encrypt a message using the Nostr client
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot be Encrypted
    /// or if the keypair is invalid. If the extension signer is not
    /// available, it will return an error.
    pub async fn encrypt(
        &self,
        pubkey: &str,
        plaintext: &str,
    ) -> Result<String, NostrWindowObjectError> {
        self.nostr.encrypt(pubkey, plaintext).await
    }
    /// Decrypt a message using the Nostr client
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot be decrypted
    /// or if the keypair is invalid. If the extension signer is not
    /// available, it will return an error.
    pub async fn decrypt(
        &self,
        pubkey: &str,
        ciphertext: &str,
    ) -> Result<String, NostrWindowObjectError> {
        self.nostr.decrypt(pubkey, ciphertext).await
    }
}

#[derive(Debug)]
pub enum CryptoKeyError {
    NotAvailable,
    JsError(wasm_bindgen::JsValue),
    SecpError(secp256k1::Error),
}
impl std::fmt::Display for CryptoKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format!("{self:?}").fmt(f)
    }
}
impl std::error::Error for CryptoKeyError {}

static SECP: std::sync::LazyLock<secp256k1::Secp256k1<secp256k1::All>> =
    std::sync::LazyLock::new(secp256k1::Secp256k1::new);

#[derive(Debug)]
pub struct NostrCryptoKey {
    pubkey: String,
    keypair: web_sys::CryptoKey,
}
impl NostrCryptoKey {
    pub async fn generate() -> Result<Self, CryptoKeyError> {
        let new_secp_key = secp256k1::Keypair::new(&*SECP, &mut secp256k1::rand::thread_rng());
        let window = web_sys::window().ok_or(CryptoKeyError::NotAvailable)?;
        let crypto = window.crypto().map_err(CryptoKeyError::JsError)?.subtle();
        let secret_array =
            web_sys::js_sys::Uint8Array::new(&JsValue::from(new_secp_key.secret_bytes().to_vec()));
        wasm_bindgen_test::console_error!("Secret: {:?}", secret_array.length());
        let usages = web_sys::js_sys::Array::new();
        usages.push(&wasm_bindgen::JsValue::from("encrypt"));

        let params = web_sys::AesGcmParams::new("AES-GCM", &JsValue::from(256).into());
        let privkey_promise = crypto
            .import_key_with_object("raw", &secret_array.into(), &params.into(), true, &usages)
            .map_err(CryptoKeyError::JsError)?;
        let privkey = wasm_bindgen_futures::JsFuture::from(privkey_promise)
            .await
            .map_err(CryptoKeyError::JsError)?
            .unchecked_into::<web_sys::CryptoKey>();
        wasm_bindgen_test::console_error!("Privkey: {privkey:?}");

        let pubkey = new_secp_key.x_only_public_key().0.serialize();
        let pubkey = hex::encode(pubkey);
        wasm_bindgen_test::console_error!("Pubkey: {pubkey:?}");

        Ok(Self {
            pubkey,
            keypair: privkey,
        })
    }
    pub async fn secret_key(&self) -> Result<String, CryptoKeyError> {
        let window = web_sys::window().ok_or(CryptoKeyError::NotAvailable)?;
        let crypto = window.crypto().map_err(CryptoKeyError::JsError)?.subtle();
        let secret_key_promise = crypto
            .export_key("raw", &self.keypair)
            .map_err(CryptoKeyError::JsError)?;
        let secret_key = wasm_bindgen_futures::JsFuture::from(secret_key_promise).await;
        wasm_bindgen_test::console_error!("SecretKey: {secret_key:?}");
        let secret_key = secret_key
            .map_err(CryptoKeyError::JsError)?
            .unchecked_into::<web_sys::js_sys::ArrayBuffer>();
        let secret_key = web_sys::js_sys::Uint8Array::new(&secret_key);
        let secret_key = hex::encode(secret_key.to_vec());
        Ok(secret_key)
    }
    pub async fn new(key: &[u8; 32]) -> Result<Self, CryptoKeyError> {
        let window = web_sys::window().ok_or(CryptoKeyError::NotAvailable)?;
        let crypto = window.crypto().map_err(CryptoKeyError::JsError)?.subtle();
        let secret_array = web_sys::js_sys::Uint8Array::new(&JsValue::from(key.to_vec()));
        wasm_bindgen_test::console_error!("Secret: {:?}", secret_array.length());
        let usages = web_sys::js_sys::Array::new();
        usages.push(&wasm_bindgen::JsValue::from("encrypt"));

        let params = web_sys::AesGcmParams::new("AES-GCM", &JsValue::from(256).into());
        let privkey_promise = crypto
            .import_key_with_object("raw", &secret_array.into(), &params.into(), true, &usages)
            .map_err(CryptoKeyError::JsError)?;
        let privkey = wasm_bindgen_futures::JsFuture::from(privkey_promise)
            .await
            .map_err(CryptoKeyError::JsError)?
            .unchecked_into::<web_sys::CryptoKey>();
        wasm_bindgen_test::console_error!("Privkey: {privkey:?}");

        let pubkey = secp256k1::Keypair::from_secret_key(
            &*SECP,
            &secp256k1::SecretKey::from_slice(key).map_err(CryptoKeyError::SecpError)?,
        )
        .x_only_public_key()
        .0
        .serialize();
        let pubkey = hex::encode(pubkey);

        Ok(Self {
            pubkey,
            keypair: privkey,
        })
    }
}

#[cfg(test)]
mod tests {

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_crypto_key() {
        let crypto_key = super::NostrCryptoKey::generate()
            .await
            .map_err(|e| wasm_bindgen_test::console_error!("Error: {e:?}"));
        wasm_bindgen_test::console_error!("CryptoKey: {crypto_key:?}");
        assert!(crypto_key.is_ok());
        let crypto_key = crypto_key.unwrap();
        assert_eq!(crypto_key.pubkey.len(), 64);
        assert!(!crypto_key.pubkey.is_empty());
        assert!(!crypto_key.keypair.is_null());

        let secret_key = crypto_key.secret_key().await;
        assert!(secret_key.is_ok());
        wasm_bindgen_test::console_error!("SecretKey: {secret_key:?}");
        let secret_key = secret_key.unwrap();

        assert_eq!(secret_key.len(), 64);
        assert!(!secret_key.is_empty());
        let secret_key = hex::decode(secret_key).expect("Failed to decode secret key");
        assert_eq!(secret_key.len(), 32);
        wasm_bindgen_test::console_error!("SecretKey: {secret_key:?}");

        let new_key = super::NostrCryptoKey::new(&secret_key.try_into().unwrap())
            .await
            .expect("Failed to create new key");
        assert_eq!(new_key.pubkey, crypto_key.pubkey);
    }

    //#[wasm_bindgen_test::wasm_bindgen_test]
    async fn _window_object() {
        let nostr = super::NostrWindowObject::new().await;
        assert!(nostr.is_some());
        let nostr = nostr.unwrap();
        let public_key = nostr.get_public_key().await;
        assert!(public_key.is_ok());
        let public_key = public_key.unwrap();
        let public_key = public_key.as_string();
        assert!(public_key.is_some());
        let public_key = public_key.unwrap();
        assert_eq!(public_key.len(), 64);
        assert!(!public_key.is_empty());
        let content = "- .... .. ... / .. ... / .- / -- . ... ... .- --.";
        let note = nostro2::note::NostrNote {
            kind: 300,
            content: content.to_string(),
            ..Default::default()
        };
        let signed_note = nostr
            .sign_event(
                serde_wasm_bindgen::to_value(&note).expect("Failed to convert note to JsValue"),
            )
            .await;
        assert!(signed_note.is_ok());
        let signed_note = signed_note.unwrap();
        let signed_note: nostro2::note::NostrNote =
            serde_wasm_bindgen::from_value(signed_note).expect("Failed to convert JsValue to note");
        assert_eq!(signed_note.pubkey, public_key);
        assert_eq!(signed_note.kind, 300);
        assert_eq!(signed_note.content, content.to_string());
        assert!(signed_note.verify());

        let plaintext = "Hello, world!";
        let ciphertext = nostr
            .encrypt(&public_key, plaintext)
            .await
            .expect("Failed to encrypt");
        assert!(!ciphertext.is_empty());
        assert_ne!(ciphertext, plaintext);
        let decrypted = nostr
            .decrypt(&public_key, &ciphertext)
            .await
            .expect("Failed to decrypt");
        assert_eq!(decrypted, plaintext);
    }
}
