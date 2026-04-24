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
mod nip_82;

pub use nip_04::*;
pub use nip_17::*;
pub use nip_44::*;
pub use nip_46::*;
pub use nip_59::*;
pub use nip_82::*;
#[cfg(test)]
mod tests {
    use k256::schnorr::signature::hazmat::PrehashSigner;
    use nostro2::{NostrKeypair, NostrSigner};

    /// Test-only keypair that wraps k256 directly. Not part of the public API.
    #[derive(Clone)]
    pub struct NipTester(k256::schnorr::SigningKey);

    impl NipTester {
        pub fn from_hex(s: &str) -> Option<Self> {
            let bytes = hex::decode(s).ok()?;
            let field_bytes: &k256::FieldBytes = bytes.as_slice().try_into().ok()?;
            k256::schnorr::SigningKey::from_bytes(field_bytes).ok().map(Self)
        }
        pub fn _peer_one() -> Self {
            Self::from_hex("30af2e27172df3fa2c202cf6a49bed35a2e0cb7994d9b437b2d945a92824c22a").unwrap()
        }
        pub fn _peer_two() -> Self {
            Self::from_hex("dd33562d81e8d00bfbe14708acdff85dffe6e6b6ca073ba3acdc6adb140cb8f1").unwrap()
        }
        pub fn _peer_three() -> Self {
            Self::from_hex("3410d9bd915643276a30795d4669a93469810a76901ce58f148c2cb84fcdc1b6").unwrap()
        }
    }

    impl std::str::FromStr for NipTester {
        type Err = ();
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Self::from_hex(s).ok_or(())
        }
    }

    impl NostrSigner for NipTester {
        fn sign_nostr_note(
            &self,
            note: &mut nostro2::NostrNote,
        ) -> Result<(), nostro2::errors::NostrErrors> {
            note.pubkey = self.public_key();
            let id = note.serialize_id_raw()?;
            let sig = self
                .0
                .sign_prehash(&id)
                .map_err(|_| nostro2::errors::NostrErrors::InvalidSignature)?;
            note.sig.replace(hex::encode(sig.to_bytes()));
            Ok(())
        }
        fn generate() -> Self {
            let mut secret = [0_u8; 32];
            getrandom::fill(&mut secret).expect("getrandom failed");
            let field_bytes = k256::FieldBytes::from(secret);
            Self(k256::schnorr::SigningKey::from_bytes(&field_bytes).expect("invalid key bytes"))
        }
        fn public_key(&self) -> String {
            hex::encode(self.0.verifying_key().to_bytes())
        }
    }

    impl NostrKeypair for NipTester {
        fn secret_key(&self) -> Option<String> {
            Some(hex::encode(self.0.to_bytes()))
        }
        fn shared_point(&self, peer_pubkey: &str) -> nostro2::Result<[u8; 32]> {
            let hex_pk = hex::decode(peer_pubkey)
                .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
            let mut compressed = [0_u8; 33];
            compressed[0] = 0x02;
            compressed[1..].copy_from_slice(&hex_pk);
            let public_key = k256::PublicKey::from_sec1_bytes(&compressed)
                .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
            let secret_key = k256::SecretKey::from_slice(&self.0.to_bytes())
                .map_err(|_| nostro2::errors::NostrErrors::InvalidSignature)?;
            let shared = k256::ecdh::diffie_hellman(
                secret_key.to_nonzero_scalar(),
                public_key.as_affine(),
            );
            let mut point = [0_u8; 32];
            point.copy_from_slice(shared.raw_secret_bytes().as_slice());
            Ok(point)
        }
        fn npub(&self) -> nostro2::Result<String> {
            let hrp = bech32::Hrp::parse("npub")
                .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
            bech32::encode::<bech32::Bech32>(hrp, &self.0.verifying_key().to_bytes())
                .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)
        }
        fn nsec(&self) -> nostro2::Result<String> {
            let hrp = bech32::Hrp::parse("nsec")
                .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)?;
            bech32::encode::<bech32::Bech32>(hrp, &self.0.to_bytes())
                .map_err(|_| nostro2::errors::NostrErrors::InvalidPublicKey)
        }
    }

    impl crate::Nip04 for NipTester {
        fn shared_secret(
            &self,
            pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, crate::Nip04Error> {
            Ok(NostrKeypair::shared_point(self, pubkey)
                .map_err(|_| crate::Nip04Error::SharedSecretError)?
                .into())
        }
    }
    impl crate::Nip44 for NipTester {
        fn shared_secret(
            &self,
            pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, crate::Nip44Error> {
            Ok(NostrKeypair::shared_point(self, pubkey)
                .map_err(|_| crate::Nip44Error::SharedSecretError)?
                .into())
        }
    }
    impl crate::Nip17 for NipTester {}
    impl crate::Nip46 for NipTester {}
    impl crate::Nip59 for NipTester {}
    impl crate::Nip82 for NipTester {}
}
