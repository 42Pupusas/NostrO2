//! Accept loop: io_uring accept → recv upgrade request → send 101 → hand fd to reader.
//!
//! The entire handshake runs through SQEs — no blocking syscalls. Each accepted
//! fd goes through a small state machine: Recv → parse HTTP → Send 101 → done.
//!
//! Backpressure: when in-flight handshakes fill the io_uring SQ, the accept SQE
//! is withheld. Pending connections queue in the kernel's TCP listen backlog.
//! When handshakes complete and free SQ slots, accept is resubmitted.

use crate::{AcceptedClient, HttpHandler, HttpRequest};
use coyoquil::{DeflateConfig, UpgradeRequest, negotiate_subprotocol};
use quetzalcoatl::spsc;
use ququmatz::Socket;
use ququmatz::types::{AcceptFlags, MsgFlags, SockAddrIn, AF_INET, SOCK_STREAM, SOL_SOCKET, SO_REUSEADDR};
use ququmatz::{IoUring, Sqe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// SQ ring size. Determines max concurrent handshakes.
const RING_SIZE: u32 = 1024;

/// Reserve this many SQ slots for non-handshake ops (accept, close).
/// When active handshakes reach RING_SIZE - RESERVED, we stop accepting.
const RESERVED: usize = 8;

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
    sock.listen(1024)?;

    Ok(sock)
}

// ── Handshake state machine ────────────────────────────────────────────

const ACCEPT_UD: u64 = u64::MAX;

fn encode_ud(slot: usize, phase: Phase) -> u64 {
    (slot as u64) | ((phase as u64) << 48)
}

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
    progress: usize,
    send_total: usize,
    active: bool,
    /// Negotiation results, populated after parsing the upgrade request.
    accepted: Option<AcceptedClient>,
    /// When true, this slot is serving a plain HTTP response — close the fd
    /// on send completion instead of handing it to a reader shard.
    http_only: bool,
}

impl HandshakeSlot {
    fn new(fd: i32) -> Self {
        Self {
            fd,
            buf: vec![0u8; 4096],
            progress: 0,
            send_total: 0,
            active: true,
            accepted: None,
            http_only: false,
        }
    }
}

// ── Accept loop ────────────────────────────────────────────────────────

pub fn listener_thread(
    listener_fd: i32,
    accept_txs: Vec<spsc::Producer<AcceptedClient>>,
    shutdown: Arc<AtomicBool>,
    subprotocols: Vec<String>,
    deflate_policy: Option<DeflateConfig>,
    http_handler: Option<Arc<HttpHandler>>,
    #[cfg(feature = "ktls")] tls: Option<Arc<rustls::ServerConfig>>,
) {
    if let Err(e) = listener_loop(
        listener_fd,
        accept_txs,
        &shutdown,
        &subprotocols,
        deflate_policy.as_ref(),
        http_handler.as_deref(),
        #[cfg(feature = "ktls")]
        tls.as_ref(),
    ) {
        eprintln!("listener thread fatal: {e}");
    }
}

