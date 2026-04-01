//! Benchmark: echo throughput — N clients each send M messages, server echoes.
//!
//! Each client connects via tokio-tungstenite, splits into independent write
//! and read halves on separate tasks. Writer blasts as fast as it can, reader
//! drains echoes. This is full-duplex per client — realistic behavior.
//!
//! Compares ring-relay-server vs tokio-tungstenite server.

use criterion::{Criterion, criterion_group, criterion_main};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tungstenite::Message;

const NUM_CLIENTS: usize = 50;
const MSGS_PER_CLIENT: usize = 1_000;
const PAYLOAD: &str = "hello from the benchmark client, this is a typical short message";

/// Connect N clients, each splits into a writer task and reader task.
/// Returns when all messages have been echoed back.
async fn run_clients(port: u16) -> usize {
    let received = Arc::new(AtomicUsize::new(0));

    let mut tasks = Vec::new();
    for _ in 0..NUM_CLIENTS {
        let recv_count = received.clone();

        tasks.push(tokio::spawn(async move {
            let url = format!("ws://127.0.0.1:{port}");
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let (mut write, mut read) = ws.split();

            // Writer: blast all messages
            let write_task = tokio::spawn(async move {
                for _ in 0..MSGS_PER_CLIENT {
                    write.send(Message::Text(PAYLOAD.into())).await.unwrap();
                }
            });

            // Reader: drain all echoes
            let read_task = tokio::spawn(async move {
                let mut count = 0;
                while count < MSGS_PER_CLIENT {
                    match read.next().await {
                        Some(Ok(_)) => {
                            count += 1;
                            recv_count.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => break,
                    }
                }
            });

            let _ = tokio::join!(write_task, read_task);
        }));
    }

    for t in tasks {
        let _ = t.await;
    }

    received.load(Ordering::Relaxed)
}

// ── Ring relay server ──────────────────────────────────────────────────

fn bench_ring_echo(c: &mut Criterion) {
    let mut group = c.benchmark_group("echo_throughput");
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

                // Echo loop on a background thread
                let echo_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let echo_flag = echo_shutdown.clone();
                let echo_thread = std::thread::spawn(move || {
                    while !echo_flag.load(Ordering::Relaxed) {
                        match server.try_recv() {
                            Some(ring_relay_server::ClientMessage::Text {
                                client_id, text, ..
                            }) => {
                                let _ = sender.send_text(client_id, text);
                            }
                            Some(_) => {}
                            None => std::thread::yield_now(),
                        }
                    }
                });

                std::thread::sleep(Duration::from_millis(10));

                // ── Timed section ──
                let start = Instant::now();
                let msgs = rt.block_on(run_clients(port));
                total += start.elapsed();

                let rate = msgs as f64 / total.as_secs_f64();
                println!("ring: {msgs} echo roundtrips in {total:.2?} ({rate:.0} msg/s)");

                echo_shutdown.store(true, Ordering::Relaxed);
                let _ = echo_thread.join();
            }

            total
        });
    });

    group.finish();
}

// ── Tokio-tungstenite server ───────────────────────────────────────────

fn bench_tokio_echo(c: &mut Criterion) {
    let mut group = c.benchmark_group("echo_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("tokio_tungstenite_server", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let (port, shutdown_tx) = rt.block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                accepted = listener.accept() => {
                                    if let Ok((stream, _)) = accepted {
                                        tokio::spawn(async move {
                                            let ws = tokio_tungstenite::accept_async(stream)
                                                .await
                                                .unwrap();
                                            let (mut write, mut read) = ws.split();
                                            while let Some(Ok(msg)) = read.next().await {
                                                if msg.is_text() || msg.is_binary() {
                                                    if write.send(msg).await.is_err() {
                                                        break;
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                                _ = &mut rx => break,
                            }
                        }
                    });

                    (port, tx)
                });

                std::thread::sleep(Duration::from_millis(10));

                // ── Timed section ──
                let start = Instant::now();
                let msgs = rt.block_on(run_clients(port));
                total += start.elapsed();

                let rate = msgs as f64 / total.as_secs_f64();
                println!("tokio: {msgs} echo roundtrips in {total:.2?} ({rate:.0} msg/s)");

                let _ = shutdown_tx.send(());
            }

            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ring_echo, bench_tokio_echo);
criterion_main!(benches);
