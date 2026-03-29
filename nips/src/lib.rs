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

    pub struct NipTester {
        pub signing_key: k256::schnorr::SigningKey,
    }
    impl crate::Nip04 for NipTester {
        fn shared_secret(
            &self,
            pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, crate::Nip04Error> {
            let hex_pk = hex::decode(pubkey)?;
            // Build compressed SEC1 point: 0x02 prefix (even parity) + 32-byte x-coordinate
            let mut compressed = [0_u8; 33];
            compressed[0] = 0x02;
            compressed[1..].copy_from_slice(&hex_pk);
            let public_key = k256::PublicKey::from_sec1_bytes(&compressed)
                .map_err(|_| crate::Nip04Error::SharedSecretError)?;
            let secret_key = k256::SecretKey::from_slice(&self.signing_key.to_bytes())
                .map_err(|_| crate::Nip04Error::SharedSecretError)?;
            let shared =
                k256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), public_key.as_affine());
            let mut point = [0_u8; 32];
            point.copy_from_slice(shared.raw_secret_bytes().as_slice());
            Ok(point.into())
        }
    }
    impl crate::Nip44 for NipTester {
        fn shared_secret(
            &self,
            pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, crate::Nip44Error> {
            let hex_pk = hex::decode(pubkey)?;
            // Build compressed SEC1 point: 0x02 prefix (even parity) + 32-byte x-coordinate
            let mut compressed = [0_u8; 33];
            compressed[0] = 0x02;
            compressed[1..].copy_from_slice(&hex_pk);
            let public_key = k256::PublicKey::from_sec1_bytes(&compressed)
                .map_err(|_| crate::Nip44Error::SharedSecretError)?;
            let secret_key = k256::SecretKey::from_slice(&self.signing_key.to_bytes())
                .map_err(|_| crate::Nip44Error::SharedSecretError)?;
            let shared =
                k256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), public_key.as_affine());
            let mut point = [0_u8; 32];
            point.copy_from_slice(shared.raw_secret_bytes().as_slice());
            Ok(point.into())
        }
    }
    impl nostro2::NostrSigner for NipTester {
        fn secret_key(&self) -> String {
            hex::encode(self.signing_key.to_bytes())
        }
        fn sign_nostr_note(
            &self,
            note: &mut nostro2::NostrNote,
        ) -> Result<(), nostro2::errors::NostrErrors> {
            note.pubkey = self.public_key();
            let id = note.serialize_id_raw()?;
            let sig = self
                .signing_key
                .sign_prehash(&id)
                .map_err(|_| nostro2::errors::NostrErrors::InvalidSignature)?;
            note.sig.replace(hex::encode(sig.to_bytes()));
            Ok(())
        }
        fn generate(_extractable: bool) -> Self {
            let mut secret = [0u8; 32];
            getrandom::fill(&mut secret).expect("getrandom failed");
            let field_bytes = k256::FieldBytes::from(secret);
            Self {
                signing_key: k256::schnorr::SigningKey::from_bytes(&field_bytes)
                    .expect("invalid key bytes"),
            }
        }
        fn public_key(&self) -> String {
            hex::encode(self.signing_key.verifying_key().to_bytes())
        }
    }
    impl crate::Nip17 for NipTester {}
    impl crate::Nip46 for NipTester {}
    impl crate::Nip59 for NipTester {}
    impl crate::Nip82 for NipTester {}
    impl NipTester {
        pub fn _peer_one() -> Self {
            let bytes = hex::decode("30af2e27172df3fa2c202cf6a49bed35a2e0cb7994d9b437b2d945a92824c22a")
                .unwrap();
            let field_bytes = k256::FieldBytes::from_slice(&bytes);
            let signing_key = k256::schnorr::SigningKey::from_bytes(field_bytes).unwrap();
            Self { signing_key }
        }
        pub fn _peer_two() -> Self {
            let bytes = hex::decode("dd33562d81e8d00bfbe14708acdff85dffe6e6b6ca073ba3acdc6adb140cb8f1")
                .unwrap();
            let field_bytes = k256::FieldBytes::from_slice(&bytes);
            let signing_key = k256::schnorr::SigningKey::from_bytes(field_bytes).unwrap();
            Self { signing_key }
        }
        pub fn _peer_three() -> Self {
            let bytes = hex::decode("3410d9bd915643276a30795d4669a93469810a76901ce58f148c2cb84fcdc1b6")
                .unwrap();
            let field_bytes = k256::FieldBytes::from_slice(&bytes);
            let signing_key = k256::schnorr::SigningKey::from_bytes(field_bytes).unwrap();
            Self { signing_key }
        }
    }
    impl std::str::FromStr for NipTester {
        type Err = ();
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let bytes = hex::decode(s).map_err(|_| ())?;
            let field_bytes = k256::FieldBytes::from_slice(&bytes);
            let signing_key = k256::schnorr::SigningKey::from_bytes(field_bytes).map_err(|_| ())?;
            Ok(Self { signing_key })
        }
    }
}
