//! kTLS setup: TLS handshake with rustls, secret extraction, and kernel TLS offload.

use crate::syscall;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection};
use std::io::Write;
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;

// kTLS constants from linux/tls.h
const SOL_TCP: i32 = 6;
const TCP_ULP: i32 = 31;
pub(crate) const SOL_TLS: i32 = 282;
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

// Compile-time verification that repr(C) structs match kernel layout.
const _: () = assert!(std::mem::size_of::<TlsCryptoInfo>() == 4);
const _: () = assert!(std::mem::size_of::<TlsCryptoInfoAesGcm128>() == 40);
const _: () = assert!(std::mem::size_of::<TlsCryptoInfoAesGcm256>() == 56);

/// Result of a successful kTLS + WebSocket connection setup.
pub struct KtlsConnection {
    /// Raw file descriptor with kTLS armed (plaintext read/write).
    pub fd: i32,
}

impl KtlsConnection {
    /// Consume the connection and return the raw fd without closing it.
    pub fn into_raw_fd(self) -> i32 {
        let fd = self.fd;
        std::mem::forget(self);
        fd
    }
}

impl Drop for KtlsConnection {
    fn drop(&mut self) {
        unsafe {
            syscall::close(self.fd);
        }
    }
}

/// Establish a TLS connection, extract secrets, set up kTLS, and perform WebSocket handshake.
pub fn connect(url: &str) -> Result<KtlsConnection, Box<dyn std::error::Error + Send + Sync>> {
    let (host, port, path) = parse_wss_url(url)?;

    // TCP connect
    let mut tcp = TcpStream::connect(format!("{host}:{port}"))?;
    let fd = tcp.as_raw_fd();

    // TLS handshake with secret extraction enabled
    let certs = rustls_native_certs::load_native_certs();
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add_parsable_certificates(certs.certs);
    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    config.enable_secret_extraction = true;

    let server_name: ServerName<'_> = host.as_str().try_into()?;
    let mut conn = ClientConnection::new(Arc::new(config), server_name.to_owned())?;

    // Drive the TLS handshake using a record-aligned reader.
    let mut tcp_write = tcp.try_clone()?;
    let mut reader = RecordAlignedReader::new(&mut tcp);
    loop {
        while conn.wants_write() {
            conn.write_tls(&mut tcp_write)?;
        }
        tcp_write.flush()?;

        if !conn.is_handshaking() {
            break;
        }

        conn.read_tls(&mut reader)?;
        conn.process_new_packets()?;
    }
    drop(reader);

    let tls_version = conn.protocol_version().ok_or("no TLS version negotiated")?;

    let secrets = conn
        .dangerous_extract_secrets()
        .map_err(|e| format!("failed to extract TLS secrets: {e}"))?;

    setup_ktls(fd, tls_version, &secrets)?;

    // Prevent TcpStream from closing the fd — we own it now
    std::mem::forget(tcp);

    ws_handshake(fd, &host, &path)?;

    Ok(KtlsConnection { fd })
}

fn setup_ktls(
    fd: i32,
    tls_version: rustls::ProtocolVersion,
    secrets: &rustls::ExtractedSecrets,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ulp = b"tls\0";
    unsafe {
        syscall::setsockopt(fd, SOL_TCP, TCP_ULP, ulp.as_ptr(), ulp.len() as u32)
    }
    .map_err(|err| {
        format!(
            "setsockopt TCP_ULP failed: {err}. Is the 'tls' kernel module loaded? Try: modprobe tls"
        )
    })?;

    let version = match tls_version {
        rustls::ProtocolVersion::TLSv1_2 => TLS_1_2_VERSION,
        rustls::ProtocolVersion::TLSv1_3 => TLS_1_3_VERSION,
        v => return Err(format!("unsupported TLS version: {v:?}").into()),
    };

    let (tx_seq, ref tx_secrets) = secrets.tx;
    let (rx_seq, ref rx_secrets) = secrets.rx;

    setup_direction(fd, TLS_TX, version, tx_secrets, tx_seq)?;
    setup_direction(fd, TLS_RX, version, rx_secrets, rx_seq)?;

    Ok(())
}

fn setup_direction(
    fd: i32,
    direction: i32,
    version: u16,
    secret: &rustls::ConnectionTrafficSecrets,
    seq: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir_name = if direction == TLS_TX { "TX" } else { "RX" };
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

            setsockopt_tls(fd, direction, &info, dir_name)
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

            setsockopt_tls(fd, direction, &info, dir_name)
        }
        _ => Err(format!("unsupported cipher suite for kTLS {dir_name}").into()),
    }
}

fn setsockopt_tls<T>(
    fd: i32,
    direction: i32,
    info: &T,
    dir_name: &str,
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
    .map_err(|err| format!("setsockopt TLS_{dir_name} failed: {err}"))?;
    Ok(())
}

fn ws_handshake(
    fd: i32,
    host: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = coyoquil::WsKey::new();
    let request = key.upgrade_request(host, path).map_err(|e| e.to_string())?;

    write_all_fd(fd, request.as_bytes())?;
    let response = read_http_response(fd)?;
    key.validate_response(&response)
        .map_err(|e| e.to_string())?;

    Ok(())
}

pub(crate) fn write_all_fd(fd: i32, mut data: &[u8]) -> Result<(), std::io::Error> {
    while !data.is_empty() {
        let n = unsafe { syscall::write(fd, data.as_ptr(), data.len()) }?;
        data = &data[n..];
    }
    Ok(())
}

