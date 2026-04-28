//! NIP-09 deletion via the `a` (address) tag.
//!
//! `a`-tag references address replaceable (NIP-16) and parameterized
//! (NIP-33) events. Format: `"<kind>:<pubkey>:<d_tag>"`. For NIP-16
//! replaceable kinds the d_tag is the empty string.
//!
//! These tests cover what NIP-09 says about `a` tags:
//! - The existing slot at the deleted address is removed from REQ replay.
//! - Re-publishing a parameterized event at the same `(kind, pubkey, d_tag)`
//!   is dropped if its `created_at` is older than the deletion event's
//!   `created_at`. A *newer* event at the same address is allowed (the
//!   spec is explicit: deletion of an address doesn't permanently
//!   block future writes; it just clears the current state).
//! - Cross-pubkey `a` tags are ignored (Bob can't delete Alice's address).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig, StorageConfig};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsClient = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct RelayGuard {
    shutdown: Option<ring_relay_nostr::ShutdownHandle>,
    handle: Option<std::thread::JoinHandle<()>>,
    _data: Option<tempfile::TempDir>,
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

fn spawn_relay() -> (u16, RelayGuard) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut config = RelayConfig::default();
        config.storage = Some(StorageConfig {
            data_dir: path,
            ..StorageConfig::default()
        });
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind");
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
            _data: Some(dir),
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
        // See deletion.rs for rationale on the bumped budget.
        let msg = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("recv timed out")
            .expect("stream closed")
            .expect("ws error");
        match msg {
            Message::Text(t) => return serde_json::from_str(&t).expect("valid json"),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => panic!("relay closed unexpectedly"),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

fn signed_param_event(
    kp: &K256Keypair,
    kind: u32,
    d_tag: &str,
    content: &str,
    created_at: i64,
) -> NostrNote {
    let mut n = NostrNote {
        pubkey: kp.public_key(),
        content: content.to_string(),
        kind,
        created_at,
        tags: vec![vec!["d".to_string(), d_tag.to_string()]].into(),
        ..NostrNote::default()
    };
    kp.sign_nostr_note(&mut n).expect("sign");
    assert!(n.verify());
    n
}

fn signed_replaceable(kp: &K256Keypair, kind: u32, content: &str, created_at: i64) -> NostrNote {
    let mut n = NostrNote {
        pubkey: kp.public_key(),
        content: content.to_string(),
        kind,
        created_at,
        ..NostrNote::default()
    };
    kp.sign_nostr_note(&mut n).expect("sign");
    assert!(n.verify());
    n
}

fn signed_addr_deletion(kp: &K256Keypair, address: &str, created_at: i64) -> NostrNote {
    let mut n = NostrNote {
        kind: 5,
        content: "delete-by-address".into(),
        pubkey: kp.public_key(),
        created_at,
        tags: vec![vec!["a".to_string(), address.to_string()]].into(),
        ..NostrNote::default()
    };
    kp.sign_nostr_note(&mut n).expect("sign deletion");
    assert!(n.verify());
    n
}

async fn publish_ack(ws: &mut WsClient, evt: &NostrNote) -> bool {
    send(ws, &serde_json::to_string(&("EVENT", evt)).unwrap()).await;
    let resp = recv_text(ws).await;
    assert_eq!(resp[0], "OK");
    resp[2].as_bool().unwrap_or(false)
}

async fn drain_until_eose(ws: &mut WsClient) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        let msg = recv_text(ws).await;
        if msg[0] == "EOSE" {
            return out;
        }
        out.push(msg);
    }
}

