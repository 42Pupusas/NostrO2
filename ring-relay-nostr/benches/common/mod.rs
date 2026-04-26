//! Shared harness for the comparison benches. Spins up either our
//! ring-relay-nostr or nostr-relay 0.4.8 (with its LMDB pointed at /dev/shm)
//! and returns the port to connect to.
//!
//! Kept minimal — benches import individual helpers.
#![allow(dead_code)] // Each bench uses only part of this file.

use std::path::PathBuf;
use std::sync::Arc;

use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;

use ring_relay_nostr::{NostrRelay, RelayConfig, StorageConfig};
use ring_relay_server::ShardConfig;

/// Pre-sign `count` distinct kind-1 events, ready to wire-send.
pub fn presign(count: usize) -> Arc<Vec<String>> {
    presign_for(count, "compare")
}

/// Pre-sign `count` distinct kind-1 events using a fresh keypair and a
/// caller-provided `tag` mixed into the content. Use a unique tag per
/// publisher in a multi-publisher bench so nostr-relay (which deduplicates
/// events by id) doesn't discard the second publisher's copy of the same
/// content.
pub fn presign_for(count: usize, tag: &str) -> Arc<Vec<String>> {
    let kp = K256Keypair::generate();
    Arc::new(
        (0..count)
            .map(|i| {
                let mut note = NostrNote::text_note(&format!("{tag} {i}"));
                note.pubkey = kp.public_key();
                kp.sign_nostr_note(&mut note).expect("sign");
                format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap())
            })
            .collect(),
    )
}

/// A running relay instance for a bench iteration.
pub struct Relay {
    pub port: u16,
    /// Drop-based teardown. Holds whatever handles / tempdirs the impl needs.
    _guard: Box<dyn std::any::Any + Send>,
}

impl Relay {
    /// Spin up ring-relay-nostr at the given shard count. Blocks until bound.
    pub fn spawn_ring(shards: usize, max_clients: usize) -> Self {
        struct Guard {
            shutdown: ring_relay_nostr::ShutdownHandle,
            // Keep the relay thread handle alive; we don't need to join.
            _thread: std::thread::JoinHandle<()>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                self.shutdown.shutdown();
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut cfg = RelayConfig::default();
            cfg.shards = ShardConfig {
                reader_shards: shards,
                writer_shards: shards,
            };
            cfg.max_clients = max_clients;
            let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("ring bind");
            let port = relay.port();
            let shutdown = relay.shutdown_handle();
            tx.send((port, shutdown)).unwrap();
            relay.run();
        });
        let (port, shutdown) = rx.recv().unwrap();
        Relay {
            port,
            _guard: Box::new(Guard {
                shutdown,
                _thread: handle,
            }),
        }
    }

    /// Spin up ring-relay-nostr **with persistence enabled**, data dir on
    /// tmpfs (/dev/shm when available) so ingest isn't disk-bound — same
    /// policy as `spawn_nostr_relay`, so the comparison is apples-to-apples.
    pub fn spawn_ring_persistent(shards: usize, max_clients: usize) -> Self {
        struct Guard {
            shutdown: ring_relay_nostr::ShutdownHandle,
            _thread: std::thread::JoinHandle<()>,
            _tempdir: tempfile::TempDir,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                self.shutdown.shutdown();
            }
        }

        let tmp_root = if PathBuf::from("/dev/shm").is_dir() {
            Some(PathBuf::from("/dev/shm"))
        } else {
            None
        };
        let mut builder = tempfile::Builder::new();
        builder.prefix("ring-relay-bench-");
        let tempdir = if let Some(root) = tmp_root {
            builder.tempdir_in(root).expect("tempdir on tmpfs")
        } else {
            builder.tempdir().expect("tempdir")
        };

        let data_path = tempdir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut cfg = RelayConfig::default();
            cfg.shards = ShardConfig {
                reader_shards: shards,
                writer_shards: shards,
            };
            cfg.max_clients = max_clients;
            cfg.storage = Some(StorageConfig {
                data_dir: data_path,
                ephemeral_slots: 200_000,
                replaceable_slots: 10_000,
                parameterized_slots: 10_000,
                max_payload: 64 * 1024,
                reader_threads: 2,
                write_ring_capacity: 8192,
                req_ring_capacity: 1024,
                fsync_interval_ms: Some(10),
            });
            let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, cfg).expect("ring bind");
            let port = relay.port();
            let shutdown = relay.shutdown_handle();
            tx.send((port, shutdown)).unwrap();
            relay.run();
        });
        let (port, shutdown) = rx.recv().unwrap();
        Relay {
            port,
            _guard: Box::new(Guard {
                shutdown,
                _thread: handle,
                _tempdir: tempdir,
            }),
        }
    }

    /// Spin up nostr-relay 0.4.8 with its LMDB data directory on tmpfs.
    ///
    /// Uses /dev/shm when available so the DB writes hit RAM rather than disk
    /// — this neutralizes storage latency as a variable. nostr-relay still
    /// runs its full DB code path (serialization, LMDB bookkeeping, the
    /// batched writer actor); just the final fsync lands in tmpfs.
    pub fn spawn_nostr_relay(http_workers: usize) -> Self {
        struct Guard {
            server_handle: actix_web::dev::ServerHandle,
            thread: Option<std::thread::JoinHandle<()>>,
            _tempdir: tempfile::TempDir,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                // Ask actix to stop; then join the thread.
                let handle = self.server_handle.clone();
                let _ = std::thread::spawn(move || {
                    let sys = actix_rt::System::new();
                    sys.block_on(handle.stop(false));
                })
                .join();
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
            }
        }

        let tmp_root = if PathBuf::from("/dev/shm").is_dir() {
            Some(PathBuf::from("/dev/shm"))
        } else {
            None
        };
        let mut builder = tempfile::Builder::new();
        builder.prefix("nostr-relay-bench-");
        let tempdir = if let Some(root) = tmp_root {
            builder.tempdir_in(root).expect("tempdir on tmpfs")
        } else {
            builder.tempdir().expect("tempdir")
        };

        // Bind ourselves so we can learn the assigned port before handing the
        // listener to actix. nostr-relay's App::web_server only takes (host, port).
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0u16)).expect("bind");
        listener.set_nonblocking(true).ok();
        let port = listener.local_addr().unwrap().port();

        let data_path = tempdir.path().to_path_buf();

        // actix needs its own runtime on a dedicated thread.
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("nostr-relay-bench".into())
            .spawn(move || {
                let sys = actix_rt::System::new();
                sys.block_on(async move {
                    // App::create with data_path override pins the DB at
                    // our tempdir (on tmpfs). Defaults everywhere else.
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

        Relay {
            port,
            _guard: Box::new(Guard {
                server_handle,
                thread: Some(thread),
                _tempdir: tempdir,
            }),
        }
    }
}
