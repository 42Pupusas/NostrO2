//! Purely event-driven auto-reconnection with exponential backoff.
//!
//! Lock-free: the pool communicates with the reconnect thread via ring
//! buffers (MPSC for commands, SPSC for results). The reconnect thread
//! exclusively owns its relay state — no shared mutable data.
//!
//! The thread parks until unparked by:
//!  - The pool (after seeing `ConnectionClosed` in recv)
//!  - A `park_timeout` expiring when a backoff deadline is reached
//! No polling, no extra threads — zero CPU when idle.

use crate::ktls;
use crate::reader::ReaderAdd;
use crate::writer::WriterAdd;
use crate::Parker;
use crate::syscall;
use coyoquil::{Frame, MaskKey};
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// When this relay is eligible for a reconnection attempt.
    /// `None` means it can be attempted immediately (or is alive).
    retry_after: Option<Instant>,
}

impl ManagedRelay {
    fn is_dead(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

/// Everything the reconnect thread needs to operate.
pub(crate) struct ReconnectContext {
    pub cmd_rx: mpsc::Consumer<ReconnectCmd>,
    pub result_tx: spsc::Producer<ReconnectResult>,
    pub reader_txs: Vec<mpsc::Producer<ReaderAdd>>,
    pub writer_tx: mpsc::Producer<WriterAdd>,
    pub broadcast_consumer: broadcast::Consumer<String>,
    pub waker: Arc<Parker>,
    pub writer_waker: std::thread::Thread,
    pub global_shutdown: Arc<AtomicBool>,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

/// Purely event-driven reconnect loop. Parks until unparked or a
/// backoff deadline expires — no extra threads, no polling.
pub(crate) fn reconnect_thread(mut ctx: ReconnectContext) {
    let num_shards = ctx.reader_txs.len();
    let mut relays: Vec<ManagedRelay> = Vec::new();
    let mut subscriptions: HashMap<String, String> = HashMap::new();

    loop {
        if ctx.global_shutdown.load(Ordering::Acquire) {
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
                        retry_after: None,
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

        // 2. Attempt reconnection for dead relays whose backoff has expired
        let now = Instant::now();
        for relay in relays.iter_mut() {
            if !relay.is_dead() {
                continue;
            }

            // Check if backoff deadline hasn't passed yet
            if let Some(deadline) = relay.retry_after {
                if now < deadline {
                    continue;
                }
            }

            match ktls::connect(&relay.url) {
                Ok(ktls_conn) => {
                    let new_fd = ktls_conn.into_raw_fd();

                    // Close old fd
                    let old_fd = relay.fd;
                    unsafe {
                        syscall::shutdown(old_fd, syscall::SHUT_RDWR);
                        syscall::close(old_fd);
                    }

                    // Re-send tracked subscriptions directly to this relay only
                    if !subscriptions.is_empty() {
                        let mut buf = Vec::new();
                        for sub_json in subscriptions.values() {
                            Frame::Text(sub_json).encode_masked(MaskKey::new(), &mut buf);
                        }
                        if ktls::write_all_fd(new_fd, &buf).is_err() {
                            unsafe {
                                syscall::shutdown(new_fd, syscall::SHUT_RDWR);
                                syscall::close(new_fd);
                            }
                            relay.retry_after = Some(Instant::now() + relay.backoff);
                            relay.backoff = (relay.backoff * 2).min(ctx.max_backoff);
                            continue;
                        }
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
                        writer_waker: ctx.writer_waker.clone(),
                    };
                    if ctx.reader_txs[shard_idx].push(reader_cmd).is_err() {
                        unsafe {
                            syscall::shutdown(new_fd, syscall::SHUT_RDWR);
                            syscall::close(new_fd);
                        }
                        relay.retry_after = Some(Instant::now() + relay.backoff);
                        relay.backoff = (relay.backoff * 2).min(ctx.max_backoff);
                        continue;
                    }

                    let writer_cmd = WriterAdd {
                        fd: new_fd,
                        outbound,
                        pong_rx,
                        shutdown: Arc::clone(&new_shutdown),
                    };
                    if ctx.writer_tx.push(writer_cmd).is_err() {
                        new_shutdown.store(true, Ordering::Release);
                        unsafe {
                            syscall::shutdown(new_fd, syscall::SHUT_RDWR);
                            syscall::close(new_fd);
                        }
                        relay.retry_after = Some(Instant::now() + relay.backoff);
                        relay.backoff = (relay.backoff * 2).min(ctx.max_backoff);
                        continue;
                    }

                    // Success — reset backoff
                    relay.fd = new_fd;
                    relay.shutdown = new_shutdown;
                    relay.backoff = ctx.initial_backoff;
                    relay.retry_after = None;

                    let _ = ctx.result_tx.push(ReconnectResult {
                        url: relay.url.clone(),
                        fd: new_fd,
                        shutdown: Arc::clone(&relay.shutdown),
                    });
                    ctx.waker.unpark();
                }
                Err(_) => {
                    relay.retry_after = Some(Instant::now() + relay.backoff);
                    relay.backoff = (relay.backoff * 2).min(ctx.max_backoff);
                }
            }
        }

        // 3. Compute next wake time from pending retry deadlines
        let next_deadline = relays
            .iter()
            .filter(|r| r.is_dead())
            .filter_map(|r| r.retry_after)
            .min();

        match next_deadline {
            Some(deadline) => {
                let now = Instant::now();
                if deadline > now {
                    std::thread::park_timeout(deadline - now);
                }
                // else: deadline already passed, loop immediately
            }
            None => {
                // No pending retries — park until woken by pool
                std::thread::park();
            }
        }
    }
}
