//! Reader IO thread: io_uring recv on all client fds, decode WebSocket frames.
//!
//! Split in two:
//! - [`ReaderCore`] owns the io_uring ring, per-client slot state, and frame
//!   decoders. It does I/O, lifetime/cleanup, and frame decoding, emitting
//!   decoded events through a user-provided callback.
//! - [`reader_thread`] is the thin driver that constructs a core and wires
//!   the callback to today's routing (Handler dispatch, MPSC event push,
//!   writer-shard routing for Pong).
//!
//! The core's callback API is zero-copy: text/binary events borrow directly
//! from the recv buffer or the decompress scratch. Callbacks must consume
//! in-place.

use crate::{AcceptedClient, ClientMessage, Handler, HandlerResult, Parker, WriteCmd};
use coyoquil::{CloseCode, DEFAULT_MAX_MESSAGE_SIZE, DeflateDecoder, Frame, FrameDecoder, Opcode, Role};
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
    deflate_decoder: Option<DeflateDecoder>,
    recv_buf: Vec<u8>,
    dead: bool,
    recv_pending: bool,
}

/// Frame-level event emitted by [`ReaderCore`] to its callback.
///
/// Text and binary payloads borrow from scratch buffers owned by the core —
/// they are only valid for the duration of the callback call. Callbacks must
/// copy before stashing.
pub enum ReaderEvent<'a> {
    Connected {
        fd: i32,
        path: String,
        subprotocol: Option<String>,
        headers: Vec<(String, String)>,
    },
    Text {
        fd: i32,
        text: &'a str,
    },
    Binary {
        fd: i32,
        data: &'a [u8],
    },
    /// Client sent a Ping. Callback is responsible for queuing a Pong on the
    /// appropriate writer shard.
    Ping {
        fd: i32,
    },
    Disconnected {
        fd: i32,
        reason: Option<String>,
        close_code: Option<CloseCode>,
    },
}

/// Owns the io_uring ring, slot state, and frame decoders for one reader shard.
///
/// Drives I/O and framing mechanically; all routing/dispatch decisions are in
/// the callback the driver passes to [`ReaderCore::poll_once`].
pub struct ReaderCore {
    ring: IoUring,
    slots: Vec<ClientSlot>,
    decompress_buf: Vec<u8>,
}

