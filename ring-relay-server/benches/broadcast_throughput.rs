//! Benchmark: broadcast throughput — server broadcasts M messages to N clients.
//!
//! Measures how fast the server can fan-out a single message to all clients.
//! Compares ring-relay-server vs tokio-tungstenite server.

use criterion::{Criterion, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tungstenite::Message;

const NUM_CLIENTS: usize = 100;
const NUM_BROADCASTS: usize = 1_000;
const PAYLOAD: &str = "broadcast payload: a typical relay event JSON would go here, padding";

// ── Ring relay server ──────────────────────────────────────────────────

fn bench_ring_broadcast(c: &mut Criterion) {
    let mut group = c.benchmark_group("broadcast_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("ring_relay_server", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let mut server =
                    ring_relay_server::WsServer::bind([127, 0, 0, 1], 0, NUM_CLIENTS * 2)
                        .expect("bind");
                let port = server.port();
                let sender = server.sender();

                // Connect reader clients via tokio-tungstenite, hold their read halves
                let received = Arc::new(AtomicUsize::new(0));
                let mut reader_tasks = Vec::new();

                rt.block_on(async {
                    for _ in 0..NUM_CLIENTS {
                        let count = received.clone();
                        let url = format!("ws://127.0.0.1:{port}");
                        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
                        let (_write, mut read) = ws.split();
                        reader_tasks.push(tokio::spawn(async move {
                            for _ in 0..NUM_BROADCASTS {
                                if read.next().await.is_none() {
                                    break;
                                }
                                count.fetch_add(1, Ordering::Relaxed);
                            }
                        }));
                    }
                });

                // Drain Connected events — registers each client with the writer
                for _ in 0..NUM_CLIENTS {
                    server.recv();
                }

                // Drain further events in background
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

                for _ in 0..NUM_BROADCASTS {
                    sender.broadcast(PAYLOAD.to_string()).unwrap();
                }

                rt.block_on(async {
                    for t in reader_tasks {
                        let _ = t.await;
                    }
                });

                total += start.elapsed();

                let got = received.load(Ordering::Relaxed);
                let expected = NUM_CLIENTS * NUM_BROADCASTS;
                let rate = got as f64 / total.as_secs_f64();
                println!("ring: {got}/{expected} deliveries in {total:.2?} ({rate:.0} msg/s)");

                drain_shutdown.store(true, Ordering::Relaxed);
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
                    let (btx, _) =
                        tokio::sync::broadcast::channel::<String>(NUM_BROADCASTS * 2);
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
                                            let (mut write, mut read) = ws.split();
                                            let write_task = tokio::spawn(async move {
                                                while let Ok(msg) = rx.recv().await {
                                                    if write
                                                        .send(Message::Text(msg.into()))
                                                        .await
                                                        .is_err()
                                                    {
                                                        break;
                                                    }
                                                }
                                            });
                                            while let Some(Ok(_)) = read.next().await {}
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

                // Connect reader clients
                let received = Arc::new(AtomicUsize::new(0));
                let mut reader_tasks = Vec::new();

                rt.block_on(async {
                    for _ in 0..NUM_CLIENTS {
                        let count = received.clone();
                        let url = format!("ws://127.0.0.1:{port}");
                        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
                        let (_write, mut read) = ws.split();
                        reader_tasks.push(tokio::spawn(async move {
                            for _ in 0..NUM_BROADCASTS {
                                if read.next().await.is_none() {
                                    break;
                                }
                                count.fetch_add(1, Ordering::Relaxed);
                            }
                        }));
                    }

                    // Let all connections settle
                    tokio::time::sleep(Duration::from_millis(50)).await;
                });

                // ── Timed section ──
                let start = Instant::now();

                for _ in 0..NUM_BROADCASTS {
                    let _ = broadcast_tx.send(PAYLOAD.to_string());
                }

                rt.block_on(async {
                    for t in reader_tasks {
                        let _ = t.await;
                    }
                });

                total += start.elapsed();

                let got = received.load(Ordering::Relaxed);
                let expected = NUM_CLIENTS * NUM_BROADCASTS;
                let rate = got as f64 / total.as_secs_f64();
                println!("tokio: {got}/{expected} deliveries in {total:.2?} ({rate:.0} msg/s)");

                let _ = shutdown_tx.send(());
            }

            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ring_broadcast, bench_tokio_broadcast);
criterion_main!(benches);
