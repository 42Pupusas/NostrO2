//! Accept loop: io_uring accept → recv upgrade request → send 101 → hand fd to reader.
//!
//! The entire handshake runs through SQEs — no blocking syscalls. Each accepted
//! fd goes through a small state machine: Recv → parse HTTP → Send 101 → done.

use coyoquil::WsKey;
use quetzalcoatl::spsc;
use ququmatz::Socket;
use ququmatz::types::{AcceptFlags, MsgFlags, SockAddrIn, AF_INET, SOCK_STREAM, SOL_SOCKET, SO_REUSEADDR};
use ququmatz::{IoUring, Sqe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set up the TCP listener socket via `ququmatz::Socket`.
pub(crate) fn setup_listener(addr: [u8; 4], port: u16) -> Result<Socket, Box<dyn std::error::Error + Send + Sync>> {
    let sock = Socket::new(AF_INET, SOCK_STREAM, 0)?;
    sock.set_option(SOL_SOCKET, SO_REUSEADDR, &1i32)?;

    let sock_addr = SockAddrIn {
        sin_family: AF_INET as u16,
        sin_port: port.to_be(),
        sin_addr: u32::from_ne_bytes(addr),
        sin_zero: [0; 8],
    };
    sock.bind(&sock_addr)?;
    sock.listen(128)?;

    Ok(sock)
}

// ── Handshake state machine ────────────────────────────────────────────

/// Sentinel user_data for the accept SQE.
const ACCEPT_UD: u64 = u64::MAX;

/// Encode slot index + phase into user_data.
fn encode_ud(slot: usize, phase: Phase) -> u64 {
    (slot as u64) | ((phase as u64) << 48)
}

/// Decode user_data back to (slot, phase).
fn decode_ud(ud: u64) -> (usize, Phase) {
    let slot = (ud & 0x0000_FFFF_FFFF_FFFF) as usize;
    let phase = match ud >> 48 {
        1 => Phase::Recv,
        2 => Phase::Send,
        _ => Phase::Recv,
    };
    (slot, phase)
}

#[derive(Debug, Clone, Copy)]
#[repr(u64)]
enum Phase {
    Recv = 1,
    Send = 2,
}

struct HandshakeSlot {
    fd: i32,
    buf: Vec<u8>,
    /// How many bytes received so far (recv phase) or sent so far (send phase).
    progress: usize,
    /// Total bytes to send (set after building the 101 response).
    send_total: usize,
    active: bool,
}

impl HandshakeSlot {
    fn new(fd: i32) -> Self {
        Self {
            fd,
            buf: vec![0u8; 4096],
            progress: 0,
            send_total: 0,
            active: true,
        }
    }
}

// ── Accept loop ────────────────────────────────────────────────────────

pub fn listener_thread(
    listener_fd: i32,
    accept_tx: spsc::Producer<i32>,
    shutdown: Arc<AtomicBool>,
) {
    if let Err(e) = listener_loop(listener_fd, accept_tx, &shutdown) {
        eprintln!("listener thread fatal: {e}");
    }
}

fn listener_loop(
    listener_fd: i32,
    accept_tx: spsc::Producer<i32>,
    shutdown: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ring = IoUring::new(256)?;

    let mut slots: Vec<HandshakeSlot> = Vec::new();

    // Submit the first accept
    submit_accept(&mut ring, listener_fd)?;

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        ring.submit_and_wait(1)?;

        while let Some(cqe) = ring.complete() {
            if shutdown.load(Ordering::Acquire) {
                break;
            }

            if cqe.user_data == ACCEPT_UD {
                // ── Accept completion ──
                if cqe.is_err() {
                    // Accept failed — if shutting down, exit cleanly
                    if shutdown.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    eprintln!("accept error: {}", cqe.result);
                } else {
                    let client_fd = cqe.result;
                    let idx = alloc_slot(&mut slots, client_fd);
                    submit_handshake_recv(&mut ring, &mut slots[idx], idx)?;
                }
                // Resubmit accept for next connection
                submit_accept(&mut ring, listener_fd)?;
                continue;
            }

            let (idx, phase) = decode_ud(cqe.user_data);
            if idx >= slots.len() || !slots[idx].active {
                continue;
            }

            match phase {
                Phase::Recv => {
                    if cqe.is_err() || cqe.result <= 0 {
                        // Handshake recv failed — close and free slot
                        close_slot(&mut ring, &mut slots[idx]);
                        continue;
                    }

                    let n = cqe.result as usize;
                    slots[idx].progress += n;
                    let total = slots[idx].progress;

                    // Check for end of HTTP headers
                    if total >= 4 && slots[idx].buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        // Parse and build response
                        match build_upgrade_response(&slots[idx].buf[..total]) {
                            Ok(response) => {
                                let resp_bytes = response.into_bytes();
                                let send_len = resp_bytes.len();
                                slots[idx].buf.clear();
                                slots[idx].buf = resp_bytes;
                                slots[idx].progress = 0;
                                slots[idx].send_total = send_len;
                                submit_handshake_send(&mut ring, &mut slots[idx], idx)?;
                            }
                            Err(e) => {
                                eprintln!("handshake parse failed for fd {}: {e}", slots[idx].fd);
                                close_slot(&mut ring, &mut slots[idx]);
                            }
                        }
                    } else if total >= 4096 {
                        eprintln!("HTTP request too large for fd {}", slots[idx].fd);
                        close_slot(&mut ring, &mut slots[idx]);
                    } else {
                        // Need more data — resubmit recv for the remainder
                        submit_handshake_recv(&mut ring, &mut slots[idx], idx)?;
                    }
                }

                Phase::Send => {
                    if cqe.is_err() || cqe.result <= 0 {
                        close_slot(&mut ring, &mut slots[idx]);
                        continue;
                    }

                    let n = cqe.result as usize;
                    slots[idx].progress += n;

                    if slots[idx].progress >= slots[idx].send_total {
                        // Handshake complete — hand fd to reader.
                        // The reader thread emits Connected after registering
                        // the client with the writer, so send_text/broadcast
                        // are safe to call immediately after recv() returns it.
                        let client_fd = slots[idx].fd;
                        slots[idx].active = false;

                        if accept_tx.push(client_fd).is_err() {
                            eprintln!("accept ring full, dropping client {client_fd}");
                            ring.push(Sqe::close(client_fd).user_data(0))?;
                        }
                    } else {
                        // Partial send — resubmit for the rest
                        submit_handshake_send(&mut ring, &mut slots[idx], idx)?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn build_upgrade_response(request_bytes: &[u8]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let request = std::str::from_utf8(request_bytes)
        .map_err(|_| "invalid UTF-8 in HTTP request")?;
    let ws_key = WsKey::from_request(request)?;
    Ok(ws_key.upgrade_response())
}

fn alloc_slot(slots: &mut Vec<HandshakeSlot>, fd: i32) -> usize {
    if let Some(i) = slots.iter().position(|s| !s.active) {
        slots[i] = HandshakeSlot::new(fd);
        i
    } else {
        let i = slots.len();
        slots.push(HandshakeSlot::new(fd));
        i
    }
}

fn close_slot(ring: &mut IoUring, slot: &mut HandshakeSlot) {
    slot.active = false;
    let _ = ring.push(Sqe::close(slot.fd).user_data(0));
}

fn submit_accept(
    ring: &mut IoUring,
    listener_fd: i32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sqe = unsafe {
        Sqe::accept(
            listener_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            AcceptFlags::default(),
        )
    }
    .user_data(ACCEPT_UD);
    ring.push(sqe)?;
    Ok(())
}

fn submit_handshake_recv(
    ring: &mut IoUring,
    slot: &mut HandshakeSlot,
    idx: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let offset = slot.progress;
    let sqe = unsafe {
        Sqe::recv(
            slot.fd,
            slot.buf.as_mut_ptr().add(offset),
            (slot.buf.len() - offset) as u32,
            MsgFlags::default(),
        )
    }
    .user_data(encode_ud(idx, Phase::Recv));
    ring.push(sqe)?;
    Ok(())
}

fn submit_handshake_send(
    ring: &mut IoUring,
    slot: &mut HandshakeSlot,
    idx: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let offset = slot.progress;
    let sqe = unsafe {
        Sqe::send(
            slot.fd,
            slot.buf.as_ptr().add(offset),
            (slot.send_total - offset) as u32,
            MsgFlags::default(),
        )
    }
    .user_data(encode_ud(idx, Phase::Send));
    ring.push(sqe)?;
    Ok(())
}
