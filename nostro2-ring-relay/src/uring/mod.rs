//! kTLS relay connection with split reader/writer threads.
//!
//! Offloads TLS to the Linux kernel via kTLS, then uses two independent
//! threads for truly concurrent lock-free read/write on the same fd:
//!
//! - Reader thread: recvmsg → parse WS frames → MPSC ring (inbound)
//! - Writer thread: broadcast ring → encode WS frames → send (outbound)

mod ktls;
mod reader;
pub mod ws;
mod writer;

use crate::PoolMessage;
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Producer, RingBuffer};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Handle to a relay connection using kTLS.
///
/// Each connection spawns two threads (reader + writer) that operate
/// concurrently on the same kTLS fd — the kernel handles TLS so both
/// threads can read/write without userspace locks.
pub struct UringRelayConnection {
    relay_url: String,
    reader_handle: Option<std::thread::JoinHandle<()>>,
    writer_handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    fd: RawFd,
}

impl UringRelayConnection {
    /// Spawn a new kTLS connection to a relay.
    ///
    /// Performs TLS handshake, kTLS offload, and WebSocket upgrade synchronously,
    /// then spawns reader and writer threads.
    pub fn spawn(
        relay_url: String,
        producer: Producer<PoolMessage>,
        outbound: broadcast::Consumer<String>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Connect: TCP -> TLS handshake -> kTLS setup -> WS upgrade
        let ktls_conn = ktls::connect(&relay_url)?;
        let fd = ktls_conn.fd;

        // Prevent KtlsConnection from closing the fd — we manage it
        std::mem::forget(ktls_conn);

        // Lock-free SPSC ring for ping/pong coordination (reader -> writer)
        let (pong_tx, pong_rx) = RingBuffer::<Vec<u8>>::new(Capacity::at_least(4)).split();

        let url = relay_url.clone();
        let reader_shutdown = Arc::clone(&shutdown);
        let reader_handle = std::thread::spawn(move || {
            if let Err(e) = reader::reader_loop(fd, producer, url.clone(), &reader_shutdown, pong_tx)
            {
                let _ = eprintln!("uring reader error [{url}]: {e}");
            }
            reader_shutdown.store(true, Ordering::Relaxed);
        });

        let url = relay_url.clone();
        let writer_shutdown = Arc::clone(&shutdown);
        let writer_handle = std::thread::spawn(move || {
            if let Err(e) = writer::writer_loop(fd, outbound, &writer_shutdown, pong_rx) {
                let _ = eprintln!("uring writer error [{url}]: {e}");
            }
            writer_shutdown.store(true, Ordering::Relaxed);
        });

        Ok(Self {
            relay_url,
            reader_handle: Some(reader_handle),
            writer_handle: Some(writer_handle),
            shutdown,
            fd,
        })
    }

    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    pub fn is_finished(&self) -> bool {
        let reader_done = self
            .reader_handle
            .as_ref()
            .is_some_and(|h| h.is_finished());
        let writer_done = self
            .writer_handle
            .as_ref()
            .is_some_and(|h| h.is_finished());
        reader_done && writer_done
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn shutdown_and_join(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Shut down the socket to unblock any pending recv
        unsafe {
            libc::shutdown(self.fd, libc::SHUT_RDWR);
        }
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
        if let Some(h) = self.writer_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for UringRelayConnection {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        unsafe {
            libc::shutdown(self.fd, libc::SHUT_RDWR);
        }
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
        if let Some(h) = self.writer_handle.take() {
            let _ = h.join();
        }
        unsafe {
            libc::close(self.fd);
        }
    }
}
