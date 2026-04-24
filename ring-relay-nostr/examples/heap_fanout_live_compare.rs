//! Apples-to-apples heap comparison: ring-relay-nostr vs nostr-relay 0.4.8.
//!
//! Identical fan-out workload (same subs, same events, same publisher) driven
//! against either our relay or nostr-relay, with dhat linked in so we can diff
//! peak heap between the two.
//!
//! Select which relay to profile via the `RELAY` env var: `RELAY=ring` (default)
//! or `RELAY=nostr`. Other env vars (`HEAP_SUBS`, `HEAP_EVENTS`, `HEAP_WORKERS`)
//! match `heap_fanout_live`.
//!
//! Both relays run in-process, so dhat's global-allocator wrapper captures
//! every heap alloc on the relay's hot path. The client-side traffic (tokio
//! tasks, tungstenite frames, pre-signed event pool) is charged identically to
//! both runs, so it subtracts out when you compare peaks.
//!
//! Tmpfs caveat: nostr-relay's LMDB data dir is placed on /dev/shm if
//! available, matching the existing comparison bench harness.

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const DEFAULT_SUBS: usize = 50;
const DEFAULT_EVENTS: usize = 200;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

enum RelayHandle {
    Ring {
        port: u16,
        shutdown: ring_relay_nostr::ShutdownHandle,
        _thread: std::thread::JoinHandle<()>,
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
            RelayHandle::Ring { shutdown, .. } => shutdown.shutdown(),
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

fn spawn_ring(max_clients: usize) -> RelayHandle {
    use ring_relay_nostr::{NostrRelay, RelayConfig};

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let cfg = RelayConfig {
            max_clients,
            max_subs_per_conn: 4,
            max_filters_per_sub: 4,
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
        _thread: handle,
    }
}

fn spawn_nostr_relay(http_workers: usize) -> RelayHandle {
    let tmp_root = if PathBuf::from("/dev/shm").is_dir() {
        Some(PathBuf::from("/dev/shm"))
    } else {
        None
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix("nostr-relay-heap-");
    let tempdir = if let Some(root) = tmp_root {
        builder.tempdir_in(root).expect("tempdir on tmpfs")
    } else {
        builder.tempdir().expect("tempdir")
    };

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0u16)).expect("bind");
    listener.set_nonblocking(true).ok();
    let port = listener.local_addr().unwrap().port();

    let data_path = tempdir.path().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("nostr-relay-heap".into())
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

                let server = actix_web::HttpServer::new(move || {
                    nostr_relay::create_web_app(data.clone())
                })
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
    let num_subs = env_usize("HEAP_SUBS", DEFAULT_SUBS);
    let num_events = env_usize("HEAP_EVENTS", DEFAULT_EVENTS);
    let worker_threads = env_usize("HEAP_WORKERS", 4);
    let relay_kind = std::env::var("RELAY").unwrap_or_else(|_| "ring".into());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();

    // Spawn the relay BEFORE starting dhat so the relay's startup
    // allocations (thread pools, ring buffers, LMDB map) are charged
    // equally — we only care about steady-state fan-out memory.
    // But spawning actix from inside a separate runtime is fine.
    let relay = match relay_kind.as_str() {
        "ring" => spawn_ring(num_subs + 16),
        "nostr" => spawn_nostr_relay(worker_threads),
        other => panic!("unknown RELAY={other}; expected ring|nostr"),
    };

    // Small warmup so the relay is fully up before we start profiling.
    std::thread::sleep(Duration::from_millis(200));

    let profiler = dhat::Profiler::new_heap();

    let port = relay.port();
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
                content: format!("heap-compare event {i}"),
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

    drop(profiler);
    eprintln!(
        "heap_fanout_live_compare: relay={relay_kind} {num_events} events × {num_subs} subs — wrote dhat-heap.json"
    );
    drop(relay);
}
