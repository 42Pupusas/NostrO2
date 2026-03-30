//! WebSocket frame codec and handshake (RFC 6455).
//!
//! Manual frame encoding/decoding for use with kTLS + io_uring,
//! bypassing tungstenite entirely.

use base64::Engine;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// Decoded WebSocket frame from the server.
#[derive(Debug)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(Option<(u16, String)>),
}

/// Incremental WebSocket frame decoder.
///
/// Handles partial frames across recv calls and reassembles fragmented messages.
pub struct FrameDecoder {
    buf: Vec<u8>,
    fragment_opcode: Option<u8>,
    fragment_payload: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(65536),
            fragment_opcode: None,
            fragment_payload: Vec::new(),
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to parse the next complete frame from the buffer.
    pub fn next_frame(&mut self) -> Option<Frame> {
        loop {
            let (fin, opcode, payload, total_len) = self.parse_raw_frame()?;
            self.buf.drain(..total_len);

            // Control frames (always FIN=1, never fragmented)
            if opcode >= 0x8 {
                return match opcode {
                    OP_PING => Some(Frame::Ping(payload)),
                    OP_PONG => Some(Frame::Pong(payload)),
                    OP_CLOSE => {
                        if payload.len() >= 2 {
                            let code = u16::from_be_bytes([payload[0], payload[1]]);
                            let reason = String::from_utf8_lossy(&payload[2..]).into_owned();
                            Some(Frame::Close(Some((code, reason))))
                        } else {
                            Some(Frame::Close(None))
                        }
                    }
                    _ => continue,
                };
            }

            // Data frames with fragmentation handling
            if fin {
                if opcode == OP_CONTINUATION {
                    if let Some(orig_opcode) = self.fragment_opcode.take() {
                        self.fragment_payload.extend_from_slice(&payload);
                        let complete = std::mem::take(&mut self.fragment_payload);
                        return Some(make_data_frame(orig_opcode, complete));
                    }
                    continue;
                }
                if self.fragment_opcode.is_some() {
                    self.fragment_opcode = None;
                    self.fragment_payload.clear();
                }
                return Some(make_data_frame(opcode, payload));
            } else if opcode != OP_CONTINUATION {
                self.fragment_opcode = Some(opcode);
                self.fragment_payload = payload;
            } else {
                self.fragment_payload.extend_from_slice(&payload);
            }
        }
    }

    /// Parse a single raw frame header + payload. Returns (fin, opcode, payload, total_consumed).
    fn parse_raw_frame(&self) -> Option<(bool, u8, Vec<u8>, usize)> {
        if self.buf.len() < 2 {
            return None;
        }

        let byte0 = self.buf[0];
        let byte1 = self.buf[1];
        let fin = byte0 & 0x80 != 0;
        let opcode = byte0 & 0x0F;
        let masked = byte1 & 0x80 != 0;
        let length_field = (byte1 & 0x7F) as usize;

        let mut offset = 2;
        let payload_len = match length_field {
            0..=125 => length_field,
            126 => {
                if self.buf.len() < 4 {
                    return None;
                }
                offset = 4;
                u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize
            }
            _ => {
                if self.buf.len() < 10 {
                    return None;
                }
                offset = 10;
                u64::from_be_bytes([
                    self.buf[2], self.buf[3], self.buf[4], self.buf[5],
                    self.buf[6], self.buf[7], self.buf[8], self.buf[9],
                ]) as usize
            }
        };

        let mask_key = if masked {
            if self.buf.len() < offset + 4 {
                return None;
            }
            let key = [
                self.buf[offset],
                self.buf[offset + 1],
                self.buf[offset + 2],
                self.buf[offset + 3],
            ];
            offset += 4;
            Some(key)
        } else {
            None
        };

        let total_len = offset + payload_len;
        if self.buf.len() < total_len {
            return None;
        }

        let mut payload = self.buf[offset..total_len].to_vec();
        if let Some(key) = mask_key {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= key[i % 4];
            }
        }

