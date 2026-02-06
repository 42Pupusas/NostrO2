use nostro2::NostrRelayEvent;
use nostro2_ring_relay::{PoolMessage, RelayPool};
use nostro2_signer::NostrKeypair;
use std::time::{Duration, Instant};

const TEST_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://relay.illuminodes.com",
    "wss://nos.lol",
    "wss://nostr.wine",
    "wss://relay.nostr.band",
    "wss://nostr.mom",
    "wss://relay.snort.social",
    "wss://nostr-pub.wellorder.net",
    "wss://relay.current.fyi",
];

/// Stats collected per reporting interval
struct IntervalStats {
    elapsed_secs: u64,
    received: usize,
    sent: usize,
    send_errors: usize,
    recv_rate: f64,
    send_rate: f64,
}

/// Aggregate results from a test run
struct TestResult {
    label: &'static str,
    duration: Duration,
    total_received: usize,
    total_sent: usize,
    total_send_errors: usize,
    avg_recv_rate: f64,
    avg_send_rate: f64,
    snapshots: Vec<IntervalStats>,
}

fn test_ring_relay() -> TestResult {
    println!("=== Ring Relay (lock-free) ===");

    let mut pool = RelayPool::new(4096, 10_000, 16384, TEST_RELAYS.len());
    let sender = pool.sender();

    println!("Connecting to {} relays...", TEST_RELAYS.len());
    for url in TEST_RELAYS {
        pool.add_relay(url.to_string());
    }

    std::thread::sleep(Duration::from_secs(2));

    let test_duration = Duration::from_secs(10);
    let report_interval = Duration::from_secs(1);
    let start = Instant::now();
    let mut last_report = Instant::now();

    let mut total_received: usize = 0;
    let mut total_sent: usize = 0;
    let mut total_send_errors: usize = 0;
    let mut interval_received: usize = 0;
    let mut interval_sent: usize = 0;
    let mut interval_send_errors: usize = 0;
    let mut snapshots: Vec<IntervalStats> = Vec::new();

    println!("Running for {}s...\n", test_duration.as_secs());

    while start.elapsed() < test_duration {
        match pool.try_recv() {
            Some(PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(_, _, ref note),
                ..
            }) => {
                total_received += 1;
                interval_received += 1;

                let throwaway = NostrKeypair::new();
                let mut echo = nostro2::NostrNote {
                    content: format!(
                        "echo:{}:{}",
                        note.id.as_deref().unwrap_or("?"),
                        total_received,
                    ),
                    kind: 21000,
                    ..Default::default()
                };

                if throwaway.sign_note(&mut echo).is_ok() {
                    match sender.send(echo) {
                        Ok(()) => {
                            total_sent += 1;
                            interval_sent += 1;
                        }
                        Err(_) => {
                            total_send_errors += 1;
                            interval_send_errors += 1;
                        }
                    }
                }
            }
            Some(PoolMessage::RelayEvent { .. }) => {
                total_received += 1;
                interval_received += 1;
            }
            Some(PoolMessage::ConnectionClosed { relay_url, error }) => {
                if let Some(err) = error {
                    eprintln!("  [ring closed] {}: {}", relay_url, err);
                }
            }
            None => {
                std::thread::sleep(Duration::from_micros(100));
            }
        }

        if last_report.elapsed() >= report_interval {
            let elapsed = start.elapsed().as_secs();
            let interval_secs = last_report.elapsed().as_secs_f64();
            let recv_rate = interval_received as f64 / interval_secs;
            let send_rate = interval_sent as f64 / interval_secs;

            snapshots.push(IntervalStats {
                elapsed_secs: elapsed,
                received: interval_received,
                sent: interval_sent,
                send_errors: interval_send_errors,
                recv_rate,
                send_rate,
            });

            println!(
                "[{:>2}s] recv: {:>5} ({:>6.1}/s) | sent: {:>5} ({:>6.1}/s) | errors: {}",
                elapsed, interval_received, recv_rate, interval_sent, send_rate, interval_send_errors,
            );

            interval_received = 0;
            interval_sent = 0;
            interval_send_errors = 0;
            last_report = Instant::now();
        }
    }

    let total_elapsed = start.elapsed();
    let avg_recv = total_received as f64 / total_elapsed.as_secs_f64();
    let avg_send = total_sent as f64 / total_elapsed.as_secs_f64();

    TestResult {
        label: "Ring Relay",
        duration: total_elapsed,
        total_received,
        total_sent,
        total_send_errors,
        avg_recv_rate: avg_recv,
        avg_send_rate: avg_send,
        snapshots,
    }
}

