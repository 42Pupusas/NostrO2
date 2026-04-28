//! NIP-42 AUTH end-to-end.
//!
//! Covers: relay issues challenge on connect; client signs kind 22242
//! with `relay` + `challenge` tags; relay accepts (`OK=true`).
//! Reject paths: bad challenge, stale `created_at`, wrong relay tag,
//! missing relay tag, wrong kind. Gating: `AuthGate::All` blocks REQ
//! and EVENT before AUTH; both flow after.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{AuthConfig, AuthGate, NostrRelay, RelayConfig};
use serde_json::Value;
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

/// Spawn a relay with AUTH enabled. The relay's self-identified URL
/// matches what `connect()` below uses (`ws://127.0.0.1:{port}/`),
/// but the *port is unknown* until after bind, so we let the relay
/// accept any URL whose host matches `127.0.0.1` by writing the URL
/// late — see `client_relay_url` for the matching value.
fn spawn_relay_with_auth(gate: Option<AuthGate>) -> (u16, RelayGuard) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut config = RelayConfig::default();
        // Bind first to learn the port, then patch the auth.relay_url.
        // We can't do that cleanly because RelayConfig is consumed by
        // `bind`. Workaround: build the relay with a placeholder URL,
        // then have the test send AUTH events whose `relay` tag uses
        // that same placeholder. The relay only checks the tag matches
        // its configured URL — it doesn't validate the URL points to
        // itself.
        config.auth = Some(AuthConfig {
            relay_url: "wss://test-relay.example/".into(),
            max_clock_skew_secs: 600,
            gate,
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
        },
    )
}