fn listener_loop(
    listener_fd: i32,
    accept_txs: Vec<spsc::Producer<AcceptedClient>>,
    shutdown: &AtomicBool,
    subprotocols: &[String],
    deflate_policy: Option<&DeflateConfig>,
    http_handler: Option<&HttpHandler>,
    #[cfg(feature = "ktls")] tls: Option<&Arc<rustls::ServerConfig>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let num_reader_shards = accept_txs.len();
    let mut ring = IoUring::new(RING_SIZE)?;
    let mut slots: Vec<HandshakeSlot> = Vec::new();
    let max_handshakes = RING_SIZE as usize - RESERVED;

    // Track whether we have an accept SQE in-flight
    // Submit the first accept — always kept in-flight
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
                if cqe.is_err() {
                    if shutdown.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let errno = -cqe.result;
                    if errno == 24 {
                        // EMFILE — out of file descriptors. Back off and let
                        // existing connections close before accepting more.
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    } else {
                        eprintln!("accept error: {}", cqe.result);
                    }
                } else {
                    let client_fd = cqe.result;

                    if active_handshakes(&slots) < max_handshakes {
                        // When kTLS is configured, terminate TLS on this fd
                        // *before* starting the HTTP upgrade exchange. The
                        // handshake is synchronous on the listener thread —
                        // acceptable for low-rate connection churn; if this
                        // becomes a bottleneck we'll move it to a pool.
                        #[cfg(feature = "ktls")]
                        let tls_ok = if let Some(cfg) = tls {
                            match crate::kernel_tls::setup(client_fd, Arc::clone(cfg)) {
                                Ok(()) => true,
                                Err(e) => {
                                    eprintln!(
                                        "kTLS setup failed for fd {client_fd}: {e}"
                                    );
                                    drop(unsafe { Socket::from_fd(client_fd) });
                                    false
                                }
                            }
                        } else {
                            true
                        };
                        #[cfg(not(feature = "ktls"))]
                        let tls_ok = true;

                        if tls_ok {
                            let idx = alloc_slot(&mut slots, client_fd);
                            submit_handshake_recv(&mut ring, &mut slots[idx], idx)?;
                        }
                    } else {
                        // At capacity — close this fd synchronously. The kernel
                        // backlog holds the rest.
                        drop(unsafe { Socket::from_fd(client_fd) });
                    }
                }

                // Always keep accept in-flight so submit_and_wait never starves
                submit_accept(&mut ring, listener_fd)?;
                continue;
            }

            // Skip close CQEs (user_data = 0) and invalid slots
            let (idx, phase) = decode_ud(cqe.user_data);
            if idx >= slots.len() || !slots[idx].active {
                continue;
            }

            match phase {
                Phase::Recv => {
                    if cqe.is_err() || cqe.result <= 0 {
                        close_slot(&mut ring, &mut slots[idx]);
                        continue;
                    }

                    let n = cqe.result as usize;
                    slots[idx].progress += n;
                    let total = slots[idx].progress;

                    if total >= 4 && slots[idx].buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        // Branch: WebSocket upgrade vs plain HTTP (NIP-11 etc).
                        let is_ws_upgrade = request_has_upgrade_websocket(
                            &slots[idx].buf[..total],
                        );

                        if is_ws_upgrade {
                            match build_upgrade(
                                &slots[idx].buf[..total],
                                slots[idx].fd,
                                subprotocols,
                                deflate_policy,
                            ) {
                                Ok((response, accepted)) => {
                                    let resp_bytes = response.into_bytes();
                                    let send_len = resp_bytes.len();
                                    slots[idx].buf = resp_bytes;
                                    slots[idx].progress = 0;
                                    slots[idx].send_total = send_len;
                                    slots[idx].accepted = Some(accepted);
                                    submit_handshake_send(&mut ring, &mut slots[idx], idx)?;
                                }
                                Err(e) => {
                                    eprintln!(
                                        "handshake parse failed for fd {}: {e}",
                                        slots[idx].fd
                                    );
                                    close_slot(&mut ring, &mut slots[idx]);
                                }
                            }
                        } else if let Some(h) = http_handler {
                            match invoke_http_handler(&slots[idx].buf[..total], h) {
                                Ok(resp_bytes) => {
                                    let send_len = resp_bytes.len();
                                    slots[idx].buf = resp_bytes;
                                    slots[idx].progress = 0;
                                    slots[idx].send_total = send_len;
                                    slots[idx].http_only = true;
                                    submit_handshake_send(&mut ring, &mut slots[idx], idx)?;
                                }
                                Err(e) => {
                                    eprintln!(
                                        "http handler failed for fd {}: {e}",
                                        slots[idx].fd
                                    );
                                    close_slot(&mut ring, &mut slots[idx]);
                                }
                            }
                        } else {
                            // Plain HTTP with no handler — send a 400 and close.
                            let body = b"400 Bad Request: WebSocket upgrade required\r\n";
                            let resp = format!(
                                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let mut resp_bytes = resp.into_bytes();
                            resp_bytes.extend_from_slice(body);
                            let send_len = resp_bytes.len();
                            slots[idx].buf = resp_bytes;
                            slots[idx].progress = 0;
                            slots[idx].send_total = send_len;
                            slots[idx].http_only = true;
                            submit_handshake_send(&mut ring, &mut slots[idx], idx)?;
                        }
                    } else if total >= 4096 {
                        eprintln!("HTTP request too large for fd {}", slots[idx].fd);
                        close_slot(&mut ring, &mut slots[idx]);
                    } else {
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
                        if slots[idx].http_only {
                            // Plain HTTP response sent — close and free the slot.
                            close_slot(&mut ring, &mut slots[idx]);
                            continue;
                        }

                        // Handshake complete — hand client to correct reader shard
                        let client_fd = slots[idx].fd;
                        slots[idx].active = false;

                        let accepted = slots[idx]
                            .accepted
                            .take()
                            .expect("accepted must be set after handshake");

                        let shard = client_fd as usize % num_reader_shards;
                        if accept_txs[shard].push(accepted).is_err() {
                            eprintln!("accept ring full on shard {shard}, dropping client {client_fd}");
                            drop(unsafe { Socket::from_fd(client_fd) });
                        }
                    } else {
                        submit_handshake_send(&mut ring, &mut slots[idx], idx)?;
                    }
                }
            }
        }

    }

    Ok(())
}

