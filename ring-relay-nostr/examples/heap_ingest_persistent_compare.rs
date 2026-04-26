//! Apples-to-apples *persistent-mode* heap comparison: ring-relay-nostr
//! (with the bounded-bucket storage layer enabled) vs nostr-relay 0.4.8
//! (with LMDB).
//!
//! Same shape as `heap_fanout_live_compare`, but the workload here is
//! ingest-only: one publisher, no subscribers. We're measuring what the
//! storage layer costs in memory while events stream in. Pre-allocated
//! slot tables, in-memory `BucketIndex`es, the broadcast `IndexUpdate`
//! ring, and LMDB's mmap'd page cache will all show up in dhat's peak.
//!
//! Select the relay via the `RELAY` env var: `RELAY=ring` (default) or
//! `RELAY=nostr`. Tune the workload via `HEAP_EVENTS` (default 5000)
//! and `HEAP_WORKERS` (tokio worker threads, default 4).
//!
//! ## Reading the output
//!
//! Open the produced `dhat-heap.json` in `dh_view.html`. The two key
//! numbers to compare are the *peak heap (live bytes)* and the *total
//! bytes allocated*. Client-side allocs (tokio runtime, tungstenite
//! frames, the pre-signed event pool) are charged identically to both
//! runs and largely cancel out when diffing.
//!
//! Tmpfs caveat: nostr-relay's LMDB data dir is placed on /dev/shm if
//! available; ring-relay-nostr's bucket logs likewise. This neutralizes
//! storage latency so we measure memory, not disk.

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const DEFAULT_EVENTS: usize = 5000;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

enum RelayHandle {
    Ring {
        port: u16,
        shutdown: ring_relay_nostr::ShutdownHandle,
        thread: Option<std::thread::JoinHandle<()>>,
        _tempdir: tempfile::TempDir,
    },
    Nostr {
        port: u16,
        server_handle: actix_web::dev::ServerHandle,
        thread: Option<std::thread::JoinHandle<()>>,
        _tempdir: tempfile::TempDir,
    },
}

impl RelayHandle {
    fn port(&self) -> u16 {
        match self {
            RelayHandle::Ring { port, .. } | RelayHandle::Nostr { port, .. } => *port,
        }
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        match self {
            RelayHandle::Ring {
                shutdown, thread, ..
            } => {
                shutdown.shutdown();
                if let Some(t) = thread.take() {
                    let _ = t.join();
                }
            }
            RelayHandle::Nostr {
                server_handle,
                thread,
                ..
            } => {
                let handle = server_handle.clone();
                let _ = std::thread::spawn(move || {
                    let sys = actix_rt::System::new();
                    sys.block_on(handle.stop(false));
                })
                .join();
                if let Some(t) = thread.take() {
                    let _ = t.join();
                }
            }
        }
    }
}

