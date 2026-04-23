//! End-to-end integration tests for the ephemeral Nostr relay.
//!
//! Drives the real `NostrRelay` over a real WebSocket and verifies NIP-01 flow:
//! EVENT is OK'd, REQ yields EOSE, subsequent matching EVENTs fan out to
//! subscribers, CLOSE stops delivery, FIFO eviction kicks in at the sub cap.

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::NostrKeypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_tungstenite::tungstenite::Message;

type WsClient = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Spawn a relay on an ephemeral port. Returns the port and a shutdown handle
/// that ends the run loop.
fn spawn_relay(config: RelayConfig) -> (u16, ring_relay_nostr::ShutdownHandle) {
    // `NostrRelay::bind` needs to be called on the thread that will run the
    // dispatch loop, so create in the worker.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut relay =
            NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    rx.recv().unwrap()
}

async fn connect(port: u16) -> WsClient {
    // Tiny retry: the relay thread needs a moment to start accepting.
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
    ws.send(Message::Text(json.to_string().into())).await.unwrap();
}

/// Read the next text frame with a timeout so tests fail fast on regressions.
async fn recv_text(ws: &mut WsClient) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timeout waiting for frame")
        .expect("stream ended")
        .expect("ws error");
    match msg {
        Message::Text(t) => serde_json::from_str(&t).expect("relay response not JSON"),
        other => panic!("expected text, got {other:?}"),
    }
}

fn signed_note(content: &str) -> NostrNote {
    let kp = NostrKeypair::new_extractable();
    let mut note = NostrNote::text_note(content);
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    assert!(note.verify());
    note
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_is_ok_acked() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    let note = signed_note("hello");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], true);

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn req_yields_immediate_eose() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    send(&mut ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "EOSE");
    assert_eq!(resp[1], "s1");

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_fanout_to_matching_subscriber() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let mut sub_ws = connect(port).await;
    let mut pub_ws = connect(port).await;

    // Subscribe to kind 1.
    send(&mut sub_ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let eose = recv_text(&mut sub_ws).await;
    assert_eq!(eose[0], "EOSE");

    // Publish a matching event from the other connection.
    let note = signed_note("hi");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;

    // Publisher sees OK.
    let ok = recv_text(&mut pub_ws).await;
    assert_eq!(ok[0], "OK");
    assert_eq!(ok[2], true);

    // Subscriber sees the event.
    let evt = recv_text(&mut sub_ws).await;
    assert_eq!(evt[0], "EVENT");
    assert_eq!(evt[1], "s1");
    assert_eq!(evt[2]["id"].as_str().unwrap(), id);

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_matching_filter_does_not_receive() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let mut sub_ws = connect(port).await;
    let mut pub_ws = connect(port).await;

    // Subscribe to kind 7 only.
    send(&mut sub_ws, r#"["REQ","s1",{"kinds":[7]}]"#).await;
    let _ = recv_text(&mut sub_ws).await;

    // Publish kind 1.
    let note = signed_note("nope");
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;
    let _ok = recv_text(&mut pub_ws).await;

    // Subscriber should not receive; use a short timeout.
    let result = tokio::time::timeout(Duration::from_millis(200), sub_ws.next()).await;
    assert!(result.is_err(), "subscriber unexpectedly received: {result:?}");

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_stops_fanout() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let mut sub_ws = connect(port).await;
    let mut pub_ws = connect(port).await;

    send(&mut sub_ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let _ = recv_text(&mut sub_ws).await;

    send(&mut sub_ws, r#"["CLOSE","s1"]"#).await;
    // Give dispatcher a moment to process the CLOSE before publishing.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let note = signed_note("after close");
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;
    let _ok = recv_text(&mut pub_ws).await;

    let result = tokio::time::timeout(Duration::from_millis(200), sub_ws.next()).await;
    assert!(result.is_err(), "closed sub received: {result:?}");

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_fifo_eviction() {
    let mut config = RelayConfig::default();
    config.max_subs_per_conn = 2;
    let (port, shutdown) = spawn_relay(config);
    let mut ws = connect(port).await;

    send(&mut ws, r#"["REQ","a",{"kinds":[1]}]"#).await;
    let _ = recv_text(&mut ws).await; // EOSE a
    send(&mut ws, r#"["REQ","b",{"kinds":[1]}]"#).await;
    let _ = recv_text(&mut ws).await; // EOSE b

    // Third sub evicts "a".
    send(&mut ws, r#"["REQ","c",{"kinds":[1]}]"#).await;

    // Expect a CLOSED for "a" followed by EOSE for "c". Order: we emit CLOSED
    // *before* EOSE in `on_req`, so read in that order.
    let closed = recv_text(&mut ws).await;
    assert_eq!(closed[0], "CLOSED");
    assert_eq!(closed[1], "a");

    let eose = recv_text(&mut ws).await;
    assert_eq!(eose[0], "EOSE");
    assert_eq!(eose[1], "c");

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_event_rejected() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    // Signed by one key, pubkey swapped to a different one → signature fails.
    let kp = NostrKeypair::new_extractable();
    let mut note = NostrNote::text_note("tampered");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).unwrap();
    note.pubkey = "0".repeat(64); // corrupt the pubkey after signing

    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], false);

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_verb_gets_notice() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    send(&mut ws, r#"["AUTH","challenge"]"#).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "NOTICE");

    shutdown.shutdown();
}
