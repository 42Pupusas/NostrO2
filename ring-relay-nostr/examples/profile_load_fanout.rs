//! Fanout load driver for profile_server. Connects N subscribers each with
//! an open filter, then a handful of publishers that stream distinct EVENTs
//! for a fixed duration. Every event fans out to every subscriber.
//!
//! Usage:
//!   cargo run --release --example profile_load_fanout -- \
//!     --port 4848 --duration 25 --pubs 2 --subs 256

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    let mut port: u16 = 4848;
    let mut duration_secs: u64 = 25;
    let mut num_pubs: usize = 2;
    let mut num_subs: usize = 256;

    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--duration" => {
                duration_secs = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--pubs" => {
                num_pubs = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--subs" => {
                num_subs = args[i + 1].parse().unwrap();
                i += 2;
            }
            _ => {
                eprintln!("unknown arg: {}", args[i]);
                std::process::exit(2);
            }
        }
    }

    let url = format!("ws://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let delivered = Arc::new(AtomicUsize::new(0));
    let sent = Arc::new(AtomicUsize::new(0));

    // Subscribers first so REQs land before any EVENT traffic. Connect in
    // small batches to avoid overloading the single-threaded accept path.
    const CONNECT_BATCH: usize = 32;
    let mut sub_tasks = Vec::with_capacity(num_subs);
    for chunk in (0..num_subs).collect::<Vec<_>>().chunks(CONNECT_BATCH) {
        for &s in chunk {
            let url = url.clone();
            let delivered = delivered.clone();
            sub_tasks.push(tokio::spawn(async move {
                let (ws, _) = tokio_tungstenite::connect_async(&url)
                    .await
                    .expect("sub connect");
                let (mut write, mut read) = ws.split();
                let req = format!(r#"["REQ","s{s}",{{"kinds":[1]}}]"#);
                write.send(Message::Text(req.into())).await.expect("REQ");
                while Instant::now() < deadline + Duration::from_secs(1) {
                    match read.next().await {
                        Some(Ok(Message::Text(t))) => {
                            if t.starts_with("[\"EVENT\"") {
                                delivered.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Some(Ok(_)) => {}
                        _ => break,
                    }
                }
            }));
        }
        // Let this batch fully establish before the next one so accept
        // doesn't get a SYN flood.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Small settle so every REQ reaches the relay (and replicates, if the
    // relay is multi-shard) before publishers start firing events.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut pub_tasks = Vec::with_capacity(num_pubs);
    for p in 0..num_pubs {
        let url = url.clone();
        let sent = Arc::clone(&sent);
        pub_tasks.push(tokio::spawn(async move {
            let kp = K256Keypair::generate();
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("pub connect");
            let (mut write, mut read) = ws.split();
            let drainer = tokio::spawn(async move { while let Some(Ok(_)) = read.next().await {} });

            let mut counter: usize = 0;
            while Instant::now() < deadline {
                let mut note = NostrNote::text_note(&format!("profile p{p} {counter}"));
                note.pubkey = kp.public_key();
                kp.sign_nostr_note(&mut note).expect("sign");
                let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
                if write.send(Message::Text(frame.into())).await.is_err() {
                    break;
                }
                counter += 1;
                sent.fetch_add(1, Ordering::Relaxed);
            }
            drop(write);
            let _ = drainer.await;
        }));
    }

    for t in pub_tasks {
        let _ = t.await;
    }
    for t in sub_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(500), t).await;
    }

    let s = sent.load(Ordering::Relaxed);
    let d = delivered.load(Ordering::Relaxed);
    eprintln!(
        "profile_load_fanout: sent {s} events, delivered {d} ({num_pubs} pubs, {num_subs} subs, {duration_secs}s)"
    );
}
