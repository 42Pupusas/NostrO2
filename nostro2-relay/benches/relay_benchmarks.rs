use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use nostro2::{NostrClientEvent, NostrNote};
use tokio::sync::mpsc;

/// Helper to create a sample note for benchmarking
fn create_sample_note(index: usize) -> NostrNote {
    NostrNote {
        id: Some(format!("event_{:016x}", index)),
        pubkey: "deadbeef".repeat(8),
        created_at: 1234567890,
        kind: 1,
        tags: vec![
            vec!["e".to_string(), format!("ref_{}", index)],
            vec!["p".to_string(), "pubkey".to_string()],
        ].into(),
        content: format!("Message number {}", index),
        sig: Some("signature".repeat(16)),
    }
}

fn bench_channel_throughput(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("channel_throughput");

    // Test unbounded MPSC channel (used in NostrRelay)
    for msg_count in [100, 1000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::new("unbounded_mpsc", msg_count), msg_count, |b, &msg_count| {
            b.to_async(&runtime).iter(|| async move {
                let (tx, mut rx) = mpsc::unbounded_channel();

                // Spawn sender
                let send_task = tokio::spawn(async move {
                    for i in 0..msg_count {
                        let note = create_sample_note(i);
                        let event: NostrClientEvent = note.into();
                        let json = serde_json::to_string(&event).unwrap();
                        tx.send(json).unwrap();
                    }
                });

                // Spawn receiver
                let recv_task = tokio::spawn(async move {
                    let mut count = 0;
                    while rx.recv().await.is_some() {
                        count += 1;
                        if count >= msg_count {
                            break;
                        }
                    }
                    count
                });

                send_task.await.unwrap();
                black_box(recv_task.await.unwrap())
            });
        });
    }

    // Test bounded MPSC channel for comparison
    for msg_count in [100, 1000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::new("bounded_mpsc_100", msg_count), msg_count, |b, &msg_count| {
            b.to_async(&runtime).iter(|| async move {
                let (tx, mut rx) = mpsc::channel(100);

                // Spawn sender and receiver concurrently
                let send_task = tokio::spawn(async move {
                    for i in 0..msg_count {
                        let note = create_sample_note(i);
                        let event: NostrClientEvent = note.into();
                        let json = serde_json::to_string(&event).unwrap();
                        tx.send(json).await.unwrap();
                    }
                });

                let recv_task = tokio::spawn(async move {
                    let mut count = 0;
                    while rx.recv().await.is_some() {
                        count += 1;
                        if count >= msg_count {
                            break;
                        }
                    }
                    count
                });

                send_task.await.unwrap();
                black_box(recv_task.await.unwrap())
            });
        });
    }

    group.finish();
}

fn bench_broadcast_channel(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("broadcast_channel");

    // Test broadcast channel (used in NostrPool for multi-relay broadcasting)
    for num_receivers in [1, 2, 4, 8].iter() {
        group.bench_with_input(BenchmarkId::new("receivers", num_receivers), num_receivers, |b, &num_receivers| {
            b.to_async(&runtime).iter(|| async move {
                let (tx, _) = tokio::sync::broadcast::channel(100);
                let msg_count = 1000;

                // Spawn multiple receivers
                let mut tasks = Vec::new();
                for _ in 0..num_receivers {
                    let mut rx = tx.subscribe();
                    tasks.push(tokio::spawn(async move {
                        let mut count = 0;
                        while rx.recv().await.is_ok() {
                            count += 1;
                            if count >= msg_count {
                                break;
                            }
                        }
                        count
                    }));
                }

                // Send messages
                let send_task = tokio::spawn(async move {
                    for i in 0..msg_count {
                        let note = create_sample_note(i);
                        let event: NostrClientEvent = note.into();
                        let _ = tx.send(event);
                    }
                });

                send_task.await.unwrap();

                // Wait for all receivers
                let mut total = 0;
                for task in tasks {
                    total += task.await.unwrap();
                }
                black_box(total)
            });
        });
    }

    group.finish();
}

