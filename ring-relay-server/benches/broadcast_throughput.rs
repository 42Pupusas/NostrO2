//! Benchmark: broadcast throughput — server broadcasts M messages to N clients.
//!
//! Measures how fast the server can fan-out a single message to all clients.
//! Compares ring-relay-server vs tokio-tungstenite server.

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const NUM_CLIENTS: usize = 100;
const NUM_BROADCASTS: usize = 1_000;
const PAYLOAD: &str = "broadcast payload: a typical relay event JSON would go here, padding";

type WsClient = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

fn connect_tungstenite(port: u16) -> WsClient {
    let url = format!("ws://127.0.0.1:{port}");
    let (ws, _) = tungstenite::connect(&url).expect("connect failed");
    ws
}

// ── Ring relay server ──────────────────────────────────────────────────

fn bench_ring_broadcast(c: &mut Criterion) {
    let mut group = c.benchmark_group("broadcast_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("ring_relay_server", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let mut server =
                    ring_relay_server::WsServer::bind([127, 0, 0, 1], 0, NUM_CLIENTS * 2)
                        .expect("bind");
                let port = server.port();
                let sender = server.sender();

                // Connect all clients
                let mut clients: Vec<_> = (0..NUM_CLIENTS)
                    .map(|_| connect_tungstenite(port))
                    .collect();

                // Drain Connected events and register clients with writer
                for _ in 0..NUM_CLIENTS {
                    server.recv();
                }
                std::thread::sleep(Duration::from_millis(10));

                // Drain server events in background so the event ring doesn't fill
                let drain_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let drain_flag = drain_shutdown.clone();
                let drain_thread = std::thread::spawn(move || {
                    while !drain_flag.load(Ordering::Relaxed) {
                        server.try_recv();
                        std::thread::yield_now();
                    }
                });

                // ── Timed section ──
                let start = Instant::now();

                // Server broadcasts N messages
                for _ in 0..NUM_BROADCASTS {
                    sender.broadcast(PAYLOAD.to_string()).unwrap();
                }

                // Each client reads all broadcasts
                let received = Arc::new(AtomicUsize::new(0));
                let mut readers = Vec::new();
                for mut client in clients {
                    let recv_count = received.clone();
                    readers.push(std::thread::spawn(move || {
                        for _ in 0..NUM_BROADCASTS {
                            let _ = client.read().unwrap();
                            recv_count.fetch_add(1, Ordering::Relaxed);
                        }
                        client
                    }));
                }

                clients = Vec::new();
                for r in readers {
                    clients.push(r.join().unwrap());
                }

                total += start.elapsed();

                let total_msgs = NUM_CLIENTS * NUM_BROADCASTS;
                let rate = total_msgs as f64 / total.as_secs_f64();
                println!(
                    "ring: {NUM_BROADCASTS} broadcasts to {NUM_CLIENTS} clients = {total_msgs} deliveries in {total:.2?} ({rate:.0} msg/s)"
                );

                drain_shutdown.store(true, Ordering::Relaxed);
                for mut c in clients {
                    let _ = c.close(None);
                }
                let _ = drain_thread.join();
            }

            total
        });
    });

    group.finish();
}

// ── Tokio-tungstenite server ───────────────────────────────────────────

fn bench_tokio_broadcast(c: &mut Criterion) {
    let mut group = c.benchmark_group("broadcast_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("tokio_tungstenite_server", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let (port, broadcast_tx, shutdown_tx) = rt.block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let (btx, _) = tokio::sync::broadcast::channel::<String>(NUM_BROADCASTS * 2);
                    let (stx, mut srx) = tokio::sync::oneshot::channel::<()>();

                    let btx_clone = btx.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                accepted = listener.accept() => {
                                    if let Ok((stream, _)) = accepted {
                                        let mut rx = btx_clone.subscribe();
                                        tokio::spawn(async move {
                                            let ws = tokio_tungstenite::accept_async(stream)
                                                .await
                                                .unwrap();
                                            let (mut write, mut read) = futures_util::StreamExt::split(ws);

                                            // Forward broadcasts to this client
                                            let write_task = tokio::spawn(async move {
                                                while let Ok(msg) = rx.recv().await {
                                                    if futures_util::SinkExt::send(
                                                        &mut write,
                                                        tungstenite::Message::Text(msg.into()),
                                                    )
                                                    .await
                                                    .is_err()
                                                    {
                                                        break;
                                                    }
                                                }
                                            });

                                            // Drain reads
                                            while let Some(Ok(_)) = futures_util::StreamExt::next(&mut read).await {}
                                            write_task.abort();
                                        });
                                    }
                                }
                                _ = &mut srx => break,
                            }
                        }
                    });

                    (port, btx, stx)
                });

                std::thread::sleep(Duration::from_millis(10));

                let mut clients: Vec<_> = (0..NUM_CLIENTS)
                    .map(|_| connect_tungstenite(port))
                    .collect();

                std::thread::sleep(Duration::from_millis(50));

                // ── Timed section ──
                let start = Instant::now();

                // Broadcast N messages
                for _ in 0..NUM_BROADCASTS {
                    let _ = broadcast_tx.send(PAYLOAD.to_string());
                }

                // Each client reads all broadcasts
                let received = Arc::new(AtomicUsize::new(0));
                let mut readers = Vec::new();
                for mut client in clients {
                    let recv_count = received.clone();
                    readers.push(std::thread::spawn(move || {
                        for _ in 0..NUM_BROADCASTS {
                            let _ = client.read().unwrap();
                            recv_count.fetch_add(1, Ordering::Relaxed);
                        }
                        client
                    }));
                }

                clients = Vec::new();
                for r in readers {
                    clients.push(r.join().unwrap());
                }

                total += start.elapsed();

                let total_msgs = NUM_CLIENTS * NUM_BROADCASTS;
                let rate = total_msgs as f64 / total.as_secs_f64();
                println!(
                    "tokio: {NUM_BROADCASTS} broadcasts to {NUM_CLIENTS} clients = {total_msgs} deliveries in {total:.2?} ({rate:.0} msg/s)"
                );

                for mut c in clients {
                    let _ = c.close(None);
                }
                let _ = shutdown_tx.send(());
            }

            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ring_broadcast, bench_tokio_broadcast);
criterion_main!(benches);