        Some((fin, opcode, payload, total_len))
    }
}

fn make_data_frame(opcode: u8, payload: Vec<u8>) -> Frame {
    match opcode {
        OP_TEXT => Frame::Text(String::from_utf8_lossy(&payload).into_owned()),
        _ => Frame::Binary(payload),
    }
}

// --- Encoder (client-to-server, masked) ---

/// Encode a masked text frame.
pub fn encode_text_frame(payload: &[u8], mask_key: [u8; 4], out: &mut Vec<u8>) {
    encode_frame(0x81, payload, mask_key, out);
}

/// Encode a masked pong frame.
pub fn encode_pong_frame(payload: &[u8], mask_key: [u8; 4], out: &mut Vec<u8>) {
    encode_frame(0x8A, payload, mask_key, out);
}

/// Encode a masked close frame.
pub fn encode_close_frame(status: u16, mask_key: [u8; 4], out: &mut Vec<u8>) {
    encode_frame(0x88, &status.to_be_bytes(), mask_key, out);
}

fn encode_frame(byte0: u8, payload: &[u8], mask_key: [u8; 4], out: &mut Vec<u8>) {
    out.push(byte0);
    let len = payload.len();
    if len < 126 {
        out.push(0x80 | len as u8);
    } else if len < 65536 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask_key);
    for (i, &byte) in payload.iter().enumerate() {
        out.push(byte ^ mask_key[i % 4]);
    }
}

// --- WebSocket Handshake ---

/// Build the HTTP upgrade request for WebSocket handshake.
pub fn ws_upgrade_request(host: &str, path: &str, key: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

/// Generate a random WebSocket key (16 random bytes, base64 encoded).
pub fn generate_ws_key() -> String {
    use rand::RngCore;
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    base64::engine::general_purpose::STANDARD.encode(key_bytes)
}

/// Compute the expected Sec-WebSocket-Accept value.
pub fn compute_accept_key(key: &str) -> String {
    let input = format!("{key}{WS_GUID}");
    let hash = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, input.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hash.as_ref())
}

