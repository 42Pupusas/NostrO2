//! Reader IO thread: io_uring recv on all client fds, decode WebSocket frames.
//!
//! Accepts new client fds from the listener via SPSC ring, keeps a recv SQE
//! in-flight per client. On completion: decode frames via coyoquil (Role::Server)
//! → push ClientMessages to the shared MPSC ring.

use crate::{ClientMessage, Handler, HandlerResult, Parker, WriteCmd};
use coyoquil::{DEFAULT_MAX_MESSAGE_SIZE, Frame, FrameDecoder, Role};
use quetzalcoatl::mpsc::Producer;
use quetzalcoatl::spsc;
use ququmatz::types::{MsgFlags, TimeoutFlags};
use ququmatz::{IoUring, Sqe, Timespec};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Sentinel user_data for the periodic timeout SQE.
const TIMEOUT_UD: u64 = u64::MAX;

/// Per-client state held by the reader thread.
struct ClientSlot {
    fd: i32,
    decoder: FrameDecoder<DEFAULT_MAX_MESSAGE_SIZE>,
    recv_buf: Vec<u8>,
    dead: bool,
    recv_pending: bool,
}

pub fn reader_thread(
    accept_rx: spsc::Consumer<i32>,
    event_tx: Producer<ClientMessage>,
    writer_txs: Vec<Producer<WriteCmd>>,
    writer_wakers: Vec<std::thread::Thread>,
    consumer_waker: Arc<Parker>,
    shutdown: Arc<AtomicBool>,
    handler: Option<Arc<Handler>>,
) {
    if let Err(e) = reader_loop(
        accept_rx,
        &event_tx,
        &writer_txs,
        &writer_wakers,
        &consumer_waker,
        &shutdown,
        handler.as_deref(),
    ) {
        eprintln!("reader IO thread fatal: {e}");
    }
}

