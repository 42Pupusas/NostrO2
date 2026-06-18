//! Shared verification trait for owned and borrowed Nostr events.
//!
//! Both [`crate::NostrNote`] and [`crate::NostrNoteView`] implement
//! [`NostrEvent`], giving them a uniform surface for ID computation,
//! signature verification, and content integrity checks — with the
//! `k256` / `secp256k1` feature-gate applied in exactly one place.

use std::borrow::Cow;

use bourne::JsonWrite;
use nostro2_traits::hex::FromHex as _;

#[cfg(any(feature = "k256", feature = "secp256k1"))]
use crate::errors::NostrErrors;
use crate::hash::Sha256Sink;

/// Types that carry the canonical Nostr event fields needed for
/// ID computation and signature verification.
///
/// Some methods only become live when a crypto backend feature
/// (`k256` or `secp256k1`) is enabled.
#[allow(dead_code)]
pub trait NostrEvent {
    fn pubkey_str(&self) -> Cow<'_, str>;
    fn created_at(&self) -> i64;
    fn kind(&self) -> u32;
    fn content_str(&self) -> Cow<'_, str>;
    fn id_hex(&self) -> Option<Cow<'_, str>>;
    fn sig_hex(&self) -> Option<Cow<'_, str>>;

    /// Write the event's tag rows as a JSON array to `sink`.
    ///
    /// # Errors
    ///
    /// Propagates the writer's error type.
    fn write_tags<W: JsonWrite + ?Sized>(&self, sink: &mut W) -> Result<(), W::Error>;

    // ── Provided: hex → byte helpers ──────────────────────

    #[must_use]
    #[inline]
    fn id_bytes(&self) -> Option<[u8; 32]> {
        let mut out = [0_u8; 32];
        self.id_hex()?.decode_hex_to_slice(&mut out).ok()?;
        Some(out)
    }

    #[must_use]
    #[inline]
    fn sig_bytes(&self) -> Option<[u8; 64]> {
        let mut out = [0_u8; 64];
        self.sig_hex()?.decode_hex_to_slice(&mut out).ok()?;
        Some(out)
    }

    #[must_use]
    #[inline]
    fn pubkey_bytes(&self) -> Option<[u8; 32]> {
        let mut out = [0_u8; 32];
        self.pubkey_str().decode_hex_to_slice(&mut out).ok()?;
        Some(out)
    }

    // ── Provided: canonical ID ────────────────────────────

    /// SHA-256 of the NIP-01 canonical serialization `[0, pubkey, created_at,
    /// kind, tags, content]`.
    #[must_use]
    fn compute_id_bytes(&self) -> [u8; 32] {
        use sha2::Digest as _;

        let mut hasher = sha2::Sha256::new();
        let mut sink = Sha256Sink(&mut hasher);

        // `Sha256Sink` never fails (Error = Infallible). The closure
        // form lets us use `?` for clean flow while keeping the borrow
        // checker happy.
        let _: Result<(), core::convert::Infallible> = (|| {
            sink.write_byte(b'[')?;
            sink.write_int_i64(0)?;
            sink.write_byte(b',')?;
            sink.write_escaped_str(&self.pubkey_str())?;
            sink.write_byte(b',')?;
            sink.write_int_i64(self.created_at())?;
            sink.write_byte(b',')?;
            sink.write_int_u64(u64::from(self.kind()))?;
            sink.write_byte(b',')?;
            self.write_tags(&mut sink)?;
            sink.write_byte(b',')?;
            sink.write_escaped_str(&self.content_str())?;
            sink.write_byte(b']')
        })();

        hasher.finalize().into()
    }

    // ── Provided: verification ────────────────────────────

    #[must_use]
    #[inline]
    fn verify_content(&self) -> bool {
        let Some(stored) = self.id_bytes() else {
            return false;
        };
        stored == self.compute_id_bytes()
    }

    /// Verify the note's Schnorr signature.
    ///
    /// # Errors
    ///
    /// Returns [`NostrErrors::MissingId`], [`NostrErrors::MissingSignature`],
    /// [`NostrErrors::InvalidPublicKey`], or a key/signature conversion error.
    #[cfg(feature = "k256")]
    fn verify_signature(&self) -> Result<bool, NostrErrors> {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
        let id = self.id_bytes().ok_or(NostrErrors::MissingId)?;
        let sig = self.sig_bytes().ok_or(NostrErrors::MissingSignature)?;
        let pubkey = self
            .pubkey_bytes()
            .ok_or(NostrErrors::InvalidPublicKey)?;
        let verifying_key = VerifyingKey::from_bytes((&pubkey).into())?;
        let signature = Signature::try_from(sig.as_slice())?;
        Ok(verifying_key.verify_prehash(&id, &signature).is_ok())
    }

    /// Verify the note's Schnorr signature (secp256k1 backend).
    ///
    /// # Errors
    ///
    /// Returns [`NostrErrors::MissingId`], [`NostrErrors::MissingSignature`],
    /// [`NostrErrors::InvalidPublicKey`], or a key/signature conversion error.
    #[cfg(feature = "secp256k1")]
    #[allow(unknown_lints, crappy)]
    fn verify_signature(&self) -> Result<bool, NostrErrors> {
        use secp256k1::{schnorr::Signature, XOnlyPublicKey, SECP256K1};
        let id = self.id_bytes().ok_or(NostrErrors::MissingId)?;
        let sig_bytes = self.sig_bytes().ok_or(NostrErrors::MissingSignature)?;
        let pubkey = self
            .pubkey_bytes()
            .ok_or(NostrErrors::InvalidPublicKey)?;
        let xonly = XOnlyPublicKey::from_byte_array(pubkey)?;
        let sig = Signature::from_byte_array(sig_bytes);
        Ok(SECP256K1.verify_schnorr(&sig, &id, &xonly).is_ok())
    }

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[must_use]
    #[inline]
    fn verify(&self) -> bool {
        self.verify_content() && self.verify_signature().is_ok_and(|t| t)
    }
}