impl ReaderCore {
    pub fn new(ring_capacity: u32) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self {
            ring: IoUring::new(ring_capacity)?,
            slots: Vec::new(),
            decompress_buf: Vec::new(),
        })
    }

    /// Register a freshly-accepted client. Sets up the WebSocket frame decoder
    /// (and optional deflate decoder), reuses a dead slot if available, and
    /// submits the first recv SQE. Emits a `Connected` event through `cb`.
    pub fn accept<F>(&mut self, client: AcceptedClient, mut cb: F)
    where
        F: FnMut(ReaderEvent<'_>),
    {
        let fd = client.fd;
        let deflate_config = client.deflate.clone();

        let mut frame_decoder = FrameDecoder::new(Role::Server);
        let deflate_decoder = deflate_config.as_ref().map(|config| {
            frame_decoder.set_allowed_rsv(0x40);
            DeflateDecoder::new(config, true, DEFAULT_MAX_MESSAGE_SIZE)
        });

        let new_slot = ClientSlot {
            fd,
            decoder: frame_decoder,
            deflate_decoder,
            recv_buf: vec![0u8; 65536],
            dead: false,
            recv_pending: false,
        };
        let idx = if let Some(i) = self.slots.iter().position(|s| s.dead && !s.recv_pending) {
            self.slots[i] = new_slot;
            i
        } else {
            let i = self.slots.len();
            self.slots.push(new_slot);
            i
        };
        // Best-effort submit — if SQ is full, recv_pending stays false
        // and the slot will be retried in poll_once's resubmit pass.
        let _ = submit_recv(&mut self.ring, &mut self.slots[idx], idx as u64);

        cb(ReaderEvent::Connected {
            fd,
            path: client.path,
            subprotocol: client.subprotocol,
            headers: client.headers,
        });
    }

    /// Return true if the core currently has no live slots.
    pub fn is_idle(&self) -> bool {
        !self.slots.iter().any(|s| !s.dead)
    }

    /// One iteration of the I/O loop: resubmit missing recvs, submit a timeout,
    /// wait for a completion, drain completions, emit decoded events, close
    /// dead fds.
    pub fn poll_once<F>(
        &mut self,
        mut cb: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnMut(ReaderEvent<'_>),
    {
        // Resubmit recv for any slots that don't have one in-flight.
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            if !slot.recv_pending && !slot.dead {
                let _ = submit_recv(&mut self.ring, slot, idx as u64);
            }
        }

        // Submit a timeout so we wake periodically even with no recv traffic,
        // giving the driver a chance to poll accept_rx / shutdown.
        let ts = Timespec::from_millis(5);
        let timeout_sqe = unsafe { Sqe::timeout(&raw const ts, 0, TimeoutFlags::default()) }
            .user_data(TIMEOUT_UD);
        self.ring.push(timeout_sqe)?;
        self.ring.submit_and_wait(1)?;

        // Drain completions.
        while let Some(cqe) = self.ring.complete() {
            if cqe.user_data == TIMEOUT_UD {
                continue;
            }
            let idx = cqe.user_data as usize;
            if idx >= self.slots.len() {
                continue;
            }

            let slot = &mut self.slots[idx];
            slot.recv_pending = false;

            if slot.dead {
                continue;
            }

            let n = if cqe.is_err() {
                mark_dead_emit(slot, &mut cb, Some(format!("recv error: {}", cqe.result)), None);
                continue;
            } else {
                let n = cqe.result as usize;
                if n == 0 {
                    mark_dead_emit(slot, &mut cb, Some("connection closed (EOF)".into()), None);
                    continue;
                }
                n
            };

            if slot.decoder.push(&slot.recv_buf[..n]).is_err() {
                mark_dead_emit(slot, &mut cb, Some("WebSocket frame decode error".into()), None);
                continue;
            }

            loop {
                let rsv1 = slot.decoder.message_rsv1();
                let Some(frame) = slot.decoder.next_frame().transpose() else {
                    break;
                };
                let frame = match frame {
                    Ok(f) => f,
                    Err(_) => {
                        mark_dead_emit(slot, &mut cb, Some("WebSocket frame decode error".into()), None);
                        break;
                    }
                };

                match frame {
                    Frame::Text(text) => {
                        cb(ReaderEvent::Text { fd: slot.fd, text });
                    }
                    Frame::Binary(data) if rsv1 && slot.deflate_decoder.is_some() => {
                        self.decompress_buf.clear();
                        if slot
                            .deflate_decoder
                            .as_mut()
                            .unwrap()
                            .decompress(data, &mut self.decompress_buf)
                            .is_err()
                        {
                            mark_dead_emit(slot, &mut cb, Some("deflate decompression failed".into()), None);
                            break;
                        }

                        if slot.decoder.message_opcode() == Some(Opcode::Text) {
                            match std::str::from_utf8(&self.decompress_buf) {
                                Ok(text) => {
                                    cb(ReaderEvent::Text { fd: slot.fd, text });
                                }
                                Err(_) => {
                                    mark_dead_emit(
                                        slot,
                                        &mut cb,
                                        Some("invalid UTF-8 in decompressed text".into()),
                                        None,
                                    );
                                    break;
                                }
                            }
                        } else {
                            cb(ReaderEvent::Binary {
                                fd: slot.fd,
                                data: &self.decompress_buf,
                            });
                        }
                    }
                    Frame::Binary(data) => {
                        cb(ReaderEvent::Binary { fd: slot.fd, data });
                    }
                    Frame::Ping(_) => {
                        cb(ReaderEvent::Ping { fd: slot.fd });
                    }
                    Frame::Close(close_info) => {
                        let close_code = close_info.map(|(code, _)| code);
                        mark_dead_emit(slot, &mut cb, None, close_code);
                        break;
                    }
                    _ => {}
                }
            }
        }

        self.ring.sync_cq();

        // Close fds of dead slots immediately to prevent fd leaks.
        // Synchronous close (not Sqe::close) so fds are freed now.
        for slot in self.slots.iter_mut() {
            if slot.dead && slot.fd >= 0 {
                drop(unsafe { ququmatz::Socket::from_fd(slot.fd) });
                slot.fd = -1;
            }
        }

        Ok(())
    }

    /// Park the thread briefly when there are no live slots to service.
    pub fn park_timeout(&self) {
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }

    /// Cancel any in-flight recvs and close any remaining client fds.
    /// Call before dropping the core at shutdown.
    pub fn shutdown(&mut self) {
        let mut pending = 0;
        for (idx, slot) in self.slots.iter().enumerate() {
            if slot.recv_pending && !slot.dead {
                let _ = self.ring.push(Sqe::cancel(idx as u64).user_data(TIMEOUT_UD));
                pending += 1;
            }
        }
        if pending > 0 {
            let _ = self.ring.submit_and_wait(pending);
            while self.ring.complete().is_some() {}
        }
        for slot in &self.slots {
            if slot.fd >= 0 {
                drop(unsafe { ququmatz::Socket::from_fd(slot.fd) });
            }
        }
    }
}

pub fn reader_thread(
    accept_rx: spsc::Consumer<AcceptedClient>,
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
    mut accept_rx: spsc::Consumer<AcceptedClient>,
    event_tx: &Producer<ClientMessage>,
    writer_txs: &[Producer<WriteCmd>],
    writer_wakers: &[std::thread::Thread],
    consumer_waker: &Parker,
    shutdown: &AtomicBool,
    handler: Option<&Handler>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let num_writer_shards = writer_txs.len();
    let mut core = ReaderCore::new(4096)?;

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // Accept new clients. On Connected we both push Register into the
        // writer ring and emit ClientMessage::Connected to the event ring.
        while let Some(client) = accept_rx.pop() {
            let fd = client.fd;
            let deflate_config = client.deflate.clone();
            core.accept(client, |event| {
                dispatch_event(
                    event,
                    handler,
                    writer_txs,
                    writer_wakers,
                    num_writer_shards,
                    event_tx,
                    consumer_waker,
                );
            });
            // Register fd with the writer so it knows about this client
            // before any sends arrive.
            let shard = fd as usize % num_writer_shards;
            let _ = writer_txs[shard].push(WriteCmd::Register {
                fd,
                deflate: deflate_config,
            });
            writer_wakers[shard].unpark();
        }

        if core.is_idle() {
            core.park_timeout();
            continue;
        }

        core.poll_once(|event| {
            dispatch_event(
                event,
                handler,
                writer_txs,
                writer_wakers,
                num_writer_shards,
                event_tx,
                consumer_waker,
            );
        })?;
    }

    core.shutdown();
    Ok(())
}

