//! Server-side kTLS: drive the rustls handshake on a freshly-accepted fd,
//! then arm the kernel's TLS engine so the io_uring data path reads/writes
//! plaintext while the kernel encrypts on the wire.
//!
//! This mirrors the client-side setup in `ring-relay-client::ktls` but uses
//! `rustls::ServerConnection`. Requires `enable_secret_extraction = true`
//! on the `rustls::ServerConfig`.

use crate::syscall;
use rustls::{ExtractedSecrets, ProtocolVersion, ServerConnection};
use std::sync::Arc;

// kTLS constants from <linux/tls.h>
const SOL_TCP: i32 = 6;
const TCP_ULP: i32 = 31;
const SOL_TLS: i32 = 282;
const TLS_TX: i32 = 1;
const TLS_RX: i32 = 2;

const TLS_1_2_VERSION: u16 = 0x0303;
const TLS_1_3_VERSION: u16 = 0x0304;

const TLS_CIPHER_AES_GCM_128: u16 = 51;
const TLS_CIPHER_AES_GCM_256: u16 = 52;

#[repr(C)]
struct TlsCryptoInfo {
    version: u16,
    cipher_type: u16,
}

#[repr(C)]
struct TlsCryptoInfoAesGcm128 {
    info: TlsCryptoInfo,
    iv: [u8; 8],
    key: [u8; 16],
    salt: [u8; 4],
    rec_seq: [u8; 8],
}

#[repr(C)]
struct TlsCryptoInfoAesGcm256 {
    info: TlsCryptoInfo,
    iv: [u8; 8],
    key: [u8; 32],
    salt: [u8; 4],
    rec_seq: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<TlsCryptoInfo>() == 4);
const _: () = assert!(std::mem::size_of::<TlsCryptoInfoAesGcm128>() == 40);
const _: () = assert!(std::mem::size_of::<TlsCryptoInfoAesGcm256>() == 56);

/// Drive the TLS handshake on a blocking TCP fd and arm kTLS.
///
/// On success the fd can be read/written as plaintext — the kernel encrypts
/// outgoing records and decrypts incoming ones. From this point forward the
/// normal io_uring recv/send loop works as if there were no TLS at all,
/// with one caveat: non-application-data records (alerts, handshake messages
/// that may appear mid-session) surface on recv and must be filtered out.
/// For the server side we handle this at the fd level in [`ktls_read`].
///
/// # Errors
/// Propagates any rustls or setsockopt failure. The caller owns the fd on
/// failure and is responsible for closing it.
pub fn setup(
    fd: i32,
    config: Arc<rustls::ServerConfig>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = ServerConnection::new(config)?;

    // Drive the handshake to completion. rustls alternates wants_read / wants_write;
    // we mirror the client-side loop shape but using our raw syscalls so this
    // stays independent of std::net::TcpStream.
    drive_handshake(fd, &mut conn)?;

    let version = conn
        .protocol_version()
        .ok_or("TLS handshake completed without a negotiated protocol version")?;

    let secrets = conn
        .dangerous_extract_secrets()
        .map_err(|e| format!("failed to extract TLS secrets: {e}"))?;

    arm_ktls(fd, version, &secrets)?;

    Ok(())
}

fn drive_handshake(
    fd: i32,
    conn: &mut ServerConnection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // rustls expects record-aligned reads; we oblige by reading exactly one
    // record at a time (5-byte TLS header + length-prefixed body).
    let mut reader = RecordAlignedFdReader::new(fd);

    loop {
        while conn.wants_write() {
            let mut writer = FdWriter { fd };
            conn.write_tls(&mut writer)?;
        }

        if !conn.is_handshaking() {
            break;
        }

        conn.read_tls(&mut reader)?;
        conn.process_new_packets()
            .map_err(|e| format!("TLS handshake error: {e}"))?;
    }

    // Drain any final flight (e.g., TLS 1.3 NewSessionTicket) so the peer
    // receives everything before we hand the fd to kTLS.
    while conn.wants_write() {
        let mut writer = FdWriter { fd };
        conn.write_tls(&mut writer)?;
    }

    Ok(())
}

