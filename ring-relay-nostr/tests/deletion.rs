//! NIP-09 deletion end-to-end.
//!
//! - Author publishes a kind 1, then kind 5 referencing it. Subsequent
//!   REQ replays the kind 5 but not the kind 1.
//! - Re-publishing the deleted id is silently dropped (no replay).
//! - An attacker's kind 5 referencing someone else's event id MUST NOT
//!   delete that event.
//! - Deletion survives a relay restart: the kind 5 is still on disk and
//!   gets replayed at startup, so the kind 1 is suppressed in the new
//!   process too.
//!
//! All cases run with storage enabled — NIP-09 is only meaningful in
//! storage mode (the ephemeral relay has no history to delete).

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig, StorageConfig};
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsClient = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct RelayGuard {
    shutdown: Option<ring_relay_nostr::ShutdownHandle>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RelayGuard {
    fn drop(&mut self) {
        if let Some(s) = self.shutdown.take() {
            s.shutdown();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn spawn_relay_with_dir(data_dir: PathBuf) -> (u16, RelayGuard) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut config = RelayConfig::default();
        config.storage = Some(StorageConfig {
            data_dir,
            ..StorageConfig::default()
        });
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    let (port, shutdown) = rx.recv().unwrap();
    (
        port,
        RelayGuard {
            shutdown: Some(shutdown),
            handle: Some(handle),
        },
    )
}

async fn connect(port: u16) -> WsClient {
    for attempt in 0..20 {
        let url = format!("ws://127.0.0.1:{port}/");
        match connect_async(&url).await {
            Ok((ws, _)) => return ws,
            Err(_) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("connect failed: {e}"),
        }
    }
    unreachable!();
}

async fn send(ws: &mut WsClient, json: &str) {
    ws.send(Message::Text(json.to_string().into()))
        .await
        .unwrap();
}

async fn recv_text(ws: &mut WsClient) -> Value {
    loop {
        // 15s budget so the test isn't flaky when cargo runs many
        // integration suites in parallel — each suite spins up its own
        // tokio runtime and ring-relay shards, oversubscribing the CPU
        // until kind-5 deletion's storage-thread work catches up.
        let msg = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("recv timed out")
            .expect("stream closed")
            .expect("ws error");
        match msg {
            Message::Text(t) => return serde_json::from_str(&t).expect("valid json"),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => panic!("relay closed connection unexpectedly"),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

fn signed_text(kp: &K256Keypair, content: &str) -> NostrNote {
    let mut n = NostrNote::text_note(content);
    n.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut n).expect("sign");
    assert!(n.verify());
    n
}

fn signed_deletion(kp: &K256Keypair, target_ids: &[&str]) -> NostrNote {
    let mut n = NostrNote {
        kind: 5,
        content: "delete".into(),
        pubkey: kp.public_key(),
        created_at: nostro2::NostrNote::now(),
        ..NostrNote::default()
    };
    for id in target_ids {
        n.tags.add_custom_tag("e", id);
    }
    kp.sign_nostr_note(&mut n).expect("sign deletion");
    assert!(n.verify());
    n
}

async fn publish_and_ack(ws: &mut WsClient, evt: &NostrNote) -> bool {
    send(ws, &serde_json::to_string(&("EVENT", evt)).unwrap()).await;
    let resp = recv_text(ws).await;
    assert_eq!(resp[0], "OK");
    resp[2].as_bool().unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kind5_removes_referenced_event_from_replay() {
    let dir = tempfile::tempdir().unwrap();
    let (port, guard) = spawn_relay_with_dir(dir.path().to_path_buf());

    let kp = K256Keypair::generate();
    let note = signed_text(&kp, "delete me");
    let note_id = note.id.clone().unwrap();

    let mut publisher = connect(port).await;
    assert!(publish_and_ack(&mut publisher, &note).await);

    let deletion = signed_deletion(&kp, &[&note_id]);
    assert!(publish_and_ack(&mut publisher, &deletion).await);

    // Give the storage thread a moment to apply the deletion (it runs
    // after commit, on the storage loop's next iteration).
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Subscribe broadly. Should see the kind-5 (deletions are visible)
    // but NOT the original kind-1 (deleted).
    let mut sub = connect(port).await;
    let req = format!(r#"["REQ","s1",{{"authors":["{}"]}}]"#, kp.public_key());
    send(&mut sub, &req).await;

    let mut saw_deletion = false;
    let mut saw_target = false;
    loop {
        let msg = recv_text(&mut sub).await;
        if msg[0] == "EOSE" {
            break;
        }
        assert_eq!(msg[0], "EVENT");
        let id = msg[2]["id"].as_str().unwrap_or("");
        if id == note_id {
            saw_target = true;
        } else if id == deletion.id.as_deref().unwrap_or("") {
            saw_deletion = true;
        }
    }
    assert!(saw_deletion, "kind-5 should be visible in replay");
    assert!(!saw_target, "deleted kind-1 must not be replayed");

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn republish_of_deleted_id_is_silently_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (port, guard) = spawn_relay_with_dir(dir.path().to_path_buf());

    let kp = K256Keypair::generate();
    let note = signed_text(&kp, "delete me");
    let note_id = note.id.clone().unwrap();

    let mut publisher = connect(port).await;
    assert!(publish_and_ack(&mut publisher, &note).await);

    let deletion = signed_deletion(&kp, &[&note_id]);
    assert!(publish_and_ack(&mut publisher, &deletion).await);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Republish the same kind-1 — relay still acks OK=true (the shard
    // can't see the storage-thread state in v1), but storage drops it
    // and a fresh REQ won't return it.
    let resp = publish_and_ack(&mut publisher, &note).await;
    let _ = resp; // accept either; the assertion is on the REQ side.

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    let req = format!(
        r#"["REQ","s1",{{"authors":["{}"],"kinds":[1]}}]"#,
        kp.public_key()
    );
    send(&mut sub, &req).await;

    let msg = recv_text(&mut sub).await;
    assert_eq!(
        msg[0], "EOSE",
        "deleted id must not reappear after republish; got: {msg:?}"
    );

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deletion_from_wrong_pubkey_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (port, guard) = spawn_relay_with_dir(dir.path().to_path_buf());

    let alice = K256Keypair::generate();
    let bob = K256Keypair::generate();

    let alice_note = signed_text(&alice, "alice's note");
    let alice_note_id = alice_note.id.clone().unwrap();

    let mut a_pub = connect(port).await;
    assert!(publish_and_ack(&mut a_pub, &alice_note).await);

    // Bob attempts to delete Alice's note.
    let bob_deletion = signed_deletion(&bob, &[&alice_note_id]);
    let mut b_pub = connect(port).await;
    assert!(publish_and_ack(&mut b_pub, &bob_deletion).await);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice's note must still appear.
    let mut sub = connect(port).await;
    let req = format!(
        r#"["REQ","s1",{{"authors":["{}"],"kinds":[1]}}]"#,
        alice.public_key()
    );
    send(&mut sub, &req).await;

    let first = recv_text(&mut sub).await;
    assert_eq!(first[0], "EVENT", "alice's note must survive bob's deletion");
    assert_eq!(first[2]["id"], alice_note_id);

    let eose = recv_text(&mut sub).await;
    assert_eq!(eose[0], "EOSE");

    drop(guard);
}

/// Pre-emptive poisoning regression: Eve publishes a kind-5 referencing
/// a 32-byte hex string that nobody has yet posted. If the relay records
/// the id unconditionally, Alice's later publish of an event whose id
/// happens to match would be silently dropped. The fix records the
/// deleter's pubkey alongside any unknown id and only suppresses
/// re-publishes from that same pubkey.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_id_deletion_does_not_poison_other_pubkeys() {
    let dir = tempfile::tempdir().unwrap();
    let (port, guard) = spawn_relay_with_dir(dir.path().to_path_buf());

    let alice = K256Keypair::generate();
    let eve = K256Keypair::generate();

    // Alice signs a note but doesn't publish it yet. Eve grabs the id
    // (e.g. by side channel) and tries to pre-delete it.
    let alice_note = signed_text(&alice, "future note");
    let alice_id = alice_note.id.clone().unwrap();

    let eve_deletion = signed_deletion(&eve, &[&alice_id]);
    let mut e_pub = connect(port).await;
    assert!(publish_and_ack(&mut e_pub, &eve_deletion).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice now publishes. Storage must accept and replay it because
    // Eve's deletion didn't cover Alice's pubkey.
    let mut a_pub = connect(port).await;
    assert!(publish_and_ack(&mut a_pub, &alice_note).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    let req = format!(
        r#"["REQ","s1",{{"authors":["{}"],"kinds":[1]}}]"#,
        alice.public_key()
    );
    send(&mut sub, &req).await;
    let first = recv_text(&mut sub).await;
    assert_eq!(
        first[0], "EVENT",
        "alice's note must survive eve's pre-emptive deletion; got: {first:?}"
    );
    assert_eq!(first[2]["id"], alice_id);

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deletion_survives_relay_restart() {
    let dir: TempDir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let kp = K256Keypair::generate();
    let note = signed_text(&kp, "delete me, persistently");
    let note_id = note.id.clone().unwrap();

    // Phase 1: write the note and the deletion, then shut down.
    {
        let (port, guard) = spawn_relay_with_dir(path.clone());
        let mut publisher = connect(port).await;
        assert!(publish_and_ack(&mut publisher, &note).await);
        let deletion = signed_deletion(&kp, &[&note_id]);
        assert!(publish_and_ack(&mut publisher, &deletion).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(guard);
    }

    // Phase 2: relaunch on the same data_dir. The deletion replay at
    // startup should re-suppress the kind-1.
    let (port, guard) = spawn_relay_with_dir(path);
    let mut sub = connect(port).await;
    let req = format!(
        r#"["REQ","s1",{{"authors":["{}"],"kinds":[1]}}]"#,
        kp.public_key()
    );
    send(&mut sub, &req).await;

    let msg = recv_text(&mut sub).await;
    assert_eq!(
        msg[0], "EOSE",
        "deletion must persist across restart; got: {msg:?}"
    );

    drop(guard);
}
