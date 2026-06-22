#![warn(
    clippy::all,
    clippy::missing_errors_doc,
    clippy::style,
    clippy::pedantic,
    clippy::nursery
)]
//! # `nostro2-traits`
//!
//! Signing surface for the `nostro2` ecosystem. Two traits, no data
//! structures, no curve dependencies — `nostro2` (data), `nostro2-nips`
//! (protocols), and `nostro2-signer` (impls) all depend on this crate.
//!
//! - [`NostrSigner`]: minimum signing surface (sign a 32-byte prehash, expose
//!   the x-only pubkey, generate a fresh signer). Hardware wallets, NIP-46
//!   remote signers, and browser extensions can implement this without ever
//!   exposing key material.
//! - [`NostrKeypair`]: extends [`NostrSigner`] with raw secret-key export and
//!   ECDH for in-process keypairs.
//! - [`hex`]: minimal hex encode/decode traits (`Hexable`, `FromHex`).

pub mod bech32;
pub mod hex;
use hex::{FromHex as _, Hexable as _};

// ── Private supertrait extensions (auto-implemented via blanket impls) ──

/// Auto-derived bech32 encoding for any signer.
///
/// Blanket-implemented for every [`NostrSigner`] — no extra work for
/// implementors. Used by `nostro2-signer` to provide `npub()` / `nsec()`
/// without reaching into the `bech32` module directly.
#[doc(hidden)]
pub trait SignerBech32: NostrSigner {
    /// Encode the public key as `npub1…` bech32.
    ///
    /// # Errors
    /// Returns [`bech32::Bech32Error`] if encoding fails.
    fn to_npub(&self) -> std::result::Result<String, bech32::Bech32Error> {
        bech32::Bech32Crypto::encode("npub", &self.pubkey_bytes())
    }
}
impl<T: NostrSigner + ?Sized> SignerBech32 for T {}

/// Auto-derived bech32 encoding for any keypair.
#[doc(hidden)]
pub trait KeypairBech32: NostrKeypair {
    /// Encode the secret key as `nsec1…` bech32.
    ///
    /// # Errors
    /// Returns [`bech32::Bech32Error`] if encoding fails.
    fn to_nsec(&self) -> std::result::Result<String, bech32::Bech32Error> {
        bech32::Bech32Crypto::encode("nsec", &self.secret_bytes())
    }
}
impl<T: NostrKeypair + ?Sized> KeypairBech32 for T {}

/// Errors returned by signing and key-derivation operations.
#[derive(Debug)]
pub enum SignerError {
    MissingId,
    MissingSignature,
    InvalidPublicKey,
    InvalidSignature,
    Backend(String),
}

impl std::fmt::Display for SignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingId => f.write_str("missing id on note"),
            Self::MissingSignature => f.write_str("missing signature on note"),
            Self::InvalidPublicKey => f.write_str("invalid public key"),
            Self::InvalidSignature => f.write_str("invalid signature"),
            Self::Backend(s) => write!(f, "signing backend error: {s}"),
        }
    }
}

impl std::error::Error for SignerError {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, SignerError>;

/// Minimal signing surface: produce a Schnorr signature over a 32-byte
/// prehash and expose the x-only public key. All hex/bech32 conversions are
/// default methods on top.
///
/// Implementors can be: in-memory keypairs, hardware wallets, NIP-46 remote
/// signers, browser extensions. The trait does not assume the implementor
/// holds the raw secret bytes (that's [`NostrKeypair`]) or that fresh keys
/// can be conjured locally (that's also [`NostrKeypair`]). It is therefore
/// dyn-compatible — `Box<dyn NostrSigner>` is a valid type.
pub trait NostrSigner {
    /// Sign a 32-byte prehash and return the 64-byte Schnorr signature.
    ///
    /// # Errors
    /// Returns an error if the underlying signer rejects the prehash or
    /// fails for transport/hardware reasons.
    fn sign_prehash(&self, id: &[u8; 32]) -> Result<[u8; 64]>;

    /// Return the x-only public key as 32 raw bytes.
    fn pubkey_bytes(&self) -> [u8; 32];

    /// Return the public key as a 64-character lowercase hex string.
    #[inline]
    fn public_key(&self) -> String {
        self.pubkey_bytes().to_hex()
    }
}

/// Extended interface for signers that hold raw secret material in process,
/// adding key export, ECDH, and local key generation.
///
/// Remote signers (NIP-46), hardware wallets, and any signer that *cannot*
/// hand out the secret bytes should implement [`NostrSigner`] only.
pub trait NostrKeypair: NostrSigner {
    /// Return the raw 32-byte secret key.
    fn secret_bytes(&self) -> [u8; 32];

    /// Generate a fresh random keypair.
    ///
    /// In-process only — hardware wallets and NIP-46 remote signers cannot
    /// satisfy this and so do not implement [`NostrKeypair`].
    fn generate() -> Self
    where
        Self: Sized;

    /// Rebuild a keypair from raw 32-byte secret material.
    ///
    /// Needed to revive ephemeral keys persisted in protocol state (e.g. the
    /// NIP-104 double ratchet), where only the secret bytes are stored.
    ///
    /// # Errors
    /// Returns [`SignerError::InvalidSignature`] if the bytes are not a valid
    /// scalar for the curve.
    fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self>
    where
        Self: Sized;

    /// Derive the ECDH shared point with a peer's x-only public key. Returns
    /// the 32-byte X coordinate of the shared point — the same value NIP-04
    /// and NIP-44 use as their shared-secret seed.
    ///
    /// # Errors
    /// Returns an error if the peer pubkey is not a valid curve point.
    fn ecdh_x(&self, peer_xonly: &[u8; 32]) -> Result<[u8; 32]>;

    /// Return the raw secret key as a 64-character lowercase hex string.
    #[inline]
    fn secret_key(&self) -> String {
        self.secret_bytes().to_hex()
    }

    /// Derive the ECDH shared point from a hex-encoded peer x-only pubkey.
    ///
    /// # Errors
    /// Returns an error if the peer pubkey is not 64 hex chars or not a
    /// valid curve point.
    fn shared_point(&self, peer_pubkey: &str) -> Result<[u8; 32]> {
        let mut buf = [0_u8; 32];
        peer_pubkey
            .decode_hex_to_slice(&mut buf)
            .map_err(|_| SignerError::InvalidPublicKey)?;
        self.ecdh_x(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signer_error_display_all_variants() {
        let cases = [
            SignerError::MissingId,
            SignerError::MissingSignature,
            SignerError::InvalidPublicKey,
            SignerError::InvalidSignature,
            SignerError::Backend("test backend error".into()),
        ];
        for err in &cases {
            let msg = format!("{err}");
            assert!(!msg.is_empty());
        }
        assert!(format!("{}", SignerError::Backend("x".into())).contains('x'));
    }

    #[test]
    fn hex_error_display_all_variants() {
        let cases = [
            hex::HexError::OddLength,
            hex::HexError::LengthMismatch,
            hex::HexError::InvalidChar(b'G'),
        ];
        for err in &cases {
            let msg = format!("{err}");
            assert!(!msg.is_empty());
        }
        assert!(format!("{}", hex::HexError::InvalidChar(b'Z')).contains('Z'));
    }
}