fn tmpfs_tempdir(prefix: &str) -> tempfile::TempDir {
    let tmp_root = if PathBuf::from("/dev/shm").is_dir() {
        Some(PathBuf::from("/dev/shm"))
    } else {
        None
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    if let Some(root) = tmp_root {
        builder.tempdir_in(root).expect("tempdir on tmpfs")
    } else {
        builder.tempdir().expect("tempdir")
    }
}

fn spawn_ring_persistent(max_clients: usize) -> RelayHandle {
    use ring_relay_nostr::{NostrRelay, RelayConfig, StorageConfig};

    let tempdir = tmpfs_tempdir("ring-heap-persistent-");
    let data_path = tempdir.path().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let cfg = RelayConfig {
            max_clients,
            storage: Some(StorageConfig {
                data_dir: data_path,
                // Sized to match `compare_ingest_persistent` so the
                // memory profile matches what the throughput bench
                // sees. Drop the slot counts proportionally if you
                // want to shrink the working set.
                ephemeral_slots: 200_000,
                replaceable_slots: 10_000,
                parameterized_slots: 10_000,
                max_payload: 64 * 1024,
                reader_threads: 2,
                write_ring_capacity: 8192,
                req_ring_capacity: 1024,
                fsync_interval_ms: Some(10),
            }),
            ..Default::default()
        };
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("ring bind");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    let (port, shutdown) = rx.recv().unwrap();
    RelayHandle::Ring {
        port,
        shutdown,
        thread: Some(handle),
        _tempdir: tempdir,
    }
}

fn spawn_nostr_relay(http_workers: usize) -> RelayHandle {
    let tempdir = tmpfs_tempdir("nostr-heap-persistent-");

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0u16)).expect("bind");
    listener.set_nonblocking(true).ok();
    let port = listener.local_addr().unwrap().port();

    let data_path = tempdir.path().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("nostr-relay-heap-persistent".into())
        .spawn(move || {
            let sys = actix_rt::System::new();
            sys.block_on(async move {
                let app = nostr_relay::App::create::<std::path::PathBuf>(
                    None,
                    false,
                    None,
                    Some(data_path),
                )
                .expect("create App");
                let data = actix_web::web::Data::new(app);

                let server =
                    actix_web::HttpServer::new(move || nostr_relay::create_web_app(data.clone()))
                        .workers(http_workers)
                        .listen(listener)
                        .expect("actix listen")
                        .run();

                let handle = server.handle();
                tx.send(handle).unwrap();
                let _ = server.await;
            });
        })
        .expect("spawn nostr-relay thread");

    let server_handle = rx.recv().expect("actix handle");

    RelayHandle::Nostr {
        port,
        server_handle,
        thread: Some(thread),
        _tempdir: tempdir,
    }
}

fn main() {
    let num_events = env_usize("HEAP_EVENTS", DEFAULT_EVENTS);
    let worker_threads = env_usize("HEAP_WORKERS", 4);
    let relay_kind = std::env::var("RELAY").unwrap_or_else(|_| "ring".into());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();

    // Spawn the relay BEFORE starting dhat so its bind-time allocations
    // (slot tables, LMDB mmap) are charged equally to both relays. We
    // only profile steady-state ingest.
    let relay = match relay_kind.as_str() {
        "ring" => spawn_ring_persistent(16),
        "nostr" => spawn_nostr_relay(worker_threads),
        other => panic!("unknown RELAY={other}; expected ring|nostr"),
    };

    // Warmup: let the relay finish opening files / preparing pools.
    std::thread::sleep(Duration::from_millis(200));

    let profiler = dhat::Profiler::new_heap();

    let port = relay.port();
    let url = format!("ws://127.0.0.1:{port}");

    rt.block_on(async {
        let kp = K256Keypair::generate();
        let (mut pubws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        use std::io::Write as _;
        let progress = std::time::Instant::now();
        for i in 0..num_events {
            let mut note = NostrNote {
                content: format!("heap-persistent event {i}"),
                kind: 1,
                pubkey: kp.public_key(),
                ..Default::default()
            };
            kp.sign_nostr_note(&mut note).unwrap();
            let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());

            // Wrap send + recv together in a per-event deadline so any
            // stall (websocket backpressure, dropped connection, missing
            // OK) surfaces as a single diagnostic line.
            let send_recv = async {
                pubws.send(Message::Text(frame.into())).await.ok()?;
                while let Some(Ok(msg)) = pubws.next().await {
                    if let Message::Text(t) = msg
                        && t.starts_with("[\"OK\"")
                    {
                        return Some(());
                    }
                }
                None
            };
            match tokio::time::timeout(Duration::from_secs(5), send_recv).await {
                Ok(Some(())) => {}
                Ok(None) => {
                    eprintln!("ws closed at event {i} after {:?}", progress.elapsed());
                    let _ = std::io::stderr().flush();
                    return;
                }
                Err(_) => {
                    eprintln!(
                        "stall at event {i} after {:?} (no OK in 5s)",
                        progress.elapsed()
                    );
                    let _ = std::io::stderr().flush();
                    return;
                }
            }
            if i % 50 == 0 {
                eprintln!("{i}/{num_events} t={:?}", progress.elapsed());
                let _ = std::io::stderr().flush();
            }
        }

        // Brief drain so the storage thread has caught up before we
        // capture the final dhat snapshot.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    drop(profiler);
    eprintln!(
        "heap_ingest_persistent_compare: relay={relay_kind} {num_events} events — wrote dhat-heap.json"
    );
    drop(relay);
}
