//! Receive a real NIP-104 (double ratchet) message from Iris — decrypted.
//!
//! Captured wire reality (from chat.iris.to): when you message us, Iris does
//! **not** publish a kind-1059 invite-response. It bootstraps the ratchet on
//! its side from our public invite and sends **kind-1060 ratchet messages
//! straight away**, signed by its fresh *session key* (the event `pubkey`).
//!
//! That session key is exactly the value a 1059 response would have carried.
//! Since we hold the invite's ephemeral secret + shared secret, we can build
//! the mirror **responder session directly** from the 1060 sender — no 1059
//! needed — and decrypt. This is the Iris-compatible receive path.
//!
//! 1. Publish our deterministic public invite (stable ephemeral + shared secret).
//! 2. Subscribe for kind-1060 authored by Iris's session key.
//! 3. First message → `Session::new_responder(sender, our_ephemeral_sk,
//!    shared_secret)`; decrypt; keep the evolving session for the rest.
//! 4. Print every decrypted message.
//! 5. On a kind:14 DM, reply through the evolved session: build a NIP-17
//!    rumor, `plan_send_event`, publish the kind-1060 back.
//!
//! CONFIRMED LIVE against chat.iris.to: decrypted a real kind:14 DM and our
//! reply ("hello from nostro2 — native NIP-104 ratchet 🦀") rendered in the
//! Iris chat, emoji intact — a fully bidirectional native double-ratchet
//! conversation with no rust-nostr dependency.
//!
//! Run: `cargo run -p nostro2-relay --example nip104_iris_listen`

use nostro2::{NostrKeypair as _, NostrRelayEvent, NostrSigner as _};
use nostro2_nips::{Invite, Session};
use nostro2_signer::NostrKeypair;
use std::time::{Duration, Instant};

const RELAYS: &[&str] = &[
    "wss://relay.primal.net",
    "wss://temp.iris.to",
    "wss://vault.iris.to",
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
];

// Our stable identity:
//   npub1hceqspekvhdafjhpzjtqfyrlhj6z7gmh7kpfnvuqwreudt2mn2xsk3rhyk
const OUR_NSEC: &str = "nsec17qf72rfytl0rdvtu3sy2m365xmxqeynghnl5tflftwnwxyhnglvsauzfgp";
// Stable ephemeral key for the invite (deterministic invite across runs).
const EPHEMERAL_NSEC: &str = "nsec13qeeps3q7xgzpwpdz8786k0h7gxw67j3jmdku8hnhw42frpvzewse6w7ql";
// Fixed 32-byte shared secret (hex).
const SHARED_SECRET: &str =
    "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

const MESSAGE_KIND: u32 = 1060;