fn bench_message_parsing_pipeline(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("message_pipeline");

    // Simulate the full relay pipeline: serialize -> send through channel -> deserialize
    for msg_count in [100, 1000, 5000].iter() {
        group.bench_with_input(BenchmarkId::new("full_pipeline", msg_count), msg_count, |b, &msg_count| {
            b.to_async(&runtime).iter(|| async move {
                let (tx, mut rx) = mpsc::unbounded_channel();

                // Sender: create notes, serialize, send
                let send_task = tokio::spawn(async move {
                    for i in 0..msg_count {
                        let note = create_sample_note(i);
                        let event: NostrClientEvent = note.into();
                        let json = serde_json::to_string(&event).unwrap();
                        tx.send(json).unwrap();
                    }
                });

                // Receiver: receive, deserialize
                let recv_task = tokio::spawn(async move {
                    let mut count = 0;
                    while let Some(json) = rx.recv().await {
                        let _event: NostrClientEvent = serde_json::from_str(&json).unwrap();
                        count += 1;
                        if count >= msg_count {
                            break;
                        }
                    }
                    count
                });

                send_task.await.unwrap();
                black_box(recv_task.await.unwrap())
            });
        });
    }

    group.finish();
}


fn bench_concurrent_relay_simulation(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_simulation");
    group.sample_size(20);

    // Simulate multiple concurrent clients sending to one relay
    for num_clients in [2, 4, 8].iter() {
        group.bench_with_input(BenchmarkId::new("clients", num_clients), num_clients, |b, &num_clients| {
            b.to_async(&runtime).iter(|| async move {
                let (tx, mut rx) = mpsc::unbounded_channel();

                // Spawn multiple client tasks
                let client_tasks: Vec<_> = (0..num_clients)
                    .map(|client_id| {
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            for i in 0..100 {
                                let note = create_sample_note(client_id * 100 + i);
                                let event: NostrClientEvent = note.into();
                                let json = serde_json::to_string(&event).unwrap();
                                tx.send(json).unwrap();
                            }
                        })
                    })
                    .collect();

                drop(tx); // Drop original sender

                // Wait for all clients to finish
                for task in client_tasks {
                    task.await.unwrap();
                }

                // Count received messages
                let mut count = 0;
                while rx.recv().await.is_some() {
                    count += 1;
                }

                black_box(count)
            });
        });
    }

    group.finish();
}

fn bench_note_content_sizes(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("note_sizes");

    // Test different note sizes through the pipeline
    for content_size in [100, 1000, 10_000, 50_000].iter() {
        group.bench_with_input(BenchmarkId::new("pipeline", content_size), content_size, |b, &content_size| {
            b.to_async(&runtime).iter(|| async move {
                let (tx, mut rx) = mpsc::unbounded_channel();

                let send_task = tokio::spawn(async move {
                    let content = "x".repeat(content_size);
                    let note = NostrNote {
                        id: Some("abc123".to_string()),
                        pubkey: "deadbeef".repeat(8),
                        created_at: 1234567890,
                        kind: 1,
                        tags: vec![vec!["e".to_string(), "ref".to_string()]].into(),
                        content,
                        sig: Some("sig".repeat(16)),
                    };

                    for _ in 0..100 {
                        let event: NostrClientEvent = note.clone().into();
                        let json = serde_json::to_string(&event).unwrap();
                        tx.send(json).unwrap();
                    }
                });

                let recv_task = tokio::spawn(async move {
                    let mut count = 0;
                    while let Some(json) = rx.recv().await {
                        let _event: NostrClientEvent = serde_json::from_str(&json).unwrap();
                        count += 1;
                        if count >= 100 {
                            break;
                        }
                    }
                    count
                });

                send_task.await.unwrap();
                black_box(recv_task.await.unwrap())
            });
        });
    }

    group.finish();
}

criterion_group!(
    relay_benches,
    bench_channel_throughput,
    bench_broadcast_channel,
    bench_message_parsing_pipeline,
    bench_concurrent_relay_simulation,
    bench_note_content_sizes,
);
criterion_main!(relay_benches);
