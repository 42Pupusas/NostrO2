//! Constructor / encoding helpers shared across backends.
//!
//! `KeypairExt` is a local trait, so it can be implemented on the foreign
//! curve-library types (`k256::schnorr::SigningKey`, `secp256k1::Keypair`)
//! despite the orphan rule.
//!
//! Each backend provides a single primitive — `from_secret_bytes` — and the
//! hex / nsec / mnemonic / npub / `from_any` constructors fall out as default
//! methods. That's how the per-backend code stays at ~30 lines.

use nostro2_traits::NostrKeypair;

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
        let (hrp, data) = bech32::decode(nsec)?;
        if hrp.as_str() != "nsec" {
            return Err(NostrKeypairError::HrpParseError);
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
    fn from_mnemonic(mnemonic: &str, language: xinachtli::Language) -> Result<Self, NostrKeypairError> {
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
    /// The mnemonic fallback only tries English and Spanish — those are the
    /// languages this crate compiles support for (see `xinachtli` features in
    /// `Cargo.toml`). For other BIP-39 languages, call
    /// [`from_mnemonic`](Self::from_mnemonic) directly with the right
    /// [`xinachtli::Language`].
    ///
    /// # Errors
    /// Returns `InvalidKey` if no encoding matches.
    fn from_any(value: &str) -> Result<Self, NostrKeypairError> {
        // `nsec1` is the bech32 HRP + separator. `starts_with("nsec")` would
        // also match strings like "nsection" and waste a decode attempt.
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
    /// Returns an error if bech32 encoding fails (unreachable for valid HRP).
    fn npub(&self) -> Result<String, NostrKeypairError> {
        let hrp = bech32::Hrp::parse("npub").map_err(|_| NostrKeypairError::HrpParseError)?;
        Ok(bech32::encode::<bech32::Bech32>(hrp, &self.pubkey_bytes())?)
    }

    /// Encode the secret key as `nsec1…` bech32.
    ///
    /// # Errors
    /// Returns an error if bech32 encoding fails (unreachable for valid HRP).
    fn nsec(&self) -> Result<String, NostrKeypairError> {
        let hrp = bech32::Hrp::parse("nsec").map_err(|_| NostrKeypairError::HrpParseError)?;
        Ok(bech32::encode::<bech32::Bech32>(hrp, &self.secret_bytes())?)
    }
}
