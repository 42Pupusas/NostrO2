#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
//! # `NostrO2` Signer
//!
//! Key management and signing for the Nostr protocol.
//!
//! `nostro2-signer` provides keypair management, signing, and cryptographic
//! operations for Nostr. It supports multiple key formats (hex, nsec, mnemonics),
//! modern encryption standards (NIP-04, NIP-44), and privacy features (NIP-59).
//!
//! ## Quick Start
//!
//! ### Creating Keypairs
//!
//! ```ignore
//! use nostro2::NostrSigner;
//! use nostro2_signer::{K256Keypair, KeypairExt, Language};
//!
//! // Generate new random keypair
//! let keypair = K256Keypair::generate();
//!
//! // From hex private key (64 hex characters)
//! let keypair = K256Keypair::from_hex(
//!     "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8",
//! )?;
//!
//! // From nsec
//! let keypair = K256Keypair::from_nsec(
//!     "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53",
//! )?;
//!
//! // From mnemonic (12 or 24 words)
//! let keypair = K256Keypair::from_mnemonic(
//!     "filter husband ridge zebra winter process upper basket wasp exact vote outdoor detect random thing upset wasp coil correct into twin catch giggle chase",
//!     Language::English,
//! )?;
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ### Signing Notes
//!
//! ```ignore
//! use nostro2::{NostrNote, NoteSignerExt, NostrSigner};
//! use nostro2_signer::K256Keypair;
//!
//! let keypair = K256Keypair::generate();
//! let mut note = NostrNote::text_note("Hello, Nostr!");
//! keypair.sign_nostr_note(&mut note)?;
//! assert!(note.verify());
//! # Ok::<(), nostro2::errors::NostrErrors>(())
//! ```
//!
//! ### Encryption (NIP-44)
//!
//! ```ignore
//! use nostro2::{NostrNote, NoteSignerExt, NostrSigner};
//! use nostro2_nips::Nip44;
//! use nostro2_signer::K256Keypair;
//!
//! let alice = K256Keypair::generate();
//! let bob = K256Keypair::generate();
//! let mut dm = NostrNote::with_kind(4).with_content("Secret message");
//!
//! let bob_pk = bob.public_key();
//! alice.nip44_encrypt_note(&mut dm, &bob_pk)?;
//! alice.sign_nostr_note(&mut dm)?;
//!
//! let alice_pk = alice.public_key();
//! let decrypted = bob.nip44_decrypt_note(&dm, &alice_pk)?;
//! assert_eq!(decrypted, "Secret message");
//! # Ok::<(), nostro2_nips::Nip44Error>(())
//! ```
//!
//! ### Gift Wrapping (NIP-59)
//!
//! ```ignore
//! use nostro2::{NostrNote, NostrSigner};
//! use nostro2_nips::Nip59;
//! use nostro2_signer::K256Keypair;
//!
//! let sender = K256Keypair::generate();
//! let recipient = K256Keypair::generate();
//! let mut rumor = NostrNote::text_note("Private message");
//!
//! let wrapped = sender.giftwrap(&mut rumor, &recipient.public_key())?;
//! let unwrapped = recipient.rumor(&wrapped)?;
//! assert_eq!(unwrapped.content, "Private message");
//! # Ok::<(), nostro2_nips::Nip59Error>(())
//! ```
//!
//! ## Features
//!
//! - **Multiple Key Formats**: Hex, nsec (bech32), and BIP39 mnemonic support
//! - **Smart Key Detection**: [`FromStr`](std::str::FromStr) tries all formats automatically
//! - **NIP-04 & NIP-44**: Modern and legacy encryption standards
//! - **NIP-59**: Gift wrap for sealed sender privacy
//! - **Feature-gated backends**: `k256` (default, pure Rust) or `secp256k1` (C library, faster)
//! - **Type Safety**: Comprehensive error handling with [`Result`](type.Result.html)
// Mirror the `nostro2` invariant: pick exactly one curve backend. Enabling
// both `k256` and `secp256k1` would compile two `Nip04`/`Nip44`/etc.
// impls, conflict with the upstream `compile_error!`, and have no useful
// "both" semantic. Enabling neither leaves no concrete keypair type; we
// reject that too rather than silently exporting an empty crate.
#[cfg(all(feature = "k256", feature = "secp256k1"))]
compile_error!("features `k256` and `secp256k1` are mutually exclusive; pick exactly one");
#[cfg(not(any(feature = "k256", feature = "secp256k1")))]
compile_error!(
    "exactly one of `k256` or `secp256k1` must be enabled; default = [\"k256\"] picks one for you"
);

pub mod errors;
pub mod ext;
#[cfg(feature = "k256")]
pub mod k256_keypair;
#[cfg(feature = "secp256k1")]
pub mod secp256k1_keypair;
pub use nostro2;
pub use nostro2_nips;
pub use nostro2_traits;

pub use bip39::Language;
pub use ext::KeypairExt;
#[cfg(feature = "k256")]
pub use k256_keypair::K256Keypair;
#[cfg(feature = "secp256k1")]
pub use secp256k1_keypair::Secp256k1Keypair;

/// Convenience type alias for Results with `NostrKeypairError`
pub type Result<T> = std::result::Result<T, errors::NostrKeypairError>;
