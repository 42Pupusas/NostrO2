//! Benchmark: connection storm — how fast can the server accept N connections.
//!
//! Measures time to connect + handshake N clients and receive all Connected events.
//! Compares ring-relay-server vs tokio-tungstenite server.

use criterion::{Criterion, criterion_group, criterion_main};
use std::time::{Duration, Instant};

const NUM_CLIENTS: usize = 500;

type WsClient = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

fn connect_tungstenite(port: u16) -> WsClient {
    let url = format!("ws://127.0.0.1:{port}");
    let (ws, _) = tungstenite::connect(&url).expect("connect failed");
    ws
}

// ── Ring relay server ──────────────────────────────────────────────────

fn bench_ring_connections(c: &mut Criterion) {
    let mut group = c.benchmark_group("connection_storm");
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

                let start = Instant::now();

                // Connect all clients in parallel threads
                let mut handles = Vec::new();
                for _ in 0..NUM_CLIENTS {
                    handles.push(std::thread::spawn(move || connect_tungstenite(port)));
                }
                let mut clients: Vec<_> = handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect();

                // Wait for all Connected events
                for _ in 0..NUM_CLIENTS {
                    match server.recv() {
                        ring_relay_server::ClientMessage::Connected { .. } => {}
                        other => panic!("expected Connected, got {other:?}"),
                    }
                }

                total += start.elapsed();

                let rate = NUM_CLIENTS as f64 / total.as_secs_f64();
                println!(
                    "ring: {NUM_CLIENTS} connections in {total:.2?} ({rate:.0} conn/s)"
                );

                for c in clients.iter_mut() {
                    let _ = c.close(None);
                }
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
                let connected = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

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
                                            let ws = tokio_tungstenite::accept_async(stream)
                                                .await
                                                .unwrap();
                                            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                            let (_write, mut read) = futures_util::StreamExt::split(ws);
                                            while let Some(Ok(_)) = futures_util::StreamExt::next(&mut read).await {}
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

                // Connect all clients in parallel threads
                let mut handles = Vec::new();
                for _ in 0..NUM_CLIENTS {
                    handles.push(std::thread::spawn(move || connect_tungstenite(port)));
                }
                let mut clients: Vec<_> = handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect();

                // Wait for all connections to be accepted
                while connected.load(std::sync::atomic::Ordering::Relaxed) < NUM_CLIENTS {
                    std::thread::yield_now();
                }

                total += start.elapsed();

                let rate = NUM_CLIENTS as f64 / total.as_secs_f64();
                println!(
                    "tokio: {NUM_CLIENTS} connections in {total:.2?} ({rate:.0} conn/s)"
                );

                for c in clients.iter_mut() {
                    let _ = c.close(None);
                }
                let _ = shutdown_tx.send(());
            }

            total
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ring_connections, bench_tokio_connections);
criterion_main!(benches);