fn arm_ktls(
    fd: i32,
    version: ProtocolVersion,
    secrets: &ExtractedSecrets,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ulp = b"tls\0";
    unsafe { syscall::setsockopt(fd, SOL_TCP, TCP_ULP, ulp.as_ptr(), ulp.len() as u32) }
        .map_err(|e| {
            format!(
                "setsockopt TCP_ULP failed: {e}. Is the 'tls' kernel module loaded? Try: modprobe tls"
            )
        })?;

    let ver = match version {
        ProtocolVersion::TLSv1_2 => TLS_1_2_VERSION,
        ProtocolVersion::TLSv1_3 => TLS_1_3_VERSION,
        v => return Err(format!("unsupported TLS version for kTLS: {v:?}").into()),
    };

    let (tx_seq, ref tx_secrets) = secrets.tx;
    let (rx_seq, ref rx_secrets) = secrets.rx;

    arm_direction(fd, TLS_TX, ver, tx_secrets, tx_seq)?;
    arm_direction(fd, TLS_RX, ver, rx_secrets, rx_seq)?;

    Ok(())
}

fn arm_direction(
    fd: i32,
    direction: i32,
    version: u16,
    secret: &rustls::ConnectionTrafficSecrets,
    seq: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = if direction == TLS_TX { "TX" } else { "RX" };
    let rec_seq = seq.to_be_bytes();

    match secret {
        rustls::ConnectionTrafficSecrets::Aes128Gcm { key, iv } => {
            let iv_bytes = iv.as_ref();
            let mut info = TlsCryptoInfoAesGcm128 {
                info: TlsCryptoInfo {
                    version,
                    cipher_type: TLS_CIPHER_AES_GCM_128,
                },
                iv: [0u8; 8],
                key: [0u8; 16],
                salt: [0u8; 4],
                rec_seq,
            };
            info.salt.copy_from_slice(&iv_bytes[..4]);
            info.iv.copy_from_slice(&iv_bytes[4..12]);
            info.key.copy_from_slice(key.as_ref());
            setsockopt_tls(fd, direction, &info, dir)
        }
        rustls::ConnectionTrafficSecrets::Aes256Gcm { key, iv } => {
            let iv_bytes = iv.as_ref();
            let mut info = TlsCryptoInfoAesGcm256 {
                info: TlsCryptoInfo {
                    version,
                    cipher_type: TLS_CIPHER_AES_GCM_256,
                },
                iv: [0u8; 8],
                key: [0u8; 32],
                salt: [0u8; 4],
                rec_seq,
            };
            info.salt.copy_from_slice(&iv_bytes[..4]);
            info.iv.copy_from_slice(&iv_bytes[4..12]);
            info.key.copy_from_slice(key.as_ref());
            setsockopt_tls(fd, direction, &info, dir)
        }
        _ => Err(format!("unsupported cipher suite for kTLS {dir}").into()),
    }
}

fn setsockopt_tls<T>(
    fd: i32,
    direction: i32,
    info: &T,
    dir: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    unsafe {
        syscall::setsockopt(
            fd,
            SOL_TLS,
            direction,
            info as *const T as *const u8,
            std::mem::size_of::<T>() as u32,
        )
    }
    .map_err(|e| format!("setsockopt TLS_{dir} failed: {e}"))?;
    Ok(())
}

// ── Blocking fd I/O adapters used during the pre-kTLS handshake ────────────

struct FdWriter {
    fd: i32,
}

impl std::io::Write for FdWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        unsafe { syscall::write(self.fd, buf.as_ptr(), buf.len()) }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Reads one complete TLS record per call so rustls always sees aligned input.
struct RecordAlignedFdReader {
    fd: i32,
    buf: Vec<u8>,
    pos: usize,
}

impl RecordAlignedFdReader {
    fn new(fd: i32) -> Self {
        Self {
            fd,
            buf: Vec::new(),
            pos: 0,
        }
    }

    fn fill_one_record(&mut self) -> std::io::Result<()> {
        let mut header = [0u8; 5];
        read_exact_fd(self.fd, &mut header)?;
        let record_len = u16::from_be_bytes([header[3], header[4]]) as usize;
        self.buf.clear();
        self.buf.reserve(5 + record_len);
        self.buf.extend_from_slice(&header);
        self.buf.resize(5 + record_len, 0);
        read_exact_fd(self.fd, &mut self.buf[5..])?;
        self.pos = 0;
        Ok(())
    }
}

impl std::io::Read for RecordAlignedFdReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            self.fill_one_record()?;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn read_exact_fd(fd: i32, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = unsafe { syscall::read(fd, buf.as_mut_ptr().add(filled), buf.len() - filled) }?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "peer closed during TLS handshake",
            ));
        }
        filled += n;
    }
    Ok(())
}
