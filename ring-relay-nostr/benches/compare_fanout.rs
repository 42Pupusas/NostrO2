//! Head-to-head fan-out bench: ring-relay-nostr vs nostr-relay 0.4.8.
//!
//! 4 publishers × 100 events × 256 subscribers. Every subscriber has an
//! open filter so it should receive every event. Matched shard/worker
//! counts across both relays.
//!
//! Same tmpfs caveat as compare_ingest: nostr-relay's LMDB is pointed at
//! /dev/shm so we don't measure disk speed, but the DB code path still
//! runs (write batching every 100ms, serialization, LMDB bookkeeping).
//! ring-relay-nostr has no persistence by design.

#[path = "common/mod.rs"]
mod common;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

use common::{Relay, presign_for};

const NUM_PUBS: usize = 4;
const EVENTS_PER_PUB: usize = 25;
const NUM_SUBS: usize = 64;

async fn run_fanout(port: u16, frame_sets: Arc<Vec<Arc<Vec<String>>>>) {
    let url = format!("ws://127.0.0.1:{port}");
    let delivered = Arc::new(AtomicUsize::new(0));
    let events_per_pub = frame_sets[0].len();
    let total_events = frame_sets.len() * events_per_pub;
    let target = NUM_SUBS * total_events;

    let mut sub_tasks = Vec::with_capacity(NUM_SUBS);
    for i in 0..NUM_SUBS {
        let url = url.clone();
        let delivered = delivered.clone();
        sub_tasks.push(tokio::spawn(async move {
            // Bound the connect to prevent a stuck handshake from hanging
            // the bench when the relay is at its connection limit.
            let ws = match tokio::time::timeout(
                Duration::from_secs(10),
                tokio_tungstenite::connect_async(&url),
            )
            .await
            {
                Ok(Ok((ws, _))) => ws,
                _ => return,
            };
            let (mut write, mut read) = ws.split();
            let req = format!(r#"["REQ","s{i}",{{"kinds":[1]}}]"#);
            if write.send(Message::Text(req.into())).await.is_err() {
                return;
            }

            let mut events = 0;
            while events < total_events {
                match read.next().await {
                    Some(Ok(Message::Text(t))) => {
                        if t.starts_with("[\"EVENT\"") {
                            events += 1;
                            delivered.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
        }));
    }

    // Let REQs settle (for both relays: SubRepl propagation on our side, DB
    // writer batching boundary on theirs).
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut pub_tasks = Vec::with_capacity(frame_sets.len());
    for frames in frame_sets.iter().cloned() {
        let url = url.clone();
        let expected = frames.len();
        pub_tasks.push(tokio::spawn(async move {
            let ws = match tokio::time::timeout(
                Duration::from_secs(10),
                tokio_tungstenite::connect_async(&url),
            )
            .await
            {
                Ok(Ok((ws, _))) => ws,
                _ => return,
            };
            let (mut write, mut read) = ws.split();
            let reader = tokio::spawn(async move {
                let mut oks = 0;
                while oks < expected {
                    match read.next().await {
                        Some(Ok(Message::Text(_))) => oks += 1,
                        Some(Ok(_)) => {}
                        _ => break,
                    }
                }
            });
            for frame in frames.iter() {
                if write
                    .send(Message::Text(frame.clone().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Give OKs a bounded window to arrive; if the relay is behind on
            // its DB writer we don't want to hang the whole bench.
            let _ = tokio::time::timeout(Duration::from_secs(30), reader).await;
        }));
    }

    // Hard timeout on total delivery. Pathological regressions give up here.
    let deadline = Instant::now() + Duration::from_secs(90);
    while delivered.load(Ordering::Relaxed) < target && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Bounded teardown — abort tasks that haven't finished by now rather
    // than await them unbounded.
    for t in pub_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(500), t).await;
    }
    for t in sub_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(200), t).await;
    }
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_fanout");
    // Keep this bench cheap — each iter spawns a fresh relay, connects
    // 64+4 WebSockets, runs fan-out, and tears down. At that scale nostr-
    // relay at workers=1 is the limiting factor; 5 samples × 10s is enough
    // to separate the relays without letting the bench run for 30+ minutes.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    let deliveries = NUM_SUBS * NUM_PUBS * EVENTS_PER_PUB;
    group.throughput(Throughput::Elements(deliveries as u64));

    // One distinct keypair per publisher so nostr-relay (which dedupes by
    // event id) doesn't discard the second pub's copy of identical content.
    let frame_sets: Arc<Vec<Arc<Vec<String>>>> = Arc::new(
        (0..NUM_PUBS)
            .map(|i| presign_for(EVENTS_PER_PUB, &format!("pub{i}")))
            .collect(),
    );

    for &workers in &[1usize, 2, 4] {
        // Each bench iteration rebuilds the relay; total connections = 256 + 4.
        let max_clients = NUM_SUBS + NUM_PUBS + 8;

        group.bench_with_input(
            BenchmarkId::new("ring", workers),
            &workers,
            |b, &w| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(6)
                    .enable_all()
                    .build()
                    .unwrap();
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let relay = Relay::spawn_ring(w, max_clients);
                        let frames = Arc::clone(&frame_sets);
                        let start = Instant::now();
                        rt.block_on(run_fanout(relay.port, frames));
                        total += start.elapsed();
                        drop(relay);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    total
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("nostr_relay", workers),
            &workers,
            |b, &w| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(6)
                    .enable_all()
                    .build()
                    .unwrap();
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let relay = Relay::spawn_nostr_relay(w);
                        let frames = Arc::clone(&frame_sets);
                        let start = Instant::now();
                        rt.block_on(run_fanout(relay.port, frames));
                        total += start.elapsed();
                        drop(relay);
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
