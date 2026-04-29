#![warn(
    clippy::all,
    clippy::missing_errors_doc,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
mod nip_04;
mod nip_17;
mod nip_44;
mod nip_46;
mod nip_59;

pub use nip_04::*;
pub use nip_17::*;
pub use nip_44::*;
pub use nip_46::*;
pub use nip_59::*;
#[cfg(test)]
mod tests {
    use nostro2_traits::{NostrKeypair, NostrSigner, SignerError};

    /// Test-only keypair that wraps k256 directly. Not part of the public API.
    #[derive(Clone)]
    pub struct NipTester(k256::schnorr::SigningKey);

    impl NipTester {
        pub fn from_hex(s: &str) -> Option<Self> {
            let bytes = hex::decode(s).ok()?;
            let field_bytes: &k256::FieldBytes = bytes.as_slice().try_into().ok()?;
            k256::schnorr::SigningKey::from_bytes(field_bytes)
                .ok()
                .map(Self)
        }
        pub fn _peer_one() -> Self {
            Self::from_hex("30af2e27172df3fa2c202cf6a49bed35a2e0cb7994d9b437b2d945a92824c22a")
                .unwrap()
        }
        pub fn _peer_two() -> Self {
            Self::from_hex("dd33562d81e8d00bfbe14708acdff85dffe6e6b6ca073ba3acdc6adb140cb8f1")
                .unwrap()
        }
        pub fn _peer_three() -> Self {
            Self::from_hex("3410d9bd915643276a30795d4669a93469810a76901ce58f148c2cb84fcdc1b6")
                .unwrap()
        }
    }

    impl std::str::FromStr for NipTester {
        type Err = ();
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Self::from_hex(s).ok_or(())
        }
    }

    impl NostrSigner for NipTester {
        // Mirrors `K256Keypair::sign_prehash`: BIP-340 §3.2 aux randomness must
        // be freshly drawn per call. The deterministic `PrehashSigner::sign_prehash`
        // path is *not* what production uses and must not be copied here, even
        // for tests — a developer reading this file as a template would inherit
        // the wrong invariant.
        fn sign_prehash(&self, id: &[u8; 32]) -> Result<[u8; 64], SignerError> {
            let mut aux_rand = [0_u8; 32];
            getrandom::fill(&mut aux_rand)
                .map_err(|e| SignerError::Backend(format!("getrandom: {e}")))?;
            let sig = self
                .0
                .sign_raw(id, &aux_rand)
                .map_err(|_| SignerError::InvalidSignature)?;
            Ok(sig.to_bytes())
        }
        fn pubkey_bytes(&self) -> [u8; 32] {
            self.0.verifying_key().to_bytes().into()
        }
    }

    impl NostrKeypair for NipTester {
        fn secret_bytes(&self) -> [u8; 32] {
            self.0.to_bytes().into()
        }
        fn generate() -> Self {
            let mut secret = [0_u8; 32];
            getrandom::fill(&mut secret).expect("getrandom failed");
            let field_bytes = k256::FieldBytes::from(secret);
            Self(k256::schnorr::SigningKey::from_bytes(&field_bytes).expect("invalid key bytes"))
        }
        fn ecdh_x(&self, peer_xonly: &[u8; 32]) -> Result<[u8; 32], SignerError> {
            let mut compressed = [0_u8; 33];
            compressed[0] = 0x02;
            compressed[1..].copy_from_slice(peer_xonly);
            let public_key = k256::PublicKey::from_sec1_bytes(&compressed)
                .map_err(|_| SignerError::InvalidPublicKey)?;
            let secret_key = k256::SecretKey::from_slice(&self.0.to_bytes())
                .map_err(|_| SignerError::InvalidSignature)?;
            let shared =
                k256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), public_key.as_affine());
            let mut point = [0_u8; 32];
            point.copy_from_slice(shared.raw_secret_bytes().as_slice());
            Ok(point)
        }
    }

    // Nip04 / Nip44 / Nip17 / Nip46 / Nip59 are blanket-implemented for every
    // `NostrKeypair`, so `NipTester` gets them all for free.
}
