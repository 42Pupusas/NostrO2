//! Send a real NIP-104 (double ratchet) message to a *specific* Iris user.
//!
//! Iris's "private messages" ARE the double ratchet — a plain NIP-17 DM won't
//! appear in that view. To land a message we must speak the ratchet:
//!
//! 1. Decode the target npub → hex.
//! 2. Fetch *their* published invite off the relays (kind 30078, authored by
//!    them, `l = double-ratchet/invites`) — Iris publishes this automatically.
//! 3. `SessionManager::accept_invite` → publish the kind-1059 response so their
//!    client opens its side of the ratchet.
//! 4. `SessionManager::send` → publish the kind-1060 ratchet message(s).
//!
//! Run: `cargo run -p nostro2-relay --example nip104_iris_dm`

use nostro2::{NostrKeypair as _, NostrRelayEvent, NostrSigner as _};
use nostro2_nips::{Invite, SessionManager};
use nostro2_signer::NostrKeypair;
use nostro2_traits::bech32::Bech32Crypto;
use std::time::{Duration, Instant};

const RELAYS: &[&str] = &[
    // Iris's own relays — where it publishes invites + reads ratchet traffic.
    "wss://temp.iris.to",
    "wss://vault.iris.to",
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://nos.lol",
    "wss://relay.nostr.band",
    "wss://relay.snort.social",
    "wss://nostr.wine",
];

const TARGET_NPUB: &str = "npub1k2flev40w4lx0c3txdymtw92ht2saxy9cyew4l64mrv4yqxz3mtsnn0tlm";
const MESSAGE: &str = "hello from nostro2 — live NIP-104 double ratchet :3";

// Stable identity for this example (so the user can add it as a contact).
//   npub1hceqspekvhdafjhpzjtqfyrlhj6z7gmh7kpfnvuqwreudt2mn2xsk3rhyk
//   hex  be3208073665dbd4cae1149604907fbcb42f2377f58299b38070f3c6ad5b9a8d
const OUR_NSEC: &str = "nsec17qf72rfytl0rdvtu3sy2m365xmxqeynghnl5tflftwnwxyhnglvsauzfgp";

const INVITE_KIND: u32 = 30078;
const INVITE_LABEL: &str = "double-ratchet/invites";

#[tokio::main]
async fn main() {
    // 1. npub → hex.
    let (hrp, bytes) = Bech32Crypto::decode(TARGET_NPUB).expect("bad npub");
    assert_eq!(hrp, "npub", "not an npub");
    let target_hex = hex_encode(&bytes);

    let me = NostrKeypair::from_nsec(OUR_NSEC).expect("bad nsec");
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();

    println!("== NIP-104 DM to a specific Iris user ==");
    println!("target npub : {TARGET_NPUB}");
    println!("target hex  : {target_hex}");
    println!("our hex      : {}", me.public_key());

    let mut manager = SessionManager::new(me);

    let pool = nostro2_relay::NostrPool::new(RELAYS);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 2. Fetch the target's published invite.
    // Debug: NO kind filter — show everything this user has published.
    let subscription = nostro2::NostrSubscription::new()
        .author(&target_hex)
        .limit(200);
    pool.send(subscription).unwrap();
    println!("\nFetching {target_hex}'s double-ratchet invite…");

    let mut their_invite: Option<Invite> = None;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(12) {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(200), pool.recv()).await
        else {
            continue;
        };
        if let NostrRelayEvent::NewNote(_, _, note) = ev {
            if note.pubkey == target_hex {
                println!("  kind {:>5}  tags={:?}", note.kind, note.tags);
            }
            if note.kind == INVITE_KIND && note.pubkey == target_hex {
                match Invite::from_event(&note) {
                    Ok(invite) => {
                        println!(
                            "  found invite  ephemeral={}…  device={:?}",
                            &invite.inviter_ephemeral_pubkey[..8],
                            invite.device_id.as_deref()
                        );
                        their_invite = Some(invite);
                        break;
                    }
                    Err(e) => println!("  (a kind-{INVITE_KIND} note didn't parse: {e})"),
                }
            }
        }
    }

    let Some(invite) = their_invite else {
        println!(
            "\nNo published invite found for that npub. The user must have opened \
             Iris at least once so it publishes their invite. Try again later."
        );
        return;
    };

    // 3. Accept → publish the kind-1059 response.
    let response = match manager.accept_invite(&invite, None, now) {
        Ok(r) => r,
        Err(e) => {
            println!("accept failed: {e}");
            return;
        }
    };
    println!("\nAccepted; publishing kind-{} response…", response.kind);
    pool.send(&response).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 4. Send the real kind-1060 ratchet message(s).
    let events = match manager.send(&invite.inviter, MESSAGE.as_bytes(), now + 1) {
        Ok(ev) => ev,
        Err(e) => {
            println!("send failed: {e}");
            return;
        }
    };
    println!(
        "Publishing {} ratchet message event(s) (kind 1060): \"{MESSAGE}\"",
        events.len()
    );
    for ev in &events {
        pool.send(ev).unwrap();
    }

    // 5. Collect OK acks.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        if let Ok(Some(NostrRelayEvent::SentOk(_, id, ok, msg))) =
            tokio::time::timeout(Duration::from_millis(200), pool.recv()).await
        {
            println!("OK id={}… accepted={ok} msg=\"{msg}\"", &id[..8.min(id.len())]);
        }
    }
    println!("\nDone — check your Iris private chats.");
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