/// Validate the server's WebSocket upgrade response.
pub fn validate_ws_response(response: &str, key: &str) -> Result<(), String> {
    let first_line = response.lines().next().ok_or("empty response")?;
    if !first_line.contains("101") {
        return Err(format!("expected 101, got: {first_line}"));
    }

    let expected_accept = compute_accept_key(key);
    for line in response.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("sec-websocket-accept:") {
            let value = line.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
            if value == expected_accept {
                return Ok(());
            }
            return Err(format!("accept mismatch: got {value}, expected {expected_accept}"));
        }
    }

    Err("missing Sec-WebSocket-Accept header".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_text_frame() {
        let payload = b"Hello, WebSocket!";
        let mask_key = [0x37, 0xfa, 0x21, 0x9d];
        let mut encoded = Vec::new();
        encode_text_frame(payload, mask_key, &mut encoded);

        assert_eq!(encoded[0], 0x81);
        assert_eq!(encoded[1], 0x80 | payload.len() as u8);
        assert_eq!(&encoded[2..6], &mask_key);

        let mut decoded = encoded[6..].to_vec();
        for (i, byte) in decoded.iter_mut().enumerate() {
            *byte ^= mask_key[i % 4];
        }
        assert_eq!(&decoded, payload);
    }

    #[test]
    fn test_decode_unmasked_text_frame() {
        let payload = b"Hello";
        let mut frame_bytes = vec![0x81, payload.len() as u8];
        frame_bytes.extend_from_slice(payload);

        let mut decoder = FrameDecoder::new();
        decoder.push(&frame_bytes);

        match decoder.next_frame() {
            Some(Frame::Text(text)) => assert_eq!(text, "Hello"),
            other => panic!("expected Text, got {:?}", other),
        }
        assert!(decoder.next_frame().is_none());
    }

    #[test]
    fn test_decode_ping_frame() {
        let mut frame = vec![0x89, 0x05];
        frame.extend_from_slice(b"hello");

        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);

        match decoder.next_frame() {
            Some(Frame::Ping(data)) => assert_eq!(data, b"hello"),
            other => panic!("expected Ping, got {:?}", other),
        }
    }

    #[test]
    fn test_decode_close_frame_with_code() {
        let mut frame = vec![0x88, 0x02];
        frame.extend_from_slice(&1000u16.to_be_bytes());

        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);

        match decoder.next_frame() {
            Some(Frame::Close(Some((code, reason)))) => {
                assert_eq!(code, 1000);
                assert!(reason.is_empty());
            }
            other => panic!("expected Close, got {:?}", other),
        }
    }

    #[test]
    fn test_decode_partial_frame() {
        let payload = b"Hello, WebSocket!";
        let mut frame = vec![0x81, payload.len() as u8];
        frame.extend_from_slice(payload);

        let mut decoder = FrameDecoder::new();
        decoder.push(&frame[..5]);
        assert!(decoder.next_frame().is_none());
        decoder.push(&frame[5..]);

        match decoder.next_frame() {
            Some(Frame::Text(text)) => assert_eq!(text, "Hello, WebSocket!"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn test_decode_multiple_frames() {
        let mut data = vec![0x81, 0x03];
        data.extend_from_slice(b"foo");
        data.extend_from_slice(&[0x81, 0x03]);
        data.extend_from_slice(b"bar");

        let mut decoder = FrameDecoder::new();
        decoder.push(&data);

        match decoder.next_frame() {
            Some(Frame::Text(t)) => assert_eq!(t, "foo"),
            other => panic!("expected foo, got {:?}", other),
        }
        match decoder.next_frame() {
            Some(Frame::Text(t)) => assert_eq!(t, "bar"),
            other => panic!("expected bar, got {:?}", other),
        }
        assert!(decoder.next_frame().is_none());
    }

    #[test]
    fn test_decode_fragmented_message() {
        let mut data = Vec::new();
        // Fragment 1: FIN=0, opcode=text
        data.extend_from_slice(&[0x01, 0x03]);
        data.extend_from_slice(b"Hel");
        // Fragment 2: FIN=1, opcode=continuation
        data.extend_from_slice(&[0x80, 0x02]);
        data.extend_from_slice(b"lo");

        let mut decoder = FrameDecoder::new();
        decoder.push(&data);

        match decoder.next_frame() {
            Some(Frame::Text(t)) => assert_eq!(t, "Hello"),
            other => panic!("expected 'Hello', got {:?}", other),
        }
    }

    #[test]
    fn test_decode_extended_length_16bit() {
        let payload = vec![b'A'; 200];
        let mut frame = vec![0x81, 126];
        frame.extend_from_slice(&200u16.to_be_bytes());
        frame.extend_from_slice(&payload);

        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);

        match decoder.next_frame() {
            Some(Frame::Text(t)) => assert_eq!(t.len(), 200),
            other => panic!("expected 200-byte text, got {:?}", other),
        }
    }

    #[test]
    fn test_ws_accept_key_rfc6455() {
        // RFC 6455 Section 4.2.2 example
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let accept = compute_accept_key(key);
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn test_encode_close_frame_structure() {
        let mask_key = [0x01, 0x02, 0x03, 0x04];
        let mut out = Vec::new();
        encode_close_frame(1000, mask_key, &mut out);

        assert_eq!(out[0], 0x88);
        assert_eq!(out[1], 0x82); // MASK + length 2
        assert_eq!(&out[2..6], &mask_key);
    }

    #[test]
    fn test_validate_ws_response_valid() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
             \r\n"
        );
        assert!(validate_ws_response(&response, key).is_ok());
    }

    #[test]
    fn test_validate_ws_response_bad_status() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let response = "HTTP/1.1 400 Bad Request\r\n\r\n";
        assert!(validate_ws_response(response, key).is_err());
    }
}
