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
                    // Start small. `Vec::extend_from_slice` will grow the
                    // buffer geometrically on first real send, so quiet
                    // connections (a huge fraction on any public relay)
                    // never pay for 64 KiB of writer scratch they don't
                    // use. At 5k idle clients that's the difference
                    // between ~320 MiB of pre-allocated send_bufs and
                    // almost nothing.
                    let new_slot = WriterSlot {
                        fd,
                        send_buf: Vec::new(),
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
                WriteCmd::SendEventFrame { fd, sub_id, note_bytes } => {
                    if let Some(slot) = slots.iter_mut().find(|s| s.fd == fd && !s.dead) {
                        encode_event_frame(slot, &sub_id, &note_bytes, &mut compress_buf);
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

/// Encode a NIP-01 `["EVENT","<sub_id>",<note>]` text frame directly into
/// the slot's WS send-buffer. Avoids allocating an intermediate `String`
/// for the JSON payload — useful when fanning one event out to many
/// subscribers, where this runs once per sub with `note_bytes` aliased via
/// `Arc<[u8]>` across all subs.
///
/// For deflate-enabled slots the payload still has to land in the
/// compressor first, so this composes into `compress_buf` (reused scratch)
/// before emitting the RSV1 frame. For plaintext slots the payload is
/// written straight into `send_buf` with a precomputed length header — no
/// extra copy.
fn encode_event_frame(
    slot: &mut WriterSlot,
    sub_id: &str,
    note_bytes: &[u8],
    compress_buf: &mut Vec<u8>,
) {
    // Payload bytes: `["EVENT","<sub_id>",<note>]`. Length depends on
    // whether the sub_id needs JSON-escaping (quotes, backslashes,
    // control chars). NIP-01 sub_ids are up to 64 ASCII alphanumerics
    // in practice, so the fast path never escapes.
    let sub_id_escape_extra = json_escape_extra_bytes(sub_id);
    // `["EVENT",` (9) + `"` (1) + sub_id + escape padding + `"` (1) + `,` (1) + note + `]` (1)
    let payload_len = 9 + 1 + sub_id.len() + sub_id_escape_extra + 1 + 1 + note_bytes.len() + 1;

    if let Some(ref mut encoder) = slot.deflate_encoder {
        // Compose the uncompressed payload into a scratch buffer, then
        // compress. The compressor input isn't the target of this helper
        // — if you have deflate clients on the fan-out path, most of the
        // gain is the shared `Arc<[u8]>` note body across subs, not the
        // write path itself.
        compress_buf.clear();
        let mut scratch: Vec<u8> = Vec::with_capacity(payload_len);
        append_event_payload(&mut scratch, sub_id, sub_id_escape_extra, note_bytes);
        encoder.compress(&scratch, compress_buf);
        encode_compressed_frame(0xC1, compress_buf, &mut slot.send_buf);
    } else {
        // Plaintext fast path: write the WS header, then splice the
        // payload components directly into send_buf. No per-frame heap
        // allocation.
        write_ws_text_header(&mut slot.send_buf, payload_len);
        append_event_payload(&mut slot.send_buf, sub_id, sub_id_escape_extra, note_bytes);
    }
}

/// Count the extra bytes needed to JSON-escape `s`. Returns 0 for the
/// common case of an ASCII-safe sub_id, so the length math stays an
/// addition.
fn json_escape_extra_bytes(s: &str) -> usize {
    let mut extra = 0;
    for b in s.bytes() {
        match b {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0C => extra += 1,
            0..=0x1F => extra += 5, // `\u00XX` is 6 chars vs the 1-byte original
            _ => {}
        }
    }
    extra
}

/// Write `["EVENT","<sub_id>",<note>]` into `out`. When
/// `sub_id_escape_extra` is 0 the sub_id bytes are copied verbatim
/// between the quotes; otherwise a full escape pass runs.
fn append_event_payload(out: &mut Vec<u8>, sub_id: &str, sub_id_escape_extra: usize, note_bytes: &[u8]) {
    out.extend_from_slice(b"[\"EVENT\",\"");
    if sub_id_escape_extra == 0 {
        out.extend_from_slice(sub_id.as_bytes());
    } else {
        append_json_escaped(out, sub_id);
    }
    out.push(b'"');
    out.push(b',');
    out.extend_from_slice(note_bytes);
    out.push(b']');
}

/// JSON-escape `s` into `out` (no surrounding quotes). Matches the
/// `serde_json` escape set: `"`, `\`, `\n`, `\r`, `\t`, `\b`, `\f`, and
/// `\u00XX` for other control chars.
fn append_json_escaped(out: &mut Vec<u8>, s: &str) {
    for b in s.bytes() {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            0..=0x1F => {
                out.extend_from_slice(b"\\u00");
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 0x0F) as usize]);
            }
            _ => out.push(b),
        }
    }
}

/// Write the unmasked WS text-frame header (`0x81 ...length...`) for a
/// payload of `payload_len` bytes. Mirrors `coyoquil::Frame::encode`
/// without requiring a `Frame::Text(&str)`, so the payload can be
/// composed piecewise afterwards.
#[allow(clippy::cast_possible_truncation)]
fn write_ws_text_header(out: &mut Vec<u8>, payload_len: usize) {
    out.push(0x81);
    if payload_len < 126 {
        out.push(payload_len as u8);
    } else if payload_len < 65536 {
        out.push(126);
        out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(payload_len as u64).to_be_bytes());
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
    let sqe = Sqe::send(slot.fd, data, MsgFlags::default()).user_data(user_data);
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
    let sqe = Sqe::send(slot.fd, &slot.send_buf, MsgFlags::default()).user_data(0);
    ring.push(sqe)?;
    ring.submit()?;
    slot.dead = true;
    Ok(())
}
