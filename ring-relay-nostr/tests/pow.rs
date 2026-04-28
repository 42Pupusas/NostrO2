//! NIP-13 proof-of-work end-to-end.
//!
//! When `RelayConfig::min_pow_difficulty > 0`, the relay rejects EVENTs
//! whose id has fewer leading zero bits than the configured minimum.
//! `min_pow_difficulty == 0` (the default) accepts every well-formed
//! event regardless of id.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig, leading_zero_bits};
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

fn spawn_relay(min_pow: u32) -> (u16, RelayGuard) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut config = RelayConfig::default();
        config.min_pow_difficulty = min_pow;
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

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = u8::from_str_radix(&s[i * 2..i * 2 + 1], 16).ok()?;
        let lo = u8::from_str_radix(&s[i * 2 + 1..i * 2 + 2], 16).ok()?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

/// Mine an EVENT whose id satisfies `target_bits` leading zero bits.
/// Walks a `nonce` tag value upward and re-signs each iteration. The
/// caller controls difficulty; 8 bits is fast (avg 256 attempts).
fn mine(kp: &K256Keypair, content: &str, target_bits: u32) -> NostrNote {
    let mut nonce: u64 = 0;
    loop {
        let mut n = NostrNote {
            pubkey: kp.public_key(),
            content: content.to_string(),
            kind: 1,
            created_at: 1_700_000_000,
            tags: vec![vec![
                "nonce".to_string(),
                nonce.to_string(),
                target_bits.to_string(),
            ]]
            .into(),
            ..NostrNote::default()
        };
        kp.sign_nostr_note(&mut n).expect("sign");
        let id_str = n.id.as_deref().unwrap_or("");
        if let Some(bytes) = decode_hex32(id_str)
            && leading_zero_bits(&bytes) >= target_bits
        {
            return n;
        }
        nonce += 1;
        if nonce > 1_000_000 {
            panic!("mine exhausted; lower target_bits");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pow_zero_accepts_every_event() {
    let (port, guard) = spawn_relay(0);
    let kp = K256Keypair::generate();
    let mut n = NostrNote::text_note("no pow required");
    n.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut n).expect("sign");

    let mut ws = connect(port).await;
    send(&mut ws, &serde_json::to_string(&("EVENT", &n)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true, "min_pow=0 must accept; got: {resp:?}");

    drop(guard);
}

/// At difficulty 8, a random-id event is rejected with high probability
/// (1 - 1/256 = ~99.6%). Retry a few times to tolerate the unlucky
/// case where an unmined id happens to start with 0x00.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pow_rejects_insufficient_difficulty() {
    let (port, guard) = spawn_relay(8);

    let mut saw_reject = false;
    for attempt in 0..16 {
        let kp = K256Keypair::generate();
        let mut n = NostrNote::text_note(&format!("attempt {attempt}"));
        n.pubkey = kp.public_key();
        kp.sign_nostr_note(&mut n).expect("sign");
        let id_bytes = decode_hex32(n.id.as_deref().unwrap_or("")).unwrap();
        if leading_zero_bits(&id_bytes) >= 8 {
            // Unlucky — this random id satisfies the bar. Try again.
            continue;
        }

        let mut ws = connect(port).await;
        send(&mut ws, &serde_json::to_string(&("EVENT", &n)).unwrap()).await;
        let resp = recv_text(&mut ws).await;
        assert_eq!(resp[0], "OK");
        assert_eq!(
            resp[2], false,
            "below-difficulty event must be rejected; got: {resp:?}"
        );
        let reason = resp[3].as_str().unwrap_or("");
        assert!(reason.contains("pow"), "expected 'pow' marker, got: {reason}");
        saw_reject = true;
        break;
    }
    assert!(
        saw_reject,
        "16 random ids all happened to satisfy difficulty 8 — astronomically unlikely; bug suspected"
    );

    drop(guard);
}

/// A mined id meeting difficulty 8 is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pow_accepts_mined_event() {
    let (port, guard) = spawn_relay(8);
    let kp = K256Keypair::generate();
    let mined = mine(&kp, "mined for difficulty 8", 8);
    // Sanity: confirm we actually mined something that meets the bar.
    let id_bytes = decode_hex32(mined.id.as_deref().unwrap()).unwrap();
    assert!(leading_zero_bits(&id_bytes) >= 8);

    let mut ws = connect(port).await;
    send(&mut ws, &serde_json::to_string(&("EVENT", &mined)).unwrap()).await;
    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], true, "mined event must be accepted; got: {resp:?}");

    drop(guard);
}
