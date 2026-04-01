//! Benchmark: connection storm — how fast can the server accept N connections.
//!
//! Measures time to connect + handshake N clients.
//! Compares ring-relay-server vs tokio-tungstenite server.

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const NUM_CLIENTS: usize = 500;

/// Connect N clients concurrently via tokio-tungstenite. Returns count of successful connects.
async fn connect_all(port: u16, num: usize) -> usize {
    let connected = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();

    for _ in 0..num {
        let count = connected.clone();
        tasks.push(tokio::spawn(async move {
            let url = format!("ws://127.0.0.1:{port}");
            if tokio_tungstenite::connect_async(&url).await.is_ok() {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for t in tasks {
        let _ = t.await;
    }

    connected.load(Ordering::Relaxed)
}

// ── Ring relay server ──────────────────────────────────────────────────

fn bench_ring_connections(c: &mut Criterion) {
    let mut group = c.benchmark_group("connection_storm");
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

                let start = Instant::now();

                let connected = rt.block_on(connect_all(port, NUM_CLIENTS));

                // Wait for all Connected events
                for _ in 0..connected {
                    server.recv();
                }

                total += start.elapsed();

                let rate = connected as f64 / total.as_secs_f64();
                println!("ring: {connected}/{NUM_CLIENTS} connections in {total:.2?} ({rate:.0} conn/s)");
            }

            total
        });
    });

    group.finish();
}

// ── Tokio-tungstenite server ───────────────────────────────────────────

fn bench_tokio_connections(c: &mut Criterion) {
    let mut group = c.benchmark_group("connection_storm");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("tokio_tungstenite_server", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let connected = Arc::new(AtomicUsize::new(0));

                let (port, shutdown_tx) = rt.block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let (stx, mut srx) = tokio::sync::oneshot::channel::<()>();

                    let conn_count = connected.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                accepted = listener.accept() => {
                                    if let Ok((stream, _)) = accepted {
                                        let count = conn_count.clone();
                                        tokio::spawn(async move {
                                            if tokio_tungstenite::accept_async(stream).await.is_ok() {
                                                count.fetch_add(1, Ordering::Relaxed);
                                            }
                                        });
                                    }
                                }
                                _ = &mut srx => break,
                            }
                        }
                    });

                    (port, stx)
                });

                let start = Instant::now();

                let client_connected = rt.block_on(connect_all(port, NUM_CLIENTS));

                // Wait for server to finish accepting all
                while connected.load(Ordering::Relaxed) < client_connected {
                    std::thread::yield_now();
                }

                total += start.elapsed();

                let got = connected.load(Ordering::Relaxed);
                let rate = got as f64 / total.as_secs_f64();
                println!("tokio: {got}/{NUM_CLIENTS} connections in {total:.2?} ({rate:.0} conn/s)");

                let _ = shutdown_tx.send(());
            }

            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ring_connections, bench_tokio_connections);
criterion_main!(benches);
