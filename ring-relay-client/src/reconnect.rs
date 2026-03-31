//! Purely event-driven auto-reconnection with exponential backoff.
//!
//! Lock-free: the pool communicates with the reconnect thread via ring
//! buffers (MPSC for commands, SPSC for results). The reconnect thread
//! exclusively owns its relay state — no shared mutable data.
//!
//! The thread parks until unparked by:
//!  - The pool (after seeing `ConnectionClosed` in recv)
//!  - A backoff delay thread (after a failed reconnection attempt)
//! No polling, no timeouts — zero CPU when idle.

use crate::ktls;
use crate::reader::ReaderAdd;
use crate::writer::WriterAdd;
use crate::Parker;
use crate::syscall;
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ── Commands: pool → reconnect thread (MPSC ring) ──────────────────────

/// Commands sent from the pool to the reconnect thread.
pub(crate) enum ReconnectCmd {
    /// Start managing a relay for auto-reconnection.
    Add {
        url: String,
        fd: i32,
        shutdown: Arc<AtomicBool>,
        shard_idx: usize,
    },
    /// Stop managing a relay (user called `remove_relay`).
    Remove { url: String },
    /// Track a subscription for re-sending on reconnect.
    TrackSub { sub_id: String, json: String },
    /// Stop tracking a subscription.
    UntrackSub { sub_id: String },
}

// ── Results: reconnect thread → pool (SPSC ring) ───────────────────────

/// Sent back to the pool when a relay successfully reconnects.
pub(crate) struct ReconnectResult {
    pub url: String,
    pub fd: i32,
    pub shutdown: Arc<AtomicBool>,
}

// ── Reconnect thread internals ─────────────────────────────────────────

/// Per-relay state owned exclusively by the reconnect thread.
struct ManagedRelay {
    url: String,
    fd: i32,
    shutdown: Arc<AtomicBool>,
    shard_idx: usize,
    backoff: Duration,
    /// Whether a delay thread is already scheduled for this relay.
    retry_scheduled: bool,
}

impl ManagedRelay {
    fn is_dead(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

/// Everything the reconnect thread needs to operate.
pub(crate) struct ReconnectContext {
    pub cmd_rx: mpsc::Consumer<ReconnectCmd>,
    pub result_tx: spsc::Producer<ReconnectResult>,
    pub reader_txs: Vec<mpsc::Producer<ReaderAdd>>,
    pub writer_tx: mpsc::Producer<WriterAdd>,
    pub broadcast_consumer: broadcast::Consumer<String>,
    pub broadcast_producer: broadcast::Producer<String>,
    pub waker: Arc<Parker>,
    pub global_shutdown: Arc<AtomicBool>,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

/// Purely event-driven reconnect loop. Parks until unparked — never
/// uses timeouts or polling.
pub(crate) fn reconnect_thread(mut ctx: ReconnectContext) {
    let num_shards = ctx.reader_txs.len();
    let mut relays: Vec<ManagedRelay> = Vec::new();
    let mut subscriptions: HashMap<String, String> = HashMap::new();
    let this_thread = std::thread::current();

    loop {
        if ctx.global_shutdown.load(Ordering::Relaxed) {
            break;
        }

        // 1. Drain incoming commands
        while let Some(cmd) = ctx.cmd_rx.pop() {
            match cmd {
                ReconnectCmd::Add {
                    url,
                    fd,
                    shutdown,
                    shard_idx,
                } => {
                    relays.push(ManagedRelay {
                        url,
                        fd,
                        shutdown,
                        shard_idx,
                        backoff: ctx.initial_backoff,
                        retry_scheduled: false,
                    });
                }
                ReconnectCmd::Remove { url } => {
                    relays.retain(|r| r.url != url);
                }
                ReconnectCmd::TrackSub { sub_id, json } => {
                    subscriptions.insert(sub_id, json);
                }
                ReconnectCmd::UntrackSub { sub_id } => {
                    subscriptions.remove(&sub_id);
                }
            }
        }

        // 2. Attempt reconnection for all dead relays ready for retry
        for relay in relays.iter_mut() {
            if !relay.is_dead() || relay.retry_scheduled {
                continue;
            }

            match ktls::connect(&relay.url) {
                Ok(ktls_conn) => {
                    let new_fd = ktls_conn.fd;
                    std::mem::forget(ktls_conn);

                    // Close old fd
                    let old_fd = relay.fd;
                    unsafe {
                        syscall::shutdown(old_fd, syscall::SHUT_RDWR);
                        syscall::close(old_fd);
                    }

                    let new_shutdown = Arc::new(AtomicBool::new(false));
                    let url: Arc<str> = relay.url.as_str().into();

                    let (pong_tx, pong_rx) =
                        spsc::RingBuffer::<Vec<u8>>::new(Capacity::at_least(4)).split();
                    let outbound = ctx.broadcast_consumer.clone();
                    let shard_idx = relay.shard_idx % num_shards;

                    let reader_cmd = ReaderAdd {
                        fd: new_fd,
                        relay_url: Arc::clone(&url),
                        pong_tx,
                        shutdown: Arc::clone(&new_shutdown),
                        waker: Arc::clone(&ctx.waker),
                    };
                    if ctx.reader_txs[shard_idx].push(reader_cmd).is_err() {
                        unsafe {
                            syscall::shutdown(new_fd, syscall::SHUT_RDWR);
                            syscall::close(new_fd);
                        }
                        schedule_retry(relay, &this_thread, ctx.max_backoff);
                        continue;
                    }

                    let writer_cmd = WriterAdd {
                        fd: new_fd,
                        outbound,
                        pong_rx,
                        shutdown: Arc::clone(&new_shutdown),
                    };
                    if ctx.writer_tx.push(writer_cmd).is_err() {
                        new_shutdown.store(true, Ordering::Relaxed);
                        unsafe {
                            syscall::shutdown(new_fd, syscall::SHUT_RDWR);
                            syscall::close(new_fd);
                        }
                        schedule_retry(relay, &this_thread, ctx.max_backoff);
                        continue;
                    }

                    // Success
                    relay.fd = new_fd;
                    relay.shutdown = new_shutdown;
                    relay.backoff = ctx.initial_backoff;
                    relay.retry_scheduled = false;

                    let _ = ctx.result_tx.push(ReconnectResult {
                        url: relay.url.clone(),
                        fd: new_fd,
                        shutdown: Arc::clone(&relay.shutdown),
                    });
                    ctx.waker.unpark();

                    for sub_json in subscriptions.values() {
                        let _ = ctx.broadcast_producer.push(sub_json.clone());
                    }
                }
                Err(_) => {
                    schedule_retry(relay, &this_thread, ctx.max_backoff);
                }
            }
        }

        // Park until woken by pool or a backoff delay thread.
        std::thread::park();
    }
}

/// Spawn a lightweight delay thread that unparks the reconnect thread
/// after the backoff period, then doubles the backoff for next time.
fn schedule_retry(
    relay: &mut ManagedRelay,
    reconnect_thread: &std::thread::Thread,
    max_backoff: Duration,
) {
    relay.retry_scheduled = true;
    let delay = relay.backoff;
    relay.backoff = (relay.backoff * 2).min(max_backoff);
    let handle = reconnect_thread.clone();
    std::thread::Builder::new()
        .name("ring-backoff".into())
        .spawn(move || {
            std::thread::sleep(delay);
            handle.unpark();
        })
        .ok();
}
