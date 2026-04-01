//! Local throughput + memory benchmark with RSS sampling.
//!
//! Connects to local Nostr relays through Caddy TLS proxy, receives 100K
//! pre-generated events per relay, measures throughput and samples RSS
//! every 100ms to show memory behavior over time.
//!
//! Prerequisites:
//!   1. cargo run -p ring-relay-client --example local_server
//!   2. caddy run --config ring-relay-client/examples/Caddyfile
//!   3. sudo modprobe tls  (for kTLS)
//!
//! Run: cargo run -p ring-relay-client --example local_bench --release

use nostro2::NostrRelayEvent;
use ring_relay_client::{PoolMessage, RelayPool};
use std::time::{Duration, Instant};

const NUM_RELAYS: usize = 24;
const BASE_PORT: u16 = 10900;
const EVENTS_PER_RELAY: usize = 500_000;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

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
    mem_before_kb: usize,
    mem_samples: Vec<(Duration, usize)>, // (elapsed, rss_kb)
}

/// Read VmRSS (resident set size) from /proc/self/status in KB.
fn rss_kb() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let trimmed = rest.trim().strip_suffix("kB").unwrap_or(rest).trim();
            return trimmed.parse().unwrap_or(0);
        }
    }
    0
}

fn fmt_mem(kb: usize) -> String {
    if kb >= 1024 {
        format!("{:.1} MB", kb as f64 / 1024.0)
    } else {
        format!("{kb} KB")
    }
}

// ── Ring Relay (kTLS + io_uring) ─────────────────────────────────────

fn bench_ring_relay() -> BenchResult {
    let urls = relay_urls();
    println!(
        "=== Ring Relay (kTLS + io_uring) — {} connections ===",
        urls.len()
    );

    let mem_before = rss_kb();

    let mut pool = RelayPool::new(1_048_576, 2_000_000, 1024, urls.len());
    let sender = pool.sender();
    println!("  Reader threads: {}", pool.reader_thread_count());
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
    ring_relay_client::recv_stats_reset(); // clear counters

    let expected = EVENTS_PER_RELAY * connected;
    let (events_received, elapsed, rate, mem_samples) =
        drain_ring_events(expected, mem_before, || pool.try_recv());

    // Print recv stats
    let (recv_bytes, recv_count, _recv_drops) = ring_relay_client::recv_stats_reset();
    let avg_recv = if recv_count > 0 { recv_bytes / recv_count } else { 0 };
    let mbps = recv_bytes as f64 / elapsed.as_secs_f64() / 1_000_000.0;
    println!("  I/O stats: {recv_count} recvs, {recv_bytes} bytes total");
    println!("  Avg {avg_recv} bytes/recv, {mbps:.0} MB/s wire throughput\n");

    std::thread::spawn(move || drop(pool));
    std::thread::sleep(Duration::from_millis(200));

    BenchResult {
        label: "Ring Relay",
        events_received,
        elapsed,
        rate,
        mem_before_kb: mem_before,
        mem_samples,
    }
}

// ── Async Relay (tokio / nostro2-relay) ──────────────────────────────

