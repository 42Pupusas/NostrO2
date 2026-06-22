//! Live NIP-104 (Double Ratchet) invite interop against real relays / Iris.
//!
//! This drives the invite bootstrap layer against *production* data:
//!
//! 1. Connect to the relays Iris (`chat.iris.to`) publishes to.
//! 2. Subscribe for real, published double-ratchet invites — kind 30078 with
//!    the `l = double-ratchet/invites` label — and parse each through our own
//!    [`Invite::from_event`]. Parsing + signature-verifying a live Iris invite
//!    is the real interop gate for the codec.
//! 3. For the first invite we successfully parse, generate a fresh identity,
//!    [`Invite::accept`] it, and publish the signed kind-1059 invite-response
//!    back to the relays. A real Iris user running that invite will have their
//!    `SessionManager` peel the response and open a session with us.
//!
//! A *fully* automated round-trip needs a human on the other end to reply, so
//! this stops after publishing — it proves everything up to and including "Iris
//! can receive what we send". Pass an invite URL as the first CLI arg to skip
//! discovery and accept a specific invite instead.
//!
//! Run (discover from the wire):
//!   `cargo run -p nostro2-relay --example nip104_iris_invite`
//! Run (accept a known invite URL):
//!   `cargo run -p nostro2-relay --example nip104_iris_invite -- 'https://iris.to/#…'`

use nostro2::{NostrKeypair as _, NostrRelayEvent, NostrSigner as _};
use nostro2_nips::Invite;
use nostro2_signer::NostrKeypair;
use std::time::{Duration, Instant};

// Relays Iris reads/writes for double-ratchet traffic.
const RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://nos.lol",
    "wss://relay.nostr.band",
    "wss://relay.snort.social",
    "wss://nostr.wine",
];

const INVITE_KIND: u32 = 30078;
const INVITE_LABEL: &str = "double-ratchet/invites";

#[tokio::main]
async fn main() {
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();

    // Fresh throwaway identity for this run.
    let me = NostrKeypair::generate();
    println!("== NIP-104 invite interop ==");
    println!("our identity npub : {}", me.npub().unwrap());
    println!("our identity hex  : {}", me.public_key());

    let pool = nostro2_relay::NostrPool::new(RELAYS);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Path A: a specific invite URL was passed — accept it directly.
    if let Some(url) = std::env::args().nth(1) {
        println!("\nAccepting invite from CLI URL…");
        match Invite::from_url(&url) {
            Ok(invite) => accept_and_publish(&pool, &invite, &me, now),
            Err(e) => println!("could not parse invite URL: {e}"),
        }
        // Give the publish a moment to flush.
        collect_oks(&pool, Duration::from_secs(6)).await;
        return;
    }

    // Path B: discover a real published invite off the wire.
    let subscription = nostro2::NostrSubscription::new()
        .kind(INVITE_KIND)
        .tag("#l", INVITE_LABEL)
        .limit(50);
    pool.send(subscription).unwrap();
    println!("\nListening for live double-ratchet invites (kind {INVITE_KIND})…\n");

    let start = Instant::now();
    let mut parsed = 0_u32;
    let mut accepted = false;

    while start.elapsed() < Duration::from_secs(20) {
        let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(200), pool.recv()).await
        else {
            continue;
        };

        match ev {
            NostrRelayEvent::NewNote(_, _, note) if note.kind == INVITE_KIND => {
                match Invite::from_event(&note) {
                    Ok(invite) => {
                        parsed += 1;
                        println!(
                            "[{parsed:>3}] parsed invite from {}  ephemeral={}…  device={:?}",
                            short(&invite.inviter),
                            short(&invite.inviter_ephemeral_pubkey),
                            invite.device_id.as_deref(),
                        );
                        // Accept the first one and publish a response back.
                        if !accepted {
                            accepted = true;
                            accept_and_publish(&pool, &invite, &me, now);
                        }
                    }
                    Err(e) => println!("     (skipped a kind-{INVITE_KIND} note: {e})"),
                }
            }
            NostrRelayEvent::SentOk(_, id, ok, msg) => {
                println!("     OK id={}… accepted={ok} msg=\"{msg}\"", short(&id));
            }
            _ => {}
        }
    }

    println!("\nParsed {parsed} live invite(s) through Invite::from_event.");
    if accepted {
        println!("Published an invite-response — a live Iris peer can now open a session with us.");
    } else {
        println!("No invites seen this run; try again or pass an invite URL as an argument.");
    }
}

/// Accept an invite with our identity and publish the kind-1059 response.
fn accept_and_publish(
    pool: &nostro2_relay::NostrPool,
    invite: &Invite,
    me: &NostrKeypair,
    now: i64,
) {
    match invite.accept::<NostrKeypair>(me, None, now) {
        Ok((_session, response)) => {
            println!(
                "  → accepted; publishing kind-{} response (id {:?})",
                response.kind, response.id
            );
            if let Err(e) = pool.send(&response) {
                println!("  → publish failed: {e}");
            }
        }
        Err(e) => println!("  → accept failed: {e}"),
    }
}

async fn collect_oks(pool: &nostro2_relay::NostrPool, dur: Duration) {
    let start = Instant::now();
    while start.elapsed() < dur {
        if let Ok(Some(NostrRelayEvent::SentOk(_, id, ok, msg))) =
            tokio::time::timeout(Duration::from_millis(200), pool.recv()).await
        {
            println!("OK id={}… accepted={ok} msg=\"{msg}\"", short(&id));
        }
    }
}

fn short(s: &str) -> &str {
    &s[..8.min(s.len())]
}
