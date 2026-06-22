//! Live NIP-44 / NIP-17 interop test against real relays.
//!
//! Generates an ephemeral sender keypair, builds a NIP-17 giftwrapped DM
//! (kind 1059, content encrypted with the *current* nostro2 NIP-44 impl),
//! and publishes it to relays Primal reads from.
//!
//! Run: `cargo run -p nostro2-relay --example nip44_interop`

use nostro2::NostrKeypair as _;
use nostro2_nips::Nip17;
use nostro2_signer::NostrKeypair;

// Relays Primal indexes for DMs.
const RELAYS: &[&str] = &[
    "wss://relay.primal.net",
    "wss://nos.lol",
    "wss://relay.damus.io",
    "wss://relay.nostr.band",
    "wss://relay.snort.social",
    "wss://nostr.wine",
    "wss://purplepag.es",
];

// Iris is logged in as this key (from its REQ #p filter) — address the giftwrap to it.
const RECIPIENT_HEX: &str = "7621d632eaa57d2f8f4f13c8a4b185731c22ad276dc7d8d105d0cbb12f7cbd01";
const MESSAGE: &str = "nostro2 nip44 interop test — if you can read this, the impl is compliant";

#[tokio::main]
async fn main() {
    // 1. Generate a fresh sender identity.
    let sender = NostrKeypair::generate();
    println!("== NIP-44 interop test ==");
    println!("sender npub : {}", sender.npub().unwrap());
    println!("sender nsec : {}", sender.nsec().unwrap());
    println!("recipient   : {RECIPIENT_HEX}");

    // 2. Recipient hex pubkey (the key Iris is subscribing for).
    let recipient = RECIPIENT_HEX.to_string();
    println!("recipient hex: {recipient}");

    // 3. Build the NIP-17 giftwrap (kind 1059) — content is NIP-44 encrypted.
    let giftwrap = sender
        .private_dm(MESSAGE, &recipient)
        .expect("failed to build giftwrap");
    println!("\ngiftwrap kind   : {}", giftwrap.kind);
    println!("giftwrap id     : {:?}", giftwrap.id);
    println!(
        "content (enc)   : {}…",
        &giftwrap.content[..60.min(giftwrap.content.len())]
    );

    // 4. Publish to relays.
    let pool = nostro2_relay::NostrPool::new(RELAYS);
    // Give the sockets a moment to connect.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    pool.send(&giftwrap).expect("failed to queue giftwrap");
    println!("\npublished to {} relays, waiting for OKs…\n", RELAYS.len());

    // 5. Collect relay OK acks for 8 seconds.
    use nostro2::NostrRelayEvent;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(8) {
        if let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), pool.recv()).await
        {
            if let NostrRelayEvent::SentOk(_, id, ok, msg) = ev {
                println!("OK id={id} accepted={ok} msg=\"{msg}\"");
            }
        }
    }
    println!("\nDone. Check Primal DMs for the recipient.");
}