fn bench_async_relay() -> BenchResult {
    let urls = relay_urls();
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
    println!("=== Async Relay (tokio) — {} connections ===", urls.len());

    let mem_before = rss_kb();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (events_received, elapsed, rate, mem_samples) = rt.block_on(async {
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
        let mut last_sample = Instant::now();
        let mut mem_samples = Vec::new();

        mem_samples.push((Duration::ZERO, rss_kb()));

        loop {
            match pool.recv().await {
                Some(NostrRelayEvent::NewNote(..)) => {
                    count += 1;
                    if last_sample.elapsed() >= SAMPLE_INTERVAL {
                        mem_samples.push((start.elapsed(), rss_kb()));
                        last_sample = Instant::now();
                    }
                    if last_print.elapsed() >= Duration::from_secs(1) {
                        let rate = count as f64 / start.elapsed().as_secs_f64();
                        println!(
                            "  [{:.1}s] {count}/{expected} events ({rate:.0}/s) RSS: {}",
                            start.elapsed().as_secs_f64(),
                            fmt_mem(rss_kb()),
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
        mem_samples.push((elapsed, rss_kb()));
        let rate = count as f64 / elapsed.as_secs_f64();
        println!("  Done: {count} events in {elapsed:.2?} ({rate:.0} events/s)\n");
        (count, elapsed, rate, mem_samples)
    });

    drop(rt);
    std::thread::sleep(Duration::from_millis(200));

    BenchResult {
        label: "Async Relay",
        events_received,
        elapsed,
        rate,
        mem_before_kb: mem_before,
        mem_samples,
    }
}

// ── Ring relay event drain with RSS sampling ─────────────────────────

fn drain_ring_events(
    expected: usize,
    mem_before: usize,
    mut try_recv: impl FnMut() -> Option<PoolMessage>,
) -> (usize, Duration, f64, Vec<(Duration, usize)>) {
    let start = Instant::now();
    let mut count = 0usize;
    let mut eose_count = 0usize;
    let mut last_print = Instant::now();
    let mut last_sample = Instant::now();
    let mut mem_samples = vec![(Duration::ZERO, mem_before)];
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
                if last_sample.elapsed() >= SAMPLE_INTERVAL {
                    mem_samples.push((start.elapsed(), rss_kb()));
                    last_sample = Instant::now();
                }
                if last_print.elapsed() >= Duration::from_secs(1) {
                    let rate = count as f64 / start.elapsed().as_secs_f64();
                    println!(
                        "  [{:.1}s] {count}/{expected} events ({rate:.0}/s) RSS: {}",
                        start.elapsed().as_secs_f64(),
                        fmt_mem(rss_kb()),
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
    mem_samples.push((elapsed, rss_kb()));
    let rate = count as f64 / elapsed.as_secs_f64();
    println!("  Done: {count} events in {elapsed:.2?} ({rate:.0} events/s) [{eose_count} EOSE]\n");

    (count, elapsed, rate, mem_samples)
}

// ── Main ─────────────────────────────────────────────────────────────

fn main() {
    println!("=== Local Throughput + Memory Benchmark ===");
    println!(
        "Relays: {NUM_RELAYS} x {EVENTS_PER_RELAY} events = {} total per test",
        NUM_RELAYS * EVENTS_PER_RELAY
    );
    println!("Baseline RSS: {}\n", fmt_mem(rss_kb()));

    let mut results: Vec<BenchResult> = Vec::new();

    results.push(bench_async_relay());
    std::thread::sleep(Duration::from_secs(2));

    results.push(bench_ring_relay());

    // Summary table
    println!("{:=^76}", "");
    println!("                          RESULTS");
    println!("{:=^76}", "");
    println!(
        "{:<15} | {:>10} | {:>10} | {:>12} | {:>10} | {:>10}",
        "Implementation", "Events", "Time", "Rate", "RSS +/-", "Peak RSS"
    );
    println!(
        "{:->15}-+-{:->10}-+-{:->10}-+-{:->12}-+-{:->10}-+-{:->10}",
        "", "", "", "", "", ""
    );
    for r in &results {
        let peak = r.mem_samples.iter().map(|(_, kb)| *kb).max().unwrap_or(0);
        let delta = peak as isize - r.mem_before_kb as isize;
        let delta_str = if delta >= 0 {
            format!("+{}", fmt_mem(delta as usize))
        } else {
            format!("-{}", fmt_mem((-delta) as usize))
        };
        println!(
            "{:<15} | {:>10} | {:>10.2?} | {:>10.0}/s | {:>10} | {:>10}",
            r.label,
            r.events_received,
            r.elapsed,
            r.rate,
            delta_str,
            fmt_mem(peak),
        );
    }

    // Speedup
    if results.len() >= 2 {
        println!();
        let fastest = results
            .iter()
            .max_by(|a, b| a.rate.partial_cmp(&b.rate).unwrap())
            .unwrap();
        for r in &results {
            if std::ptr::eq(r, fastest) {
                println!("  {}: fastest", r.label);
            } else if r.rate > 0.0 {
                println!("  {}: {:.2}x slower", r.label, fastest.rate / r.rate);
            }
        }
    }

    // Memory timeline
    println!("\n{:=^76}", "");
    println!("                     MEMORY TIMELINE");
    println!("{:=^76}", "");
    for r in &results {
        println!("\n  {}:", r.label);
        println!("  {:>8} | {:>10}", "Time", "RSS");
        println!("  {:->8}-+-{:->10}", "", "");
        for (elapsed, kb) in &r.mem_samples {
            println!("  {:>7.1}s | {:>10}", elapsed.as_secs_f64(), fmt_mem(*kb));
        }
    }
}
