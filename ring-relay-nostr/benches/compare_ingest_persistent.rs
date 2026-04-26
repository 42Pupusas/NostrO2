//! Head-to-head ingest bench **with persistence on both sides**:
//! ring-relay-nostr (bounded buckets + storage thread, data on tmpfs) vs
//! nostr-relay 0.4.8 (actix + LMDB on tmpfs).
//!
//! Same shape as `compare_ingest` (8 publishers, N events/iter, no subs).
//! The difference: ring-relay-nostr is configured with `StorageConfig`, so
//! every EVENT is parsed, verified, OK'd, fanned-out to (zero) subs, *and*
//! handed to the storage thread which writes + indexes + group-commit
//! fsyncs. This is the apples-to-apples comparison for a persistent relay.

#[path = "common/mod.rs"]
mod common;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use common::{Relay, presign_for};

const NUM_PUBS: usize = 8;
const EVENTS_PER_ITER: usize = 200;

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

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
            .map(|i| presign_for(total_events_per_pub, &format!("pub{i}")))
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

        let deadline = Instant::now() + Duration::from_secs(60);
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
    let mut group = c.benchmark_group("compare_ingest_persistent");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    let events_per_iter = NUM_PUBS * EVENTS_PER_ITER;
    group.throughput(Throughput::Elements(events_per_iter as u64));

    for &workers in &[1usize, 2, 4] {
        let max_clients = NUM_PUBS + 8;

        group.bench_with_input(
            BenchmarkId::new("ring_persistent", workers),
            &workers,
            |b, &w| {
                b.iter_custom(|iters| {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(NUM_PUBS.min(8))
                        .enable_all()
                        .build()
                        .unwrap();
                    let relay = Relay::spawn_ring_persistent(w, max_clients);
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
