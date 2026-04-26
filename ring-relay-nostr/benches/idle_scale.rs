//! Idle-connection scaling bench: ring-relay-nostr vs nostr-relay 0.4.8.
//!
//! Measures fan-out throughput as the number of **idle** connected clients
//! grows. Idle clients connect over WebSocket, stay connected, and do
//! nothing — no REQ, no EVENT. They exist to pressure the I/O layer.
//!
//! The hypothesis being tested: io_uring should scale better on
//! concurrent-connection count than epoll-backed tokio, because fd count
//! in itself shouldn't cost anything at recv time — the ring only serves
//! fds that complete an operation. epoll pays a syscall + bookkeeping per
//! ready fd per round.
//!
//! Active cohort (kept tiny so idle clients dominate the comparison):
//!   - 4 publishers
//!   - 16 subscribers with an open filter (receive every event)
//!
//! Idle cohort (variable): 0, 1000, 5000, 10000 connected clients with no
//! subscription.
//!
//! Each timed iteration publishes 25 events per publisher and waits for
//! every active subscriber to receive all 100 events.

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
const EVENTS_PER_ITER: usize = 25;
const NUM_ACTIVE_SUBS: usize = 16;
const WORKERS: usize = 2;

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Harness with an active cohort (pubs + active subs) and an idle cohort.
struct IdleHarness {
    relay: Option<Relay>,
    rt: Runtime,
    pub_sinks: Vec<WsSink>,
    delivered: Arc<AtomicUsize>,
    pub_pools: Vec<Arc<Vec<String>>>,
    pub_cursor: usize,
    active_sub_tasks: Vec<JoinHandle<()>>,
    pub_drain_tasks: Vec<JoinHandle<()>>,
    idle_tasks: Vec<JoinHandle<()>>,
}

impl IdleHarness {
    fn new(rt: Runtime, relay: Relay, total_iters: u64, idle_count: usize) -> Self {
        let port = relay.port;
        let url = format!("ws://127.0.0.1:{port}");
        let delivered = Arc::new(AtomicUsize::new(0));
        let total_events_per_pub = (total_iters as usize) * EVENTS_PER_ITER;

        let pub_pools: Vec<Arc<Vec<String>>> = (0..NUM_PUBS)
            .map(|i| presign_for(total_events_per_pub, &format!("pub{i}")))
            .collect();

        // Idle clients first so the active cohort competes with them for
        // whatever per-fd work the relay does.
        let idle_tasks = rt.block_on(connect_idle(&url, idle_count));

        let (active_sub_tasks, pub_sinks, pub_drain_tasks) =
            rt.block_on(connect_active(&url, delivered.clone()));

        Self {
            relay: Some(relay),
            rt,
            pub_sinks,
            delivered,
            pub_pools,
            pub_cursor: 0,
            active_sub_tasks,
            pub_drain_tasks,
            idle_tasks,
        }
    }

