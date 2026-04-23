//! Head-to-head ingest bench: ring-relay-nostr (ephemeral, io_uring) vs
//! nostr-relay 0.4.8 (actix + LMDB on tmpfs).
//!
//! 8 publishers, each sending 500 pre-signed EVENTs and draining their OK
//! acks. No subscribers. Matched shard/worker counts across both relays.
//!
//! Caveat: nostr-relay's LMDB is pointed at /dev/shm so disk I/O is not a
//! variable, but the full DB code path (serialization, LMDB bookkeeping,
//! batched writer actor) still runs. ring-relay-nostr has no persistence
//! by design. The comparison is "what's the throughput ceiling of the two
//! designs when storage is not the bottleneck," not "relay logic only."

#[path = "common/mod.rs"]
mod common;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

use common::{Relay, presign_for};

const EVENTS_PER_PUB: usize = 500;
const NUM_PUBS: usize = 8;

async fn run_ingest(port: u16, frame_sets: Arc<Vec<Arc<Vec<String>>>>) {
    let url = format!("ws://127.0.0.1:{port}");

    let mut pubs = Vec::with_capacity(frame_sets.len());
    for frames in frame_sets.iter().cloned() {
        let url = url.clone();
        let expected = frames.len();
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
            // Bound the OK drain so a stuck writer can't hang the bench.
            let _ = tokio::time::timeout(Duration::from_secs(30), reader).await;
        }));
    }

    for p in pubs {
        let _ = tokio::time::timeout(Duration::from_secs(35), p).await;
    }
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_ingest");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(25));
    group.throughput(Throughput::Elements((NUM_PUBS * EVENTS_PER_PUB) as u64));

    // Distinct keypair per pub so event ids differ across publishers and
    // nostr-relay doesn't dedupe away 7 of the 8 streams' writes.
    let frame_sets: Arc<Vec<Arc<Vec<String>>>> = Arc::new(
        (0..NUM_PUBS)
            .map(|i| presign_for(EVENTS_PER_PUB, &format!("pub{i}")))
            .collect(),
    );

    for &workers in &[1usize, 2, 4] {
        group.bench_with_input(
            BenchmarkId::new("ring", workers),
            &workers,
            |b, &w| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(NUM_PUBS.min(8))
                    .enable_all()
                    .build()
                    .unwrap();
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let relay = Relay::spawn_ring(w, NUM_PUBS + 8);
                        let frames = Arc::clone(&frame_sets);
                        let start = Instant::now();
                        rt.block_on(run_ingest(relay.port, frames));
                        total += start.elapsed();
                        drop(relay);
                        std::thread::sleep(Duration::from_millis(30));
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
                    .worker_threads(NUM_PUBS.min(8))
                    .enable_all()
                    .build()
                    .unwrap();
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let relay = Relay::spawn_nostr_relay(w);
                        let frames = Arc::clone(&frame_sets);
                        let start = Instant::now();
                        rt.block_on(run_ingest(relay.port, frames));
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
