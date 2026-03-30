//! kTLS setup: TLS handshake with rustls, secret extraction, and kernel TLS offload.

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection};
use std::io::Write;
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;

// kTLS constants from linux/tls.h
const SOL_TCP: libc::c_int = 6;
const TCP_ULP: libc::c_int = 31;
pub(crate) const SOL_TLS: libc::c_int = 282;
const TLS_TX: libc::c_int = 1;
const TLS_RX: libc::c_int = 2;

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

/// Result of a successful kTLS + WebSocket connection setup.
pub struct KtlsConnection {
    /// Raw file descriptor with kTLS armed (plaintext read/write).
    pub fd: libc::c_int,
}

impl Drop for KtlsConnection {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
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
    // This ensures we only read complete TLS records from TCP, so the
    // TCP buffer is always at a record boundary when we hand off to kTLS.
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

    // Get TLS version and cipher before consuming the connection
    let tls_version = conn
        .protocol_version()
        .ok_or("no TLS version negotiated")?;

    let secrets = conn
        .dangerous_extract_secrets()
        .map_err(|e| format!("failed to extract TLS secrets: {e}"))?;

    // Set up kTLS
    setup_ktls(fd, tls_version, &secrets)?;

    // Prevent TcpStream from closing the fd — we own it now
    std::mem::forget(tcp);

    // WebSocket handshake over the kTLS fd
    ws_handshake(fd, &host, &path)?;

    Ok(KtlsConnection { fd })
}

fn setup_ktls(
    fd: libc::c_int,
    tls_version: rustls::ProtocolVersion,
    secrets: &rustls::ExtractedSecrets,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Enable TLS ULP on the socket
    let ulp = b"tls\0";
    let ret = unsafe {
        libc::setsockopt(
            fd,
            SOL_TCP,
            TCP_ULP,
            ulp.as_ptr() as *const libc::c_void,
            ulp.len() as libc::socklen_t,
        )
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "setsockopt TCP_ULP failed: {err}. Is the 'tls' kernel module loaded? Try: modprobe tls"
        )
        .into());
    }

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
    fd: libc::c_int,
    direction: libc::c_int,
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
    fd: libc::c_int,
    direction: libc::c_int,
    info: &T,
    dir_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ret = unsafe {
        libc::setsockopt(
            fd,
            SOL_TLS,
            direction,
            info as *const T as *const libc::c_void,
            std::mem::size_of::<T>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(format!(
            "setsockopt TLS_{dir_name} failed: {}",
            std::io::Error::last_os_error()
        )
        .into());
    }
    Ok(())
}

fn ws_handshake(
    fd: libc::c_int,
    host: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = super::ws::generate_ws_key();
    let request = super::ws::ws_upgrade_request(host, path, &key);

    write_all_fd(fd, request.as_bytes())?;
    let response = read_http_response(fd)?;
    super::ws::validate_ws_response(&response, &key)?;

    Ok(())
}

fn write_all_fd(fd: libc::c_int, mut data: &[u8]) -> Result<(), std::io::Error> {
    while !data.is_empty() {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        data = &data[n as usize..];
    }
    Ok(())
}

/// Read from a kTLS fd, transparently skipping non-application-data records.
///
/// TLS 1.3 post-handshake messages (e.g. NewSessionTicket) have inner content
/// type `handshake`, not `application_data`. The kernel kTLS `read()` returns
/// EIO for these. We use `recvmsg()` with cmsg to get the record type and
/// skip non-data records automatically.
pub fn ktls_read(fd: libc::c_int, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    const TLS_GET_RECORD_TYPE: libc::c_int = 2;
    const TLS_RECORD_TYPE_DATA: u8 = 0x17;

    loop {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };

        let mut cmsg_buf = [0u8; 64];

        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len();

        let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Ok(0);
        }

        // Check cmsg for TLS record type
        let mut record_type = TLS_RECORD_TYPE_DATA;
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == SOL_TLS
                    && (*cmsg).cmsg_type == TLS_GET_RECORD_TYPE
                {
                    record_type = *libc::CMSG_DATA(cmsg);
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }

        if record_type == TLS_RECORD_TYPE_DATA {
            return Ok(n as usize);
        }
        // Non-data record (e.g. NewSessionTicket) — skip and read again
    }
}

fn read_http_response(fd: libc::c_int) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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
///
/// Guarantees the TCP receive buffer is always at a TLS record boundary,
/// which is critical for kTLS handoff.
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

        // Read exactly one complete TLS record.
        // Header: content_type(1) + version(2) + length(2) = 5 bytes
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

    let (host, port) = match host_port.rfind(':') {
        Some(idx) => {
            let port_str = &host_port[idx + 1..];
            match port_str.parse::<u16>() {
                Ok(port) => (&host_port[..idx], port),
                Err(_) => (host_port, 443),
            }
        }
        None => (host_port, 443),
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
}