#[tokio::main]
async fn main() {
    let me = NostrKeypair::from_nsec(OUR_NSEC).expect("bad nsec");
    let our_hex = me.public_key();
    let eph = NostrKeypair::from_nsec(EPHEMERAL_NSEC).expect("bad ephemeral nsec");
    let ephemeral = eph.public_key();
    let shared = hex32(SHARED_SECRET);
    let eph_secret = eph.secret_bytes();

    println!("== NIP-104 listener — decrypt Iris ratchet messages ==");
    println!("our npub : {}", me.npub().unwrap());
    println!("invite ephemeral key : {ephemeral}");

    // 1. (Re)publish our deterministic public invite so Iris can find it.
    let invite = Invite {
        inviter_ephemeral_pubkey: ephemeral.clone(),
        shared_secret: SHARED_SECRET.to_string(),
        inviter: our_hex.clone(),
        inviter_ephemeral_privkey: Some(eph.secret_key()),
        device_id: Some(our_hex.clone()),
    };
    let mut invite_event = invite.to_event(unix_now()).expect("build invite event");
    invite_event.sign_with(&me).expect("sign invite");

    let pool = nostro2_relay::NostrPool::new(RELAYS);
    tokio::time::sleep(Duration::from_secs(2)).await;
    pool.send(&invite_event).expect("publish invite");

    // 2. kind-1060 from the last 2h — wide enough to tolerate clock skew and
    //    NIP-104 timestamp jitter. We dedup + trial-decrypt in code.
    let since = u64::try_from(unix_now() - 7200).unwrap();
    let sub = nostro2::NostrSubscription::new()
        .kind(MESSAGE_KIND)
        .since(since)
        .limit(500);
    pool.send(sub).unwrap();
    println!("Listening 120s. Send a message from Iris now if you haven't…\n");

    // 3. Trial-decrypt every 1060 against our invite. Keep a per-sender session.
    let mut sessions: std::collections::HashMap<String, Session<NostrKeypair>> =
        std::collections::HashMap::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut decrypted = 0_u32;
    let mut total_1060 = 0_u32;
    let mut replied = false;

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(120) {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), pool.recv()).await
        else {
            continue;
        };
        let NostrRelayEvent::NewNote(_, _, note) = ev else {
            continue;
        };
        if note.kind != MESSAGE_KIND {
            continue;
        }
        if !seen.insert(format!("{}:{}", note.pubkey, note.content)) {
            continue; // dedup across relays
        }
        total_1060 += 1;

        // Try an existing session for this sender first, else bootstrap one.
        let sender = note.pubkey.clone();
        let their_session = hex32(&sender);
        let entry = sessions.entry(sender.clone()).or_insert_with(|| {
            Session::<NostrKeypair>::new_responder(&their_session, &eph_secret, &shared)
                .expect("bootstrap responder")
        });
        match entry.plan_receive_event(&note) {
            Ok((next, plaintext)) => {
                entry.apply(next);
                decrypted += 1;
                let text = String::from_utf8_lossy(&plaintext);
                println!(
                    "\n\u{1f4e9} DECRYPTED from {} (t={}) : \"{text}\"\n",
                    short(&sender), note.created_at
                );

                // The ratchet payload IS a NIP-17 rumor (a NostrNote). Parse it.
                // Only reply to real DMs (kind 14), not typing (kind 25), once.
                let Ok(rumor): Result<nostro2::NostrNote, _> = bourne::parse_str(&text) else {
                    continue;
                };
                let their_id = rumor.pubkey.clone();
                if rumor.kind != 14 || their_id.is_empty() || replied {
                    continue;
                }

                // Build a NIP-17 kind:14 rumor reply: our pubkey, their id in `p`.
                let reply_text = "hello from nostro2 — native NIP-104 ratchet 🦀";
                let mut rumor_reply = nostro2::NostrNote {
                    pubkey: our_hex.clone(),
                    created_at: unix_now(),
                    kind: 14,
                    content: reply_text.to_string(),
                    ..Default::default()
                };
                rumor_reply.tags.add_custom_tag("p", &their_id);
                rumor_reply.serialize_id().ok();
                let rumor_json = rumor_reply.serialize().unwrap();

                // Send it through the evolved ratchet session.
                match entry.plan_send_event(rumor_json.as_bytes(), unix_now()) {
                    Ok((next2, reply_event)) => {
                        entry.apply(next2);
                        pool.send(&reply_event).expect("publish reply");
                        replied = true;
                        println!(
                            "\u{2709}  REPLIED to {} : \"{reply_text}\" (kind-1060 {})\n",
                            short(&their_id),
                            reply_event.id.as_deref().map_or("?", |s| &s[..8])
                        );
                    }
                    Err(e) => println!("  · reply send failed: {e}"),
                }
            }
            Err(e) => {
                println!("  · 1060 from {} (t={}) — no decrypt ({e})", short(&sender), note.created_at);
            }
        }
    }

    println!("\nSaw {total_1060} kind-1060 event(s); decrypted {decrypted}.");
}

fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

/// Decode a 64-char hex string into 32 bytes.
fn hex32(s: &str) -> [u8; 32] {
    let mut out = [0_u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("bad hex");
    }
    out
}

fn short(s: &str) -> &str {
    &s[..8.min(s.len())]
}