fn reader_loop(
    mut accept_rx: spsc::Consumer<i32>,
    event_tx: &Producer<ClientMessage>,
    writer_txs: &[Producer<WriteCmd>],
    writer_wakers: &[std::thread::Thread],
    consumer_waker: &Parker,
    shutdown: &AtomicBool,
    handler: Option<&Handler>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let num_writer_shards = writer_txs.len();
    let mut ring = IoUring::new(4096)?;
    let mut slots: Vec<ClientSlot> = Vec::new();

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // 1. Accept new clients from the listener thread
        while let Some(fd) = accept_rx.pop() {
            let new_slot = ClientSlot {
                fd,
                decoder: FrameDecoder::new(Role::Server),
                recv_buf: vec![0u8; 65536],
                dead: false,
                recv_pending: false,
            };
            let idx = if let Some(i) = slots.iter().position(|s| s.dead && !s.recv_pending) {
                slots[i] = new_slot;
                i
            } else {
                let i = slots.len();
                slots.push(new_slot);
                i
            };
            // Best-effort submit — if SQ is full, recv_pending stays false
            // and the slot will be retried in the resubmit pass below.
            let _ = submit_recv(&mut ring, &mut slots[idx], idx as u64);

            // When a handler is set, register the fd with the writer
            // directly from the reader thread so the writer knows about
            // this client before any inline replies arrive.
            if handler.is_some() {
                let shard = fd as usize % num_writer_shards;
                let _ = writer_txs[shard].push(WriteCmd::Register { fd });
                writer_wakers[shard].unpark();
            }

            push_event(event_tx, consumer_waker, ClientMessage::Connected { client_id: fd });
        }

        // 2. Resubmit recv for any slots that don't have one in-flight
        for (idx, slot) in slots.iter_mut().enumerate() {
            if !slot.recv_pending && !slot.dead {
                let _ = submit_recv(&mut ring, slot, idx as u64);
            }
        }

        if !slots.iter().any(|s| !s.dead) {
            std::thread::park_timeout(std::time::Duration::from_millis(1));
            continue;
        }

        // 3. Submit a timeout so we wake up periodically to check accept_rx,
        //    then wait for at least one completion (recv or timeout).
        let ts = Timespec::from_millis(5);
        let timeout_sqe = unsafe {
            Sqe::timeout(&raw const ts, 0, TimeoutFlags::default())
        }
        .user_data(TIMEOUT_UD);
        ring.push(timeout_sqe)?;
        ring.submit_and_wait(1)?;

        // 4. Drain completions
        while let Some(cqe) = ring.complete() {
            if cqe.user_data == TIMEOUT_UD {
                // Timeout expired — loop back to check accept_rx
                continue;
            }
            let idx = cqe.user_data as usize;
            if idx >= slots.len() {
                continue;
            }

            let slot = &mut slots[idx];
            slot.recv_pending = false;

            if slot.dead {
                continue;
            }

            let n = if cqe.is_err() {
                mark_dead(slot, event_tx, consumer_waker, Some(format!("recv error: {}", cqe.result)));
                continue;
            } else {
                let n = cqe.result as usize;
                if n == 0 {
                    mark_dead(slot, event_tx, consumer_waker, Some("connection closed (EOF)".into()));
                    continue;
                }
                n
            };

            // Decode WebSocket frames
            if slot.decoder.push(&slot.recv_buf[..n]).is_err() {
                mark_dead(slot, event_tx, consumer_waker, Some("WebSocket frame decode error".into()));
                continue;
            }

            while let Ok(Some(frame)) = slot.decoder.next_frame() {
                match frame {
                    Frame::Text(text) => {
                        if let Some(h) = handler {
                            match h(slot.fd, text) {
                                HandlerResult::Reply(response) => {
                                    let shard = slot.fd as usize % num_writer_shards;
                                    let _ = writer_txs[shard].push(WriteCmd::SendText {
                                        fd: slot.fd,
                                        text: response,
                                    });
                                    writer_wakers[shard].unpark();
                                }
                                HandlerResult::Consumed => {}
                                HandlerResult::PassThrough => {
                                    push_event(
                                        event_tx,
                                        consumer_waker,
                                        ClientMessage::Text {
                                            client_id: slot.fd,
                                            text: text.to_string(),
                                        },
                                    );
                                }
                            }
                        } else {
                            push_event(
                                event_tx,
                                consumer_waker,
                                ClientMessage::Text {
                                    client_id: slot.fd,
                                    text: text.to_string(),
                                },
                            );
                        }
                    }
                    Frame::Binary(data) => {
                        push_event(
                            event_tx,
                            consumer_waker,
                            ClientMessage::Binary {
                                client_id: slot.fd,
                                data: data.to_vec(),
                            },
                        );
                    }
                    Frame::Ping(_) => {
                        let shard = slot.fd as usize % num_writer_shards;
                        let _ = writer_txs[shard].push(WriteCmd::Pong { fd: slot.fd });
                        writer_wakers[shard].unpark();
                    }
                    Frame::Close(_) => {
                        mark_dead(slot, event_tx, consumer_waker, None);
                        break;
                    }
                    _ => {}
                }
            }

        }

        ring.sync_cq();

        // 5. Close fds of dead slots immediately to prevent fd leaks.
        // Uses Socket::from_fd → drop (synchronous close) rather than
        // Sqe::close, because we need the fds freed NOW, not queued.
        for slot in slots.iter_mut() {
            if slot.dead && slot.fd >= 0 {
                drop(unsafe { ququmatz::Socket::from_fd(slot.fd) });
                slot.fd = -1;
            }
        }
    }

    // Cancel all in-flight recv SQEs so the kernel releases our buffers
    // before they go out of scope.
    let mut pending = 0;
    for (idx, slot) in slots.iter().enumerate() {
        if slot.recv_pending && !slot.dead {
            let _ = ring.push(Sqe::cancel(idx as u64).user_data(TIMEOUT_UD));
            pending += 1;
        }
    }
    if pending > 0 {
        let _ = ring.submit_and_wait(pending);
        while ring.complete().is_some() {}
    }

    // Close remaining client fds synchronously.
    for slot in &slots {
        if slot.fd >= 0 {
            drop(unsafe { ququmatz::Socket::from_fd(slot.fd) });
        }
    }

    Ok(())
}

fn submit_recv(
    ring: &mut IoUring,
    slot: &mut ClientSlot,
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

/// Push a message to the event ring with backpressure.
/// Spins briefly, then yields, preventing silent message drops that would
/// cause downstream consumers to hang waiting for events that were lost.
fn push_event(event_tx: &Producer<ClientMessage>, consumer_waker: &Parker, mut msg: ClientMessage) {
    let mut spins = 0u32;
    loop {
        match event_tx.push(msg) {
            Ok(()) => {
                consumer_waker.unpark();
                return;
            }
            Err(returned) => {
                msg = returned;
                consumer_waker.unpark();
                if spins < 64 {
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
                spins = spins.saturating_add(1);
            }
        }
    }
}

fn mark_dead(
    slot: &mut ClientSlot,
    event_tx: &Producer<ClientMessage>,
    consumer_waker: &Parker,
    reason: Option<String>,
) {
    slot.dead = true;
    push_event(
        event_tx,
        consumer_waker,
        ClientMessage::Disconnected {
            client_id: slot.fd,
            reason,
        },
    );
}
