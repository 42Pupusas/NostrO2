//! End-to-end persistence tests.
//!
//! Drives a real `NostrRelay` with a `StorageConfig` pointing at a tempdir,
//! publishes N events, tears the relay down, re-opens it pointing at the
//! same dir, issues a REQ, and verifies the events come back.

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig, StorageConfig};
use serde_json::Value;
use std::time::Duration;
use tempfile::tempdir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsClient = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn spawn_relay(
    config: RelayConfig,
) -> (
    u16,
    ring_relay_nostr::ShutdownHandle,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let jh = std::thread::spawn(move || {
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    let (port, shutdown) = rx.recv().unwrap();
    (port, shutdown, jh)
}

async fn connect(port: u16) -> WsClient {
    for attempt in 0..30 {
        let url = format!("ws://127.0.0.1:{port}/");
        match connect_async(&url).await {
            Ok((ws, _)) => return ws,
            Err(_) if attempt < 29 => tokio::time::sleep(Duration::from_millis(20)).await,
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
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("ws error");
    match msg {
        Message::Text(t) => serde_json::from_str(&t).expect("not json"),
        other => panic!("expected text, got {other:?}"),
    }
}

fn signed_note(content: &str, kp: &K256Keypair, kind: u32) -> NostrNote {
    let mut note = NostrNote {
        pubkey: kp.public_key(),
        content: content.to_string(),
        kind,
        ..Default::default()
    };
    kp.sign_nostr_note(&mut note).expect("sign");
    assert!(note.verify());
    note
}

fn storage_cfg(dir: &std::path::Path) -> StorageConfig {
    StorageConfig {
        data_dir: dir.to_path_buf(),
        ephemeral_slots: 256,
        replaceable_slots: 32,
        parameterized_slots: 32,
        max_payload: 16 * 1024,
        reader_threads: 1,
        write_ring_capacity: 256,
        req_ring_capacity: 64,
        fsync_interval_ms: Some(10),
        ..StorageConfig::default()
    }
}

async fn drain_until_eose(ws: &mut WsClient) -> Vec<Value> {
    let mut events = Vec::new();
    loop {
        let msg = recv_text(ws).await;
        match msg[0].as_str() {
            Some("EVENT") => events.push(msg),
            Some("EOSE") => return events,
            other => panic!("unexpected frame waiting for EOSE: {other:?}: {msg}"),
        }
    }
}

async fn publish(ws: &mut WsClient, note: &NostrNote) {
    let frame = format!("[\"EVENT\",{}]", serde_json::to_string(note).unwrap());
    send(ws, &frame).await;
    let ok = recv_text(ws).await;
    assert_eq!(ok[0], "OK", "unexpected OK frame: {ok}");
    assert_eq!(ok[2], true, "OK rejected: {ok}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_replay_after_reopen() {
    let dir = tempdir().unwrap();

    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    // Phase 1: publish 3 kind-1 events, then shut down.
    {
        let mut cfg = RelayConfig::default();
        cfg.storage = Some(storage_cfg(dir.path()));
        let (port, shutdown, jh) = spawn_relay(cfg);
        let mut ws = connect(port).await;
        for i in 0..3 {
            let note = signed_note(&format!("event-{i}"), &kp, 1);
            let frame = format!("[\"EVENT\",{}]", serde_json::to_string(&note).unwrap());
            send(&mut ws, &frame).await;
            // expect an OK
            let ok = recv_text(&mut ws).await;
            assert_eq!(ok[0], "OK", "unexpected: {ok}");
            assert_eq!(ok[2], true, "OK rejected: {ok}");
        }
        // Give the storage thread a moment to persist + fsync.
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(ws);
        shutdown.shutdown();
        jh.join().unwrap();
    }

    // Phase 2: reopen, subscribe, expect the 3 events back plus EOSE.
    {
        let mut cfg = RelayConfig::default();
        cfg.storage = Some(storage_cfg(dir.path()));
        let (port, shutdown, jh) = spawn_relay(cfg);
        let mut ws = connect(port).await;

        let req = format!(r#"["REQ","sub1",{{"authors":["{}"],"kinds":[1]}}]"#, pk);
        send(&mut ws, &req).await;

        let mut got_events: Vec<String> = Vec::new();
        let mut saw_eose = false;
        for _ in 0..10 {
            let msg = recv_text(&mut ws).await;
            match msg[0].as_str() {
                Some("EVENT") => {
                    let content = msg[2]["content"].as_str().unwrap_or("").to_string();
                    got_events.push(content);
                }
                Some("EOSE") => {
                    saw_eose = true;
                    break;
                }
                other => panic!("unexpected frame: {other:?}: {msg}"),
            }
        }
        assert!(saw_eose, "no EOSE");
        got_events.sort();
        assert_eq!(got_events, vec!["event-0", "event-1", "event-2"]);

        drop(ws);
        shutdown.shutdown();
        jh.join().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn req_with_limit_returns_newest() {
    let dir = tempdir().unwrap();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let mut cfg = RelayConfig::default();
    cfg.storage = Some(storage_cfg(dir.path()));
    let (port, shutdown, jh) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    // Publish 5 events with monotonically increasing created_at.
    let mut ids: Vec<String> = Vec::new();
    for i in 0..5u32 {
        let mut note = NostrNote {
            pubkey: pk.clone(),
            content: format!("e{i}"),
            kind: 1,
            created_at: 1_000_000 + i as i64,
            ..Default::default()
        };
        kp.sign_nostr_note(&mut note).unwrap();
        ids.push(note.id.clone().unwrap());
        publish(&mut ws, &note).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    // REQ with limit=2 should return the two newest.
    let req = format!(
        r#"["REQ","sub1",{{"authors":["{}"],"kinds":[1],"limit":2}}]"#,
        pk
    );
    send(&mut ws, &req).await;
    let events = drain_until_eose(&mut ws).await;
    assert_eq!(events.len(), 2, "expected 2 events, got {}", events.len());

    let mut got_contents: Vec<String> = events
        .iter()
        .map(|e| e[2]["content"].as_str().unwrap().to_string())
        .collect();
    got_contents.sort();
    assert_eq!(got_contents, vec!["e3", "e4"]);

    drop(ws);
    shutdown.shutdown();
    jh.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaceable_kind_overwrites_in_place() {
    let dir = tempdir().unwrap();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let mut cfg = RelayConfig::default();
    cfg.storage = Some(storage_cfg(dir.path()));
    let (port, shutdown, jh) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    // kind 10002 (NIP-65 relay list) — replaceable by pubkey.
    for (i, content) in ["v1", "v2", "v3"].iter().enumerate() {
        let mut note = NostrNote {
            pubkey: pk.clone(),
            content: (*content).to_string(),
            kind: 10002,
            created_at: 1_000_000 + i as i64,
            ..Default::default()
        };
        kp.sign_nostr_note(&mut note).unwrap();
        publish(&mut ws, &note).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let req = format!(r#"["REQ","s",{{"authors":["{}"],"kinds":[10002]}}]"#, pk);
    send(&mut ws, &req).await;
    let events = drain_until_eose(&mut ws).await;
    assert_eq!(
        events.len(),
        1,
        "expected 1 replaceable event, got {}",
        events.len()
    );
    assert_eq!(events[0][2]["content"], "v3");

    drop(ws);
    shutdown.shutdown();
    jh.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parameterized_kind_overwrites_per_d_tag() {
    let dir = tempdir().unwrap();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let mut cfg = RelayConfig::default();
    cfg.storage = Some(storage_cfg(dir.path()));
    let (port, shutdown, jh) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    // kind 30000, two distinct d-tags. Each (pubkey, kind, d) keeps only newest.
    for (i, (d, content)) in [("a", "a1"), ("b", "b1"), ("a", "a2"), ("b", "b2")]
        .iter()
        .enumerate()
    {
        let mut note = NostrNote {
            pubkey: pk.clone(),
            content: (*content).to_string(),
            kind: 30000,
            created_at: 1_000_000 + i as i64,
            tags: vec![vec!["d".to_string(), (*d).to_string()]].into(),
            ..Default::default()
        };
        kp.sign_nostr_note(&mut note).unwrap();
        publish(&mut ws, &note).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let req = format!(r#"["REQ","s",{{"authors":["{}"],"kinds":[30000]}}]"#, pk);
    send(&mut ws, &req).await;
    let events = drain_until_eose(&mut ws).await;
    assert_eq!(
        events.len(),
        2,
        "expected 2 (one per d-tag), got {}",
        events.len()
    );

    let mut contents: Vec<String> = events
        .iter()
        .map(|e| e[2]["content"].as_str().unwrap().to_string())
        .collect();
    contents.sort();
    assert_eq!(contents, vec!["a2", "b2"]);

    drop(ws);
    shutdown.shutdown();
    jh.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ephemeral_bucket_evicts_oldest_on_wrap() {
    let dir = tempdir().unwrap();
    let kp = K256Keypair::generate();
    let pk = kp.public_key();

    let mut storage = storage_cfg(dir.path());
    storage.ephemeral_slots = 4; // tiny ring
    let mut cfg = RelayConfig::default();
    cfg.storage = Some(storage);
    let (port, shutdown, jh) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    // Publish 7 kind-1 events into a 4-slot ring.
    for i in 0..7u32 {
        let mut note = NostrNote {
            pubkey: pk.clone(),
            content: format!("e{i}"),
            kind: 1,
            created_at: 1_000_000 + i as i64,
            ..Default::default()
        };
        kp.sign_nostr_note(&mut note).unwrap();
        publish(&mut ws, &note).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let req = format!(r#"["REQ","s",{{"authors":["{}"],"kinds":[1]}}]"#, pk);
    send(&mut ws, &req).await;
    let events = drain_until_eose(&mut ws).await;
    assert_eq!(events.len(), 4, "ring keeps last 4, got {}", events.len());

    let mut contents: Vec<String> = events
        .iter()
        .map(|e| e[2]["content"].as_str().unwrap().to_string())
        .collect();
    contents.sort();
    // Oldest 3 (e0, e1, e2) evicted; latest 4 (e3..e6) remain.
    assert_eq!(contents, vec!["e3", "e4", "e5", "e6"]);

    drop(ws);
    shutdown.shutdown();
    jh.join().unwrap();
}
