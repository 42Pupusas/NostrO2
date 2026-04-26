//! Head-to-head fan-out bench: ring-relay-nostr vs nostr-relay 0.4.8.
//!
//! Measures **only** the fan-out cycle — publish a batch of events, wait
//! for every subscriber to receive every event. Relay spawn, the 64 sub +
//! 4 pub TCP connects, REQ propagation, and teardown are all outside the
//! timed region.
//!
//! Tmpfs caveat: nostr-relay's LMDB is pointed at /dev/shm so we don't
//! measure disk speed, but the DB code path still runs (write batching
//! every 100ms, serialization, LMDB bookkeeping). ring-relay-nostr has no
//! persistence by design.

#[path = "common/mod.rs"]
mod common;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use common::{Relay, presign_for};

const NUM_PUBS: usize = 4;
const EVENTS_PER_ITER: usize = 25; // per pub, per iteration
const NUM_SUBS: usize = 64;

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Pre-built harness. The relay is running, every subscriber is connected
/// and subscribed, every publisher is connected. Each timed iteration pushes
/// a batch of events down `iter_trigger_tx` and waits on `delivered` to
/// reach the new target.
struct FanoutHarness {
    relay: Option<Relay>,
    rt: Runtime,
    pub_sinks: Vec<WsSink>,
    delivered: Arc<AtomicUsize>,
    /// Pool of pre-signed events per publisher. Each iteration consumes
    /// `EVENTS_PER_ITER` entries from each.
    pub_pools: Vec<Arc<Vec<String>>>,
    /// Cursor into the pool — how many events each pub has already sent.
    pub_cursor: usize,
    sub_tasks: Vec<JoinHandle<()>>,
    pub_drain_tasks: Vec<JoinHandle<()>>,
}

impl FanoutHarness {
    /// Spin up a relay, connect NUM_SUBS subscribers and NUM_PUBS publishers,
    /// send REQs and drain EOSEs. Pre-sign `total_iters * EVENTS_PER_ITER`
    /// events per publisher so every iteration has fresh content (avoids
    /// nostr-relay's event-id dedup).
    fn new(rt: Runtime, relay: Relay, total_iters: u64) -> Self {
        let port = relay.port;
        let url = format!("ws://127.0.0.1:{port}");
        let delivered = Arc::new(AtomicUsize::new(0));
        let total_events_per_pub = (total_iters as usize) * EVENTS_PER_ITER;

        // Pre-sign all events upfront (untimed).
        let pub_pools: Vec<Arc<Vec<String>>> = (0..NUM_PUBS)
            .map(|i| presign_for(total_events_per_pub, &format!("pub{i}")))
            .collect();

        // Connect and subscribe all subs, then all pubs. Return pub sinks
        // and the background tasks that will keep receiving forever.
        let (sub_tasks, pub_sinks, pub_drain_tasks) =
            rt.block_on(connect_all(&url, delivered.clone()));

        Self {
            relay: Some(relay),
            rt,
            pub_sinks,
            delivered,
            pub_pools,
            pub_cursor: 0,
            sub_tasks,
            pub_drain_tasks,
        }
    }

