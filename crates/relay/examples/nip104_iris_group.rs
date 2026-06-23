//! Join a real Iris **group** and decrypt its messages — by establishing a
//! proper 1:1 session to a *known target npub* first.
//!
//! ## Why the passive listener failed
//! Earlier versions tried to *mirror* a responder session from whatever
//! kind-1060 author key drifted by on the relay. That only ever decrypts a
//! session's message #0, and the group creator's distribution rides a session
//! we were never part of — so we decrypted nothing the creator's device sent.
//!
//! ## The fix: actively `accept` the target's invite
//! Iris publishes every device's **public invite** (kind-30078,
//! `l=double-ratchet/invites`). To talk to a specific person we:
//!   1. fetch *their* published invites by **owner pubkey**,
//!   2. [`SessionManager::accept_invite`] each → install an **initiator**
//!      session keyed under the target and publish the kind-1059 response,
//!   3. the target's device now shares a real ratchet session with us, and
//!      its **sender-key distribution** (kind-10446) arrives over it,
//!   4. feed group outer events (kind-1060, empty tags) to the
//!      [`GroupManager`] to decrypt the actual messages.
//!
//! Run: `cargo run -p nostro2-relay --example nip104_iris_group -- <npub>`
//! (defaults to the configured TARGET_NPUB below).

use nostro2::{NostrRelayEvent, NostrSigner as _};
use nostro2_nips::{GroupManager, Invite, SessionManager};
use nostro2_signer::NostrKeypair;
use nostro2_traits::bech32::Bech32Crypto;
use std::time::{Duration, Instant};

const RELAYS: &[&str] = &[
    "wss://relay.primal.net",
    "wss://temp.iris.to",
    "wss://vault.iris.to",
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
];

// Same stable identity as the 1:1 listener:
//   npub1hceqspekvhdafjhpzjtqfyrlhj6z7gmh7kpfnvuqwreudt2mn2xsk3rhyk
const OUR_NSEC: &str = "nsec17qf72rfytl0rdvtu3sy2m365xmxqeynghnl5tflftwnwxyhnglvsauzfgp";

// The person whose group we want to join (their owner npub).
const TARGET_NPUB: &str =
    "npub1k2flev40w4lx0c3txdymtw92ht2saxy9cyew4l64mrv4yqxz3mtsnn0tlm";

const INVITE_KIND: u32 = 30078;
const MESSAGE_KIND: u32 = 1060;

