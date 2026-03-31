//! Global reader IO thread: one io_uring instance for all relay fds.
//!
//! Keeps a recv SQE in-flight per registered fd. On completion:
//! decode frames via coyoquil → push to MPSC ring → immediately resubmit recv.
//! Falls back to libc recvmsg for kTLS EIO (non-data TLS records).

use crate::PoolMessage;
use coyoquil::{DEFAULT_MAX_MESSAGE_SIZE, Frame, FrameDecoder, Role};
use nostro2::NostrRelayEvent;
use quetzalcoatl::mpsc::{Consumer, Producer};
use ququmatz::types::MsgFlags;
use ququmatz::{IoUring, Sqe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ktls;

const EIO: i32 = 5;

/// Command sent from the pool to the reader thread to register a new fd.
pub struct ReaderAdd {
    pub fd: i32,
    pub relay_url: Arc<str>,
    pub pong_tx: Producer<Vec<u8>>,
    pub shutdown: Arc<AtomicBool>,
}

/// Per-connection state held by the reader thread.
struct ReaderSlot {
    fd: i32,
    relay_url: Arc<str>,
    decoder: FrameDecoder<DEFAULT_MAX_MESSAGE_SIZE>,
    recv_buf: Vec<u8>,
    pong_tx: Producer<Vec<u8>>,
    shutdown: Arc<AtomicBool>,
    /// Whether this slot has a recv SQE in-flight.
    recv_pending: bool,
    /// Marked dead after close/error — will be cleaned up.
    dead: bool,
}

/// Run the global reader IO loop. Blocks until the global shutdown flag is set.
pub fn reader_thread(
    mut cmd_rx: Consumer<ReaderAdd>,
    event_tx: Producer<PoolMessage>,
    global_shutdown: Arc<AtomicBool>,
) {
    if let Err(e) = reader_loop(&mut cmd_rx, &event_tx, &global_shutdown) {
        eprintln!("reader IO thread fatal: {e}");
    }
}

fn reader_loop(
    cmd_rx: &mut Consumer<ReaderAdd>,
    event_tx: &Producer<PoolMessage>,
    global_shutdown: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ring = IoUring::new(256)?;
    let mut slots: Vec<ReaderSlot> = Vec::new();
    let mut needs_resubmit = false;

    loop {
        if global_shutdown.load(Ordering::Relaxed) {
            break;
        }

        // 1. Accept new connections
        while let Some(cmd) = cmd_rx.pop() {
            let idx = slots.len() as u64;
            slots.push(ReaderSlot {
                fd: cmd.fd,
                relay_url: cmd.relay_url,
                decoder: FrameDecoder::new(Role::Client),
                recv_buf: vec![0u8; 65536],
                pong_tx: cmd.pong_tx,
                shutdown: cmd.shutdown,
                recv_pending: false,
                dead: false,
            });
            submit_recv(&mut ring, &mut slots[idx as usize], idx)?;
        }

        // 2. Resubmit recv for slots that need it (after completions were processed)
        if needs_resubmit {
            for (idx, slot) in slots.iter_mut().enumerate() {
                if !slot.recv_pending && !slot.dead {
                    submit_recv(&mut ring, slot, idx as u64)?;
                }
            }
            needs_resubmit = false;
        }

        if !slots.iter().any(|s| !s.dead) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }

        // 3. Submit and block until at least one completion
        ring.submit_and_wait(1)?;

        // 4. Drain all completions, resubmit recv immediately for each
        while let Some(cqe) = ring.complete() {
            let idx = cqe.user_data as usize;
            if idx >= slots.len() {
                continue;
            }

            // Split borrow: process this slot while keeping ring accessible
            let slot = &mut slots[idx];
            slot.recv_pending = false;

            if slot.dead {
                continue;
            }

            // Handle recv result
            let n = if cqe.is_err() {
                let errno = -cqe.result;
                if errno == EIO {
                    match ktls::ktls_read(slot.fd, &mut slot.recv_buf) {
                        Ok(0) => {
                            mark_dead(slot, event_tx);
                            continue;
                        }
                        Ok(n) => n,
                        Err(_) => {
                            mark_dead(slot, event_tx);
                            continue;
                        }
                    }
                } else {
                    mark_dead(slot, event_tx);
                    continue;
                }
            } else {
                let n = cqe.result as usize;
                if n == 0 {
                    mark_dead(slot, event_tx);
                    continue;
                }
                n
            };

            // Decode frames
            if slot.decoder.push(&slot.recv_buf[..n]).is_err() {
                mark_dead(slot, event_tx);
                continue;
            }

            while let Ok(Some(frame)) = slot.decoder.next_frame() {
                match frame {
                    Frame::Text(text) => {
                        if let Ok(event) = text.parse::<NostrRelayEvent>() {
                            let msg = PoolMessage::RelayEvent {
                                relay_url: slot.relay_url.to_string(),
                                event,
                            };
                            // Spin-push into the MPSC ring
                            let mut msg = msg;
                            loop {
                                match event_tx.push(msg) {
                                    Ok(()) => break,
                                    Err(returned) => {
                                        msg = returned;
                                        std::hint::spin_loop();
                                    }
                                }
                            }
                        }
                    }
                    Frame::Ping(data) => {
                        let _ = slot.pong_tx.push(data.to_vec());
                    }
                    Frame::Close(_) => {
                        mark_dead(slot, event_tx);
                        break;
                    }
                    _ => {}
                }
            }

            // Mark that we need to resubmit recvs after draining completions.
            // We can't resubmit inside this loop because ring.complete() borrows ring,
            // and submit_recv also needs &mut ring.
            needs_resubmit = true;
        }

        ring.sync_cq();
    }

    Ok(())
}

fn submit_recv(
    ring: &mut IoUring,
    slot: &mut ReaderSlot,
    user_data: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sqe = unsafe {
        Sqe::recv(
            slot.fd,
            slot.recv_buf.as_mut_ptr(),
            slot.recv_buf.len() as u32,
            MsgFlags::default(),
        )
    }
    .user_data(user_data);
    ring.push(sqe)?;
    slot.recv_pending = true;
    Ok(())
}

fn mark_dead(slot: &mut ReaderSlot, event_tx: &Producer<PoolMessage>) {
    slot.dead = true;
    slot.shutdown.store(true, Ordering::Relaxed);
    let _ = event_tx.push(PoolMessage::ConnectionClosed {
        relay_url: slot.relay_url.to_string(),
        error: None,
    });
}