/// Route a ReaderEvent to the old ClientMessage / WriteCmd rings and invoke
/// the inline Handler where applicable. This is the thin adapter that keeps
/// today's public behavior unchanged while ReaderCore moves underneath.
fn dispatch_event(
    event: ReaderEvent<'_>,
    handler: Option<&Handler>,
    writer_txs: &[Producer<WriteCmd>],
    writer_wakers: &[std::thread::Thread],
    num_writer_shards: usize,
    event_tx: &Producer<ClientMessage>,
    consumer_waker: &Parker,
) {
    match event {
        ReaderEvent::Connected {
            fd,
            path,
            subprotocol,
            headers,
        } => {
            push_event(
                event_tx,
                consumer_waker,
                ClientMessage::Connected {
                    client_id: fd,
                    path,
                    subprotocol,
                    headers,
                },
            );
        }
        ReaderEvent::Text { fd, text } => {
            handle_text(
                fd,
                text,
                handler,
                writer_txs,
                writer_wakers,
                num_writer_shards,
                event_tx,
                consumer_waker,
            );
        }
        ReaderEvent::Binary { fd, data } => {
            push_event(
                event_tx,
                consumer_waker,
                ClientMessage::Binary {
                    client_id: fd,
                    data: data.to_vec(),
                },
            );
        }
        ReaderEvent::Ping { fd } => {
            let shard = fd as usize % num_writer_shards;
            let _ = writer_txs[shard].push(WriteCmd::Pong { fd });
            writer_wakers[shard].unpark();
        }
        ReaderEvent::Disconnected {
            fd,
            reason,
            close_code,
        } => {
            push_event(
                event_tx,
                consumer_waker,
                ClientMessage::Disconnected {
                    client_id: fd,
                    reason,
                    close_code,
                },
            );
        }
    }
}

/// Process a decoded text message through the handler or push to event queue.
#[allow(clippy::too_many_arguments)]
fn handle_text(
    fd: i32,
    text: &str,
    handler: Option<&Handler>,
    writer_txs: &[Producer<WriteCmd>],
    writer_wakers: &[std::thread::Thread],
    num_writer_shards: usize,
    event_tx: &Producer<ClientMessage>,
    consumer_waker: &Parker,
) {
    if let Some(h) = handler {
        match h(fd, text) {
            HandlerResult::Reply(response) => {
                let shard = fd as usize % num_writer_shards;
                let _ = writer_txs[shard].push(WriteCmd::SendText {
                    fd,
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
                        client_id: fd,
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
                client_id: fd,
                text: text.to_string(),
            },
        );
    }
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

/// Mark a slot dead and emit a Disconnected event through the core's callback.
fn mark_dead_emit<F>(
    slot: &mut ClientSlot,
    cb: &mut F,
    reason: Option<String>,
    close_code: Option<CloseCode>,
) where
    F: FnMut(ReaderEvent<'_>),
{
    slot.dead = true;
    cb(ReaderEvent::Disconnected {
        fd: slot.fd,
        reason,
        close_code,
    });
}
