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

/// Defaults; override with env vars HEAP_SUBS / HEAP_EVENTS.
const DEFAULT_SUBS: usize = 50;
const DEFAULT_EVENTS: usize = 200;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let num_subs = env_usize("HEAP_SUBS", DEFAULT_SUBS);
    let num_events = env_usize("HEAP_EVENTS", DEFAULT_EVENTS);
    let worker_threads = env_usize("HEAP_WORKERS", 4);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();

    let profiler = dhat::Profiler::new_heap();

    rt.block_on(async {
        // Relay bound on an OS-assigned port. `max_clients` includes the
        // publisher plus a few slack slots so we don't hit the per-shard
        // FIFO eviction on the last subscriber.
        let cfg = RelayConfig {
            max_clients: num_subs + 16,
            max_subs_per_conn: 4,
            max_filters_per_sub: 4,
            ..Default::default()
        };
        let relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("bind");
        let port = relay.port();
        let url = format!("ws://127.0.0.1:{port}");

        // Connect N subscribers in parallel batches — serial connect at
        // 5k would dominate wall time and exercise mostly handshake code.
        // Batch size keeps the handshake pressure bounded so the server
        // doesn't dip into eviction during bring-up.
        const CONNECT_BATCH: usize = 128;
        let mut sub_sockets = Vec::with_capacity(num_subs);
        for chunk_start in (0..num_subs).step_by(CONNECT_BATCH) {
            let chunk_end = (chunk_start + CONNECT_BATCH).min(num_subs);
            let mut futs = Vec::with_capacity(chunk_end - chunk_start);
            for i in chunk_start..chunk_end {
                let url = url.clone();
                futs.push(tokio::spawn(async move {
                    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
                    let sub_id = format!("s{i:05}");
                    let req = format!(r#"["REQ","{sub_id}",{{"kinds":[1]}}]"#);
                    ws.send(Message::Text(req.into())).await.unwrap();
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(t) = msg
                            && t.starts_with("[\"EOSE\"")
                        {
                            break;
                        }
                    }
                    ws
                }));
            }
            for fut in futs {
                sub_sockets.push(fut.await.unwrap());
            }
        }

        // Each subscriber drains incoming EVENT frames on its own task so
        // the writer doesn't backpressure while we publish.
        let sub_handles: Vec<_> = sub_sockets
            .into_iter()
            .map(|mut ws| {
                tokio::spawn(async move {
                    let mut count = 0;
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(_) = msg {
                            count += 1;
                            if count >= num_events {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();

        // Publisher: signs and sends `num_events` kind:1 notes.
        let kp = K256Keypair::generate();
        let (mut pubws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        for i in 0..num_events {
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

        // Wait for subscribers to drain, bounded so a stalled writer can't
        // hang the bin indefinitely.
        let _ = tokio::time::timeout(
            Duration::from_secs(60),
            futures_util::future::join_all(sub_handles),
        )
        .await;
    });

    drop(profiler);
    eprintln!("heap_fanout_live: {num_events} events × {num_subs} subs — wrote dhat-heap.json");
}
