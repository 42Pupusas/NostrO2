//! Truthful `OK` semantics in storage mode.
//!
//! `OK=true` is now sent only after the storage thread reports
//! `Stored` or `Duplicate`. Drop paths (deleted id, address-deleted
//! republish, oversized payload, ring overflow) produce `OK=false`
//! with a specific reason, instead of the old "OK=true then silent
//! storage drop" lie.
//!
//! See `tests/deletion.rs::republish_of_deleted_id_returns_explicit_reject`
//! for the e-tag deletion case; this file covers the other paths.

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

fn spawn_relay_with_storage(
    storage_overrides: impl FnOnce(&mut StorageConfig) + Send + 'static,
) -> (u16, RelayGuard) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut sc = StorageConfig {
            data_dir: path,
            ..StorageConfig::default()
        };
        storage_overrides(&mut sc);
        let mut config = RelayConfig::default();
        config.storage = Some(sc);
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

fn signed_text(kp: &K256Keypair, content: &str) -> NostrNote {
    let mut n = NostrNote::text_note(content);
    n.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut n).expect("sign");
    assert!(n.verify());
    n
}

/// Happy-path commit returns `OK=true` with empty message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_event_returns_ok_true() {
    let (port, guard) = spawn_relay_with_storage(|_| {});
    let kp = K256Keypair::generate();
    let note = signed_text(&kp, "hello world");
    let id = note.id.clone().unwrap();

    let mut ws = connect(port).await;
    send(&mut ws, &serde_json::to_string(&("EVENT", &note)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], true, "happy-path commit must produce OK=true");
    assert_eq!(
        resp[3], "",
        "stored OK message must be empty in v1 (no extra context to add)"
    );

    drop(guard);
}

/// Posting the same id twice in a row: the second one returns
/// `OK=true` with `"duplicate: ..."` reason. Per NIP-01 a relay SHOULD
/// treat duplicates as success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_event_returns_ok_true_with_duplicate_reason() {
    let (port, guard) = spawn_relay_with_storage(|_| {});
    let kp = K256Keypair::generate();
    let note = signed_text(&kp, "duplicate me");
    let id = note.id.clone().unwrap();

    let mut ws = connect(port).await;
    send(&mut ws, &serde_json::to_string(&("EVENT", &note)).unwrap()).await;
    let first = recv_text(&mut ws).await;
    assert_eq!(first[2], true);

    send(&mut ws, &serde_json::to_string(&("EVENT", &note)).unwrap()).await;
    let second = recv_text(&mut ws).await;
    assert_eq!(second[0], "OK");
    assert_eq!(second[1], id);
    assert_eq!(
        second[2], true,
        "duplicate must still be OK=true (NIP-01 dedupe success)"
    );
    let msg = second[3].as_str().unwrap_or("");
    assert!(
        msg.contains("duplicate"),
        "expected 'duplicate' marker in OK message, got: {msg}"
    );

    drop(guard);
}

/// An EVENT whose JSON exceeds `max_payload` is rejected with
/// `OK=false`. Previously the shard returned `OK=true` and storage
/// silently dropped it; the truthful-OK ring exposes the real verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_payload_returns_ok_false() {
    // Tight max_payload so a normal-looking note still trips the cap.
    let (port, guard) = spawn_relay_with_storage(|sc| {
        sc.max_payload = 256;
    });
    let kp = K256Keypair::generate();
    // 4 KiB content blows past the 256-byte cap.
    let note = signed_text(&kp, &"X".repeat(4096));
    let id = note.id.clone().unwrap();

    let mut ws = connect(port).await;
    send(&mut ws, &serde_json::to_string(&("EVENT", &note)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], false, "oversized payload must produce OK=false");
    let reason = resp[3].as_str().unwrap_or("");
    assert!(
        reason.contains("payload") || reason.contains("max_payload"),
        "expected payload-size mention, got: {reason}"
    );

    drop(guard);
}

/// Republishing an older parameterized event after its address was
/// deleted returns `OK=false` with a "blocked: address" reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn address_deleted_republish_returns_ok_false() {
    let (port, guard) = spawn_relay_with_storage(|_| {});
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    // Publish a parameterized event.
    let mut original = NostrNote {
        pubkey: pk.clone(),
        content: "v1".into(),
        kind: 30000,
        created_at: 1_000_000,
        tags: vec![vec!["d".to_string(), "draft-1".to_string()]].into(),
        ..NostrNote::default()
    };
    kp.sign_nostr_note(&mut original).expect("sign");

    // Delete by `a` ref with a strictly newer created_at.
    let mut deletion = NostrNote {
        kind: 5,
        pubkey: pk.clone(),
        content: "delete-by-address".into(),
        created_at: 1_000_010,
        tags: vec![vec!["a".to_string(), format!("30000:{pk}:draft-1")]].into(),
        ..NostrNote::default()
    };
    kp.sign_nostr_note(&mut deletion).expect("sign");

    let mut ws = connect(port).await;
    send(
        &mut ws,
        &serde_json::to_string(&("EVENT", &original)).unwrap(),
    )
    .await;
    let r = recv_text(&mut ws).await;
    assert_eq!(r[2], true);
    send(
        &mut ws,
        &serde_json::to_string(&("EVENT", &deletion)).unwrap(),
    )
    .await;
    let r = recv_text(&mut ws).await;
    assert_eq!(r[2], true);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Republish original — its created_at is now older than the deletion.
    send(
        &mut ws,
        &serde_json::to_string(&("EVENT", &original)).unwrap(),
    )
    .await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], original.id.as_deref().unwrap_or(""));
    assert_eq!(
        resp[2], false,
        "republish at deleted address must produce OK=false"
    );
    let reason = resp[3].as_str().unwrap_or("");
    assert!(
        reason.contains("blocked") || reason.contains("deleted") || reason.contains("address"),
        "expected blocked/deleted/address marker, got: {reason}"
    );

    drop(guard);
}
