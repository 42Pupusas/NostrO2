//! Ingest load driver for profile_server. Connects N pubs, each publishing
//! a stream of distinct EVENTs for a fixed duration. Exits when the
//! duration is up (server's own deadline should be a bit longer so it
//! captures the tail).
//!
//! Usage:
//!   cargo run --release --example profile_load_ingest -- \
//!     --port 4848 --duration 25 --pubs 8

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
    let mut num_pubs: usize = 8;

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
            _ => {
                eprintln!("unknown arg: {}", args[i]);
                std::process::exit(2);
            }
        }
    }

    let url = format!("ws://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let sent = Arc::new(AtomicUsize::new(0));

    let mut tasks = Vec::new();
    for p in 0..num_pubs {
        let url = url.clone();
        let sent = Arc::clone(&sent);
        tasks.push(tokio::spawn(async move {
            let kp = K256Keypair::generate();
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("connect");
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

    for t in tasks {
        let _ = t.await;
    }

    let total = sent.load(Ordering::Relaxed);
    let rate = total as f64 / duration_secs as f64;
    eprintln!(
        "profile_load_ingest: sent {total} events over {duration_secs}s ({rate:.0} ev/s, {num_pubs} pubs)"
    );
}