#[tokio::main]
async fn main() {
    let me = NostrKeypair::from_nsec(OUR_NSEC).expect("bad nsec");
    let our_hex = me.public_key();

    // Resolve target npub (CLI arg overrides the default) → x-only hex.
    let target_npub = std::env::args().nth(1).unwrap_or_else(|| TARGET_NPUB.to_owned());
    let target_hex = npub_to_hex(&target_npub).expect("bad target npub");

    println!("== NIP-104 GROUP join via active invite-accept ==");
    println!("our npub    : {}", me.npub().unwrap());
    println!("target npub : {target_npub}");
    println!("target hex  : {target_hex}\n");

    let pool = nostro2_relay::NostrPool::new(RELAYS);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The 1:1 router (follows key rotation) and the group state machine.
    let mut manager = SessionManager::<NostrKeypair>::new(me);
    let mut groups = GroupManager::<NostrKeypair>::new(our_hex.clone());

    // ── Phase 0: discover the target's DEVICE keys ──
    // In Iris's multi-device model, invites are published by each *device*
    // key, not the owner. The owner publishes an `app-keys` registry
    // (kind-30078, d=double-ratchet/app-keys) listing its devices via
    // `["device", <hex>, <ts>]` tags. Fetch it to learn who to look for.
    let mut appkeys_sub = nostro2::NostrSubscription::new()
        .kind(INVITE_KIND)
        .author(target_hex.clone())
        .limit(10);
    appkeys_sub.add_tag("#d", "double-ratchet/app-keys");
    pool.send(appkeys_sub).unwrap();

    println!("Phase 0: discovering target's device keys (6s)…");
    let mut devices: Vec<String> = Vec::new();
    let phase0 = Instant::now();
    while phase0.elapsed() < Duration::from_secs(6) {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), pool.recv()).await
        else {
            continue;
        };
        let NostrRelayEvent::NewNote(_, _, note) = ev else {
            continue;
        };
        if note.kind != INVITE_KIND || note.pubkey != target_hex {
            continue;
        }
        for row in note.tags.iter() {
            if row.first().map(String::as_str) == Some("device") {
                if let Some(dev) = row.get(1) {
                    if !devices.contains(dev) {
                        println!("  📱 device {}", short(dev));
                        devices.push(dev.clone());
                    }
                }
            }
        }
    }
    // The owner key itself can also publish an invite (single-device case).
    if !devices.contains(&target_hex) {
        devices.push(target_hex.clone());
    }
    println!("Phase 0 done: {} device key(s) known.\n", devices.len());

    // ── Phase 1: fetch invites published by ANY of those device keys ──
    let mut inv_sub = nostro2::NostrSubscription::new()
        .kind(INVITE_KIND)
        .authors(devices.iter().cloned().collect())
        .limit(100);
    inv_sub.add_tag("#l", "double-ratchet/invites");
    pool.send(inv_sub).unwrap();

    println!("Phase 1: fetching target's invites (10s)…");
    let mut accepted = 0_u32;
    let mut invites: Vec<Invite> = Vec::new();
    // Sessions are keyed under each invite's DEVICE pubkey, so remember them.
    let mut peer_keys: Vec<String> = Vec::new();
    let phase1 = Instant::now();
    while phase1.elapsed() < Duration::from_secs(10) {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), pool.recv()).await
        else {
            continue;
        };
        let NostrRelayEvent::NewNote(_, _, note) = ev else {
            continue;
        };
        if note.kind != INVITE_KIND || !devices.contains(&note.pubkey) {
            continue;
        }
        let Ok(invite) = Invite::from_event(&note) else {
            continue;
        };
        // Skip the app-keys/device record (no ephemeralKey → from_event errors
        // already), and dedupe by ephemeral key.
        if invites
            .iter()
            .any(|i| i.inviter_ephemeral_pubkey == invite.inviter_ephemeral_pubkey)
        {
            continue;
        }
        let device = invite.device_id.clone().unwrap_or_else(|| "?".into());
        println!(
            "  📨 invite: device {} ephemeral {}",
            short(&device),
            short(&invite.inviter_ephemeral_pubkey),
        );
        // Accept it: install an initiator session under the target owner and
        // publish the kind-1059 response so their device opens its side.
        match manager.accept_invite(&invite, Some(&our_hex), unix_now()) {
            Ok(response) => {
                pool.send(&response).expect("publish invite-response");
                accepted += 1;
                peer_keys.push(invite.inviter.clone());
                println!(
                    "  🤝 accepted → installed initiator session, published 1059 {}",
                    short(response.id.as_deref().unwrap_or("?")),
                );
            }
            Err(e) => println!("  · accept failed: {e}"),
        }
        invites.push(invite);
    }
    println!(
        "Phase 1 done: accepted {accepted} invite(s); {} session(s) with target.\n",
        manager.session_count(),
    );
    if accepted == 0 {
        println!(
            "No invites from target — they may not have published one, or use a\n\
             different relay set. Cannot join their group without a session."
        );
        return;
    }

    // ── Phase 2: send a hello so OUR send-chain opens, then listen ──
    // As initiator we MUST speak first: the ratchet's receiving chain on the
    // target's side only opens once it processes an initiator message. Until
    // then their device cannot send us the sender-key distribution. Send to
    // each device session we just installed (keyed under the device pubkey).
    let hello = hello_rumor(&our_hex, &target_hex);
    let mut sent = 0_u32;
    for peer in &peer_keys {
        match manager.send(peer, hello.as_bytes(), unix_now()) {
            Ok(events) => {
                for e in &events {
                    pool.send(e).ok();
                }
                sent += u32::try_from(events.len()).unwrap_or(0);
            }
            Err(e) => println!("  · hello to {} failed: {e}", short(peer)),
        }
    }
    println!("Phase 2: sent {sent} hello(s) to open send chain.");

    // Listen for: kind-1059 responses from the target's devices (rotated
    // sessions), kind-10446 distributions over our sessions, and the group's
    // kind-1060 outer messages.
    let since = u64::try_from(unix_now() - 7 * 24 * 3600).unwrap();
    let msg_sub = nostro2::NostrSubscription::new()
        .kind(MESSAGE_KIND)
        .since(since)
        .limit(2000);
    pool.send(msg_sub).unwrap();

    println!(
        "Phase 3: listening 150s. ⏰ NOW create a NEW group in Iris, add my npub,\n\
         and send a message — the sender-key distribution rides our live session.\n"
    );

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pending: Vec<nostro2::NostrNote> = Vec::new();
    let mut distributions = 0_u32;
    let mut group_msgs = 0_u32;
    let mut dms = 0_u32;

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(150) {
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
        let id = note
            .id
            .clone()
            .unwrap_or_else(|| format!("{}:{}", note.pubkey, note.content));
        if !seen.insert(id) {
            continue;
        }

        let has_header = !note.tags.find_tags("header").is_empty();
        if has_header {
            // 1:1 ratchet event — route through the manager (rotation-aware).
            if let Some(msg) = manager.process_event(&note) {
                if let Some(rumor) = parse_rumor(&msg.plaintext) {
                    match groups.apply_distribution_rumor(&rumor) {
                        Ok(Some(dist)) => {
                            distributions += 1;
                            println!(
                                "🔑 DISTRIBUTION from {} — group {}, sender-event {}",
                                short(&msg.peer),
                                short(&dist.group_id),
                                short(&dist.sender_event_pubkey),
                            );
                            // May unlock buffered group outer events.
                            let retry = std::mem::take(&mut pending);
                            for g in retry {
                                if !try_group(&g, &mut groups, &mut group_msgs) {
                                    pending.push(g);
                                }
                            }
                        }
                        _ => {
                            dms += 1;
                            println!(
                                "🔓 1:1 from {} — inner kind {} : {}",
                                short(&msg.peer),
                                rumor.kind,
                                preview(&rumor.content),
                            );
                        }
                    }
                }
            }
        } else {
            // Empty-tag 1060 — a group outer event.
            if !try_group(&note, &mut groups, &mut group_msgs) {
                pending.push(note);
            }
        }
    }

    println!(
        "\nDone. {dms} DM(s), {distributions} distribution(s), {group_msgs} group message(s). \
         {} group event(s) still pending (no key).",
        pending.len(),
    );
}

