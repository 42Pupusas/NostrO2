//! Multi-publisher ingest bench.
//!
//! P publisher connections × E events each, measured across N reader shards.
//! No subscribers — pure ingest: parse + verify + OK-ack.
//!
//! With the per-shard dispatcher architecture, events arriving on different
//! reader shards are verified in parallel. At P ≥ N (publishers spread across
//! shards) we expect aggregate throughput to scale roughly linearly with N
//! until verify is no longer the bottleneck.
//!
//! Baseline (pre-per-shard, central dispatcher): one verify thread caps the
//! whole relay at ~11.6K ev/s regardless of P or N.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use ring_relay_server::ShardConfig;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const EVENTS_PER_PUB: usize = 500;
const NUM_PUBS: usize = 8;

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
                let mut note = NostrNote::text_note(&format!("bench {i}"));
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
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
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
                write.send(Message::Text(frame.clone().into())).await.expect("send");
            }

            reader.await.unwrap();
        }));
    }

    for p in pubs {
        let _ = p.await;
    }
}

fn bench_multi_pub_ingest(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_ingest_multi");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(25));

    let frames = presign(EVENTS_PER_PUB);

    for &shards in &[1usize, 2, 4] {
        let total_events = NUM_PUBS * EVENTS_PER_PUB;
        group.throughput(Throughput::Elements(total_events as u64));
        group.bench_with_input(
            BenchmarkId::new("pubs8", shards),
            &shards,
            |b, &shards| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(NUM_PUBS.min(8))
                    .enable_all()
                    .build()
                    .unwrap();

                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let (port, shutdown) = spawn_relay(shards);
                        let frames = Arc::clone(&frames);

                        let start = Instant::now();
                        rt.block_on(run_multi_pub(port, frames, NUM_PUBS));
                        total += start.elapsed();

                        shutdown.shutdown();
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_multi_pub_ingest);
criterion_main!(benches);
