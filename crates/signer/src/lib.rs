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
//! `nostro2-signer` provides a unified [`NostrKeypair`] type for keypair
//! management, signing, and cryptographic operations for Nostr. It supports
//! multiple key formats (hex, nsec, mnemonics), modern encryption standards
//! (NIP-44), and privacy features (NIP-59).
//!
//! The cryptographic backend is selected at compile time via Cargo features
//! (`k256` or `secp256k1`). The public API is identical regardless of
//! backend — just recompile with the other feature flag to A/B test.
//!
//! ## Quick Start
//!
//! ### Creating Keypairs
//!
//! ```ignore
//! use nostro2_signer::NostrKeypair;
//!
//! // Generate new random keypair
//! let keypair = NostrKeypair::generate();
//!
//! // From hex private key (64 hex characters)
//! let keypair = NostrKeypair::from_hex(
//!     "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8",
//! )?;
//!
//! // From nsec
//! let keypair = NostrKeypair::from_nsec(
//!     "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53",
//! )?;
//!
//! // From mnemonic (12 or 24 words)
//! let keypair = NostrKeypair::from_mnemonic(
//!     "filter husband ridge zebra winter process upper basket wasp exact vote outdoor detect random thing upset wasp coil correct into twin catch giggle chase",
//!     xinachtli::Language::English,
//! )?;
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ### Signing Notes
//!
//! ```ignore
//! use nostro2::{NostrNote, NostrNoteBuilder, NostrEvent};
//! use nostro2_signer::NostrKeypair;
//!
//! let keypair = NostrKeypair::generate();
//! let mut note = NostrNoteBuilder::text_note("Hello, Nostr!").build();
//! note.sign_with(&keypair)?;
//! assert!(note.verify());
//! # Ok::<(), nostro2::errors::NostrErrors>(())
//! ```
//!
//! ### Encryption (NIP-44)
//!
//! ```ignore
//! use nostro2::{NostrNote, NostrNoteBuilder};
//! use nostro2_nips::Nip44;
//! use nostro2_signer::NostrKeypair;
//!
//! let alice = NostrKeypair::generate();
//! let bob = NostrKeypair::generate();
//! let mut dm = NostrNoteBuilder::new().kind(4).content("Secret message").build();
//!
//! let bob_pk = bob.public_key();
//! alice.nip44_encrypt_note(&mut dm, &bob_pk)?;
//! dm.sign_with(&alice)?;
//!
//! let alice_pk = alice.public_key();
//! let decrypted = bob.nip44_decrypt_note(&dm, &alice_pk)?;
//! assert_eq!(decrypted, "Secret message");
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ### Gift Wrapping (NIP-59)
//!
//! ```ignore
//! use nostro2::{NostrNote, NostrNoteBuilder};
//! use nostro2_nips::Nip59;
//! use nostro2_signer::NostrKeypair;
//!
//! let sender = NostrKeypair::generate();
//! let recipient = NostrKeypair::generate();
//! let mut rumor = NostrNoteBuilder::text_note("Private message").build();
//!
//! let wrapped = sender.giftwrap(&mut rumor, &recipient.public_key())?;
//! let unwrapped = recipient.rumor(&wrapped)?;
//! assert_eq!(unwrapped.content, "Secret message");
//! # Ok::<(), nostro2_nips::Nip59Error>(())
//! ```
//!
//! ## Features
//!
//! - **Unified [`NostrKeypair`]**: same type regardless of backend, A/B
//!   test by swapping Cargo features
//! - **Multiple Key Formats**: Hex, nsec (bech32), and BIP39 mnemonic support
//! - **Smart Key Detection**: [`FromStr`](std::str::FromStr) tries all
//!   formats automatically
//! - **NIP-44**: Modern encryption standard
//! - **NIP-59**: Gift wrap for sealed sender privacy
//! - **Feature-gated backends**: `k256` (default, pure Rust) or
//!   `secp256k1` (C library, faster)
//! - **Type Safety**: Comprehensive error handling with
//!   [`Result`](type.Result.html)
// Mirror the `nostro2` invariant: pick exactly one curve backend. Enabling
// both `k256` and `secp256k1` would compile two `Nip44`/etc.
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
mod keypair;
pub use keypair::NostrKeypair;

// Re-exports for convenience
pub use nostro2;
pub use nostro2_nips;
pub use nostro2_traits;
pub use xinachtli;

/// Convenience type alias for Results with `NostrKeypairError`
pub type Result<T> = std::result::Result<T, errors::NostrKeypairError>;
