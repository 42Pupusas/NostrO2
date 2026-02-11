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
//! ```rust
//! use nostro2_signer::{NostrKeypair, Language};
//!
//! // Generate new random keypair
//! let keypair = NostrKeypair::new();
//!
//! // Generate extractable keypair (allows exporting private key)
//! let keypair = NostrKeypair::new_extractable();
//!
//! // From hex private key (64 hex characters)
//! let keypair = NostrKeypair::from_hex(
//!     "a992011980303ea8c43f66087634283026e7796e7fcea8b61710239e19ee28c8",
//!     true
//! )?;
//!
//! // From nsec
//! let keypair = NostrKeypair::from_nsec(
//!     "nsec14xfqzxvqxql233plvcy8vdpgxqnww7tw0l823dshzq3eux0w9ryqulcv53",
//!     true
//! )?;
//!
//! // From mnemonic (12 or 24 words)
//! let keypair = NostrKeypair::from_mnemonic(
//!     "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
//!     Language::English,
//!     true
//! )?;
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ### Signing Notes
//!
//! ```rust
//! use nostro2::NostrNote;
//! use nostro2_signer::NostrKeypair;
//! use nostro2::NostrSigner;
//!
//! let keypair = NostrKeypair::new();
//! let mut note = NostrNote::text_note("Hello, Nostr!");
//!
//! // Sign the note
//! keypair.sign_note(&mut note)?;
//! assert!(note.verify());
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ### Encryption (NIP-44)
//!
//! ```rust
//! use nostro2::NostrNote;
//! use nostro2_signer::{NostrKeypair, EncryptionScheme};
//!
//! let alice = NostrKeypair::new_extractable();
//! let bob = NostrKeypair::new_extractable();
//!
//! let mut dm = NostrNote::with_kind(4)
//!     .with_content("Secret message");
//!
//! // Encrypt and sign
//! let bob_pk = bob.pubkey();
//! alice.sign_encrypted_note(
//!     &mut dm,
//!     &bob_pk,
//!     &EncryptionScheme::Nip44
//! )?;
//!
//! // Decrypt
//! let alice_pk = alice.pubkey();
//! let decrypted = bob.decrypt_note(
//!     &dm,
//!     &alice_pk,
//!     &EncryptionScheme::Nip44
//! )?;
//! assert_eq!(decrypted, "Secret message");
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ### Gift Wrapping (NIP-59)
//!
//! ```rust
//! use nostro2::NostrNote;
//! use nostro2_signer::{NostrKeypair, GiftwrapScheme};
//!
//! let sender = NostrKeypair::new_extractable();
//! let recipient = NostrKeypair::new_extractable();
//!
//! let mut rumor = NostrNote::text_note("Private message");
//!
//! // Wrap the note
//! let wrapped = sender.giftwrap_note(
//!     &mut rumor,
//!     &recipient.pubkey(),
//!     &GiftwrapScheme::Ephemeral
//! )?;
//!
//! // Unwrap
//! let unwrapped = recipient.extract_rumor(&wrapped)?;
//! assert_eq!(unwrapped.content, "Private message");
//! # Ok::<(), nostro2_signer::errors::NostrKeypairError>(())
//! ```
//!
//! ## Features
//!
//! - **Multiple Key Formats**: Hex, nsec (bech32), and BIP39 mnemonic support
//! - **Smart Key Detection**: [`FromStr`](std::str::FromStr) tries all formats automatically
//! - **NIP-04 & NIP-44**: Modern and legacy encryption standards
//! - **NIP-59**: Gift wrap for sealed sender privacy
//! - **Extractable Keys**: Optional key extraction protection
//! - **Type Safety**: Comprehensive error handling with [`Result`](type.Result.html)
//!
//! ## Security
//!
//! - Keys are zeroized on drop when using extractable mode
//! - Constant-time operations for cryptographic primitives
//! - Uses pure Rust `k256` library for WASM compatibility
//! - Optional key extraction protection
pub mod errors;
pub mod k256_keypair;
pub extern crate nostro2;
pub extern crate nostro2_nips;

pub use bip39::Language;
pub use k256_keypair::{EncryptionScheme, GiftwrapScheme, K256Keypair};

/// Type alias for compatibility - `K256Keypair` is now the default
pub type NostrKeypair = K256Keypair;

/// Convenience type alias for Results with `NostrKeypairError`
pub type Result<T> = std::result::Result<T, errors::NostrKeypairError>;
