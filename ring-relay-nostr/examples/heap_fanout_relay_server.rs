//! Relay-only heap profiling harness.
//!
//! Spawns a single relay (either our `NostrRelay` or `nostr-relay 0.4.8`),
//! prints `PORT=<n>` on stdout so the driver process can connect, then blocks
//! reading stdin. When the driver closes stdin (EOF), we drop the relay and
//! let dhat flush `dhat-heap.json`.
//!
//! Since the client side (tokio, tungstenite, the event-pool signer) lives in
//! a separate driver process, this binary's dhat output is pure relay-side
//! heap — nothing else.
//!
//! Invocation:
//!     heap_fanout_relay_server ring   <max_clients> <workers>
//!     heap_fanout_relay_server nostr  <http_workers>

use std::io::{BufRead, Write};
use std::path::PathBuf;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    let mut args = std::env::args().skip(1);
    let kind = args
        .next()
        .expect("usage: heap_fanout_relay_server ring|nostr ...");

    let profiler = dhat::Profiler::new_heap();

    let _guard: Box<dyn std::any::Any> = match kind.as_str() {
        "ring" => {
            let max_clients: usize = args
                .next()
                .expect("max_clients")
                .parse()
                .expect("max_clients int");
            let _workers: usize = args
                .next()
                .unwrap_or_else(|| "4".into())
                .parse()
                .unwrap_or(4);
            Box::new(spawn_ring(max_clients))
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

    // Block on stdin. The driver closes stdin (EOF) when the workload
    // completes — that's our signal to shut down.
    let stdin = std::io::stdin();
    let mut line = String::new();
    // Reading a line that never arrives waits indefinitely; when the driver
    // closes our stdin we return 0 bytes.
    let _ = stdin.lock().read_line(&mut line);

    drop(_guard);
    drop(profiler);
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

fn spawn_ring(max_clients: usize) -> RingGuard {
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
    println!("PORT={port}");
    std::io::stdout().flush().ok();

    NostrGuard {
        server_handle,
        thread: Some(thread),
        _tempdir: tempdir,
    }
}
