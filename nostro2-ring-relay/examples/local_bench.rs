//! Local throughput benchmark: measures pure I/O pipeline performance.
//!
//! Connects to a local Nostr relay through Caddy TLS proxy, receives 100K
//! pre-generated events, and measures wall-clock throughput for each implementation.
//!
//! Prerequisites:
//!   1. cargo run -p nostro2-ring-relay --example local_server
//!   2. caddy run --config nostro2-ring-relay/examples/Caddyfile
//!   3. sudo modprobe tls  (for kTLS)
//!
//! Run: cargo run -p nostro2-ring-relay --example local_bench --release

use nostro2::NostrRelayEvent;
use nostro2_ring_relay::{PoolMessage, RelayPool};
use std::time::{Duration, Instant};

const NUM_RELAYS: usize = 12;
const BASE_PORT: u16 = 10900;
const EVENTS_PER_RELAY: usize = 100_000;

fn relay_urls() -> Vec<String> {
    (0..NUM_RELAYS)
        .map(|i| format!("wss://localhost:{}", BASE_PORT + i as u16))
        .collect()
}

struct BenchResult {
    label: &'static str,
    events_received: usize,
    elapsed: Duration,
    rate: f64,
}

fn bench_ring_relay() -> BenchResult {
    let urls = relay_urls();
    println!(
        "=== Ring Relay (kTLS + io_uring) — {} connections ===",
        urls.len()
    );

    let mut pool = RelayPool::new(131072, 2_000_000, 1024, urls.len());
    let sender = pool.sender();
    let mut connected = 0;
    for url in &urls {
        match pool.add_relay(url.clone()) {
            Ok(()) => connected += 1,
            Err(e) => eprintln!("  connect failed: {url}: {e}"),
        }
    }
    println!("  Connected: {connected}/{}", urls.len());

    std::thread::sleep(Duration::from_millis(500));

    let subscription = nostro2::NostrSubscription {
        kinds: vec![1].into(),
        ..Default::default()
    };
    sender.send(subscription).unwrap();

    let expected = EVENTS_PER_RELAY * connected;
    let result = drain_events("Ring Relay", expected, || pool.try_recv());

    std::thread::spawn(move || drop(pool));
    std::thread::sleep(Duration::from_millis(200));

    result
}

fn bench_async_relay() -> BenchResult {
    let urls = relay_urls();
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
    println!("=== Async Relay (tokio) — {} connections ===", urls.len());

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = nostro2_relay::NostrPool::new(&url_refs);

        let subscription = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            ..Default::default()
        };
        pool.send(subscription).unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;

        let expected = EVENTS_PER_RELAY * urls.len();
        let start = Instant::now();
        let mut count = 0usize;
        let mut eose_count = 0usize;
        let mut last_print = Instant::now();

        loop {
            match pool.recv().await {
                Some(NostrRelayEvent::NewNote(..)) => {
                    count += 1;
                    if last_print.elapsed() >= Duration::from_secs(1) {
                        let rate = count as f64 / start.elapsed().as_secs_f64();
                        println!(
                            "  [{:.1}s] {count}/{expected} events ({rate:.0}/s)",
                            start.elapsed().as_secs_f64()
                        );
                        last_print = Instant::now();
                    }
                }
                Some(NostrRelayEvent::EndOfSubscription(..)) => {
                    eose_count += 1;
                    if eose_count >= urls.len() {
                        break;
                    }
                }
                None => break,
                _ => {}
            }
        }

        let elapsed = start.elapsed();
        let rate = count as f64 / elapsed.as_secs_f64();
        println!("  Done: {count} events in {elapsed:.2?} ({rate:.0} events/s)\n");

        BenchResult {
            label: "Async Relay",
            events_received: count,
            elapsed,
            rate,
        }
    })
}

/// Drain events from a try_recv function until all EOSE received or timeout.
fn drain_events(
    label: &'static str,
    expected: usize,
    mut try_recv: impl FnMut() -> Option<PoolMessage>,
) -> BenchResult {
    let start = Instant::now();
    let mut count = 0usize;
    let mut eose_count = 0usize;
    let mut last_print = Instant::now();
    let timeout = Duration::from_secs(60);

    loop {
        if start.elapsed() > timeout {
            eprintln!("  TIMEOUT after {timeout:?}");
            break;
        }

        match try_recv() {
            Some(PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(..),
                ..
            }) => {
                count += 1;
                if last_print.elapsed() >= Duration::from_secs(1) {
                    let rate = count as f64 / start.elapsed().as_secs_f64();
                    println!(
                        "  [{:.1}s] {count}/{expected} events ({rate:.0}/s)",
                        start.elapsed().as_secs_f64()
                    );
                    last_print = Instant::now();
                }
            }
            Some(PoolMessage::RelayEvent {
                event: NostrRelayEvent::EndOfSubscription(..),
                ..
            }) => {
                eose_count += 1;
                if eose_count >= NUM_RELAYS {
                    break;
                }
            }
            Some(PoolMessage::ConnectionClosed { error, .. }) => {
                if let Some(e) = &error {
                    eprintln!("  connection closed: {e}");
                }
            }
            _ => {
                std::hint::spin_loop();
            }
        }
    }

    let elapsed = start.elapsed();
    let rate = count as f64 / elapsed.as_secs_f64();
    println!("  Done: {count} events in {elapsed:.2?} ({rate:.0} events/s) [{eose_count} EOSE]\n");

    BenchResult {
        label,
        events_received: count,
        elapsed,
        rate,
    }
}

fn main() {
    println!("=== Local Throughput Benchmark ===");
    println!(
        "Relays: {NUM_RELAYS} x {EVENTS_PER_RELAY} events = {} total per test\n",
        NUM_RELAYS * EVENTS_PER_RELAY
    );

    let mut results: Vec<BenchResult> = Vec::new();

    results.push(bench_async_relay());
    std::thread::sleep(Duration::from_secs(2));

    results.push(bench_ring_relay());

    // Summary
    println!("{:=^60}", "");
    println!("                    RESULTS");
    println!("{:=^60}", "");
    println!(
        "{:<15} | {:>10} | {:>12} | {:>12}",
        "Implementation", "Events", "Time", "Rate"
    );
    println!("{:->15}-+-{:->10}-+-{:->12}-+-{:->12}", "", "", "", "");
    for r in &results {
        println!(
            "{:<15} | {:>10} | {:>10.2?} | {:>10.0}/s",
            r.label, r.events_received, r.elapsed, r.rate,
        );
    }

    if results.len() >= 2 {
        let fastest = results
            .iter()
            .max_by(|a, b| a.rate.partial_cmp(&b.rate).unwrap())
            .unwrap();
        let slowest = results
            .iter()
            .min_by(|a, b| a.rate.partial_cmp(&b.rate).unwrap())
            .unwrap();
        if slowest.rate > 0.0 {
            println!(
                "\n{} is {:.2}x faster than {}",
                fastest.label,
                fastest.rate / slowest.rate,
                slowest.label,
            );
        }
    }
}