/// Parameterized: kind 30000 with d-tag "draft-1" published, then deleted
/// by `a`-tag. The existing slot must disappear from REQ replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_removes_existing_parameterized_slot() {
    let (port, guard) = spawn_relay();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let evt = signed_param_event(&kp, 30000, "draft-1", "v1", 1_000_000);
    let mut publisher = connect(port).await;
    assert!(publish_ack(&mut publisher, &evt).await);

    let address = format!("30000:{pk}:draft-1");
    let deletion = signed_addr_deletion(&kp, &address, 1_000_001);
    assert!(publish_ack(&mut publisher, &deletion).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    send(
        &mut sub,
        &format!(r#"["REQ","s1",{{"authors":["{pk}"],"kinds":[30000]}}]"#),
    )
    .await;
    let events = drain_until_eose(&mut sub).await;
    assert!(
        events.is_empty(),
        "deleted parameterized slot must not appear in replay; got: {events:?}"
    );

    drop(guard);
}

/// A new parameterized event published at the *same* address but with
/// `created_at` newer than the deletion is allowed. This is the spec
/// behavior — deletion clears state, doesn't permanently ban the address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_allows_newer_at_same_address() {
    let (port, guard) = spawn_relay();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let evt = signed_param_event(&kp, 30000, "draft-1", "v1", 1_000_000);
    let mut publisher = connect(port).await;
    assert!(publish_ack(&mut publisher, &evt).await);

    let address = format!("30000:{pk}:draft-1");
    let deletion = signed_addr_deletion(&kp, &address, 1_000_005);
    assert!(publish_ack(&mut publisher, &deletion).await);

    // Newer event at same address — must be accepted.
    let newer = signed_param_event(&kp, 30000, "draft-1", "v2", 1_000_010);
    assert!(publish_ack(&mut publisher, &newer).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    send(
        &mut sub,
        &format!(r#"["REQ","s1",{{"authors":["{pk}"],"kinds":[30000]}}]"#),
    )
    .await;
    let events = drain_until_eose(&mut sub).await;
    assert_eq!(events.len(), 1, "expected newer slot only; got: {events:?}");
    assert_eq!(events[0][2]["content"], "v2");

    drop(guard);
}

/// A re-publish of an event with `created_at` older than the deletion
/// must be silently dropped. This is the gating behavior `deleted_addresses`
/// is supposed to provide.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_rejects_older_at_same_address() {
    let (port, guard) = spawn_relay();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let evt = signed_param_event(&kp, 30000, "draft-1", "v1", 1_000_000);
    let mut publisher = connect(port).await;
    assert!(publish_ack(&mut publisher, &evt).await);

    let address = format!("30000:{pk}:draft-1");
    let deletion = signed_addr_deletion(&kp, &address, 1_000_010);
    assert!(publish_ack(&mut publisher, &deletion).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Republish the original (older than the deletion). Storage drops it.
    let _ = publish_ack(&mut publisher, &evt).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    send(
        &mut sub,
        &format!(r#"["REQ","s1",{{"authors":["{pk}"],"kinds":[30000]}}]"#),
    )
    .await;
    let events = drain_until_eose(&mut sub).await;
    assert!(
        events.is_empty(),
        "republish of older event at deleted address must not reappear; got: {events:?}"
    );

    drop(guard);
}

/// Cross-pubkey `a` deletion is ignored: Bob signs a kind-5 with an
/// `a` tag whose pubkey field is Alice's. Alice's event must survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_cross_pubkey_is_ignored() {
    let (port, guard) = spawn_relay();
    let alice = K256Keypair::generate();
    let bob = K256Keypair::generate();
    let alice_pk = alice.public_key();

    let alice_evt = signed_param_event(&alice, 30000, "draft-1", "alice's v1", 1_000_000);
    let mut a_pub = connect(port).await;
    assert!(publish_ack(&mut a_pub, &alice_evt).await);

    // Bob targets Alice's address. Should be rejected by ownership check.
    let address = format!("30000:{alice_pk}:draft-1");
    let bob_deletion = signed_addr_deletion(&bob, &address, 1_000_005);
    let mut b_pub = connect(port).await;
    assert!(publish_ack(&mut b_pub, &bob_deletion).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    send(
        &mut sub,
        &format!(
            r#"["REQ","s1",{{"authors":["{alice_pk}"],"kinds":[30000]}}]"#
        ),
    )
    .await;
    let events = drain_until_eose(&mut sub).await;
    assert_eq!(
        events.len(),
        1,
        "alice's slot must survive bob's cross-pubkey deletion; got: {events:?}"
    );
    assert_eq!(events[0][2]["content"], "alice's v1");

    drop(guard);
}

/// Replaceable (NIP-16): kind-10000-range with empty d_tag. The
/// `a`-tag for replaceable is `"<kind>:<pubkey>:"` (trailing empty
/// segment).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_removes_existing_replaceable_slot() {
    let (port, guard) = spawn_relay();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let evt = signed_replaceable(&kp, 10002, "relay list v1", 1_000_000);
    let mut publisher = connect(port).await;
    assert!(publish_ack(&mut publisher, &evt).await);

    let address = format!("10002:{pk}:");
    let deletion = signed_addr_deletion(&kp, &address, 1_000_005);
    assert!(publish_ack(&mut publisher, &deletion).await);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut sub = connect(port).await;
    send(
        &mut sub,
        &format!(r#"["REQ","s1",{{"authors":["{pk}"],"kinds":[10002]}}]"#),
    )
    .await;
    let events = drain_until_eose(&mut sub).await;
    assert!(
        events.is_empty(),
        "deleted replaceable slot must not appear; got: {events:?}"
    );

    drop(guard);
}
