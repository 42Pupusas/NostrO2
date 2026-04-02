//! Writer IO thread: io_uring send for all client fds.
//!
//! Receives all commands (Register, SendText, Broadcast, Close, Pong) from a
//! single MPSC ring, ensuring ordering between registration and send commands.

use crate::WriteCmd;
use coyoquil::{CloseCode, DeflateEncoder, Frame};
use quetzalcoatl::mpsc::Consumer;
use ququmatz::types::MsgFlags;
use ququmatz::{IoUring, Sqe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Per-client state in the writer thread.
struct WriterSlot {
    fd: i32,
    send_buf: Vec<u8>,
    send_offset: usize,
    send_pending: bool,
    dead: bool,
    deflate_encoder: Option<DeflateEncoder>,
}

pub fn writer_thread(
    write_rx: Consumer<WriteCmd>,
    shutdown: Arc<AtomicBool>,
) {
    if let Err(e) = writer_loop(write_rx, &shutdown) {
        eprintln!("writer IO thread fatal: {e}");
    }
}

fn writer_loop(
    mut write_rx: Consumer<WriteCmd>,
    shutdown: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ring = IoUring::new(4096)?;
    let mut slots: Vec<WriterSlot> = Vec::new();
    // Scratch buffer for compression — reused across frames.
    let mut compress_buf: Vec<u8> = Vec::new();

    loop {
        if shutdown.load(Ordering::Acquire) {
            for slot in &mut slots {
                if !slot.dead {
                    let _ = send_close(&mut ring, slot, CloseCode::Normal);
                }
            }
            break;
        }

        // Drain all commands from the ring
        let mut cmds: Vec<WriteCmd> = Vec::new();
        write_rx.drain(|cmd| cmds.push(cmd));

        for cmd in cmds {
            match cmd {
                WriteCmd::Register { fd, deflate } => {
                    let deflate_encoder = deflate.as_ref().map(|config| {
                        DeflateEncoder::new(config, true)
                    });
                    let new_slot = WriterSlot {
                        fd,
                        send_buf: Vec::with_capacity(65536),
                        send_offset: 0,
                        send_pending: false,
                        dead: false,
                        deflate_encoder,
                    };
                    if let Some(i) = slots.iter().position(|s| s.dead && !s.send_pending) {
                        slots[i] = new_slot;
                    } else {
                        slots.push(new_slot);
                    }
                }
                WriteCmd::SendText { fd, text } => {
                    if let Some(slot) = slots.iter_mut().find(|s| s.fd == fd && !s.dead) {
                        encode_text(slot, &text, &mut compress_buf);
                    }
                }
                WriteCmd::SendBinary { fd, data } => {
                    if let Some(slot) = slots.iter_mut().find(|s| s.fd == fd && !s.dead) {
                        encode_binary(slot, &data, &mut compress_buf);
                    }
                }
                WriteCmd::Broadcast { text } => {
                    // Pre-encode the uncompressed frame for non-deflate slots
                    let mut plain = Vec::new();
                    Frame::Text(&text).encode(&mut plain).ok();

                    for slot in slots.iter_mut() {
                        if slot.dead {
                            continue;
                        }
                        if slot.deflate_encoder.is_some() {
                            encode_text(slot, &text, &mut compress_buf);
                        } else {
                            slot.send_buf.extend_from_slice(&plain);
                        }
                    }
                }
                WriteCmd::BroadcastBinary { data } => {
                    let mut plain = Vec::new();
                    Frame::Binary(&data).encode(&mut plain).ok();

                    for slot in slots.iter_mut() {
                        if slot.dead {
                            continue;
                        }
                        if slot.deflate_encoder.is_some() {
                            encode_binary(slot, &data, &mut compress_buf);
                        } else {
                            slot.send_buf.extend_from_slice(&plain);
                        }
                    }
                }
                WriteCmd::Close { fd, code } => {
                    if let Some(slot) = slots.iter_mut().find(|s| s.fd == fd && !s.dead) {
                        let _ = send_close(&mut ring, slot, code);
                    }
                }
                WriteCmd::Pong { fd } => {
                    if let Some(slot) = slots.iter_mut().find(|s| s.fd == fd && !s.dead) {
                        Frame::Pong(&[]).encode(&mut slot.send_buf).ok();
                    }
                }
            }
        }

        // Drain any ready completions before submitting new work —
        // this frees SQ slots and prevents SQ-full under load.
        drain_completions(&mut ring, &mut slots);

        // Submit sends for slots with buffered data.
        // If SQ is full, the slot keeps send_pending=false and will
        // be retried on the next loop iteration.
        let mut any_work = false;
        for (idx, slot) in slots.iter_mut().enumerate() {
            if slot.dead || slot.send_pending || slot.send_buf.is_empty() {
                continue;
            }
            if try_submit_send(&mut ring, slot, idx as u64) {
                any_work = true;
            }
        }

        if any_work {
            ring.submit_and_wait(1)?;
            drain_completions(&mut ring, &mut slots);
        } else {
            // No outbound data — park until woken by ServerSender or reader.
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
    }

    Ok(())
}

/// Encode a text frame, compressing if the slot has a deflate encoder.
fn encode_text(slot: &mut WriterSlot, text: &str, compress_buf: &mut Vec<u8>) {
    if let Some(ref mut encoder) = slot.deflate_encoder {
        compress_buf.clear();
        encoder.compress(text.as_bytes(), compress_buf);
        // RSV1 + FIN + Text opcode, unmasked (server→client)
        encode_compressed_frame(0xC1, compress_buf, &mut slot.send_buf);
    } else {
        Frame::Text(text).encode(&mut slot.send_buf).ok();
    }
}

/// Encode a binary frame, compressing if the slot has a deflate encoder.
fn encode_binary(slot: &mut WriterSlot, data: &[u8], compress_buf: &mut Vec<u8>) {
    if let Some(ref mut encoder) = slot.deflate_encoder {
        compress_buf.clear();
        encoder.compress(data, compress_buf);
        // RSV1 + FIN + Binary opcode, unmasked
        encode_compressed_frame(0xC2, compress_buf, &mut slot.send_buf);
    } else {
        Frame::Binary(data).encode(&mut slot.send_buf).ok();
    }
}

/// Write a compressed frame directly: byte0 (FIN+RSV1+opcode) + length + payload.
///
/// This bypasses `Frame::encode_compressed` because that method uses the
/// Frame variant's payload (e.g. `Frame::Text` requires `&str`), but
/// compressed bytes are not valid UTF-8.
#[allow(clippy::cast_possible_truncation)]
fn encode_compressed_frame(byte0: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(byte0);
    let len = payload.len();
    if len < 126 {
        out.push(len as u8);
    } else if len < 65536 {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
}

/// Drain all ready CQEs, advancing send state for each completed slot.
/// Resubmits partial sends (best-effort — skips if SQ is full).
fn drain_completions(ring: &mut IoUring, slots: &mut [WriterSlot]) {
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
            continue;
        }

        let n = cqe.result as usize;
        slot.send_offset += n;

        if slot.send_offset < slot.send_buf.len() {
            // Partial send — resubmit remainder (best-effort).
            // If SQ is full, it stays !send_pending with data in
            // send_buf and will be picked up in the main loop.
            if try_submit_send(ring, slot, idx as u64) {
                let _ = ring.submit();
            }
        } else {
            slot.send_buf.clear();
            slot.send_offset = 0;
        }
    }

    ring.sync_cq();
}

/// Push a send SQE. Returns true on success, false if the SQ is full.
/// On failure, `send_pending` stays false so the caller can retry later.
fn try_submit_send(ring: &mut IoUring, slot: &mut WriterSlot, user_data: u64) -> bool {
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
    if ring.push(sqe).is_ok() {
        slot.send_pending = true;
        true
    } else {
        false
    }
}

fn send_close(
    ring: &mut IoUring,
    slot: &mut WriterSlot,
    code: CloseCode,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    slot.send_buf.clear();
    slot.send_offset = 0;
    Frame::Close(Some((code, &[]))).encode(&mut slot.send_buf).ok();
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
    slot.dead = true;
    Ok(())
}