fn active_handshakes(slots: &[HandshakeSlot]) -> usize {
    slots.iter().filter(|s| s.active).count()
}

/// Parse the HTTP upgrade request, negotiate subprotocol and deflate,
/// and return the 101 response string + the accepted client info.
fn build_upgrade(
    request_bytes: &[u8],
    fd: i32,
    subprotocols: &[String],
    deflate_policy: Option<&DeflateConfig>,
) -> Result<(String, AcceptedClient), Box<dyn std::error::Error + Send + Sync>> {
    let request = std::str::from_utf8(request_bytes)
        .map_err(|_| "invalid UTF-8 in HTTP request")?;

    let upgrade_req = UpgradeRequest::parse(request)?;

    // Negotiate subprotocol
    let supported_refs: Vec<&str> = subprotocols.iter().map(String::as_str).collect();
    let subprotocol = if !upgrade_req.subprotocols().is_empty() && !supported_refs.is_empty() {
        negotiate_subprotocol(upgrade_req.subprotocols(), &supported_refs)
            .map(String::from)
    } else {
        None
    };

    // Negotiate deflate
    let (deflate, deflate_header) = match (upgrade_req.extensions(), deflate_policy) {
        (Some(ext), Some(policy)) if ext.contains("permessage-deflate") => {
            match DeflateConfig::negotiate(ext, policy) {
                Ok((config, header)) => (Some(config), Some(header)),
                Err(_) => (None, None),
            }
        }
        _ => (None, None),
    };

    // Capture path before building response (into_response borrows key only)
    let path = upgrade_req.path().to_string();

    // Build response
    let mut response = upgrade_req.into_response();
    if let Some(ref proto) = subprotocol {
        response = response.subprotocol(proto);
    }
    if let Some(ref ext) = deflate_header {
        response = response.extensions(ext);
    }
    let response_str = response.build();

    // Extract all headers from the raw request
    let headers = parse_headers(request);

    let accepted = AcceptedClient {
        fd,
        path,
        subprotocol,
        deflate,
        headers,
    };

    Ok((response_str, accepted))
}

/// Cheap scan to decide whether the request is a WebSocket upgrade.
///
/// Looks for a line that contains both `Upgrade` (header name) and `websocket`
/// (token, case-insensitive). False positives from request bodies would be
/// possible in principle, but GET-based WS upgrades have no body.
fn request_has_upgrade_websocket(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    for line in text.lines() {
        if line.is_empty() {
            break; // end of headers
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("upgrade")
            && val
                .split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("websocket"))
        {
            return true;
        }
    }
    false
}

/// Run the user's HTTP handler and return its response bytes.
fn invoke_http_handler(
    request_bytes: &[u8],
    handler: &HttpHandler,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let request = std::str::from_utf8(request_bytes)
        .map_err(|_| "invalid UTF-8 in HTTP request")?;
    let request_line = request.lines().next().ok_or("empty request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("missing method")?;
    let path = parts.next().ok_or("missing path")?;

    let headers = parse_headers(request);
    let req = HttpRequest {
        path,
        method,
        headers: &headers,
    };
    Ok(handler(req))
}

/// Extract all HTTP headers as key-value pairs.
fn parse_headers(request: &str) -> Vec<(String, String)> {
    request
        .lines()
        .skip(1) // skip request line (GET /path HTTP/1.1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
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

fn close_slot(_ring: &mut IoUring, slot: &mut HandshakeSlot) {
    slot.active = false;
    drop(unsafe { Socket::from_fd(slot.fd) });
}

fn submit_accept(
    ring: &mut IoUring,
    listener_fd: i32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sqe = Sqe::accept(listener_fd, AcceptFlags::default()).user_data(ACCEPT_UD);
    ring.push(sqe)?;
    Ok(())
}

fn submit_handshake_recv(
    ring: &mut IoUring,
    slot: &mut HandshakeSlot,
    idx: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let offset = slot.progress;
    let sqe = Sqe::recv(slot.fd, &mut slot.buf[offset..], MsgFlags::default())
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
    let sqe = Sqe::send(
        slot.fd,
        &slot.buf[offset..slot.send_total],
        MsgFlags::default(),
    )
    .user_data(encode_ud(idx, Phase::Send));
    ring.push(sqe)?;
    Ok(())
}
