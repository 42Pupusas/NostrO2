//! Plain relay server for throughput benchmarking.
//!
//! Same shape as `heap_fanout_relay_server` — prints `PORT=<n>`, shuts down
//! when stdin closes, runs either `ring-relay-nostr` or `nostr-relay 0.4.8` —
//! but without the dhat global allocator. The throughput driver can then
//! measure wall-clock fan-out without the allocator-wrapper overhead dhat
//! adds (which is real: dhat hooks every alloc/dealloc and can slow
//! allocation-heavy code 2–5×, swamping the signal we care about).
//!
//! Invocation:
//!     fanout_relay_server ring   <max_clients> <workers>
//!     fanout_relay_server nostr  <http_workers>

use std::io::{BufRead, Write};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let kind = args.next().expect("usage: fanout_relay_server ring|nostr ...");

    let _guard: Box<dyn std::any::Any> = match kind.as_str() {
        "ring" => {
            let max_clients: usize = args
                .next()
                .expect("max_clients")
                .parse()
                .expect("max_clients int");
            let workers: usize = args
                .next()
                .unwrap_or_else(|| "4".into())
                .parse()
                .unwrap_or(4);
            Box::new(spawn_ring(max_clients, workers))
        }
        "nostr" => {
            let http_workers: usize = args
                .next()
                .unwrap_or_else(|| "4".into())
                .parse()
                .unwrap_or(4);
            Box::new(spawn_nostr_relay(http_workers))
        }
        other => panic!("unknown relay kind: {other}"),
    };

    let stdin = std::io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);

    drop(_guard);
}

struct RingGuard {
    shutdown: ring_relay_nostr::ShutdownHandle,
    _thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for RingGuard {
    fn drop(&mut self) {
        self.shutdown.shutdown();
    }
}

fn spawn_ring(max_clients: usize, workers: usize) -> RingGuard {
    use ring_relay_nostr::{NostrRelay, RelayConfig};
    use ring_relay_server::ShardConfig;

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let cfg = RelayConfig {
            max_clients,
            max_subs_per_conn: 4,
            max_filters_per_sub: 4,
            shards: ShardConfig {
                reader_shards: workers,
                writer_shards: workers,
            },
            ..Default::default()
        };
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("ring bind");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    let (port, shutdown) = rx.recv().unwrap();
    println!("PORT={port}");
    std::io::stdout().flush().ok();
    RingGuard {
        shutdown,
        _thread: Some(handle),
    }
}

struct NostrGuard {
    server_handle: actix_web::dev::ServerHandle,
    thread: Option<std::thread::JoinHandle<()>>,
    _tempdir: tempfile::TempDir,
}
impl Drop for NostrGuard {
    fn drop(&mut self) {
        let h = self.server_handle.clone();
        let _ = std::thread::spawn(move || {
            let sys = actix_rt::System::new();
            sys.block_on(h.stop(false));
        })
        .join();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn spawn_nostr_relay(http_workers: usize) -> NostrGuard {
    let tmp_root = if PathBuf::from("/dev/shm").is_dir() {
        Some(PathBuf::from("/dev/shm"))
    } else {
        None
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix("nostr-relay-tput-");
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
        .name("nostr-relay-tput".into())
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
    println!("PORT={port}");
    std::io::stdout().flush().ok();

    NostrGuard {
        server_handle,
        thread: Some(thread),
        _tempdir: tempdir,
    }
}
