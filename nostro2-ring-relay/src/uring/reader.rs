//! Reader thread: recvmsg loop -> WebSocket frame decode -> MPSC ring push.
//!
//! Uses `ktls_read` (recvmsg with cmsg) to transparently skip non-data TLS records
//! like NewSessionTicket, then parses WebSocket frames and pushes to the MPSC ring.

use crate::PoolMessage;
use nostro2::NostrRelayEvent;
use quetzalcoatl::mpsc::Producer;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use super::ktls;
use super::ws::{Frame, FrameDecoder};

/// Run the reader loop. Blocks until shutdown or connection close.
pub fn reader_loop(
    fd: RawFd,
    producer: Producer<PoolMessage>,
    relay_url: String,
    shutdown: &AtomicBool,
    pong_tx: Producer<Vec<u8>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut recv_buf = vec![0u8; 65536];
    let mut decoder = FrameDecoder::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Read application data, skipping non-data TLS records (NewSessionTicket etc.)
        let n = match ktls::ktls_read(fd, &mut recv_buf) {
            Ok(0) => {
                shutdown.store(true, Ordering::Relaxed);
                break;
            }
            Ok(n) => n,
            Err(e) => {
                shutdown.store(true, Ordering::Relaxed);
                return Err(e.into());
            }
        };

        decoder.push(&recv_buf[..n]);

        while let Some(frame) = decoder.next_frame() {
            match frame {
                Frame::Text(text) => {
                    if let Ok(event) = text.parse::<NostrRelayEvent>() {
                        let mut msg = PoolMessage::RelayEvent {
                            relay_url: relay_url.clone(),
                            event,
                        };
                        loop {
                            match producer.push(msg) {
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
                    // Best-effort: push to pong channel for writer thread
                    let _ = pong_tx.push(data);
                }
                Frame::Close(_) => {
                    shutdown.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    Ok(())
}
