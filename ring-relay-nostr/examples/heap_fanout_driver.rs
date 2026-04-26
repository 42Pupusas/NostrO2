//! Driver for the relay-only heap comparison.
//!
//! Spawns `heap_fanout_relay_server` as a subprocess, reads `PORT=<n>` from
//! its stdout, runs the fan-out workload (N subs with an open filter, one
//! publisher streaming events), then closes the subprocess's stdin so the
//! server shuts down and dhat flushes its JSON.
//!
//! The client side (tokio, tungstenite, the signer) runs here, so it doesn't
//! pollute the relay's dhat profile.
//!
//! Env:
//!   RELAY=ring|nostr   (default: ring)
//!   HEAP_SUBS=<n>      (default: 50)
//!   HEAP_EVENTS=<n>    (default: 200)
//!   HEAP_WORKERS=<n>   (default: 4, threads inside the relay)
//!   RELAY_BIN=<path>   (default: target/release/examples/heap_fanout_relay_server)
//!   DHAT_OUT=<path>    where the relay writes dhat-heap.json (default: ./dhat-heap.json)

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let relay_kind = std::env::var("RELAY").unwrap_or_else(|_| "ring".into());
    let num_subs = env_usize("HEAP_SUBS", 50);
    let num_events = env_usize("HEAP_EVENTS", 200);
    let worker_threads = env_usize("HEAP_WORKERS", 4);

    let bin = std::env::var("RELAY_BIN").unwrap_or_else(|_| {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop();
        p.push("heap_fanout_relay_server");
        p.to_string_lossy().into_owned()
    });

    let dhat_out = std::env::var("DHAT_OUT").unwrap_or_else(|_| "dhat-heap.json".into());

    let mut cmd = Command::new(&bin);
    match relay_kind.as_str() {
        "ring" => {
            cmd.arg("ring")
                .arg((num_subs + 16).to_string())
                .arg(worker_threads.to_string());
        }
        "nostr" => {
            cmd.arg("nostr").arg(worker_threads.to_string());
        }
        other => panic!("unknown RELAY={other}"),
    }

    // Put the dhat output next to whatever the caller asked for. The server
    // process writes `dhat-heap.json` in its cwd on drop of Profiler, so
    // run the subprocess with cwd = parent of DHAT_OUT.
    let out_path = PathBuf::from(&dhat_out);
    let cwd = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    cmd.current_dir(&cwd);

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    // Let the child's stderr (dhat's summary) pass straight through so we
    // can capture it in the bench logs.
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

    // Drain further stdout asynchronously so the child never blocks.
    std::thread::spawn(move || {
        let mut junk = String::new();
        loop {
            junk.clear();
            if reader.read_line(&mut junk).unwrap_or(0) == 0 {
                break;
            }
        }
    });

    // Small settling delay so the listener is accepting before we storm it.
    std::thread::sleep(Duration::from_millis(200));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();

    let url = format!("ws://127.0.0.1:{port}");

    rt.block_on(async {
        const CONNECT_BATCH: usize = 128;
        let mut sub_sockets = Vec::with_capacity(num_subs);
        for chunk_start in (0..num_subs).step_by(CONNECT_BATCH) {
            let chunk_end = (chunk_start + CONNECT_BATCH).min(num_subs);
            let mut futs = Vec::with_capacity(chunk_end - chunk_start);
            for i in chunk_start..chunk_end {
                let url = url.clone();
                futs.push(tokio::spawn(async move {
                    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
                    let sub_id = format!("s{i:05}");
                    let req = format!(r#"["REQ","{sub_id}",{{"kinds":[1]}}]"#);
                    ws.send(Message::Text(req.into())).await.unwrap();
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(t) = msg
                            && t.starts_with("[\"EOSE\"")
                        {
                            break;
                        }
                    }
                    ws
                }));
            }
            for fut in futs {
                sub_sockets.push(fut.await.unwrap());
            }
        }

        let sub_handles: Vec<_> = sub_sockets
            .into_iter()
            .map(|mut ws| {
                tokio::spawn(async move {
                    let mut count = 0;
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(_) = msg {
                            count += 1;
                            if count >= num_events {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();

        let kp = K256Keypair::generate();
        let (mut pubws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        for i in 0..num_events {
            let mut note = NostrNote {
                content: format!("heap-driver event {i}"),
                kind: 1,
                pubkey: kp.public_key(),
                ..Default::default()
            };
            kp.sign_nostr_note(&mut note).unwrap();
            let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
            pubws.send(Message::Text(frame.into())).await.unwrap();
            while let Some(Ok(msg)) = pubws.next().await {
                if let Message::Text(t) = msg
                    && t.starts_with("[\"OK\"")
                {
                    break;
                }
            }
        }

        let _ = tokio::time::timeout(
            Duration::from_secs(60),
            futures_util::future::join_all(sub_handles),
        )
        .await;
    });

    // Let the relay quiesce a moment so any post-delivery bookkeeping lands
    // in the same steady state before we trigger shutdown / flush.
    std::thread::sleep(Duration::from_millis(300));

    // Signal EOF on stdin → relay exits main, drops Profiler, writes JSON.
    drop(child.stdin.take());

    let status = child.wait().expect("relay wait");
    if !status.success() {
        eprintln!("relay subprocess exited with {status}");
    }

    // Relay wrote dhat-heap.json into `cwd`. If the caller requested a
    // different filename, move it.
    let default_out = cwd.join("dhat-heap.json");
    if default_out != out_path {
        let _ = std::fs::rename(&default_out, &out_path);
    }

    eprintln!(
        "heap_fanout_driver: relay={relay_kind} {num_events} events × {num_subs} subs → {}",
        out_path.display()
    );
}
