//! Full-process large-event ingest driver.
//!
//! Spawns `fanout_relay_server` as a subprocess (either `RELAY=ring` or
//! `RELAY=nostr`) so the relay runs in its own process with its own
//! allocator, threads, and kernel state — the same shape as a real
//! deployment. The driver connects N publishers over plain WebSocket and
//! injects pre-signed large events, counting OKs until every event has
//! round-tripped an ack.
//!
//! Reports wall-clock throughput in MB/s (uncompressed wire bytes) and
//! events/s so ring-relay-nostr vs nostr-relay 0.4.8 can be compared on
//! realistic large-payload ingest. Unlike the criterion bench this runs
//! outside the criterion measurement loop, so per-run startup cost is
//! amortized across thousands of events rather than repeated per sample.
//!
//! Env:
//!   RELAY=ring|nostr       (default: ring)
//!   LARGE_PUBS=<n>         (default: 4)
//!   LARGE_EVENTS=<n>       (default: 100 per publisher)
//!   LARGE_WARMUP=<n>       (default: 5 events per publisher, untimed)
//!   LARGE_CONTENT=<bytes>  (default: 393_216 ≈ 384 KiB, fits inside the
//!                           nostr-relay 0.4.8 512 KiB frame cap)
//!   LARGE_WORKERS=<n>      (default: 8, tokio client workers)
//!   RELAY_WORKERS=<n>      (default: 4, threads inside the relay)
//!   RELAY_BIN=<path>       (default: next to this exe, `fanout_relay_server`)

