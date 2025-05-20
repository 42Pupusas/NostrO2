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

mod tests {
    pub struct NipTester {
        pub private_key: secp256k1::Keypair,
    }
    impl crate::Nip04 for NipTester {
        fn shared_secret(
            &self,
            pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, crate::Nip04Error> {
            let hex_pk = hex::decode(pubkey)?;
            let x_only_public_key = secp256k1::XOnlyPublicKey::from_slice(hex_pk.as_slice())?;
            let public_key = secp256k1::PublicKey::from_x_only_public_key(
                x_only_public_key,
                secp256k1::Parity::Even,
            );
            let mut ssp =
                secp256k1::ecdh::shared_secret_point(&public_key, &self.private_key.secret_key())
                    .as_slice()
                    .to_owned();
            ssp.resize(32, 0); // toss the Y part
            let slice: [u8; 32] = ssp.try_into().map_err(|_| {
                crate::Nip04Error::SharedSecretError("Failed to convert to array".to_string())
            })?;
            Ok(slice.into())
        }
    }
    impl crate::Nip44 for NipTester {
        fn shared_secret(
            &self,
            pubkey: &str,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, crate::Nip44Error> {
            let hex_pk = hex::decode(pubkey)?;
            let x_only_public_key = secp256k1::XOnlyPublicKey::from_slice(hex_pk.as_slice())?;
            let public_key = secp256k1::PublicKey::from_x_only_public_key(
                x_only_public_key,
                secp256k1::Parity::Even,
            );
            let mut ssp =
                secp256k1::ecdh::shared_secret_point(&public_key, &self.private_key.secret_key())
                    .as_slice()
                    .to_owned();
            ssp.resize(32, 0); // toss the Y part
            let slice: [u8; 32] = ssp.try_into().map_err(|_| {
                crate::Nip44Error::SharedSecretError("Failed to convert to array".to_string())
            })?;
            Ok(slice.into())
        }
    }
    impl nostro2::NostrSigner for NipTester {
        fn secret_key(&self) -> String {
            hex::encode(self.private_key.secret_key().secret_bytes())
        }
        fn sign_nostr_note(
            &self,
            note: &mut nostro2::NostrNote,
        ) -> Result<(), nostro2::errors::NostrErrors> {
            note.pubkey = self.public_key();
            note.serialize_id()?;
            let sig = secp256k1::Secp256k1::signing_only()
                .sign_schnorr_no_aux_rand(
                    note.id_bytes().as_ref().unwrap_or(&[0_u8; 32]),
                    &self.private_key,
                )
                .to_string();
            note.sig.replace(sig);
            Ok(())
        }
        fn generate(_extractable: bool) -> Self {
            Self {
                private_key: secp256k1::Keypair::new(
                    &secp256k1::Secp256k1::signing_only(),
                    &mut secp256k1::rand::thread_rng(),
                ),
            }
        }
        fn public_key(&self) -> String {
            hex::encode(self.private_key.x_only_public_key().0.serialize())
        }
    }
    impl crate::Nip17 for NipTester {}
    impl crate::Nip46 for NipTester {}
    impl crate::Nip59 for NipTester {}
    impl crate::Nip82 for NipTester {}
    impl NipTester {
        pub fn _peer_one() -> Self {
            let private_key = secp256k1::Keypair::from_secret_key(
                &secp256k1::Secp256k1::new(),
                &secp256k1::SecretKey::from_slice(
                    &hex::decode(
                        "30af2e27172df3fa2c202cf6a49bed35a2e0cb7994d9b437b2d945a92824c22a",
                    )
                    .unwrap(),
                )
                .unwrap(),
            );
            Self { private_key }
        }
        pub fn _peer_two() -> Self {
            let private_key = secp256k1::Keypair::from_secret_key(
                &secp256k1::Secp256k1::new(),
                &secp256k1::SecretKey::from_slice(
                    &hex::decode(
                        "dd33562d81e8d00bfbe14708acdff85dffe6e6b6ca073ba3acdc6adb140cb8f1",
                    )
                    .unwrap(),
                )
                .unwrap(),
            );
            Self { private_key }
        }
        pub fn _peer_three() -> Self {
            let private_key = secp256k1::Keypair::from_secret_key(
                &secp256k1::Secp256k1::new(),
                &secp256k1::SecretKey::from_slice(
                    &hex::decode(
                        "3410d9bd915643276a30795d4669a93469810a76901ce58f148c2cb84fcdc1b6",
                    )
                    .unwrap(),
                )
                .unwrap(),
            );
            Self { private_key }
        }
    }
    impl std::str::FromStr for NipTester {
        type Err = ();
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Ok(Self {
                private_key: secp256k1::Keypair::from_seckey_str(&secp256k1::Secp256k1::new(), s)
                    .map_err(|_| ())?,
            })
        }
    }
}
