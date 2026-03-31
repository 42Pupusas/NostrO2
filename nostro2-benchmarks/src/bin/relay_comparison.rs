use std::time::{Duration, Instant};

const RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://nos.lol",
    "wss://relay.nostr.band",
    "wss://relay.snort.social",
    "wss://offchain.pub",
    "wss://nostr.mom",
    "wss://relay.mostr.pub",
    "wss://nostr-pub.wellorder.net",
    "wss://relay.illuminodes.com",
    "wss://freespeech.casa",
    "wss://nostr.0x7e.xyz",
];

const NOTE_LIMIT: usize = 1000;
const TIMEOUT: Duration = Duration::from_secs(15);
const N_RUNS: usize = 3;
/// Stop waiting after this many EOSE (some relays may be slow/unresponsive)
const MIN_EOSE: usize = 10;

// ── Result types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct RunResult {
    total_time: Duration,
    first_note_time: Option<Duration>,
    note_count: usize,
    eose_count: usize,
}

// ── nostro2-relay (async/tokio) ────────────────────────────────────

async fn run_nostro2() -> RunResult {
    let start = Instant::now();
    let pool = nostro2_relay::NostrPool::new(RELAYS);

    let filter = nostro2_relay::nostro2::NostrSubscription {
        kinds: Some(vec![1]),
        limit: Some(NOTE_LIMIT as u32),
        ..Default::default()
    };
    pool.send(&filter).expect("Failed to send filter");

    let mut note_count = 0_usize;
    let mut eose_count = 0_usize;
    let mut first_note_time: Option<Duration> = None;

    let deadline = start + TIMEOUT;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let msg = tokio::time::timeout(remaining, pool.recv()).await;

        match msg {
            Ok(Some(nostro2_relay::nostro2::NostrRelayEvent::NewNote(..))) => {
                note_count += 1;
                if first_note_time.is_none() {
                    first_note_time = Some(start.elapsed());
                }
            }
            Ok(Some(nostro2_relay::nostro2::NostrRelayEvent::EndOfSubscription(..))) => {
                eose_count += 1;
                if eose_count >= MIN_EOSE {
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }

    RunResult {
        total_time: start.elapsed(),
        first_note_time,
        note_count,
        eose_count,
    }
}

// ── nostro2-ring-relay (kTLS + io_uring) ───────────────────────────

async fn run_ring_relay() -> RunResult {
    // Ring relay uses blocking I/O, run in a blocking thread
    tokio::task::spawn_blocking(|| {
        let start = Instant::now();

        let mut pool = nostro2_ring_relay::RelayPool::new(
            4096,         // ring capacity
            10_000,       // dedup cache size
            64,           // broadcast capacity
            RELAYS.len() + 2, // max relays
        );

        for url in RELAYS {
            if let Err(e) = pool.add_relay(url.to_string()) {
                eprintln!("  ring-relay connect failed: {url}: {e}");
            }
        }

        // Send subscription
        let filter = nostro2::NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(NOTE_LIMIT as u32),
            ..Default::default()
        };
        pool.sender().send(filter).expect("Failed to send filter");

        let mut note_count = 0_usize;
        let mut eose_count = 0_usize;
        let mut first_note_time: Option<Duration> = None;

        let deadline = start + TIMEOUT;

        loop {
            if Instant::now() >= deadline {
                break;
            }

            match pool.try_recv() {
                Some(nostro2_ring_relay::PoolMessage::RelayEvent { event, .. }) => match event {
                    nostro2::NostrRelayEvent::NewNote(..) => {
                        note_count += 1;
                        if first_note_time.is_none() {
                            first_note_time = Some(start.elapsed());
                        }
                    }
                    nostro2::NostrRelayEvent::EndOfSubscription(..) => {
                        eose_count += 1;
                        if eose_count >= MIN_EOSE {
                            break;
                        }
                    }
                    _ => {}
                },
                Some(nostro2_ring_relay::PoolMessage::ConnectionClosed { .. }) => {}
                None => {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }

        RunResult {
            total_time: start.elapsed(),
            first_note_time,
            note_count,
            eose_count,
        }
    })
    .await
    .expect("ring-relay task panicked")
}

// ── nostr-sdk ──────────────────────────────────────────────────────

async fn run_nostr_sdk() -> RunResult {
    use futures_util::StreamExt;
    use nostr_sdk::prelude::*;

    let start = Instant::now();

    let client = Client::default();
    for url in RELAYS {
        let _ = client.add_relay(*url).await;
    }
    client.connect().await;

    let filter = Filter::new().kind(Kind::TextNote).limit(NOTE_LIMIT);

    let mut note_count = 0_usize;
    let mut first_note_time: Option<Duration> = None;

    // Use stream_events_from for a fair comparison — we control when to stop
    match client
        .stream_events_from(RELAYS.iter().copied(), filter, TIMEOUT)
        .await
    {
        Ok(mut stream) => {
            while let Some(_event) = stream.next().await {
                note_count += 1;
                if first_note_time.is_none() {
                    first_note_time = Some(start.elapsed());
                }
            }
        }
        Err(e) => {
            eprintln!("  nostr-sdk error: {e}");
        }
    }

    let total_time = start.elapsed();
    let _ = client.disconnect().await;

    RunResult {
        total_time,
        first_note_time,
        note_count,
        eose_count: 0, // stream API doesn't expose per-relay EOSE
    }
}

// ── Reporting ──────────────────────────────────────────────────────

fn fmt_duration(d: Duration) -> String {
    if d.as_secs() >= 1 {
        format!("{:.2}s", d.as_secs_f64())
    } else {
        format!("{:.0}ms", d.as_millis() as f64)
    }
}

fn print_results(name: &str, results: &[RunResult]) {
    println!("\n  {name}:");
    println!(
        "  {:<8} {:>12} {:>14} {:>10} {:>8}",
        "Run", "Total", "First Note", "Notes", "EOSE"
    );
    println!("  {}", "-".repeat(56));

    for (i, r) in results.iter().enumerate() {
        let first = r
            .first_note_time
            .map(fmt_duration)
            .unwrap_or_else(|| "n/a".into());
        println!(
            "  {:<8} {:>12} {:>14} {:>10} {:>8}",
            format!("#{}", i + 1),
            fmt_duration(r.total_time),
            first,
            r.note_count,
            r.eose_count,
        );
    }

    let avg_total = results
        .iter()
        .map(|r| r.total_time.as_secs_f64())
        .sum::<f64>()
        / results.len() as f64;
    let avg_notes = results.iter().map(|r| r.note_count as f64).sum::<f64>() / results.len() as f64;
    println!("  {}", "-".repeat(56));
    println!(
        "  {:<8} {:>12} {:>14} {:>10.0}",
        "avg",
        fmt_duration(Duration::from_secs_f64(avg_total)),
        "",
        avg_notes,
    );
}

fn avg_time(results: &[RunResult]) -> f64 {
    results
        .iter()
        .map(|r| r.total_time.as_secs_f64())
        .sum::<f64>()
        / results.len() as f64
}

fn avg_notes(results: &[RunResult]) -> f64 {
    results.iter().map(|r| r.note_count as f64).sum::<f64>() / results.len() as f64
}

// ── Main ───────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    println!("==========================================================");
    println!("  Relay Pool Benchmark: 3-Way Comparison");
    println!("==========================================================");
    println!();
    println!("  Relays:     {}", RELAYS.len());
    println!("  Limit:      {} notes per relay", NOTE_LIMIT);
    println!("  Timeout:    {}s", TIMEOUT.as_secs());
    println!("  Runs:       {}", N_RUNS);
    // ── nostro2-ring-relay ──
    println!("\n  Running nostro2-ring-relay (kTLS + io_uring)...");
    let mut ring_results = Vec::with_capacity(N_RUNS);
    for i in 0..N_RUNS {
        eprint!("    run {}... ", i + 1);
        let result = run_ring_relay().await;
        eprintln!(
            "{} ({} notes)",
            fmt_duration(result.total_time),
            result.note_count
        );
        ring_results.push(result);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // ── nostro2-relay ──
    println!("\n  Running nostro2-relay (async/tokio)...");
    let mut nostro2_results = Vec::with_capacity(N_RUNS);
    for i in 0..N_RUNS {
        eprint!("    run {}... ", i + 1);
        let result = run_nostro2().await;
        eprintln!(
            "{} ({} notes)",
            fmt_duration(result.total_time),
            result.note_count
        );
        nostro2_results.push(result);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // ── nostr-sdk ──
    println!("\n  Running nostr-sdk...");
    let mut nostr_sdk_results = Vec::with_capacity(N_RUNS);
    for i in 0..N_RUNS {
        eprint!("    run {}... ", i + 1);
        let result = run_nostr_sdk().await;
        eprintln!(
            "{} ({} notes)",
            fmt_duration(result.total_time),
            result.note_count
        );
        nostr_sdk_results.push(result);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // ── Report ──
    println!("\n==========================================================");
    println!("  Results");
    println!("==========================================================");

    print_results("nostro2-relay (async)", &nostro2_results);
    print_results("nostro2-ring-relay (threads)", &ring_results);
    print_results("nostr-sdk", &nostr_sdk_results);

    // ── Summary ──
    let t_nostro2 = avg_time(&nostro2_results);
    let t_ring = avg_time(&ring_results);
    let t_sdk = avg_time(&nostr_sdk_results);
    let n_nostro2 = avg_notes(&nostro2_results);
    let n_ring = avg_notes(&ring_results);
    let n_sdk = avg_notes(&nostr_sdk_results);

    println!("\n==========================================================");
    println!("  Summary");
    println!("==========================================================");
    println!();
    println!(
        "  {:20} {:>14} {:>14} {:>14}",
        "", "nostro2", "ring-relay", "nostr-sdk"
    );
    println!("  {}", "-".repeat(64));
    println!(
        "  {:20} {:>14} {:>14} {:>14}",
        "Avg time",
        fmt_duration(Duration::from_secs_f64(t_nostro2)),
        fmt_duration(Duration::from_secs_f64(t_ring)),
        fmt_duration(Duration::from_secs_f64(t_sdk)),
    );
    println!(
        "  {:20} {:>14.0} {:>14.0} {:>14.0}",
        "Avg notes", n_nostro2, n_ring, n_sdk,
    );

    // Find the fastest
    let times = [
        ("nostro2-relay", t_nostro2),
        ("ring-relay", t_ring),
        ("nostr-sdk", t_sdk),
    ];
    let (fastest_name, fastest_time) = times
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();

    println!();
    for &(name, time) in &times {
        if name == *fastest_name {
            println!("  {name}: fastest");
        } else {
            println!("  {name}: {:.1}x slower", time / fastest_time);
        }
    }
    println!();
}
