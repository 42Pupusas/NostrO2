#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
pub mod errors;
pub mod keypair;
pub extern crate nostro2;
pub extern crate nostro2_nips;
pub use bip39::Language;
pub static SECP: std::sync::LazyLock<secp256k1::Secp256k1<secp256k1::SignOnly>> =
    std::sync::LazyLock::new(secp256k1::Secp256k1::signing_only);

