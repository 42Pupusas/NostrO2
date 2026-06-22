//! Print a fresh nsec/npub pair. Run: `cargo run -p nostro2-relay --example keygen`
use nostro2::{NostrKeypair as _, NostrSigner};
use nostro2_signer::NostrKeypair;

fn main() {
    let kp = NostrKeypair::generate();
    println!("npub: {}", kp.npub().unwrap());
    println!("nsec: {}", kp.nsec().unwrap());
    println!("hex pubkey: {}", kp.public_key());
}