/// Read from a kTLS fd, transparently skipping non-application-data records.
pub fn ktls_read(fd: i32, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    const TLS_GET_RECORD_TYPE: i32 = 2;
    const TLS_RECORD_TYPE_DATA: u8 = 0x17;

    loop {
        let mut iov = unsafe { syscall::IoVec::new(buf.as_mut_ptr(), buf.len()) };

        let mut cmsg_buf = [0u8; 64];

        let mut msg = syscall::MsgHdr::default();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr();
        msg.msg_controllen = cmsg_buf.len();

        let n = unsafe { syscall::recvmsg(fd, &mut msg, 0) }?;
        if n == 0 {
            return Ok(0);
        }

        // Check cmsg for TLS record type
        let mut record_type = TLS_RECORD_TYPE_DATA;
        unsafe {
            let mut cmsg = syscall::cmsg_firsthdr(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == SOL_TLS && (*cmsg).cmsg_type == TLS_GET_RECORD_TYPE {
                    record_type = *syscall::cmsg_data(cmsg);
                }
                cmsg = syscall::cmsg_nxthdr(&msg, cmsg);
            }
        }

        if record_type == TLS_RECORD_TYPE_DATA {
            return Ok(n);
        }
    }
}

fn read_http_response(fd: i32) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = vec![0u8; 4096];
    let mut total = 0;

    loop {
        let n = ktls_read(fd, &mut buf[total..])?;
        if n == 0 {
            return Err("connection closed during WebSocket handshake".into());
        }
        total += n;

        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf[..total]).into_owned());
        }

        if total >= buf.len() {
            return Err("HTTP response too large".into());
        }
    }
}

/// Reads exactly one complete TLS record at a time from the underlying TCP stream.
struct RecordAlignedReader<'a> {
    tcp: &'a mut TcpStream,
    buf: Vec<u8>,
    pos: usize,
}

impl<'a> RecordAlignedReader<'a> {
    fn new(tcp: &'a mut TcpStream) -> Self {
        Self {
            tcp,
            buf: Vec::new(),
            pos: 0,
        }
    }
}

impl std::io::Read for RecordAlignedReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.buf.len() {
            let n = std::cmp::min(out.len(), self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }

        let mut header = [0u8; 5];
        self.tcp.read_exact(&mut header)?;

        let record_len = u16::from_be_bytes([header[3], header[4]]) as usize;

        self.buf.clear();
        self.buf.reserve(5 + record_len);
        self.buf.extend_from_slice(&header);
        self.buf.resize(5 + record_len, 0);
        self.tcp.read_exact(&mut self.buf[5..])?;

        let n = std::cmp::min(out.len(), self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.pos = n;
        Ok(n)
    }
}

fn parse_wss_url(
    url: &str,
) -> Result<(String, u16, String), Box<dyn std::error::Error + Send + Sync>> {
    let url = url
        .strip_prefix("wss://")
        .ok_or("URL must start with wss://")?;

    let (host_port, path) = match url.find('/') {
        Some(idx) => (&url[..idx], url[idx..].to_string()),
        None => (url, "/".to_string()),
    };

    // Handle IPv6 bracket notation: [::1]:8080
    let (host, port) = if host_port.starts_with('[') {
        match host_port.find(']') {
            Some(end) => {
                let host = &host_port[1..end];
                let after = &host_port[end + 1..];
                let port = if let Some(port_str) = after.strip_prefix(':') {
                    port_str.parse::<u16>().unwrap_or(443)
                } else {
                    443
                };
                (host, port)
            }
            None => return Err("malformed IPv6 address (missing ']')".into()),
        }
    } else {
        match host_port.rfind(':') {
            Some(idx) => {
                let port_str = &host_port[idx + 1..];
                match port_str.parse::<u16>() {
                    Ok(port) => (&host_port[..idx], port),
                    Err(_) => (host_port, 443),
                }
            }
            None => (host_port, 443),
        }
    };

    Ok((host.to_string(), port, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wss_url_basic() {
        let (host, port, path) = parse_wss_url("wss://relay.damus.io").unwrap();
        assert_eq!(host, "relay.damus.io");
        assert_eq!(port, 443);
        assert_eq!(path, "/");
    }

    #[test]
    fn test_parse_wss_url_with_port() {
        let (host, port, path) = parse_wss_url("wss://localhost:8080/ws").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8080);
        assert_eq!(path, "/ws");
    }

    #[test]
    fn test_parse_wss_url_with_path() {
        let (host, port, path) = parse_wss_url("wss://relay.example.com/nostr").unwrap();
        assert_eq!(host, "relay.example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/nostr");
    }

    #[test]
    fn test_parse_wss_url_invalid() {
        assert!(parse_wss_url("ws://relay.damus.io").is_err());
        assert!(parse_wss_url("http://example.com").is_err());
    }

    #[test]
    fn test_parse_wss_url_ipv6() {
        let (host, port, path) = parse_wss_url("wss://[::1]:8080/ws").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 8080);
        assert_eq!(path, "/ws");
    }

    #[test]
    fn test_parse_wss_url_ipv6_no_port() {
        let (host, port, path) = parse_wss_url("wss://[2001:db8::1]/nostr").unwrap();
        assert_eq!(host, "2001:db8::1");
        assert_eq!(port, 443);
        assert_eq!(path, "/nostr");
    }

    #[test]
    fn test_parse_wss_url_ipv6_malformed() {
        assert!(parse_wss_url("wss://[::1/ws").is_err());
    }
}