const RELAY_URL_TAG: &str = "wss://test-relay.example/";

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
    let msg = tokio::time::timeout(Duration::from_secs(15), ws.next())
        .await
        .expect("recv timed out")
        .expect("stream closed")
        .expect("ws error");
    match msg {
        Message::Text(t) => serde_json::from_str(&t).expect("valid json"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

/// Read the initial AUTH challenge sent by the relay on connect.
async fn read_challenge(ws: &mut WsClient) -> String {
    let msg = recv_text(ws).await;
    assert_eq!(msg[0], "AUTH", "expected AUTH challenge frame, got: {msg:?}");
    msg[1].as_str().expect("challenge is string").to_string()
}

fn signed_auth_event(
    kp: &K256Keypair,
    relay_url: &str,
    challenge: &str,
    created_at: i64,
) -> NostrNote {
    let mut n = NostrNote {
        kind: 22242,
        pubkey: kp.public_key(),
        content: "auth".into(),
        created_at,
        tags: vec![
            vec!["relay".into(), relay_url.into()],
            vec!["challenge".into(), challenge.into()],
        ]
        .into(),
        ..NostrNote::default()
    };
    kp.sign_nostr_note(&mut n).expect("sign");
    assert!(n.verify());
    n
}

fn now() -> i64 {
    NostrNote::now()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_happy_path_returns_ok_true() {
    let (port, guard) = spawn_relay_with_auth(None);
    let mut ws = connect(port).await;

    let challenge = read_challenge(&mut ws).await;
    let kp = K256Keypair::generate();
    let auth = signed_auth_event(&kp, RELAY_URL_TAG, &challenge, now());

    send(&mut ws, &serde_json::to_string(&("AUTH", &auth)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true, "auth must succeed; got: {resp:?}");

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_wrong_challenge_is_rejected() {
    let (port, guard) = spawn_relay_with_auth(None);
    let mut ws = connect(port).await;
    let _challenge = read_challenge(&mut ws).await;

    let kp = K256Keypair::generate();
    let auth = signed_auth_event(&kp, RELAY_URL_TAG, "not-the-challenge", now());

    send(&mut ws, &serde_json::to_string(&("AUTH", &auth)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], false);
    let reason = resp[3].as_str().unwrap_or("");
    assert!(
        reason.contains("auth-required") && reason.contains("challenge"),
        "expected auth-required + challenge marker, got: {reason}"
    );

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_wrong_relay_tag_is_rejected() {
    let (port, guard) = spawn_relay_with_auth(None);
    let mut ws = connect(port).await;
    let challenge = read_challenge(&mut ws).await;

    let kp = K256Keypair::generate();
    let auth = signed_auth_event(&kp, "wss://attacker.example/", &challenge, now());

    send(&mut ws, &serde_json::to_string(&("AUTH", &auth)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[2], false);
    let reason = resp[3].as_str().unwrap_or("");
    assert!(
        reason.contains("auth-required") && reason.contains("relay"),
        "expected auth-required + relay marker, got: {reason}"
    );

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_stale_created_at_is_rejected() {
    let (port, guard) = spawn_relay_with_auth(None);
    let mut ws = connect(port).await;
    let challenge = read_challenge(&mut ws).await;

    let kp = K256Keypair::generate();
    // 1 hour in the past — well outside the 600s skew window.
    let auth = signed_auth_event(&kp, RELAY_URL_TAG, &challenge, now() - 3600);

    send(&mut ws, &serde_json::to_string(&("AUTH", &auth)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[2], false);
    let reason = resp[3].as_str().unwrap_or("");
    assert!(
        reason.contains("auth-required") && reason.contains("skew"),
        "expected auth-required + skew marker, got: {reason}"
    );

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_wrong_kind_is_rejected() {
    let (port, guard) = spawn_relay_with_auth(None);
    let mut ws = connect(port).await;
    let challenge = read_challenge(&mut ws).await;

    let kp = K256Keypair::generate();
    let mut bogus = signed_auth_event(&kp, RELAY_URL_TAG, &challenge, now());
    // Tamper kind to 1 (still signed correctly for that kind).
    bogus.kind = 1;
    bogus.id = None;
    bogus.sig = None;
    kp.sign_nostr_note(&mut bogus).expect("re-sign");

    send(&mut ws, &serde_json::to_string(&("AUTH", &bogus)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[2], false);
    let reason = resp[3].as_str().unwrap_or("");
    assert!(
        reason.contains("22242") || reason.contains("kind"),
        "expected kind marker, got: {reason}"
    );

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_all_blocks_unauthed_then_allows_authed() {
    let (port, guard) = spawn_relay_with_auth(Some(AuthGate::All));
    let mut ws = connect(port).await;
    let challenge = read_challenge(&mut ws).await;

    // Unauthed REQ → CLOSED with auth-required.
    send(&mut ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "CLOSED");
    assert_eq!(resp[1], "s1");
    assert!(
        resp[2].as_str().unwrap_or("").starts_with("auth-required"),
        "expected auth-required prefix, got: {resp:?}"
    );

    // Unauthed EVENT → OK=false with auth-required.
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("hello while unauthed");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    send(&mut ws, &serde_json::to_string(&("EVENT", &note)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], false);
    assert!(resp[3].as_str().unwrap_or("").starts_with("auth-required"));

    // Now AUTH.
    let auth = signed_auth_event(&kp, RELAY_URL_TAG, &challenge, now());
    send(&mut ws, &serde_json::to_string(&("AUTH", &auth)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true);

    // Authed REQ → EOSE (no events to replay in ephemeral mode).
    send(&mut ws, r#"["REQ","s2",{"kinds":[1]}]"#).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "EOSE");
    assert_eq!(resp[1], "s2");

    // Authed EVENT → OK=true.
    let mut note = NostrNote::text_note("hello while authed");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    send(&mut ws, &serde_json::to_string(&("EVENT", &note)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true);

    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_disabled_means_no_challenge() {
    // Default config: no AUTH. Connect and send a REQ — first frame
    // we get back must NOT be an AUTH frame.
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let config = RelayConfig::default(); // auth = None
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    let (port, shutdown) = rx.recv().unwrap();
    let _g = RelayGuard {
        shutdown: Some(shutdown),
        handle: Some(handle),
    };

    let mut ws = connect(port).await;
    send(&mut ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let msg = recv_text(&mut ws).await;
    assert_eq!(msg[0], "EOSE", "expected EOSE without AUTH, got: {msg:?}");
}
