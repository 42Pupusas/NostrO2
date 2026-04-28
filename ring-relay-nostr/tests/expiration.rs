//! NIP-40 expiration end-to-end.
//!
//! - EVENT with expiration in the past → OK=false `invalid: event expired`.
//! - EVENT with expiration in the future is OK'd, persisted, and replayed
//!   to a later REQ.
//! - Once the expiration passes, a fresh REQ does NOT replay the event.
//!
//! All three cases run in storage mode so the read-side filter
//! (`scan_bucket` in storage/reader.rs) is exercised.

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
    _data_dir: Option<TempDir>,
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

fn spawn_relay(config: RelayConfig, data_dir: Option<TempDir>) -> (u16, RelayGuard) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
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
            _data_dir: data_dir,
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
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
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

fn make_storage_config() -> (RelayConfig, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = RelayConfig::default();
    config.storage = Some(StorageConfig {
        data_dir: dir.path().to_path_buf(),
        ..StorageConfig::default()
    });
    (config, dir)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn signed_event_with_expiration(kp: &K256Keypair, expiration: i64) -> NostrNote {
    let mut evt = NostrNote::text_note("expiring");
    evt.pubkey = kp.public_key();
    evt.tags.add_custom_tag("expiration", &expiration.to_string());
    kp.sign_nostr_note(&mut evt).expect("sign");
    assert!(evt.verify());
    evt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_event_is_rejected_on_ingest() {
    let (config, dir) = make_storage_config();
    let (port, guard) = spawn_relay(config, Some(dir));

    let kp = K256Keypair::generate();
    let evt = signed_event_with_expiration(&kp, now_secs() - 1).await;

    let mut ws = connect(port).await;
    send(&mut ws, &serde_json::to_string(&("EVENT", &evt)).unwrap()).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], false);
    let reason = resp[3].as_str().unwrap();
    assert!(
        reason.contains("expired"),
        "expected 'expired' reason, got: {reason}"
    );

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_expiration_is_accepted_and_replayed() {
    let (config, dir) = make_storage_config();
    let (port, guard) = spawn_relay(config, Some(dir));

    let kp = K256Keypair::generate();
    // Far enough in the future that the test never sees it expire.
    let evt = signed_event_with_expiration(&kp, now_secs() + 3600).await;

    let mut pubws = connect(port).await;
    send(&mut pubws, &serde_json::to_string(&("EVENT", &evt)).unwrap()).await;
    let resp = recv_text(&mut pubws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true);

    // New connection asks for it. Storage replay must include it.
    let mut sub = connect(port).await;
    let req = format!(r#"["REQ","s1",{{"authors":["{}"]}}]"#, kp.public_key());
    send(&mut sub, &req).await;

    let first = recv_text(&mut sub).await;
    assert_eq!(first[0], "EVENT");
    assert_eq!(first[1], "s1");
    assert_eq!(first[2]["id"], evt.id.as_deref().unwrap_or(""));

    let eose = recv_text(&mut sub).await;
    assert_eq!(eose[0], "EOSE");

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_event_is_skipped_on_replay() {
    let (config, dir) = make_storage_config();
    let (port, guard) = spawn_relay(config, Some(dir));

    let kp = K256Keypair::generate();
    // Expires 2 seconds out — accepted on ingest, then a delayed REQ
    // sees it past expiration and the reader skips it.
    let exp_ts = now_secs() + 2;
    let evt = signed_event_with_expiration(&kp, exp_ts).await;

    let mut pubws = connect(port).await;
    send(&mut pubws, &serde_json::to_string(&("EVENT", &evt)).unwrap()).await;
    let resp = recv_text(&mut pubws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true);

    // Wait until the event is past its expiration. 3s gives 1s slack.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let mut sub = connect(port).await;
    let req = format!(r#"["REQ","s1",{{"authors":["{}"]}}]"#, kp.public_key());
    send(&mut sub, &req).await;

    // Should immediately receive EOSE with no EVENT in between.
    let msg = recv_text(&mut sub).await;
    assert_eq!(
        msg[0], "EOSE",
        "expected EOSE without prior EVENT, got: {msg:?}"
    );

    drop(guard);
}
