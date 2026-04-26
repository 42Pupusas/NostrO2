//! Large-frame ingest comparison: ring-relay-nostr vs nostr-relay 0.4.8.
//!
//! Same harness as `compare_ingest`, but each EVENT carries a large content
//! payload (~384 KiB) so the timed region is dominated by WebSocket read,
//! sha256(id) recompute, and Schnorr verify on a realistic upper-end event.
//!
//! Content size is chosen to fit inside both relays' default frame caps:
//! - ring-relay-nostr: `max_message_length = 2 MiB`
//! - nostr-relay 0.4.8: `max_message_length = 512 KiB`
//!
//! Smaller counts per iter than `compare_ingest` because each event is ~384x
//! larger; otherwise a single iteration would move ~600 MB of payload.

#[path = "common/mod.rs"]
mod common;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use common::Relay;

const NUM_PUBS: usize = 4;
const EVENTS_PER_ITER: usize = 10;
const CONTENT_BYTES: usize = 384 * 1024;

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Pre-sign `count` distinct kind-1 events each carrying ~CONTENT_BYTES of
/// payload. Uses a unique per-pub prefix so nostr-relay doesn't dedup across
/// publishers.
fn presign_large(count: usize, tag: &str) -> Arc<Vec<String>> {
    let kp = K256Keypair::generate();
    // Build the filler once — each note gets the same large suffix plus a
    // small distinct prefix to keep event ids distinct. Signing dominates
    // the cost here either way, so reusing the payload is fine.
    let filler: String = "x".repeat(CONTENT_BYTES - 32);
    Arc::new(
        (0..count)
            .map(|i| {
                let mut note = NostrNote::text_note(&format!("{tag}-{i:08} {filler}"));
                note.pubkey = kp.public_key();
                kp.sign_nostr_note(&mut note).expect("sign");
                format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
            })
            .collect(),
    )
}

struct IngestHarness {
    relay: Option<Relay>,
    rt: Runtime,
    pub_sinks: Vec<WsSink>,
    ok_count: Arc<AtomicUsize>,
    pub_pools: Vec<Arc<Vec<String>>>,
    pub_cursor: usize,
    reader_tasks: Vec<JoinHandle<()>>,
}

impl IngestHarness {
    fn new(rt: Runtime, relay: Relay, total_iters: u64) -> Self {
        let port = relay.port;
        let url = format!("ws://127.0.0.1:{port}");
        let ok_count = Arc::new(AtomicUsize::new(0));
        let total_events_per_pub = (total_iters as usize) * EVENTS_PER_ITER;

        let pub_pools: Vec<Arc<Vec<String>>> = (0..NUM_PUBS)
            .map(|i| presign_large(total_events_per_pub, &format!("pub{i}")))
            .collect();

        let (pub_sinks, reader_tasks) = rt.block_on(connect_pubs(&url, ok_count.clone()));

        Self {
            relay: Some(relay),
            rt,
            pub_sinks,
            ok_count,
            pub_pools,
            pub_cursor: 0,
            reader_tasks,
        }
    }

    fn iterate(&mut self) {
        let start_cursor = self.pub_cursor;
        let end_cursor = start_cursor + EVENTS_PER_ITER;
        let before = self.ok_count.load(Ordering::Relaxed);
        let target = before + NUM_PUBS * EVENTS_PER_ITER;

        let pools = self.pub_pools.clone();
        let sinks = std::mem::take(&mut self.pub_sinks);
        let returned: Vec<WsSink> = self.rt.block_on(async {
            let mut handles = Vec::with_capacity(sinks.len());
            for (mut sink, pool) in sinks.into_iter().zip(pools.iter().cloned()) {
                handles.push(tokio::spawn(async move {
                    for frame in &pool[start_cursor..end_cursor] {
                        sink.send(Message::Text(frame.clone().into()))
                            .await
                            .expect("pub send");
                    }
                    sink
                }));
            }
            let mut out = Vec::with_capacity(handles.len());
            for h in handles {
                out.push(h.await.unwrap());
            }
            out
        });
        self.pub_sinks = returned;

        // Larger per-event deadline — verifying a 384 KiB note is ~10x the
        // tiny-event cost, and per-iter latency can drift if the OS is busy.
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut spins: u32 = 0;
        loop {
            let n = self.ok_count.load(Ordering::Relaxed);
            if n >= target {
                break;
            }
            spins = spins.wrapping_add(1);
            if spins & 0x3ff == 0 {
                if Instant::now() > deadline {
                    panic!(
                        "iter timed out: ok_count={n} target={target} (pub_cursor={})",
                        self.pub_cursor
                    );
                }
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }

        self.pub_cursor = end_cursor;
    }
}

impl Drop for IngestHarness {
    fn drop(&mut self) {
        for t in self.reader_tasks.drain(..) {
            t.abort();
        }
        self.pub_sinks.clear();
        drop(self.relay.take());
    }
}

async fn connect_pubs(url: &str, ok_count: Arc<AtomicUsize>) -> (Vec<WsSink>, Vec<JoinHandle<()>>) {
    let mut sinks = Vec::with_capacity(NUM_PUBS);
    let mut readers = Vec::with_capacity(NUM_PUBS);
    for _ in 0..NUM_PUBS {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("pub connect");
        let (write, mut read) = ws.split();
        sinks.push(write);
        let ok = ok_count.clone();
        readers.push(tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                if matches!(msg, Message::Text(_)) {
                    ok.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    (sinks, readers)
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_ingest_large");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    let events_per_iter = NUM_PUBS * EVENTS_PER_ITER;
    // Report throughput in bytes so results express real wire bandwidth, not
    // just OK/s — large-payload runs are bandwidth-bound.
    group.throughput(Throughput::Bytes((events_per_iter * CONTENT_BYTES) as u64));

    for &workers in &[1usize, 2, 4] {
        let max_clients = NUM_PUBS + 8;

        group.bench_with_input(BenchmarkId::new("ring", workers), &workers, |b, &w| {
            b.iter_custom(|iters| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(NUM_PUBS.min(8))
                    .enable_all()
                    .build()
                    .unwrap();
                let relay = Relay::spawn_ring(w, max_clients);
                let mut h = IngestHarness::new(rt, relay, iters);
                let start = Instant::now();
                for _ in 0..iters {
                    h.iterate();
                }
                let elapsed = start.elapsed();
                drop(h);
                elapsed
            });
        });

        group.bench_with_input(
            BenchmarkId::new("nostr_relay", workers),
            &workers,
            |b, &w| {
                b.iter_custom(|iters| {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(NUM_PUBS.min(8))
                        .enable_all()
                        .build()
                        .unwrap();
                    let relay = Relay::spawn_nostr_relay(w);
                    let mut h = IngestHarness::new(rt, relay, iters);
                    let start = Instant::now();
                    for _ in 0..iters {
                        h.iterate();
                    }
                    let elapsed = start.elapsed();
                    drop(h);
                    elapsed
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
