//! Heap profile of the multi-publisher ingest workload.
//!
//! Mirrors `benches/event_ingest_multi.rs` (8 publishers × 500 events,
//! ephemeral relay, no subscribers) under dhat. Used to A/B the
//! verify-pool topology — the throughput bench tells us which design is
//! faster; this tells us what each costs in heap.
//!
//! Knobs (env vars):
//! - `HEAP_SHARDS` (default 4): reader/writer shard count.
//! - `HEAP_EVENTS` (default 500): events per publisher.
//! - `HEAP_PUBS`   (default 8): publisher count.
//! - `HEAP_WORKERS` (default 8): tokio worker threads for the driver.
//!
//! The relay is spawned **before** the dhat profiler starts, so static
//! ring allocations (verify-pool jobs/results buffers, etc.) are
//! *included* in the profile via the steady-state allocs that follow.
//! Bind-time-only allocations are still captured because we install the
//! global dhat allocator at process start — but to keep the runs
//! comparable we also do a 200ms warmup before snapshotting.
//!
//! Open the produced `dhat-heap.json` in `dh_view.html`. The numbers
//! that matter for the verify-pool comparison:
//! - **Total bytes**: end-to-end allocation traffic. Should be ~equal
//!   between M-SPSC and SPMC because per-event `Arc<[u8]>` /
//!   `Arc<str>` traffic dominates.
//! - **Peak heap (live bytes)**: the steady-state working set. This is
//!   where the ring-shape difference shows up: M-SPSC = M × cap slots,
//!   SPMC = 1 × cap slots, plus per-slot `ready[s]` / `done[s]`
//!   cache-padded atomics.

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use ring_relay_server::ShardConfig;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn spawn_relay(reader_shards: usize) -> (u16, ring_relay_nostr::ShutdownHandle) {
    let mut cfg = RelayConfig::default();
    cfg.shards = ShardConfig {
        reader_shards,
        writer_shards: reader_shards,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    rx.recv().unwrap()
}

fn presign(count: usize) -> Arc<Vec<String>> {
    let kp = K256Keypair::generate();
    Arc::new(
        (0..count)
            .map(|i| {
                let mut note = NostrNote::text_note(&format!("heap {i}"));
                note.pubkey = kp.public_key();
                kp.sign_nostr_note(&mut note).expect("sign");
                format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
            })
            .collect(),
    )
}

async fn run_multi_pub(port: u16, frames: Arc<Vec<String>>, num_pubs: usize) {
    let url = format!("ws://127.0.0.1:{port}");
    let expected = frames.len();

    let mut pubs = Vec::with_capacity(num_pubs);
    for _ in 0..num_pubs {
        let url = url.clone();
        let frames = Arc::clone(&frames);
        pubs.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("connect");
            let (mut write, mut read) = ws.split();

            let reader = tokio::spawn(async move {
                let mut seen = 0;
                while seen < expected {
                    match read.next().await {
                        Some(Ok(Message::Text(_))) => seen += 1,
                        Some(Ok(_)) => {}
                        _ => break,
                    }
                }
            });

            for frame in frames.iter() {
                write
                    .send(Message::Text(frame.clone().into()))
                    .await
                    .expect("send");
            }

            reader.await.unwrap();
        }));
    }

    for p in pubs {
        let _ = p.await;
    }
}

fn main() {
    let shards = env_usize("HEAP_SHARDS", 4);
    let events_per_pub = env_usize("HEAP_EVENTS", 500);
    let num_pubs = env_usize("HEAP_PUBS", 8);
    let worker_threads = env_usize("HEAP_WORKERS", 8);

    let frames = presign(events_per_pub);

    let (port, shutdown) = spawn_relay(shards);

    // Warmup so bind-time allocations settle before we measure.
    std::thread::sleep(Duration::from_millis(200));

    let profiler = dhat::Profiler::new_heap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(run_multi_pub(port, frames, num_pubs));

    // Brief drain so any in-flight verify results / write commits land
    // before the snapshot.
    std::thread::sleep(Duration::from_millis(200));

    drop(profiler);
    eprintln!(
        "heap_ingest_multi: shards={shards} pubs={num_pubs} events_per_pub={events_per_pub} \
         total={} — wrote dhat-heap.json",
        num_pubs * events_per_pub
    );

    shutdown.shutdown();
}
