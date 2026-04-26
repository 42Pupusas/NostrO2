//! Profiling server: bind a relay on a fixed port and run for a fixed
//! duration, then exit. Designed to be profiled with `cargo flamegraph`
//! while a sibling load process (`profile_load_ingest` or
//! `profile_load_fanout`) drives traffic.
//!
//! Usage:
//!   cargo flamegraph --release --example profile_server -- \
//!     --port 4848 --duration 30 --shards 1
//!
//! Then in another terminal, run the matching load example.

use ring_relay_nostr::{NostrRelay, RelayConfig};
use ring_relay_server::ShardConfig;
use std::env;
use std::time::{Duration, Instant};

fn main() {
    let mut port: u16 = 4848;
    let mut duration_secs: u64 = 30;
    let mut shards: usize = 1;
    let mut max_clients: usize = 4096;
    let mut verify_threads: usize = 1;

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
            "--shards" => {
                shards = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--max-clients" => {
                max_clients = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--verify-threads" => {
                verify_threads = args[i + 1].parse().unwrap();
                i += 2;
            }
            _ => {
                eprintln!("unknown arg: {}", args[i]);
                std::process::exit(2);
            }
        }
    }

    let mut cfg = RelayConfig::default();
    cfg.max_clients = max_clients;
    cfg.shards = ShardConfig {
        reader_shards: shards,
        writer_shards: shards,
    };
    cfg.verify_threads_per_shard = verify_threads;

    let relay = NostrRelay::bind([127, 0, 0, 1], port, cfg).expect("bind");
    let shutdown = relay.shutdown_handle();
    eprintln!(
        "profile_server: listening on 127.0.0.1:{port} for {duration_secs}s (shards={shards} verify_threads={verify_threads})"
    );

    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let deadline_handle = shutdown.clone();
    std::thread::spawn(move || {
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        deadline_handle.shutdown();
    });

    // NostrRelay's run() blocks on the shutdown flag; drop the mut binding
    // requirement by taking via an Option.
    let mut relay = Some(relay);
    if let Some(r) = relay.as_mut() {
        r.run();
    }
    drop(relay);
    eprintln!("profile_server: exiting");
}
