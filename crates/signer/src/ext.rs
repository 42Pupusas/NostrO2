//! Constructor / encoding helpers shared across backends.
//!
//! `KeypairExt` is a local trait, so it can be implemented on the foreign
//! curve-library types (`k256::schnorr::SigningKey`, `secp256k1::Keypair`)
//! despite the orphan rule.
//!
//! Each backend provides a single primitive — `from_secret_bytes` — and the
//! hex / nsec / mnemonic / npub / `from_any` constructors fall out as default
//! methods. That's how the per-backend code stays at ~30 lines.

use nostro2_traits::{NostrKeypair, SignerBech32, KeypairBech32};

use crate::errors::NostrKeypairError;

/// Constructor and bech32/mnemonic encoding helpers for in-memory keypairs.
pub trait KeypairExt: NostrKeypair + Sized {
    /// Build a keypair from raw 32-byte secret material. The only primitive
    /// each backend has to provide; everything else defaults on top.
    ///
    /// # Errors
    /// Returns an error if the bytes are not a valid scalar for the curve.
    fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, NostrKeypairError>;

    /// Parse from a 64-char hex secret key string.
    ///
    /// # Errors
    /// Returns an error if the string is not 64 hex chars or not a valid scalar.
    fn from_hex(hex: &str) -> Result<Self, NostrKeypairError> {
        let mut buf = [0_u8; 32];
        nostro2_traits::hex::FromHex::decode_hex_to_slice(hex, &mut buf)
            .map_err(|_| NostrKeypairError::InvalidKey)?;
        Self::from_secret_bytes(&buf)
    }

    /// Parse from an `nsec1…` bech32 string.
    ///
    /// # Errors
    /// Returns an error if the HRP is not `nsec` or the payload is not 32 bytes.
    fn from_nsec(nsec: &str) -> Result<Self, NostrKeypairError> {
        let (hrp, data) = nostro2_traits::bech32::Bech32Crypto::decode(nsec)?;
        if hrp != "nsec" {
            return Err(NostrKeypairError::InvalidKey);
        }
        let bytes: &[u8; 32] = data
            .as_slice()
            .try_into()
            .map_err(|_| NostrKeypairError::InvalidKey)?;
        Self::from_secret_bytes(bytes)
    }

    /// Parse from a BIP-39 mnemonic phrase.
    ///
    /// # Errors
    /// Returns an error if the mnemonic is invalid or the entropy is not a
    /// valid scalar.
    fn from_mnemonic(
        mnemonic: &str,
        language: xinachtli::Language,
    ) -> Result<Self, NostrKeypairError> {
        let mnemonic = xinachtli::Mnemonic::from_phrase(mnemonic, language)?;
        let bytes: &[u8; 32] = mnemonic
            .entropy()
            .try_into()
            .map_err(|_| NostrKeypairError::InvalidKey)?;
        Self::from_secret_bytes(bytes)
    }

    /// Try every supported encoding (nsec → hex → mnemonic in English then
    /// Spanish) and return the first that parses.
    ///
    /// # Errors
    /// Returns `InvalidKey` if no encoding matches.
    fn from_any(value: &str) -> Result<Self, NostrKeypairError> {
        if value.starts_with("nsec1") {
            if let Ok(kp) = Self::from_nsec(value) {
                return Ok(kp);
            }
        }
        if value.len() == 64 {
            if let Ok(kp) = Self::from_hex(value) {
                return Ok(kp);
            }
        }
        for language in [xinachtli::Language::English, xinachtli::Language::Spanish] {
            if let Ok(kp) = Self::from_mnemonic(value, language) {
                return Ok(kp);
            }
        }
        Err(NostrKeypairError::InvalidKey)
    }

    /// Render the secret key as a BIP-39 mnemonic in the given language.
    ///
    /// # Errors
    /// Returns an error if the entropy is not a valid 32-byte scalar.
    fn mnemonic(&self, language: xinachtli::Language) -> Result<String, NostrKeypairError> {
        let secret = self.secret_bytes();
        let m = xinachtli::Mnemonic::from_entropy(&secret, language)?;
        Ok(m.phrase())
    }

    /// Encode the public key as `npub1…` bech32.
    ///
    /// # Errors
    /// Returns an error if bech32 encoding fails.
    fn npub(&self) -> Result<String, NostrKeypairError> {
        Ok(SignerBech32::to_npub(self)?)
    }

    /// Encode the secret key as `nsec1…` bech32.
    ///
    /// # Errors
    /// Returns an error if bech32 encoding fails.
    fn nsec(&self) -> Result<String, NostrKeypairError> {
        Ok(KeypairBech32::to_nsec(self)?)
    }
}
