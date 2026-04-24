//! End-to-end heap profile of the fan-out path.
//!
//! Spins up a real `NostrRelay`, connects N subscribers (each with an open
//! filter) plus one publisher, streams a fixed number of events through,
//! then drops the profiler to produce `dhat-heap.json`.
//!
//! Unlike the in-process `heap_fanout_view` bins, this exercises the
//! actual shard dispatcher → writer thread → io_uring path, so the
//! measurement reflects every allocation along the real fan-out route —
//! parse, filter, writer composition, WS framing, everything.
//!
//! Run:
//!   cargo run --release --example heap_fanout_live

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const NUM_SUBS: usize = 50;
const NUM_EVENTS: usize = 200;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    let profiler = dhat::Profiler::new_heap();

    rt.block_on(async {
        // Relay bound on an OS-assigned port.
        let cfg = RelayConfig {
            max_clients: NUM_SUBS + 4,
            max_subs_per_conn: 4,
            max_filters_per_sub: 4,
            ..Default::default()
        };
        let relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("bind");
        let port = relay.port();
        let url = format!("ws://127.0.0.1:{port}");

        // Connect N subscribers, each with an open filter (kind:1).
        let mut sub_sockets = Vec::with_capacity(NUM_SUBS);
        for i in 0..NUM_SUBS {
            let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let sub_id = format!("s{i:03}");
            let req = format!(r#"["REQ","{sub_id}",{{"kinds":[1]}}]"#);
            ws.send(Message::Text(req.into())).await.unwrap();
            // Drain the immediate EOSE.
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(t) = msg
                    && t.starts_with("[\"EOSE\"")
                {
                    break;
                }
            }
            sub_sockets.push(ws);
        }

        // Each subscriber drains incoming EVENT frames on its own task
        // so the writer doesn't backpressure while we publish.
        let sub_handles: Vec<_> = sub_sockets
            .into_iter()
            .map(|mut ws| {
                tokio::spawn(async move {
                    let mut count = 0;
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(_) = msg {
                            count += 1;
                            if count >= NUM_EVENTS {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();

        // Publisher: signs and sends NUM_EVENTS kind:1 notes.
        let kp = K256Keypair::generate();
        let (mut pubws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        for i in 0..NUM_EVENTS {
            let mut note = NostrNote {
                content: format!("heap-live event {i}"),
                kind: 1,
                pubkey: kp.public_key(),
                ..Default::default()
            };
            kp.sign_nostr_note(&mut note).unwrap();
            let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
            pubws.send(Message::Text(frame.into())).await.unwrap();

            // Drain the OK.
            while let Some(Ok(msg)) = pubws.next().await {
                if let Message::Text(t) = msg
                    && t.starts_with("[\"OK\"")
                {
                    break;
                }
            }
        }

        // Give subscribers a moment to catch up, then wait.
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            futures_util::future::join_all(sub_handles),
        )
        .await;
    });

    drop(profiler);
    eprintln!(
        "heap_fanout_live: {NUM_EVENTS} events × {NUM_SUBS} subs — wrote dhat-heap.json"
    );
}
