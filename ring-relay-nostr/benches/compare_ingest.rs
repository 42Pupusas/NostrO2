//! Head-to-head ingest bench: ring-relay-nostr (ephemeral, io_uring) vs
//! nostr-relay 0.4.8 (actix + LMDB on tmpfs).
//!
//! 8 publishers, each publishing a batch of distinct EVENTs per iteration.
//! No subscribers. Measures pure ingest throughput: parse + verify + OK
//! encode + write. Setup (relay spawn, 8 TCP connects) and teardown happen
//! once per configuration, outside the timed region.
//!
//! Caveat: nostr-relay's LMDB is pointed at /dev/shm so disk I/O is not a
//! variable, but the full DB code path (serialization, LMDB bookkeeping,
//! batched writer actor) still runs. ring-relay-nostr has no persistence
//! by design. The comparison is "what's the throughput ceiling of the two
//! designs when storage is not the bottleneck," not "relay logic only."

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
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::Message;

use common::{Relay, presign_for};

const NUM_PUBS: usize = 8;
const EVENTS_PER_ITER: usize = 200; // per pub, per iteration

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Harness holding an already-running relay and pre-opened publisher
/// connections. Each iteration publishes the next `EVENTS_PER_ITER` events
/// per publisher and waits for their OK acks.
struct IngestHarness {
    relay: Option<Relay>,
    rt: Runtime,
    pub_sinks: Vec<WsSink>,
    ok_count: Arc<AtomicUsize>,
    /// Pre-signed events per publisher; each iter consumes EVENTS_PER_ITER.
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

        // Publish all 8 pubs in parallel so the client side isn't a serial
        // bottleneck. Each sink gets its own task; join_all at the end.
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

        // Tight spin with periodic yield to the scheduler. A blocking sleep
        // here would add poll-grain latency per iter; since the reader tasks
        // run on tokio workers (not this thread), we don't starve anything
        // by spinning.
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

async fn connect_pubs(
    url: &str,
    ok_count: Arc<AtomicUsize>,
) -> (Vec<WsSink>, Vec<JoinHandle<()>>) {
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
    let mut group = c.benchmark_group("compare_ingest");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    let events_per_iter = NUM_PUBS * EVENTS_PER_ITER;
    group.throughput(Throughput::Elements(events_per_iter as u64));

    for &workers in &[1usize, 2, 4] {
        let max_clients = NUM_PUBS + 8;

        group.bench_with_input(
            BenchmarkId::new("ring", workers),
            &workers,
            |b, &w| {
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