fn test_async_relay() -> TestResult {
    println!("\n=== Async Relay (tokio) ===");

    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let pool = nostro2_relay::NostrPool::new(TEST_RELAYS);

        let subscription = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(1000),
            ..Default::default()
        };
        pool.send(subscription).unwrap();

        println!("Connecting to {} relays...", TEST_RELAYS.len());
        tokio::time::sleep(Duration::from_secs(2)).await;

        let test_duration = Duration::from_secs(10);
        let report_interval = Duration::from_secs(1);
        let start = Instant::now();
        let mut last_report = Instant::now();

        let mut total_received: usize = 0;
        let mut total_sent: usize = 0;
        let mut total_send_errors: usize = 0;
        let mut interval_received: usize = 0;
        let mut interval_sent: usize = 0;
        let mut interval_send_errors: usize = 0;
        let mut snapshots: Vec<IntervalStats> = Vec::new();

        println!("Running for {}s...\n", test_duration.as_secs());

        while start.elapsed() < test_duration {
            // Use a short timeout so we can check elapsed and report intervals
            let recv_result = tokio::time::timeout(
                Duration::from_millis(10),
                pool.recv(),
            )
            .await;

            match recv_result {
                Ok(Some(NostrRelayEvent::NewNote(_, _, ref note))) => {
                    total_received += 1;
                    interval_received += 1;

                    let throwaway = NostrKeypair::new();
                    let mut echo = nostro2::NostrNote {
                        content: format!(
                            "echo:{}:{}",
                            note.id.as_deref().unwrap_or("?"),
                            total_received,
                        ),
                        kind: 21000,
                        ..Default::default()
                    };

                    if throwaway.sign_note(&mut echo).is_ok() {
                        match pool.send(echo) {
                            Ok(_) => {
                                total_sent += 1;
                                interval_sent += 1;
                            }
                            Err(_) => {
                                total_send_errors += 1;
                                interval_send_errors += 1;
                            }
                        }
                    }
                }
                Ok(Some(_)) => {
                    total_received += 1;
                    interval_received += 1;
                }
                Ok(None) => {
                    // Channel closed
                    break;
                }
                Err(_) => {
                    // Timeout — no event ready
                }
            }

            if last_report.elapsed() >= report_interval {
                let elapsed = start.elapsed().as_secs();
                let interval_secs = last_report.elapsed().as_secs_f64();
                let recv_rate = interval_received as f64 / interval_secs;
                let send_rate = interval_sent as f64 / interval_secs;

                snapshots.push(IntervalStats {
                    elapsed_secs: elapsed,
                    received: interval_received,
                    sent: interval_sent,
                    send_errors: interval_send_errors,
                    recv_rate,
                    send_rate,
                });

                println!(
                    "[{:>2}s] recv: {:>5} ({:>6.1}/s) | sent: {:>5} ({:>6.1}/s) | errors: {}",
                    elapsed, interval_received, recv_rate, interval_sent, send_rate, interval_send_errors,
                );

                interval_received = 0;
                interval_sent = 0;
                interval_send_errors = 0;
                last_report = Instant::now();
            }
        }

        let total_elapsed = start.elapsed();
        let avg_recv = total_received as f64 / total_elapsed.as_secs_f64();
        let avg_send = total_sent as f64 / total_elapsed.as_secs_f64();

        TestResult {
            label: "Async Relay",
            duration: total_elapsed,
            total_received,
            total_sent,
            total_send_errors,
            avg_recv_rate: avg_recv,
            avg_send_rate: avg_send,
            snapshots,
        }
    })
}