/// Try to decrypt `note` as a group outer event. Returns true on success.
fn try_group(
    note: &nostro2::NostrNote,
    groups: &mut GroupManager<NostrKeypair>,
    group_msgs: &mut u32,
) -> bool {
    match groups.decrypt_event(note) {
        Ok(Some(msg)) => {
            *group_msgs += 1;
            let text = String::from_utf8_lossy(&msg.plaintext);
            let shown = bourne::parse_str::<nostro2::NostrNote>(&text)
                .map(|r| r.content)
                .unwrap_or_else(|_| text.into_owned());
            println!(
                "\n💬 GROUP MSG in {} from {} : \"{shown}\"\n",
                short(&msg.group_id),
                short(&msg.sender_event_pubkey),
            );
            true
        }
        _ => false,
    }
}

/// Build a minimal kind-14 chat rumor as our hello plaintext.
fn hello_rumor(our_hex: &str, target_hex: &str) -> String {
    format!(
        r#"{{"pubkey":"{our_hex}","created_at":{},"kind":14,"tags":[["p","{target_hex}"]],"content":"hello from nostro2"}}"#,
        unix_now(),
    )
}

fn parse_rumor(plaintext: &[u8]) -> Option<nostro2::NostrNote> {
    let text = String::from_utf8_lossy(plaintext);
    bourne::parse_str(&text).ok()
}

fn npub_to_hex(npub: &str) -> Option<String> {
    let (hrp, bytes) = Bech32Crypto::decode(npub).ok()?;
    if hrp != "npub" || bytes.len() != 32 {
        return None;
    }
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    Some(s)
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

fn short(s: &str) -> &str {
    &s[..8.min(s.len())]
}

fn preview(s: &str) -> String {
    s.chars().take(120).collect::<String>().replace('\n', " ")
}