    fn iterate(&mut self) {
        let start_cursor = self.pub_cursor;
        let end_cursor = start_cursor + EVENTS_PER_ITER;
        let before = self.delivered.load(Ordering::Relaxed);
        let target = before + NUM_ACTIVE_SUBS * NUM_PUBS * EVENTS_PER_ITER;

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

        let deadline = Instant::now() + Duration::from_secs(120);
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

impl Drop for IdleHarness {
    fn drop(&mut self) {
        for t in self.active_sub_tasks.drain(..) {
            t.abort();
        }
        for t in self.pub_drain_tasks.drain(..) {
            t.abort();
        }
        for t in self.idle_tasks.drain(..) {
            t.abort();
        }
        self.pub_sinks.clear();
        drop(self.relay.take());
    }
}

/// Open `count` WebSocket connections that stay silent forever. No REQ.
/// The relay should hold the fd but do no Nostr work per idle client.
///
/// We connect in batches to avoid SYN floods / accept backpressure.
async fn connect_idle(url: &str, count: usize) -> Vec<JoinHandle<()>> {
    let mut tasks = Vec::with_capacity(count);
    if count == 0 {
        return tasks;
    }

    const BATCH: usize = 256;
    for chunk_start in (0..count).step_by(BATCH) {
        let chunk_end = (chunk_start + BATCH).min(count);
        let mut batch_handles = Vec::with_capacity(chunk_end - chunk_start);
        for _ in chunk_start..chunk_end {
            let url = url.to_string();
            batch_handles.push(tokio::spawn(async move {
                match tokio_tungstenite::connect_async(&url).await {
                    Ok((ws, _)) => {
                        let (_write, mut read) = ws.split();
                        // Drain forever; we never send anything. An idle
                        // client should just sit here waiting for frames
                        // that never come (no REQ = no EVENT deliveries).
                        while let Some(Ok(_)) = read.next().await {}
                    }
                    Err(_) => {}
                }
            }));
        }
        tasks.extend(batch_handles);
        // Small pause so the relay can keep up with accept / HTTP upgrade.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Wait a beat so the last batch of connects is definitely established
    // before the bench's timed region starts.
    tokio::time::sleep(Duration::from_millis(200)).await;

    tasks
}

async fn connect_active(
    url: &str,
    delivered: Arc<AtomicUsize>,
) -> (Vec<JoinHandle<()>>, Vec<WsSink>, Vec<JoinHandle<()>>) {
    let mut active_sub_tasks = Vec::with_capacity(NUM_ACTIVE_SUBS);
    let mut eose_rxs = Vec::with_capacity(NUM_ACTIVE_SUBS);
    for i in 0..NUM_ACTIVE_SUBS {
        let url = url.to_string();
        let delivered = delivered.clone();
        let (eose_tx, eose_rx) = mpsc::unbounded_channel::<()>();
        eose_rxs.push(eose_rx);
        active_sub_tasks.push(tokio::spawn(async move {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("active sub connect");
            let (mut write, mut read) = ws.split();
            let req = format!(r#"["REQ","a{i}",{{"kinds":[1]}}]"#);
            write.send(Message::Text(req.into())).await.expect("REQ");
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
        }));
    }
    for mut rx in eose_rxs {
        let _ = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("active EOSE timeout");
    }

    let mut pub_sinks = Vec::with_capacity(NUM_PUBS);
    let mut pub_drain_tasks = Vec::with_capacity(NUM_PUBS);
    for _ in 0..NUM_PUBS {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("pub connect");
        let (write, mut read) = ws.split();
        pub_sinks.push(write);
        pub_drain_tasks.push(tokio::spawn(async move {
            while let Some(Ok(_)) = read.next().await {}
        }));
    }

    (active_sub_tasks, pub_sinks, pub_drain_tasks)
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("idle_scale");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    let deliveries_per_iter = NUM_ACTIVE_SUBS * NUM_PUBS * EVENTS_PER_ITER;
    group.throughput(Throughput::Elements(deliveries_per_iter as u64));

    for &idle in &[0usize, 1000, 5000, 10000] {
        let total_clients = idle + NUM_ACTIVE_SUBS + NUM_PUBS;
        // ring-relay-nostr's max_clients has to fit everyone + slack.
        let max_clients = total_clients + 64;

        group.bench_with_input(BenchmarkId::new("ring", idle), &idle, |b, &n| {
            b.iter_custom(|iters| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(6)
                    .enable_all()
                    .build()
                    .unwrap();
                let relay = Relay::spawn_ring(WORKERS, max_clients);
                let mut h = IdleHarness::new(rt, relay, iters, n);
                let start = Instant::now();
                for _ in 0..iters {
                    h.iterate();
                }
                let elapsed = start.elapsed();
                drop(h);
                elapsed
            });
        });

        group.bench_with_input(BenchmarkId::new("nostr_relay", idle), &idle, |b, &n| {
            b.iter_custom(|iters| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(6)
                    .enable_all()
                    .build()
                    .unwrap();
                let relay = Relay::spawn_nostr_relay(WORKERS);
                let mut h = IdleHarness::new(rt, relay, iters, n);
                let start = Instant::now();
                for _ in 0..iters {
                    h.iterate();
                }
                let elapsed = start.elapsed();
                drop(h);
                elapsed
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