fn print_result(result: &TestResult) {
    println!("\n--- {} Interval Breakdown ---", result.label);
    println!(
        "{:>8} | {:>10} | {:>10} | {:>12} | {:>12} | {:>6}",
        "Time", "Received", "Sent", "Recv (ev/s)", "Send (ev/s)", "Errors"
    );
    println!(
        "{:->8}-+-{:->10}-+-{:->10}-+-{:->12}-+-{:->12}-+-{:->6}",
        "", "", "", "", "", ""
    );
    for s in &result.snapshots {
        println!(
            "{:>7}s | {:>10} | {:>10} | {:>12.1} | {:>12.1} | {:>6}",
            s.elapsed_secs, s.received, s.sent, s.recv_rate, s.send_rate, s.send_errors,
        );
    }
    println!(
        "{:->8}-+-{:->10}-+-{:->10}-+-{:->12}-+-{:->12}-+-{:->6}",
        "", "", "", "", "", ""
    );
    println!(
        "{:>8} | {:>10} | {:>10} | {:>12.1} | {:>12.1} | {:>6}",
        "TOTAL",
        result.total_received,
        result.total_sent,
        result.avg_recv_rate,
        result.avg_send_rate,
        result.total_send_errors,
    );
}

fn print_comparison(ring: &TestResult, async_: &TestResult) {
    println!("\n{:=^60}", "");
    println!("          COMPARISON: Ring vs Async Relay");
    println!("{:=^60}", "");
    println!(
        "{:<20} | {:>15} | {:>15}",
        "Metric", "Ring Relay", "Async Relay"
    );
    println!("{:->20}-+-{:->15}-+-{:->15}", "", "", "");

    println!(
        "{:<20} | {:>15?} | {:>15?}",
        "Duration", ring.duration, async_.duration,
    );
    println!(
        "{:<20} | {:>15} | {:>15}",
        "Total received", ring.total_received, async_.total_received,
    );
    println!(
        "{:<20} | {:>15} | {:>15}",
        "Total sent", ring.total_sent, async_.total_sent,
    );
    println!(
        "{:<20} | {:>15} | {:>15}",
        "Send errors", ring.total_send_errors, async_.total_send_errors,
    );
    println!(
        "{:<20} | {:>13.1}/s | {:>13.1}/s",
        "Avg recv rate", ring.avg_recv_rate, async_.avg_recv_rate,
    );
    println!(
        "{:<20} | {:>13.1}/s | {:>13.1}/s",
        "Avg send rate", ring.avg_send_rate, async_.avg_send_rate,
    );

    // Speedup calculations
    if async_.avg_recv_rate > 0.0 {
        let recv_speedup = ring.avg_recv_rate / async_.avg_recv_rate;
        println!(
            "\nRecv throughput:  Ring is {:.2}x {} async",
            if recv_speedup >= 1.0 { recv_speedup } else { 1.0 / recv_speedup },
            if recv_speedup >= 1.0 { "faster than" } else { "slower than" },
        );
    }
    if async_.avg_send_rate > 0.0 {
        let send_speedup = ring.avg_send_rate / async_.avg_send_rate;
        println!(
            "Send throughput:  Ring is {:.2}x {} async",
            if send_speedup >= 1.0 { send_speedup } else { 1.0 / send_speedup },
            if send_speedup >= 1.0 { "faster than" } else { "slower than" },
        );
    }
}

fn main() {
    println!("=== Bidirectional Relay Comparison ===");
    println!("Receive kind 1 notes, echo back kind 21000 (ephemeral) for each");
    println!("Kind 21000 = ephemeral (relays won't persist these)");
    println!("Relays: {}\n", TEST_RELAYS.len());

    // Phase 1: Ring relay
    let ring_result = test_ring_relay();
    print_result(&ring_result);

    // Brief pause between tests
    println!("\n--- Pausing 5s before async test ---\n");
    std::thread::sleep(Duration::from_secs(5));

    // Phase 2: Async relay
    let async_result = test_async_relay();
    print_result(&async_result);

    // Phase 3: Comparison
    print_comparison(&ring_result, &async_result);
}