    /// Run one timed iteration. Publish the next slice of events from each
    /// pub, wait for every subscriber to have received each event.
    /// Returns when `delivered` has caught up to the new target.
    fn iterate(&mut self) {
        let start_cursor = self.pub_cursor;
        let end_cursor = start_cursor + EVENTS_PER_ITER;
        // Deliveries from this iteration: NUM_SUBS × (NUM_PUBS × EVENTS_PER_ITER).
        let before = self.delivered.load(Ordering::Relaxed);
        let target = before + NUM_SUBS * NUM_PUBS * EVENTS_PER_ITER;

        // Publish all pubs in parallel so the client-side send is not a
        // serial bottleneck on the measurement.
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

        // Tight spin with periodic yield. Subscriber tasks run on tokio
        // workers, so blocking this thread with thread::sleep only adds
        // poll-grain latency without helping throughput.
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut spins: u32 = 0;
        loop {
            let n = self.delivered.load(Ordering::Relaxed);
            if n >= target {
                break;
            }
            spins = spins.wrapping_add(1);
            if spins & 0x3ff == 0 {
                if Instant::now() > deadline {
                    panic!(
                        "iter timed out: delivered={n} target={target} (pub_cursor={})",
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

impl Drop for FanoutHarness {
    fn drop(&mut self) {
        // Abort background tasks and close sinks before dropping the relay.
        for t in self.sub_tasks.drain(..) {
            t.abort();
        }
        for t in self.pub_drain_tasks.drain(..) {
            t.abort();
        }
        // Dropping pub_sinks closes their halves. Then drop the relay.
        self.pub_sinks.clear();
        drop(self.relay.take());
    }
}

async fn connect_all(
    url: &str,
    delivered: Arc<AtomicUsize>,
) -> (Vec<JoinHandle<()>>, Vec<WsSink>, Vec<JoinHandle<()>>) {
    // Subscribers: connect, send REQ, wait for EOSE. Keep the reader half
    // running forever incrementing `delivered` on every EVENT frame.
    let mut sub_tasks = Vec::with_capacity(NUM_SUBS);
    let mut eose_rxs = Vec::with_capacity(NUM_SUBS);
    for i in 0..NUM_SUBS {
        let url = url.to_string();
        let delivered = delivered.clone();
        let (eose_tx, eose_rx) = mpsc::unbounded_channel::<()>();
        eose_rxs.push(eose_rx);
        sub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("sub connect");
            let (mut write, mut read) = ws.split();
            let req = format!(r#"["REQ","s{i}",{{"kinds":[1]}}]"#);
            write.send(Message::Text(req.into())).await.expect("REQ");
            // Forever: drain. First text frame is EOSE; signal and keep going.
            let mut eose_sent = false;
            loop {
                match read.next().await {
                    Some(Ok(Message::Text(t))) => {
                        if !eose_sent && t.starts_with("[\"EOSE\"") {
                            let _ = eose_tx.send(());
                            eose_sent = true;
                        } else if t.starts_with("[\"EVENT\"") {
                            delivered.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
            drop(write);
        }));
    }

    // Wait for every sub's EOSE before proceeding.
    for mut rx in eose_rxs {
        let _ = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("EOSE timeout");
    }

    // Publishers: connect. Split; hand the sink back to the harness, spawn
    // a drain task for the reader half so OK acks don't back up.
    let mut pub_sinks = Vec::with_capacity(NUM_PUBS);
    let mut pub_drain_tasks = Vec::with_capacity(NUM_PUBS);
    for _ in 0..NUM_PUBS {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("pub connect");
        let (write, mut read) = ws.split();
        pub_sinks.push(write);
        pub_drain_tasks.push(tokio::spawn(async move {
            while let Some(Ok(_)) = read.next().await {
                // Drain OKs silently; they're not part of the measurement.
            }
        }));
    }

    (sub_tasks, pub_sinks, pub_drain_tasks)
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_fanout");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    // Each iteration delivers NUM_SUBS × (NUM_PUBS × EVENTS_PER_ITER) events.
    let deliveries_per_iter = NUM_SUBS * NUM_PUBS * EVENTS_PER_ITER;
    group.throughput(Throughput::Elements(deliveries_per_iter as u64));

    for &workers in &[1usize, 2, 4] {
        let max_clients = NUM_SUBS + NUM_PUBS + 8;

        group.bench_with_input(BenchmarkId::new("ring", workers), &workers, |b, &w| {
            b.iter_custom(|iters| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(6)
                    .enable_all()
                    .build()
                    .unwrap();
                let relay = Relay::spawn_ring(w, max_clients);
                let mut h = FanoutHarness::new(rt, relay, iters);
                // Timed region: iters × fan-out cycles.
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
                        .worker_threads(6)
                        .enable_all()
                        .build()
                        .unwrap();
                    let relay = Relay::spawn_nostr_relay(w);
                    let mut h = FanoutHarness::new(rt, relay, iters);
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
