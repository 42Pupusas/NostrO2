//! Global writer IO thread: one io_uring instance for all relay fds.
//!
//! Drains the broadcast ring for outbound messages and per-connection
//! pong queues, encodes frames via coyoquil, and submits batched
//! send SQEs through a single io_uring.

use coyoquil::{Frame, MaskKey};
use quetzalcoatl::broadcast;
use quetzalcoatl::mpsc::Consumer;
use quetzalcoatl::spsc;
use ququmatz::types::MsgFlags;
use ququmatz::{IoUring, Sqe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Command sent from the pool to the writer thread to register a new fd.
pub struct WriterAdd {
    pub fd: i32,
    pub outbound: broadcast::Consumer<String>,
    pub pong_rx: spsc::Consumer<Vec<u8>>,
    pub shutdown: Arc<AtomicBool>,
}

/// Per-connection state held by the writer thread.
struct WriterSlot {
    fd: i32,
    outbound: broadcast::Consumer<String>,
    pong_rx: spsc::Consumer<Vec<u8>>,
    shutdown: Arc<AtomicBool>,
    dead: bool,
    /// Buffered frame data waiting to be sent.
    send_buf: Vec<u8>,
    /// How many bytes of send_buf have been sent so far.
    send_offset: usize,
    /// Whether this slot has a send SQE in-flight.
    send_pending: bool,
}

/// Run the global writer IO loop. Blocks until the global shutdown flag is set.
pub fn writer_thread(mut cmd_rx: Consumer<WriterAdd>, global_shutdown: Arc<AtomicBool>) {
    if let Err(e) = writer_loop(&mut cmd_rx, &global_shutdown) {
        eprintln!("writer IO thread fatal: {e}");
    }
}

fn writer_loop(
    cmd_rx: &mut Consumer<WriterAdd>,
    global_shutdown: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ring = IoUring::new(256)?;
    let mut slots: Vec<WriterSlot> = Vec::new();

    loop {
        if global_shutdown.load(Ordering::Acquire) {
            // Send close frames to all live connections
            for slot in &mut slots {
                if !slot.dead {
                    send_close(&mut ring, slot)?;
                }
            }
            break;
        }

        // 1. Accept new connections
        while let Some(cmd) = cmd_rx.pop() {
            slots.push(WriterSlot {
                fd: cmd.fd,
                outbound: cmd.outbound,
                pong_rx: cmd.pong_rx,
                shutdown: cmd.shutdown,
                dead: false,
                send_buf: Vec::with_capacity(65536),
                send_offset: 0,
                send_pending: false,
            });
        }

        // 2. For each slot that isn't already sending, build a frame batch
        let mut any_work = false;
        for (idx, slot) in slots.iter_mut().enumerate() {
            if slot.dead || slot.send_pending {
                continue;
            }

            // Check per-connection shutdown
            if slot.shutdown.load(Ordering::Acquire) {
                slot.dead = true;
                continue;
            }

            slot.send_buf.clear();
            slot.send_offset = 0;

            // Pong responses (highest priority)
            while let Some(ping_data) = slot.pong_rx.pop() {
                Frame::Pong(&ping_data).encode_masked(MaskKey::new(), &mut slot.send_buf);
            }

            // Outbound messages
            while let Some(json) = slot.outbound.pop() {
                Frame::Text(&json).encode_masked(MaskKey::new(), &mut slot.send_buf);
            }

            if !slot.send_buf.is_empty() {
                submit_send(&mut ring, slot, idx as u64)?;
                any_work = true;
            }
        }

        // 3. Submit and wait for completions if we have pending sends
        if any_work {
            ring.submit_and_wait(1)?;

            while let Some(cqe) = ring.complete() {
                let idx = cqe.user_data as usize;
                if idx >= slots.len() {
                    continue;
                }
                let slot = &mut slots[idx];
                slot.send_pending = false;

                if slot.dead {
                    continue;
                }

                if cqe.is_err() {
                    slot.dead = true;
                    slot.shutdown.store(true, Ordering::Release);
                    continue;
                }

                let n = cqe.result as usize;
                slot.send_offset += n;

                // Partial send — resubmit remainder
                if slot.send_offset < slot.send_buf.len() {
                    submit_send(&mut ring, slot, idx as u64)?;
                    ring.submit()?;
                }
            }
        } else {
            // No outbound data — park briefly before checking again
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
    }

    Ok(())
}

fn submit_send(
    ring: &mut IoUring,
    slot: &mut WriterSlot,
    user_data: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data = &slot.send_buf[slot.send_offset..];
    let sqe = unsafe {
        Sqe::send(
            slot.fd,
            data.as_ptr(),
            data.len() as u32,
            MsgFlags::default(),
        )
    }
    .user_data(user_data);
    ring.push(sqe)?;
    slot.send_pending = true;
    Ok(())
}

fn send_close(
    ring: &mut IoUring,
    slot: &mut WriterSlot,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    slot.send_buf.clear();
    slot.send_offset = 0;
    Frame::Close(Some((1000, &[]))).encode_masked(MaskKey::new(), &mut slot.send_buf);
    let sqe = unsafe {
        Sqe::send(
            slot.fd,
            slot.send_buf.as_ptr(),
            slot.send_buf.len() as u32,
            MsgFlags::default(),
        )
    }
    .user_data(0);
    ring.push(sqe)?;
    ring.submit()?;
    // Best-effort, don't wait for completion
    Ok(())
}
