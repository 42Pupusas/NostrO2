use nostro2::NostrSigner;
use nostro2::NostrRelayEvent;
use ring_relay_client::{PoolMessage, RelayPool};
use nostro2_signer::K256Keypair;
use std::time::{Duration, Instant};

const TEST_RELAYS: &[&str] = &[
    "wss://relay.illuminodes.com",
    "wss://nos.lol",
    "wss://nostr.mom",
    "wss://relay.snort.social",
    "wss://nostr-pub.wellorder.net",
    "wss://relay.jerseyplebs.com",
    "wss://relay.primal.net",
    "wss://relay.bostr.shop",
    "wss://relay.albylabs.com",
    "wss://relay.bitcoindistrict.org",
    "wss://relay.nsite.run",
    "wss://git.shakespeare.diy",
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
    println!("=== Ring Relay (kTLS + io_uring) ===");

    let mut pool = RelayPool::new(4096, 10_000, 16384, TEST_RELAYS.len());
    let sender = pool.sender();

    println!("Connecting to {} relays...", TEST_RELAYS.len());
    let mut connected = 0;
    for url in TEST_RELAYS {
        match pool.add_relay(url.to_string()) {
            Ok(()) => connected += 1,
            Err(e) => eprintln!("  connect failed: {url}: {e}"),
        }
    }
    println!("  Connected: {connected}/{}", TEST_RELAYS.len());

    std::thread::sleep(Duration::from_secs(2));

    let subscription = nostro2::NostrSubscription {
        kinds: vec![1].into(),
        limit: Some(1000),
        ..Default::default()
    };
    sender.send(subscription).unwrap();

    let result = run_test("Ring Relay", &sender, || pool.try_recv());

    std::thread::spawn(move || drop(pool));
    std::thread::sleep(Duration::from_millis(500));

    result
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
            let recv_result = tokio::time::timeout(Duration::from_millis(10), pool.recv()).await;

            match recv_result {
                Ok(Some(NostrRelayEvent::NewNote(_, _, ref note))) => {
                    total_received += 1;
                    interval_received += 1;

                    let throwaway = K256Keypair::generate();
                    let mut echo = nostro2::NostrNote {
                        content: format!(
                            "echo:{}:{}",
                            note.id.as_deref().unwrap_or("?"),
                            total_received,
                        ),
                        kind: 21000,
                        ..Default::default()
                    };

                    if throwaway.sign_nostr_note(&mut echo).is_ok() {
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
                Ok(None) => break,
                Err(_) => {}
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
                    elapsed,
                    interval_received,
                    recv_rate,
                    interval_sent,
                    send_rate,
                    interval_send_errors,
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

/// Generic test runner for ring-buffer-based relays.
fn run_test(
    label: &'static str,
    sender: &ring_relay_client::PoolSender,
    mut try_recv: impl FnMut() -> Option<PoolMessage>,
) -> TestResult {
    run_test_raw(label, &mut try_recv, |note| {
        let throwaway = K256Keypair::generate();
        let mut echo = nostro2::NostrNote {
            content: format!("echo:{}:{}", note.id.as_deref().unwrap_or("?"), 0),
            kind: 21000,
            ..Default::default()
        };
        if throwaway.sign_nostr_note(&mut echo).is_ok() {
            return sender.send(echo).is_ok();
        }
        false
    })
}

fn run_test_raw(
    label: &'static str,
    mut try_recv: impl FnMut() -> Option<PoolMessage>,
    mut send_echo: impl FnMut(&nostro2::NostrNote) -> bool,
) -> TestResult {
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
        match try_recv() {
            Some(PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(_, _, ref note),
                ..
            }) => {
                total_received += 1;
                interval_received += 1;

                if send_echo(note) {
                    total_sent += 1;
                    interval_sent += 1;
                } else {
                    total_send_errors += 1;
                    interval_send_errors += 1;
                }
            }
            Some(PoolMessage::RelayEvent { .. }) => {
                total_received += 1;
                interval_received += 1;
            }
            Some(PoolMessage::ConnectionClosed { relay_url, error }) => {
                if let Some(err) = error {
                    eprintln!("  [{label} closed] {relay_url}: {err}");
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
                elapsed,
                interval_received,
                recv_rate,
                interval_sent,
                send_rate,
                interval_send_errors,
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
        label,
        duration: total_elapsed,
        total_received,
        total_sent,
        total_send_errors,
        avg_recv_rate: avg_recv,
        avg_send_rate: avg_send,
        snapshots,
    }
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

fn print_comparison(results: &[&TestResult]) {
    println!("\n{:=^80}", "");
    println!("                        COMPARISON");
    println!("{:=^80}", "");

    // Header
    print!("{:<20}", "Metric");
    for r in results {
        print!(" | {:>15}", r.label);
    }
    println!();
    print!("{:->20}", "");
    for _ in results {
        print!("-+-{:->15}", "");
    }
    println!();

    // Duration
    print!("{:<20}", "Duration");
    for r in results {
        print!(" | {:>15.1?}", r.duration);
    }
    println!();

    // Total received
    print!("{:<20}", "Total received");
    for r in results {
        print!(" | {:>15}", r.total_received);
    }
    println!();

    // Total sent
    print!("{:<20}", "Total sent");
    for r in results {
        print!(" | {:>15}", r.total_sent);
    }
    println!();

    // Send errors
    print!("{:<20}", "Send errors");
    for r in results {
        print!(" | {:>15}", r.total_send_errors);
    }
    println!();

    // Avg recv rate
    print!("{:<20}", "Avg recv rate");
    for r in results {
        print!(" | {:>13.1}/s", r.avg_recv_rate);
    }
    println!();

    // Avg send rate
    print!("{:<20}", "Avg send rate");
    for r in results {
        print!(" | {:>13.1}/s", r.avg_send_rate);
    }
    println!();

    // Speedup vs last (async)
    if results.len() >= 2 {
        let baseline = results.last().unwrap();
        println!();
        for r in &results[..results.len() - 1] {
            if baseline.avg_recv_rate > 0.0 {
                let speedup = r.avg_recv_rate / baseline.avg_recv_rate;
                println!(
                    "Recv: {} is {:.2}x {} {}",
                    r.label,
                    if speedup >= 1.0 {
                        speedup
                    } else {
                        1.0 / speedup
                    },
                    if speedup >= 1.0 {
                        "faster than"
                    } else {
                        "slower than"
                    },
                    baseline.label,
                );
            }
        }
    }
}

fn main() {
    println!("=== Bidirectional Relay Comparison ===");
    println!("Receive kind 1 notes, echo back kind 21000 (ephemeral) for each");
    println!("Relays: {}\n", TEST_RELAYS.len());

    let mut results: Vec<TestResult> = Vec::new();

    // Phase 1: Ring relay (kTLS + io_uring)
    let ring_result = test_ring_relay();
    print_result(&ring_result);
    results.push(ring_result);

    println!("\n--- Pausing 5s ---\n");
    std::thread::sleep(Duration::from_secs(5));

    // Phase 2: Async relay (tokio)
    let async_result = test_async_relay();
    print_result(&async_result);
    results.push(async_result);

    // Comparison
    let refs: Vec<&TestResult> = results.iter().collect();
    print_comparison(&refs);
}
