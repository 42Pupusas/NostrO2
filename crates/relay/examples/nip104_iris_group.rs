//! Receive a real NIP-104 **group** message from Iris — decrypted, via the
//! `SessionManager` so we follow Double-Ratchet **key rotation**.
//!
//! ## Why the naive approach failed
//! NIP-104 rotates the Nostr event key on (nearly) every message. A 1:1
//! session bootstrapped from message #0 tracks those rotations through the
//! header-advertised next-key — *but only if you keep that session and
//! trial-decrypt later messages against it*. Bootstrapping a fresh responder
//! per event pubkey only ever decrypts a true message #0; every rotated
//! follow-up fails with "could not decrypt message header".
//!
//! ## The fix
//! Route every header-bearing kind-1060 through [`SessionManager`]:
//! 1. `process_event` trial-decrypts against **all** installed sessions —
//!    this is what follows rotation.
//! 2. On a miss, bootstrap a *candidate* responder from this event's pubkey
//!    (Iris's invite-less flow lets us mirror directly from the sender key)
//!    and install it. Candidates accumulate, so whichever one was the real
//!    message #0 will decrypt that session's rotated follow-ups.
//! 3. Each decrypted inner rumor: kind-10446 → install a group sender chain;
//!    a group's kind-1060 outer events (empty tags) → `decrypt_event`.
//!
//! Run: `cargo run -p nostro2-relay --example nip104_iris_group`

use nostro2::{NostrKeypair as _, NostrRelayEvent, NostrSigner as _};
use nostro2_nips::{GroupManager, Invite, Session, SessionManager, INVITE_RESPONSE_KIND};
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

// Same stable identity as the 1:1 listener:
//   npub1hceqspekvhdafjhpzjtqfyrlhj6z7gmh7kpfnvuqwreudt2mn2xsk3rhyk
const OUR_NSEC: &str = "nsec17qf72rfytl0rdvtu3sy2m365xmxqeynghnl5tflftwnwxyhnglvsauzfgp";
const EPHEMERAL_NSEC: &str = "nsec13qeeps3q7xgzpwpdz8786k0h7gxw67j3jmdku8hnhw42frpvzewse6w7ql";
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

    println!("== NIP-104 GROUP listener (SessionManager) ==");
    println!("our npub : {}", me.npub().unwrap());
    println!("invite ephemeral key : {ephemeral}");

    // 1. (Re)publish our deterministic public invite so Iris can reach us.
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

    // 2. Wide kind-1060 window: distributions + group messages both live here.
    //    Iris stamps events with a randomized clock running HOURS ahead of the
    //    bootstrap message's real timestamp, so a narrow `since` filters out
    //    message #0 — the one event that bootstraps the session. Look back a
    //    full week and lean on `limit` instead.
    let since = u64::try_from(unix_now() - 7 * 24 * 3600).unwrap();
    let sub = nostro2::NostrSubscription::new()
        .kind(MESSAGE_KIND)
        .since(since)
        .limit(2000);
    pool.send(sub).unwrap();

    // 2b. **The missing piece for multi-device peers.** A multi-device sender
    //     (like Iris's group creator) doesn't bootstrap from our invite by
    //     sending a 1060 straight away — it ACCEPTS our invite by publishing a
    //     kind-1059 invite-response to our ephemeral key, carrying its real
    //     session key inside. The distribution then rides THAT session. Without
    //     this subscription we never install the session and decrypt nothing
    //     the creator's device sends. Matches SessionManager.startInviteResponseListener.
    let mut resp_sub = nostro2::NostrSubscription::new()
        .kind(INVITE_RESPONSE_KIND)
        .since(since)
        .limit(500);
    resp_sub.add_tag("#p", &ephemeral);
    pool.send(resp_sub).unwrap();

    println!("Listening 180s. Create the new group, add our npub, send a message…\n");

    // The 1:1 router (follows key rotation) and the group state machine.
    let mut manager = SessionManager::<NostrKeypair>::new(me);
    let mut groups = GroupManager::<NostrKeypair>::new(our_hex.clone());

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Pubkeys we've already tried (and failed) to bootstrap a candidate from,
    // so we don't reinstall the same dead-end session repeatedly.
    let mut bootstrapped: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Group outer events we couldn't place yet (distribution may arrive later).
    let mut pending: Vec<nostro2::NostrNote> = Vec::new();
    // Header-bearing 1:1 events (incl. distributions) that didn't decrypt yet.
    // A 1059 invite-response can arrive AFTER the distributions it unlocks, so
    // we buffer and retry these whenever a new session is installed.
    let mut pending_dr: Vec<nostro2::NostrNote> = Vec::new();

    let mut distributions = 0_u32;
    let mut group_msgs = 0_u32;
    let mut dms = 0_u32;
    let mut total_1060 = 0_u32;

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(180) {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), pool.recv()).await
        else {
            continue;
        };
        let NostrRelayEvent::NewNote(_, _, note) = ev else {
            continue;
        };

        // Kind-1059 invite-response: install the peer's real session (the one
        // its distributions ride on), keyed under the peer's owner pubkey.
        if note.kind == INVITE_RESPONSE_KIND {
            match invite.receive::<NostrKeypair>(&note, &eph) {
                Ok((session, recovered)) => {
                    let peer = recovered
                        .owner_public_key
                        .clone()
                        .unwrap_or_else(|| recovered.invitee_identity.clone());
                    manager.install_session(&peer, &recovered.invitee_identity, session);
                    println!(
                        "🤝 INVITE-RESPONSE accepted — installed session for owner {} (device {})",
                        short(&peer),
                        short(&recovered.invitee_identity),
                    );
                    // A freshly installed session may decrypt distributions we
                    // already buffered. Retry the whole header backlog.
                    let retry = std::mem::take(&mut pending_dr);
                    for q in retry {
                        if let Some(rumor) = decrypt_1to1(
                            &q, &mut manager, &mut bootstrapped, &eph_secret, &shared,
                        ) {
                            handle_inner_rumor(&rumor, &q.pubkey, &mut groups, &mut distributions, &mut dms);
                            let gretry = std::mem::take(&mut pending);
                            for g in gretry {
                                if !try_group(&g, &mut groups, &mut group_msgs) {
                                    pending.push(g);
                                }
                            }
                        } else {
                            pending_dr.push(q);
                        }
                    }
                }
                Err(e) => {
                    println!("  · invite-response from {} rejected: {e}", short(&note.pubkey));
                }
            }
            continue;
        }

        if note.kind != MESSAGE_KIND {
            continue;
        }
        let id = note
            .id
            .clone()
            .unwrap_or_else(|| format!("{}:{}", note.pubkey, note.content));
        if !seen.insert(id) {
            continue; // dedup across relays
        }
        total_1060 += 1;

        let has_header = !note.tags.find_tags("header").is_empty();

        if has_header {
            // A genuine 1:1 ratchet event. Route it through the manager.
            if let Some(rumor) = decrypt_1to1(&note, &mut manager, &mut bootstrapped, &eph_secret, &shared) {
                handle_inner_rumor(&rumor, &note.pubkey, &mut groups, &mut distributions, &mut dms);
                // A new distribution may unlock earlier group outer events.
                let retry = std::mem::take(&mut pending);
                for q in retry {
                    if !try_group(&q, &mut groups, &mut group_msgs) {
                        pending.push(q);
                    }
                }
            } else {
                // No session matched yet — buffer for retry after a 1059 lands.
                pending_dr.push(note);
            }
        } else {
            // Empty-tag 1060: a group outer event. Decrypt via the group chains.
            if !try_group(&note, &mut groups, &mut group_msgs) {
                pending.push(note); // hold for a later distribution
            }
        }
    }

    println!(
        "\nSaw {total_1060} kind-1060; {dms} DM(s), {distributions} distribution(s), \
         {group_msgs} group message(s). {} group + {} header pending undecrypted.",
        pending.len(),
        pending_dr.len(),
    );
}

