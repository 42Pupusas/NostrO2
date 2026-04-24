//! End-to-end bench: 1 publisher + N subscribers, all subs match every
//! event. Measures fan-out throughput: events-delivered per second.
//!
//! This is the primary scaling bench. With the current implementation
//! fan-out walks every client × every sub × every filter on every
//! EVENT, so throughput should drop roughly linearly with N. Adding
//! sub indexing or parallelizing fan-out should flatten that curve.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const NUM_EVENTS: usize = 200;
const SUB_COUNTS: &[usize] = &[64, 256, 1024];

fn spawn_relay(max_clients: usize) -> (u16, ring_relay_nostr::ShutdownHandle) {
    let mut cfg = RelayConfig::default();
    cfg.max_clients = max_clients;
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

fn presign(count: usize) -> Vec<String> {
    let kp = K256Keypair::generate();
    (0..count)
        .map(|i| {
            let mut note = NostrNote::text_note(&format!("bench {i}"));
            note.pubkey = kp.public_key();
            kp.sign_nostr_note(&mut note).expect("sign");
            format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
        })
        .collect()
}

async fn run_fanout(port: u16, frames: &[String], num_subs: usize) {
    let url = format!("ws://127.0.0.1:{port}");

    // Firehose subscribers: empty filter matches every event.
    let delivered = Arc::new(AtomicUsize::new(0));
    let target = num_subs * frames.len();

    let mut sub_tasks = Vec::with_capacity(num_subs);
    for i in 0..num_subs {
        let url = url.clone();
        let delivered = delivered.clone();
        let expected = frames.len();
        sub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("sub connect");
            let (mut write, mut read) = ws.split();
            let sub_id = format!("s{i}");
            let req = format!(r#"["REQ","{sub_id}",{{}}]"#);
            write.send(Message::Text(req.into())).await.expect("send REQ");

            // Drain: first frame will be EOSE, then expected EVENT frames.
            let mut events = 0;
            while events < expected {
                match read.next().await {
                    Some(Ok(Message::Text(t))) => {
                        // Skip EOSE; count EVENT frames only.
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

    // Publisher: separate connection, blasts all events.
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("pub connect");
    let (mut pub_write, mut pub_read) = ws.split();

    // Drain OKs from the publisher so its socket doesn't back-pressure.
    let pub_drain = tokio::spawn(async move {
        let mut oks = 0;
        while let Some(Ok(_)) = pub_read.next().await {
            oks += 1;
            if oks >= NUM_EVENTS {
                break;
            }
        }
    });

    // Tiny settle so subs are registered before publishing.
    tokio::time::sleep(Duration::from_millis(50)).await;

    for frame in frames {
        pub_write
            .send(Message::Text(frame.clone().into()))
            .await
            .expect("pub send");
    }

    // Wait for every sub to see every event, with a hard timeout so a
    // pathological regression can't hang the bench suite.
    let deadline = Instant::now() + Duration::from_secs(60);
    while delivered.load(Ordering::Relaxed) < target && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let _ = pub_drain.await;
    for t in sub_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(200), t).await;
    }
}

fn bench_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    let frames = presign(NUM_EVENTS);

    for &num_subs in SUB_COUNTS {
        // Throughput = deliveries per event-batch = num_subs * NUM_EVENTS.
        group.throughput(Throughput::Elements((num_subs * NUM_EVENTS) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(num_subs),
            &num_subs,
            |b, &num_subs| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .unwrap();

                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        // +2 for publisher connection and a bit of slack.
                        let (port, shutdown) = spawn_relay(num_subs + 8);

                        let start = Instant::now();
                        rt.block_on(run_fanout(port, &frames, num_subs));
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

criterion_group!(benches, bench_fanout);
criterion_main!(benches);
