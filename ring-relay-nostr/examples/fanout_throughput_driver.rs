//! Throughput driver for the fan-out comparison.
//!
//! Spawns `fanout_relay_server` (either RELAY=ring or RELAY=nostr), opens
//! N subscribers with a kinds:[1] filter + P publishers, warms up, then
//! times a timed phase where each publisher emits E events in parallel and
//! we wait for every subscriber to receive every event (total = N × P × E).
//!
//! Reports:
//!   - wall-clock elapsed for the timed phase
//!   - publishes/sec (EVENT frames injected into the relay)
//!   - deliveries/sec (EVENT frames sent from the relay to subscribers)
//!
//! Env:
//!   RELAY=ring|nostr       (default: ring)
//!   TPUT_SUBS=<n>          (default: 500)
//!   TPUT_PUBS=<n>          (default: 4)
//!   TPUT_EVENTS=<n>        (default: 50 per publisher per timed phase)
//!   TPUT_WARMUP=<n>        (default: 5 events per publisher, untimed)
//!   TPUT_WORKERS=<n>       (default: 8, tokio worker threads on the client)
//!   RELAY_WORKERS=<n>      (default: 8, threads inside the relay)
//!   RELAY_BIN=<path>       (default: target/release/examples/fanout_relay_server)

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    let relay_kind = std::env::var("RELAY").unwrap_or_else(|_| "ring".into());
    let num_subs = env_usize("TPUT_SUBS", 500);
    let num_pubs = env_usize("TPUT_PUBS", 4);
    let events_per_pub = env_usize("TPUT_EVENTS", 50);
    let warmup_per_pub = env_usize("TPUT_WARMUP", 5);
    let client_workers = env_usize("TPUT_WORKERS", 8);
    let relay_workers = env_usize("RELAY_WORKERS", 8);

    let bin = std::env::var("RELAY_BIN").unwrap_or_else(|_| {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop();
        p.push("fanout_relay_server");
        p.to_string_lossy().into_owned()
    });

    // Allow caller to force a larger max_clients so the server-side writer
    // rings (sized from max_clients × 16) are never the bottleneck in the
    // comparison.
    let max_clients_override = env_usize("TPUT_MAX_CLIENTS", num_subs + num_pubs + 32);

    let mut cmd = Command::new(&bin);
    match relay_kind.as_str() {
        "ring" => {
            cmd.arg("ring")
                .arg(max_clients_override.to_string())
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
    let delivered = Arc::new(AtomicUsize::new(0));

    // Per-sub counters so we can see the distribution on stall.
    let per_sub: Arc<Vec<AtomicUsize>> =
        Arc::new((0..num_subs).map(|_| AtomicUsize::new(0)).collect());

    rt.block_on(async {
        // --------------- subscribers ---------------
        const CONNECT_BATCH: usize = 128;
        let mut sub_tasks = Vec::with_capacity(num_subs);
        let mut eose_rxs = Vec::with_capacity(num_subs);
        for chunk_start in (0..num_subs).step_by(CONNECT_BATCH) {
            let chunk_end = (chunk_start + CONNECT_BATCH).min(num_subs);
            for i in chunk_start..chunk_end {
                let url = url.clone();
                let delivered = delivered.clone();
                let per_sub = per_sub.clone();
                let (eose_tx, eose_rx) = tokio::sync::oneshot::channel::<()>();
                eose_rxs.push(eose_rx);
                sub_tasks.push(tokio::spawn(async move {
                    let (ws, _) = tokio_tungstenite::connect_async(&url)
                        .await
                        .expect("sub connect");
                    let (mut write, mut read) = ws.split();
                    let sub_id = format!("s{i:05}");
                    let req = format!(r#"["REQ","{sub_id}",{{"kinds":[1]}}]"#);
                    write.send(Message::Text(req.into())).await.expect("REQ");
                    // Keep the write half alive for the lifetime of the task
                    // so tokio-tungstenite doesn't send a close frame on the
                    // split-stream going out of scope, which would terminate
                    // the subscription prematurely on the server side.
                    let _write = write;
                    let mut eose_tx = Some(eose_tx);
                    while let Some(Ok(msg)) = read.next().await {
                        if let Message::Text(t) = msg {
                            if let Some(tx) = eose_tx.take() {
                                if t.starts_with("[\"EOSE\"") {
                                    let _ = tx.send(());
                                    continue;
                                } else {
                                    // First frame was an EVENT (unlikely
                                    // given empty DB but possible on
                                    // nostr-relay). Count it and signal.
                                    let _ = tx.send(());
                                }
                            }
                            if t.starts_with("[\"EVENT\"") {
                                delivered.fetch_add(1, Ordering::Relaxed);
                                per_sub[i].fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        for rx in eose_rxs {
            let _ = tokio::time::timeout(Duration::from_secs(30), rx)
                .await
                .expect("EOSE timeout");
        }

        // --------------- publishers ---------------
        let mut pub_sinks = Vec::with_capacity(num_pubs);
        let mut pub_drain_tasks = Vec::with_capacity(num_pubs);
        for _ in 0..num_pubs {
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("pub connect");
            let (write, mut read) = ws.split();
            pub_sinks.push(write);
            pub_drain_tasks.push(tokio::spawn(async move {
                while let Some(Ok(_)) = read.next().await {}
            }));
        }

        // Pre-sign events so the timed phase measures the relay, not k256.
        // Each publisher needs a distinct keypair so nostr-relay's LMDB
        // dedup by event id doesn't drop duplicates.
        let total_per_pub = warmup_per_pub + events_per_pub;
        let pools: Vec<Arc<Vec<String>>> = (0..num_pubs)
            .map(|pub_idx| {
                let kp = K256Keypair::generate();
                Arc::new(
                    (0..total_per_pub)
                        .map(|i| {
                            let mut note = NostrNote {
                                content: format!("tput p{pub_idx} e{i}"),
                                kind: 1,
                                pubkey: kp.public_key(),
                                ..Default::default()
                            };
                            kp.sign_nostr_note(&mut note).unwrap();
                            format!(
                                r#"["EVENT",{}]"#,
                                serde_json::to_string(&note).unwrap()
                            )
                        })
                        .collect(),
                )
            })
            .collect();

        // --------------- warmup ---------------
        if warmup_per_pub > 0 {
            let warmup_target = warmup_per_pub * num_pubs * num_subs;
            let before = delivered.load(Ordering::Relaxed);
            let target = before + warmup_target;

            let sinks = std::mem::take(&mut pub_sinks);
            let pools_cl = pools.clone();
            pub_sinks = send_batch(sinks, pools_cl, 0, warmup_per_pub).await;

            eprintln!("warmup: waiting for {warmup_target} deliveries (target={target})");
            wait_for_deliveries(&delivered, target).await;
            eprintln!("warmup: done, delivered={}", delivered.load(Ordering::Relaxed));
        }

        // --------------- timed ---------------
        let timed_target = events_per_pub * num_pubs * num_subs;
        let before = delivered.load(Ordering::Relaxed);
        let target = before + timed_target;
        eprintln!("timed: starting, before={before}, target={target}, deliveries to drive={timed_target}");

        let start = Instant::now();
        let sinks = std::mem::take(&mut pub_sinks);
        pub_sinks = send_batch(sinks, pools.clone(), warmup_per_pub, events_per_pub).await;
        let wait_res =
            tokio::time::timeout(Duration::from_secs(10), wait_for_deliveries(&delivered, target)).await;
        if wait_res.is_err() {
            let counts: Vec<usize> = per_sub.iter().map(|a| a.load(Ordering::Relaxed)).collect();
            let min = counts.iter().min().copied().unwrap_or(0);
            let max = counts.iter().max().copied().unwrap_or(0);
            let sum: usize = counts.iter().sum();
            let zero = counts.iter().filter(|&&c| c == 0).count();
            eprintln!("STALL: sum={sum} min={min} max={max} zero_subs={zero}");
            eprintln!("per-sub sample: {:?}", &counts[..counts.len().min(20)]);
            panic!("stall diagnosed");
        }
        let elapsed = start.elapsed();

        let publishes = events_per_pub * num_pubs;
        let deliveries = timed_target;
        let publishes_per_sec = publishes as f64 / elapsed.as_secs_f64();
        let deliveries_per_sec = deliveries as f64 / elapsed.as_secs_f64();

        eprintln!(
            "tput relay={} subs={} pubs={} events={}  elapsed={:.3}s  publishes={}/s  deliveries={:.0}/s",
            relay_kind,
            num_subs,
            num_pubs,
            events_per_pub,
            elapsed.as_secs_f64(),
            publishes_per_sec as u64,
            deliveries_per_sec,
        );

        for task in sub_tasks {
            task.abort();
        }
        for task in pub_drain_tasks {
            task.abort();
        }
        pub_sinks.clear();
    });

    drop(child.stdin.take());
    let _ = child.wait();
}

async fn send_batch(
    sinks: Vec<futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >>,
    pools: Vec<Arc<Vec<String>>>,
    start_idx: usize,
    count: usize,
) -> Vec<futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>> {
    let mut handles = Vec::with_capacity(sinks.len());
    for (pub_idx, (mut sink, pool)) in sinks.into_iter().zip(pools.into_iter()).enumerate() {
        handles.push(tokio::spawn(async move {
            for (i, frame) in pool[start_idx..start_idx + count].iter().enumerate() {
                sink.send(Message::Text(frame.clone().into()))
                    .await
                    .unwrap_or_else(|e| {
                        panic!("pub {pub_idx} send failed at event {i}: {e}")
                    });
            }
            eprintln!("  pub {pub_idx}: sent {count} frames");
            sink
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

async fn wait_for_deliveries(delivered: &AtomicUsize, target: usize) {
    let mut last_logged = Instant::now();
    let mut last_count = 0usize;
    loop {
        let n = delivered.load(Ordering::Relaxed);
        if n >= target {
            return;
        }
        if last_logged.elapsed() > Duration::from_millis(500) {
            let delta = n.saturating_sub(last_count);
            eprintln!("  ... delivered={n}/{target}  (+{delta} since last tick)");
            last_count = n;
            last_logged = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