/// Decrypt a header-bearing 1060 through the `SessionManager`, following key
/// rotation. On a miss, bootstrap a candidate responder from the event pubkey
/// and retry once. Returns the decrypted inner rumor on success.
fn decrypt_1to1(
    note: &nostro2::NostrNote,
    manager: &mut SessionManager<NostrKeypair>,
    bootstrapped: &mut std::collections::HashSet<String>,
    eph_secret: &[u8; 32],
    shared: &[u8; 32],
) -> Option<nostro2::NostrNote> {
    // 1. Trial-decrypt against every installed session (rotation-following).
    if let Some(msg) = manager.process_event(note) {
        return parse_rumor(&msg.plaintext);
    }
    // 2. Miss: this may be a session message #0 we haven't bootstrapped yet.
    //    Mirror a responder directly from the sender key (Iris invite-less
    //    flow), install it as a candidate, and retry through the manager.
    let sender = note.pubkey.clone();
    if bootstrapped.insert(sender.clone()) {
        let their_session = hex32(&sender);
        if let Ok(session) =
            Session::<NostrKeypair>::new_responder(&their_session, eph_secret, shared)
        {
            manager.install_session(&sender, &sender, session);
            if let Some(msg) = manager.process_event(note) {
                return parse_rumor(&msg.plaintext);
            }
        }
    }
    None
}

/// Dispatch a decrypted inner rumor: a kind-10446 sender-key distribution
/// installs a group chain; anything else (kind-14 DM, kind-25 typing) is just
/// reported.
fn handle_inner_rumor(
    rumor: &nostro2::NostrNote,
    sender: &str,
    groups: &mut GroupManager<NostrKeypair>,
    distributions: &mut u32,
    dms: &mut u32,
) {
    // Trust model: the owner is the *session* peer, not rumor.pubkey. For this
    // single-peer test they coincide; a multi-peer client passes the
    // authenticated session identity here.
    match groups.apply_distribution_rumor(rumor) {
        Ok(Some(dist)) => {
            *distributions += 1;
            println!(
                "🔑 DISTRIBUTION from {} — group {}, sender-event {}",
                short(sender),
                short(&dist.group_id),
                short(&dist.sender_event_pubkey),
            );
        }
        _ => {
            *dms += 1;
            println!(
                "🔓 1:1 from {} — inner kind {} : {}",
                short(sender),
                rumor.kind,
                preview(&rumor.content),
            );
        }
    }
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
            // Inner is a kind-14 group chat rumor; show content if it parses.
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

fn parse_rumor(plaintext: &[u8]) -> Option<nostro2::NostrNote> {
    let text = String::from_utf8_lossy(plaintext);
    bourne::parse_str(&text).ok()
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

fn preview(s: &str) -> String {
    s.chars().take(120).collect::<String>().replace('\n', " ")
}
