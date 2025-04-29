#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
#![allow(clippy::future_not_send)]

use wasm_bindgen::prelude::*;

#[derive(Debug)]
pub enum NostrWindowObjectError {
    NotAvailable,
    NotReady,
    NotNostr,
}
impl std::fmt::Display for NostrWindowObjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable => write!(f, "Nostr is not available"),
            Self::NotReady => write!(f, "Nostr is not ready"),
            Self::NotNostr => write!(f, "Nostr is not a Nostr object"),
        }
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
    pub type NostrWindowObject;

    #[wasm_bindgen(method, js_name = getPublicKey)]
    pub async fn get_public_key(this: &NostrWindowObject) -> JsValue;

    #[wasm_bindgen(method, js_name = signEvent)]
    #[wasm_bindgen(catch)]
    pub async fn sign_event(this: &NostrWindowObject, event: JsValue) -> Result<JsValue, JsValue>;

    pub type NostrWindowObjectEncryption;

    #[wasm_bindgen(method, js_name = encrypt)]
    #[wasm_bindgen(catch)]
    pub async fn encrypt(
        this: &NostrWindowObjectEncryption,
        pubkey: JsValue,
        plaintext: JsValue,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, js_name = decrypt)]
    #[wasm_bindgen(catch)]
    pub async fn decrypt(
        this: &NostrWindowObjectEncryption,
        pubkey: JsValue,
        ciphertext: JsValue,
    ) -> Result<JsValue, JsValue>;
}
impl NostrWindowObject {
    pub async fn new() -> Option<Self> {
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
    /// Returns the public key from the web extension.
    ///
    /// # Errors
    ///
    /// If the Nostr client is not available or the public key is not available.
    pub async fn public_key(&self) -> Result<String, NostrWindowObjectError> {
        self.get_public_key()
            .await
            .as_string()
            .ok_or(NostrWindowObjectError::NotAvailable)
    }
    /// Returns the public key from the web extension.
    ///
    /// # Errors
    ///
    /// If the Nostr client is not available or the public key is not available.
    #[cfg(target_arch = "wasm32")]
    pub async fn sign_note(
        &self,
        event: nostro2::note::NostrNote,
    ) -> Result<nostro2::note::NostrNote, NostrWindowObjectError> {
        let event: JsValue = event.into();
        let signed_event = self
            .sign_event(event)
            .await
            .map_err(|_| NostrWindowObjectError::NotAvailable)?;
        Ok(TryInto::<nostro2::note::NostrNote>::try_into(signed_event)
            .map_err(|_| NostrWindowObjectError::NotAvailable)?)
    }
    /// Signs a Nostr event using the Nostr `Nip 07` extension.
    ///
    /// # Errors
    ///
    /// If the Nostr client is not available or the event is not available.
    pub async fn encrypt(
        &self,
        pubkey: &str,
        plaintext: &str,
    ) -> Result<String, NostrWindowObjectError> {
        let nip_44 = web_sys::js_sys::Reflect::get(self, &"nip44".into())?
            .unchecked_into::<crate::NostrWindowObjectEncryption>();
        nip_44
            .encrypt(pubkey.into(), plaintext.into())
            .await?
            .as_string()
            .ok_or(NostrWindowObjectError::NotAvailable)
    }
    /// Decrypts a Nostr event using the Nostr `Nip 07` extension.
    ///
    /// # Errors
    ///
    /// If the Nostr client is not available or the event is not available.
    pub async fn decrypt(
        &self,
        pubkey: &str,
        ciphertext: &str,
    ) -> Result<String, NostrWindowObjectError> {
        let nip_44 = web_sys::js_sys::Reflect::get(self, &"nip44".into())?
            .unchecked_into::<crate::NostrWindowObjectEncryption>();
        nip_44
            .decrypt(pubkey.into(), ciphertext.into())
            .await?
            .as_string()
            .ok_or(NostrWindowObjectError::NotAvailable)
    }
}

#[cfg(test)]
mod tests {
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _window_nostr_decrypt() {
        let nostr = crate::NostrWindowObject::new()
            .await
            .expect("nostr is not available");
        let public_key = nostr
            .public_key()
            .await
            .expect("public key is not available");
        let plaintext = "Hello, world!";
        let ciphertext = nostr
            .encrypt(&public_key, &plaintext)
            .await
            .expect("encryption failed");
        let decrypted = nostr
            .decrypt(&public_key, &ciphertext)
            .await
            .expect("decryption failed");
        assert!(
            decrypted == plaintext,
            "decrypted is not the same as plaintext"
        );
    }
}
