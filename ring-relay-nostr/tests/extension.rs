//! Black-box integration test for the [`Extension`] seam.
//!
//! Boots a real relay with a single extension that blocks events from a
//! banned pubkey and asserts that:
//!  1. The publisher receives `OK=false` with the extension's denial frame.
//!  2. A separate REQ subscriber never receives the blocked event.
//!  3. A REQ from the same banned pubkey still works (only EVENT is gated).
//!
//! This proves the seam fires before the verify pool / fan-out / storage
//! path, that `Stop` short-circuits cleanly, and that the connection
//! survives a deny.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{
    Extension, ExtensionAction, MessageRef, NostrRelay, RelayConfig, Session,
};
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

fn spawn_relay(config: RelayConfig) -> (u16, RelayGuard) {
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

/// Bans EVENTs whose pubkey is on the configured deny list. Lets every
/// other verb through unchanged.
struct DenyByPubkey {
    banned_hex: String,
}

impl Extension for DenyByPubkey {
    fn name(&self) -> &'static str {
        "deny-by-pubkey"
    }

    fn on_message(&self, msg: &MessageRef<'_>, _session: &mut Session) -> ExtensionAction {
        if let MessageRef::Event(note) = msg
            && note.pubkey.as_ref() == self.banned_hex.as_str()
        {
            // OK=false with a clear reason. The id may be missing on
            // malformed events but here we're testing the well-formed
            // path so it'll be present.
            let id = note.id.as_deref().unwrap_or("");
            let frame = serde_json::to_string(&("OK", id, false, "blocked: banned pubkey"))
                .expect("ok frame");
            return ExtensionAction::Stop(Some(frame));
        }
        ExtensionAction::Continue
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_blocks_event_and_short_circuits_fanout() {
    // Banned publisher's keypair.
    let banned = K256Keypair::generate();
    let banned_pk = banned.public_key();

    // Allowed publisher.
    let allowed = K256Keypair::generate();

    let mut config = RelayConfig::default();
    config.extensions.push(Arc::new(DenyByPubkey {
        banned_hex: banned_pk.clone(),
    }));

    let (port, guard) = spawn_relay(config);

    // Subscriber: opens a REQ for kind 1, expects to see the allowed event
    // and never the banned one.
    let mut sub = connect(port).await;
    send(
        &mut sub,
        r#"["REQ","s1",{"kinds":[1]}]"#,
    )
    .await;
    // EOSE arrives first since the relay is ephemeral.
    let eose = recv_text(&mut sub).await;
    assert_eq!(eose[0], "EOSE");
    assert_eq!(eose[1], "s1");

    // Banned publisher: signs a kind-1 event and posts it.
    let mut pub_banned = connect(port).await;
    let mut banned_evt = NostrNote {
        kind: 1,
        content: "from banned".into(),
        pubkey: banned_pk.clone(),
        ..NostrNote::default()
    };
    banned.sign_nostr_note(&mut banned_evt).expect("sign banned");
    let banned_json = serde_json::to_string(&("EVENT", &banned_evt)).unwrap();
    send(&mut pub_banned, &banned_json).await;

    // Publisher receives OK=false with the extension's denial.
    let resp = recv_text(&mut pub_banned).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], banned_evt.id.as_deref().unwrap_or(""));
    assert_eq!(resp[2], false);
    assert!(resp[3].as_str().unwrap().contains("banned pubkey"));

    // Allowed publisher posts a kind-1; subscriber must see it (not the banned one).
    let mut pub_ok = connect(port).await;
    let mut ok_evt = NostrNote {
        kind: 1,
        content: "from allowed".into(),
        pubkey: allowed.public_key(),
        ..NostrNote::default()
    };
    allowed.sign_nostr_note(&mut ok_evt).expect("sign allowed");
    let ok_json = serde_json::to_string(&("EVENT", &ok_evt)).unwrap();
    send(&mut pub_ok, &ok_json).await;

    // Allowed publisher gets OK=true.
    let resp = recv_text(&mut pub_ok).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true);

    // Subscriber should see exactly the allowed event next — banned was
    // dropped before fan-out.
    let evt = recv_text(&mut sub).await;
    assert_eq!(evt[0], "EVENT");
    assert_eq!(evt[1], "s1");
    assert_eq!(evt[2]["content"], "from allowed");
    assert_eq!(evt[2]["pubkey"], allowed.public_key());

    drop(guard);
}

/// REQ from the banned pubkey's connection still works — only EVENT is
/// gated. This guards against a future maintainer accidentally widening
/// the deny check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_does_not_block_req() {
    let banned = K256Keypair::generate();
    let banned_pk = banned.public_key();

    let mut config = RelayConfig::default();
    config.extensions.push(Arc::new(DenyByPubkey {
        banned_hex: banned_pk,
    }));

    let (port, guard) = spawn_relay(config);

    let mut ws = connect(port).await;
    send(&mut ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let eose = recv_text(&mut ws).await;
    assert_eq!(eose[0], "EOSE");
    assert_eq!(eose[1], "s1");

    drop(guard);
}
