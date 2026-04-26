//! Cross-shard fan-out bench.
//!
//! P publishers × S subscribers across N reader shards. Every EVENT from
//! any publisher should reach every subscriber regardless of shard placement
//! — this exercises the sub replication ring.
//!
//! Why multi-publisher: with a single publisher all matching happens on the
//! shard that owns the publisher's connection, so multiple shards don't help
//! and only add replication overhead. Multiple publishers spread the per-
//! EVENT filter-match work across shards and this is where the per-shard
//! architecture earns its keep.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use ring_relay_server::ShardConfig;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const NUM_PUBS: usize = 4;
const EVENTS_PER_PUB: usize = 100;
const NUM_SUBS: usize = 256;

fn spawn_relay(reader_shards: usize) -> (u16, ring_relay_nostr::ShutdownHandle) {
    let mut cfg = RelayConfig::default();
    cfg.shards = ShardConfig {
        reader_shards,
        writer_shards: reader_shards,
    };
    // Be generous so all subs + publisher connections fit.
    cfg.max_clients = NUM_SUBS + 32;
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
                let mut note = NostrNote::text_note(&format!("fan {i}"));
                note.pubkey = kp.public_key();
                kp.sign_nostr_note(&mut note).expect("sign");
                format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
            })
            .collect(),
    )
}

async fn run_fanout(port: u16, frames: Arc<Vec<String>>, num_subs: usize, num_pubs: usize) {
    let url = format!("ws://127.0.0.1:{port}");
    let delivered = Arc::new(AtomicUsize::new(0));
    let total_events = num_pubs * frames.len();
    let target = num_subs * total_events;

    let mut sub_tasks = Vec::with_capacity(num_subs);
    for i in 0..num_subs {
        let url = url.clone();
        let delivered = delivered.clone();
        sub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("sub connect");
            let (mut write, mut read) = ws.split();
            let req = format!(r#"["REQ","s{i}",{{}}]"#);
            write
                .send(Message::Text(req.into()))
                .await
                .expect("send REQ");

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

    // Wait a beat so sub replication has time to propagate REQs to peers
    // before the first EVENT lands.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut pub_tasks = Vec::with_capacity(num_pubs);
    for _ in 0..num_pubs {
        let url = url.clone();
        let frames = Arc::clone(&frames);
        let expected = frames.len();
        pub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("pub connect");
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
                write
                    .send(Message::Text(frame.clone().into()))
                    .await
                    .expect("pub send");
            }
            reader.await.unwrap();
        }));
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    while delivered.load(Ordering::Relaxed) < target && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    for t in pub_tasks {
        let _ = t.await;
    }
    for t in sub_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(200), t).await;
    }
}

fn bench_fanout_multi(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout_multi");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    let frames = presign(EVENTS_PER_PUB);

    for &shards in &[1usize, 2, 4] {
        let deliveries = NUM_SUBS * NUM_PUBS * EVENTS_PER_PUB;
        group.throughput(Throughput::Elements(deliveries as u64));
        group.bench_with_input(
            BenchmarkId::new("pubs4_subs256", shards),
            &shards,
            |b, &shards| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(6)
                    .enable_all()
                    .build()
                    .unwrap();

                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let (port, shutdown) = spawn_relay(shards);
                        let frames = Arc::clone(&frames);

                        let start = Instant::now();
                        rt.block_on(run_fanout(port, frames, NUM_SUBS, NUM_PUBS));
                        total += start.elapsed();

                        shutdown.shutdown();
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_fanout_multi);
criterion_main!(benches);