use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let relay_kind = std::env::var("RELAY").unwrap_or_else(|_| "ring".into());
    let num_pubs = env_usize("LARGE_PUBS", 4);
    let events_per_pub = env_usize("LARGE_EVENTS", 100);
    let warmup_per_pub = env_usize("LARGE_WARMUP", 5);
    let content_bytes = env_usize("LARGE_CONTENT", 384 * 1024);
    let client_workers = env_usize("LARGE_WORKERS", 8);
    let relay_workers = env_usize("RELAY_WORKERS", 4);
    let max_clients = num_pubs + 16;

    let bin = std::env::var("RELAY_BIN").unwrap_or_else(|_| {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop();
        p.push("fanout_relay_server");
        p.to_string_lossy().into_owned()
    });

    let mut cmd = Command::new(&bin);
    match relay_kind.as_str() {
        "ring" => {
            cmd.arg("ring")
                .arg(max_clients.to_string())
                .arg(relay_workers.to_string());
        }
        "nostr" => {
            cmd.arg("nostr").arg(relay_workers.to_string());
        }
        other => panic!("unknown RELAY={other}"),
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let mut child = cmd.spawn().expect("spawn relay subprocess");
    let child_stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(child_stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read PORT from child stdout");
    let port: u16 = line
        .trim()
        .strip_prefix("PORT=")
        .expect("child did not print PORT=<n>")
        .parse()
        .expect("child printed non-numeric PORT");

    // Drain any further child stdout so its pipe doesn't back up.
    std::thread::spawn(move || {
        let mut junk = String::new();
        loop {
            junk.clear();
            if reader.read_line(&mut junk).unwrap_or(0) == 0 {
                break;
            }
        }
    });

    std::thread::sleep(Duration::from_millis(200));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(client_workers)
        .enable_all()
        .build()
        .unwrap();

    let url = format!("ws://127.0.0.1:{port}");
    let ok_count = Arc::new(AtomicUsize::new(0));

    rt.block_on(async {
        // Pre-sign large events once, outside the timed region. Every pub
        // gets its own keypair so nostr-relay's id-based dedup doesn't drop
        // cross-pub duplicates.
        let total_per_pub = warmup_per_pub + events_per_pub;
        let filler: String = "x".repeat(content_bytes.saturating_sub(32));
        eprintln!(
            "pre-signing {} events/pub × {num_pubs} pubs at ~{} KiB each...",
            total_per_pub,
            content_bytes / 1024,
        );
        let presign_start = Instant::now();
        let pools: Vec<Arc<Vec<String>>> = (0..num_pubs)
            .map(|pub_idx| {
                let kp = K256Keypair::generate();
                let filler = filler.clone();
                Arc::new(
                    (0..total_per_pub)
                        .map(|i| {
                            let mut note = NostrNote::text_note(&format!(
                                "pub{pub_idx:02}-{i:08} {filler}"
                            ));
                            note.pubkey = kp.public_key();
                            kp.sign_nostr_note(&mut note).expect("sign");
                            format!(
                                r#"["EVENT",{}]"#,
                                serde_json::to_string(&note).unwrap()
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        eprintln!("pre-sign done in {:.2}s", presign_start.elapsed().as_secs_f64());

        // Wire byte size per event (after JSON encoding). Use the first
        // pool's first frame as the representative — sizes are effectively
        // constant across pubs because content_bytes dominates.
        let bytes_per_event = pools[0][0].len();
        eprintln!("wire size per EVENT frame ≈ {} bytes", bytes_per_event);

        // Connect publishers and spawn reader tasks that count OK acks.
        let mut pub_sinks: Vec<WsSink> = Vec::with_capacity(num_pubs);
        let mut reader_tasks = Vec::with_capacity(num_pubs);
        for _ in 0..num_pubs {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("pub connect");
            let (write, mut read) = ws.split();
            pub_sinks.push(write);
            let ok = ok_count.clone();
            reader_tasks.push(tokio::spawn(async move {
                while let Some(Ok(msg)) = read.next().await {
                    if let Message::Text(t) = msg {
                        if t.starts_with("[\"OK\"") {
                            ok.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }));
        }

        // --------------- warmup (untimed) ---------------
        if warmup_per_pub > 0 {
            let target = ok_count.load(Ordering::Relaxed) + warmup_per_pub * num_pubs;
            let sinks = std::mem::take(&mut pub_sinks);
            pub_sinks = send_batch(sinks, pools.clone(), 0, warmup_per_pub).await;
            wait_for_acks(&ok_count, target, Duration::from_secs(120)).await;
            eprintln!(
                "warmup: {} acks received",
                ok_count.load(Ordering::Relaxed)
            );
        }

        // --------------- timed ---------------
        let timed_events = events_per_pub * num_pubs;
        let timed_bytes = (timed_events as u64) * (bytes_per_event as u64);
        let before = ok_count.load(Ordering::Relaxed);
        let target = before + timed_events;

        eprintln!(
            "timed: driving {timed_events} events ({} MiB total wire)...",
            timed_bytes / (1024 * 1024)
        );
        let start = Instant::now();
        let sinks = std::mem::take(&mut pub_sinks);
        pub_sinks = send_batch(sinks, pools.clone(), warmup_per_pub, events_per_pub).await;
        wait_for_acks(&ok_count, target, Duration::from_secs(300)).await;
        let elapsed = start.elapsed();

        let secs = elapsed.as_secs_f64();
        let events_per_sec = timed_events as f64 / secs;
        let mib_per_sec = (timed_bytes as f64) / (1024.0 * 1024.0) / secs;

        println!();
        println!("=========================================================");
        println!("  relay:         {}", relay_kind);
        println!("  publishers:    {}", num_pubs);
        println!("  events/pub:    {}", events_per_pub);
        println!("  content size:  {} KiB", content_bytes / 1024);
        println!("  frame size:    {} bytes", bytes_per_event);
        println!("  total bytes:   {} MiB", timed_bytes / (1024 * 1024));
        println!("  elapsed:       {:.3} s", secs);
        println!("  throughput:    {:.1} MiB/s  ({:.0} events/s)", mib_per_sec, events_per_sec);
        println!("=========================================================");

        for t in reader_tasks {
            t.abort();
        }
        pub_sinks.clear();
    });

    // Dropping the subprocess's stdin closes it, which `fanout_relay_server`
    // reads as a shutdown signal.
    drop(child.stdin.take());
    let _ = child.wait();
}

async fn send_batch(
    sinks: Vec<WsSink>,
    pools: Vec<Arc<Vec<String>>>,
    start_idx: usize,
    count: usize,
) -> Vec<WsSink> {
    let mut handles = Vec::with_capacity(sinks.len());
    for (mut sink, pool) in sinks.into_iter().zip(pools.into_iter()) {
        handles.push(tokio::spawn(async move {
            for frame in &pool[start_idx..start_idx + count] {
                sink.send(Message::Text(frame.clone().into()))
                    .await
                    .expect("pub send");
            }
            sink
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

async fn wait_for_acks(ok_count: &AtomicUsize, target: usize, deadline: Duration) {
    let t0 = Instant::now();
    let mut last_logged = Instant::now();
    let mut last_count = ok_count.load(Ordering::Relaxed);
    loop {
        let n = ok_count.load(Ordering::Relaxed);
        if n >= target {
            return;
        }
        if t0.elapsed() > deadline {
            panic!("timeout waiting for acks: {n}/{target}");
        }
        if last_logged.elapsed() > Duration::from_millis(1000) {
            let delta = n.saturating_sub(last_count);
            eprintln!("  ... acks={n}/{target}  (+{delta} last second)");
            last_count = n;
            last_logged = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
