//! End-to-end bench: single publisher blasts signed EVENTs, relay replies
//! with OK. No subscribers — this measures ingest throughput:
//! `parse + verify + FIFO bookkeeping + OK encode + write`.
//!
//! Today the dispatch loop is single-threaded and runs `note.verify()`
//! inline, so we expect this to be verify-bound. Moving verify off the
//! dispatch thread should show up here directly.

use criterion::{Criterion, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::NostrKeypair;
use ring_relay_nostr::{NostrRelay, RelayConfig};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const NUM_EVENTS: usize = 2_000;

fn spawn_relay() -> (u16, ring_relay_nostr::ShutdownHandle) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut relay =
            NostrRelay::bind([127, 0, 0, 1], 0, RelayConfig::default()).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    rx.recv().unwrap()
}

/// Pre-sign a batch of notes up front. Signing is expensive and we want
/// the bench to measure the relay, not the client's signer.
fn presign(count: usize) -> Vec<String> {
    let kp = NostrKeypair::new_extractable();
    (0..count)
        .map(|i| {
            let mut note = NostrNote::text_note(&format!("bench {i}"));
            note.pubkey = kp.public_key();
            kp.sign_nostr_note(&mut note).expect("sign");
            format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
        })
        .collect()
}

async fn run_ingest(port: u16, frames: &[String]) {
    let url = format!("ws://127.0.0.1:{port}");
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
    let (mut write, mut read) = ws.split();

    let expected = frames.len();
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

    for frame in frames {
        write
            .send(Message::Text(frame.clone().into()))
            .await
            .expect("send");
    }

    reader.await.unwrap();
}

fn bench_event_ingest(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_ingest");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(criterion::Throughput::Elements(NUM_EVENTS as u64));

    let frames = presign(NUM_EVENTS);

    group.bench_function("ingest_2000_events", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let (port, shutdown) = spawn_relay();

                let start = Instant::now();
                rt.block_on(run_ingest(port, &frames));
                total += start.elapsed();

                shutdown.shutdown();
                // Give the relay thread a beat to exit before we rebind.
                std::thread::sleep(Duration::from_millis(20));
            }
            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_event_ingest);
criterion_main!(benches);
