//! Smoke test: can we spawn nostr-relay programmatically via the bench
//! harness, send an EVENT, and get an OK back? Catches harness breakage
//! before running the full benches.

#[path = "../benches/common/mod.rs"]
mod common;

use common::Relay;
use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn signed_frame() -> (String, String) {
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("compare-smoke");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    (id, frame)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nostr_relay_harness_round_trips_event() {
    let relay = Relay::spawn_nostr_relay(1);
    // Tiny settle so the actix server is ready to accept.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let url = format!("ws://127.0.0.1:{}", relay.port);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
    let (mut write, mut read) = ws.split();

    let (id, frame) = signed_frame();
    write.send(Message::Text(frame.into())).await.expect("send");

    let msg = tokio::time::timeout(Duration::from_secs(5), read.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("ws error");
    let text = match msg {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(text.contains("\"OK\""), "unexpected reply: {text}");
    assert!(text.contains(&id), "reply missing event id: {text}");

    drop(relay);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ring_relay_harness_round_trips_event() {
    let relay = Relay::spawn_ring(1, 64);

    let url = format!("ws://127.0.0.1:{}", relay.port);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
    let (mut write, mut read) = ws.split();

    let (id, frame) = signed_frame();
    write.send(Message::Text(frame.into())).await.expect("send");

    let msg = tokio::time::timeout(Duration::from_secs(2), read.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("ws error");
    let text = match msg {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(text.contains("\"OK\""));
    assert!(text.contains(&id));

    drop(relay);
}

/// Tiny fan-out sanity check: 16 subs, 2 pubs, 5 events each, against
/// nostr-relay at workers=1. If this hangs, the full compare_fanout bench
/// has no chance. If it succeeds, the problem is workload scale, not the
/// harness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nostr_relay_tiny_fanout() {
    use futures_util::{SinkExt, StreamExt};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SUBS: usize = 16;
    const PUBS: usize = 2;
    const EVENTS: usize = 5;

    let relay = Relay::spawn_nostr_relay(1);
    tokio::time::sleep(Duration::from_millis(250)).await;

    let url = format!("ws://127.0.0.1:{}", relay.port);

    // Distinct keypair per pub so ids differ and nostr-relay doesn't dedupe.
    let frame_sets: Vec<Arc<Vec<String>>> = (0..PUBS)
        .map(|pi| {
            let kp = nostro2_signer::K256Keypair::generate();
            Arc::new(
                (0..EVENTS)
                    .map(|i| {
                        let mut note = NostrNote::text_note(&format!("tiny p{pi} {i}"));
                        note.pubkey = kp.public_key();
                        kp.sign_nostr_note(&mut note).expect("sign");
                        format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    let delivered = Arc::new(AtomicUsize::new(0));
    let total_events = PUBS * EVENTS;
    let target = SUBS * total_events;

    let mut sub_tasks = Vec::with_capacity(SUBS);
    for i in 0..SUBS {
        let url = url.clone();
        let delivered = delivered.clone();
        sub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("sub connect");
            let (mut write, mut read) = ws.split();
            let req = format!(r#"["REQ","s{i}",{{"kinds":[1]}}]"#);
            write.send(Message::Text(req.into())).await.expect("REQ");
            let mut events = 0;
            while events < total_events {
                match read.next().await {
                    Some(Ok(Message::Text(t))) if t.starts_with("[\"EVENT\"") => {
                        events += 1;
                        delivered.fetch_add(1, Ordering::Relaxed);
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
        }));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut pub_tasks = Vec::with_capacity(PUBS);
    for frames in frame_sets.iter().cloned() {
        let url = url.clone();
        pub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("pub connect");
            let (mut write, mut read) = ws.split();
            let reader = tokio::spawn(async move {
                let mut oks = 0;
                while oks < EVENTS {
                    match read.next().await {
                        Some(Ok(Message::Text(_))) => oks += 1,
                        _ => break,
                    }
                }
                oks
            });
            for frame in frames.iter() {
                write.send(Message::Text(frame.clone().into())).await.expect("send");
            }
            tokio::time::timeout(Duration::from_secs(10), reader)
                .await
                .expect("timed out waiting for OKs")
                .unwrap()
        }));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while delivered.load(Ordering::Relaxed) < target && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let got = delivered.load(Ordering::Relaxed);
    assert_eq!(got, target, "expected {target} deliveries, got {got}");

    for t in pub_tasks {
        let oks = t.await.unwrap();
        assert_eq!(oks, EVENTS, "pub missing OKs");
    }
    for t in sub_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(200), t).await;
    }

    drop(relay);
}
