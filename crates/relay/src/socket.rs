//! A blocking WebSocket socket for one relay.
//!
//! [`WsSocket`] owns the transport, the RFC-6455 codec, and the read buffer
//! of a single connection. It is deliberately synchronous: the driver thread
//! that owns it does blocking IO, which keeps the crate free of any async
//! runtime. Nothing here is shared, so nothing here needs a lock.

use std::io::{Read, Write};

/// Read chunk handed to the frame decoder on each read.
const READ_CHUNK: usize = 64 * 1024;
/// Upper bound on the HTTP upgrade response head. A hostile server could
/// otherwise stream headers forever.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// One inbound application message.
///
/// Control frames never reach the caller: [`WsSocket::poll`] answers a ping
/// with a pong and drops pongs itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// A text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
    /// The peer closed the connection, with its reason when it gave one.
    Close(Option<String>),
}

/// Why a socket operation failed.
#[derive(Debug)]
pub enum WsSocketError {
    /// DNS resolution produced no address for the host.
    Resolve(String),
    /// A transport read, write, or connect failed.
    Io(std::io::Error),
    /// TLS setup failed.
    Tls(crate::tls::RelayTlsError),
    /// The server refused or botched the RFC-6455 upgrade.
    Handshake(coyoquil::WsError),
    /// The peer sent bytes that are not a valid frame stream.
    Protocol(coyoquil::FrameError),
    /// The upgrade response head exceeded [`MAX_HEAD_BYTES`].
    HeadTooLarge,
    /// The peer closed the transport without a close frame.
    Eof,
}

impl std::fmt::Display for WsSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(host) => write!(f, "no address found for '{host}'"),
            Self::Io(e) => write!(f, "socket io error: {e}"),
            Self::Tls(e) => write!(f, "{e}"),
            Self::Handshake(e) => write!(f, "websocket upgrade failed: {e}"),
            Self::Protocol(e) => write!(f, "websocket protocol error: {e}"),
            Self::HeadTooLarge => f.write_str("websocket upgrade response head is too large"),
            Self::Eof => f.write_str("relay closed the connection"),
        }
    }
}

impl std::error::Error for WsSocketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Tls(e) => Some(e),
            Self::Resolve(_) | Self::Handshake(_) | Self::Protocol(_) | Self::HeadTooLarge
            | Self::Eof => None,
        }
    }
}

impl From<std::io::Error> for WsSocketError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<crate::tls::RelayTlsError> for WsSocketError {
    fn from(value: crate::tls::RelayTlsError) -> Self {
        Self::Tls(value)
    }
}

impl From<coyoquil::WsError> for WsSocketError {
    fn from(value: coyoquil::WsError) -> Self {
        Self::Handshake(value)
    }
}

impl From<coyoquil::FrameError> for WsSocketError {
    fn from(value: coyoquil::FrameError) -> Self {
        Self::Protocol(value)
    }
}

/// The byte stream under the WebSocket framing, with or without TLS.
enum Transport {
    Plain(std::net::TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>>),
}

impl Transport {
    fn tcp(&self) -> &std::net::TcpStream {
        match self {
            Self::Plain(stream) => stream,
            Self::Tls(stream) => stream.get_ref(),
        }
    }

    fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> std::io::Result<()> {
        self.tcp().set_read_timeout(timeout)
    }
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl Transport {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Plain(_) => "plain",
            Self::Tls(_) => "tls",
        }
    }
}

/// A live WebSocket connection to one relay.
pub struct WsSocket {
    transport: Transport,
    decoder: coyoquil::FrameDecoder,
    read_buf: Vec<u8>,
    encode_buf: Vec<u8>,
}

impl std::fmt::Debug for WsSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsSocket")
            .field("transport", &self.transport.kind())
            .field("peer", &self.transport.tcp().peer_addr().ok())
            .finish_non_exhaustive()
    }
}

impl WsSocket {
    /// Dials `url`, runs TLS when the scheme asks for it, and completes the
    /// RFC-6455 upgrade.
    ///
    /// `connect_timeout` bounds the TCP handshake, the TLS handshake, and
    /// the upgrade exchange alike: each waits on a relay that has not
    /// answered yet. `read_timeout` applies only afterwards, as the driver
    /// loop's steady-state pace.
    ///
    /// The two must not be confused. The pace is short by design, so using
    /// it during the upgrade would fail every relay that takes longer than
    /// a few milliseconds to reply.
    ///
    /// # Errors
    ///
    /// Returns [`WsSocketError`] when resolution, connection, TLS, or the
    /// upgrade fails.
    pub fn connect(
        url: &crate::url::RelayUrl,
        tls: &crate::tls::RelayTls,
        connect_timeout: std::time::Duration,
        read_timeout: std::time::Duration,
    ) -> Result<Self, WsSocketError> {
        let stream = Self::dial(url, connect_timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(connect_timeout))?;
        let transport = if url.is_secure() {
            Transport::Tls(Box::new(tls.connect(url.host(), stream)?))
        } else {
            Transport::Plain(stream)
        };

        let mut socket = Self {
            transport,
            decoder: coyoquil::FrameDecoder::new(coyoquil::Role::Client),
            read_buf: vec![0_u8; READ_CHUNK],
            encode_buf: Vec::with_capacity(READ_CHUNK),
        };
        socket.upgrade(url)?;
        socket.set_read_timeout(read_timeout)?;
        Ok(socket)
    }

    fn dial(
        url: &crate::url::RelayUrl,
        timeout: std::time::Duration,
    ) -> Result<std::net::TcpStream, WsSocketError> {
        use std::net::ToSocketAddrs as _;

        let addrs = (url.host(), url.port())
            .to_socket_addrs()
            .map_err(|_| WsSocketError::Resolve(url.host().to_owned()))?;
        let mut last: Option<std::io::Error> = None;
        for addr in addrs {
            match std::net::TcpStream::connect_timeout(&addr, timeout) {
                Ok(stream) => return Ok(stream),
                Err(e) => last = Some(e),
            }
        }
        Err(last.map_or_else(
            || WsSocketError::Resolve(url.host().to_owned()),
            WsSocketError::Io,
        ))
    }

    fn upgrade(&mut self, url: &crate::url::RelayUrl) -> Result<(), WsSocketError> {
        let key = coyoquil::WsKey::new();
        let request = key.upgrade_request(url.authority(), url.target())?;
        self.transport.write_all(request.as_bytes())?;
        self.transport.flush()?;
        let head = self.read_response_head()?;
        key.validate_response(head.trim())?;
        Ok(())
    }

    fn read_response_head(&mut self) -> Result<String, WsSocketError> {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            if head.len() > MAX_HEAD_BYTES {
                return Err(WsSocketError::HeadTooLarge);
            }
            match self.transport.read(&mut byte)? {
                0 => return Err(WsSocketError::Eof),
                _ => head.push(byte[0]),
            }
            if head.ends_with(b"\r\n\r\n") {
                return Ok(String::from_utf8_lossy(&head).into_owned());
            }
        }
    }

    /// Replaces the read timeout that paces [`Self::poll`].
    ///
    /// # Errors
    ///
    /// Returns [`WsSocketError::Io`] when the socket refuses the option.
    pub fn set_read_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(), WsSocketError> {
        self.transport.set_read_timeout(Some(timeout))?;
        Ok(())
    }

    /// Writes one masked text frame.
    ///
    /// # Errors
    ///
    /// Returns [`WsSocketError::Io`] when the write fails.
    pub fn send_text(&mut self, text: &str) -> Result<(), WsSocketError> {
        self.write_frame(&coyoquil::Frame::Text(text))
    }

    /// Writes a close frame with a normal status.
    ///
    /// # Errors
    ///
    /// Returns [`WsSocketError::Io`] when the write fails.
    pub fn send_close(&mut self) -> Result<(), WsSocketError> {
        self.write_frame(&coyoquil::Frame::Close(Some((
            coyoquil::CloseCode::Normal,
            b"",
        ))))
    }

    fn write_frame(&mut self, frame: &coyoquil::Frame<'_>) -> Result<(), WsSocketError> {
        self.encode_buf.clear();
        frame.encode_masked(coyoquil::MaskKey::new(), &mut self.encode_buf);
        self.transport.write_all(&self.encode_buf)?;
        self.transport.flush()?;
        Ok(())
    }

    /// Returns the next application message.
    ///
    /// `Ok(None)` means the read timeout elapsed with no complete message,
    /// which is the driver loop's cue to do its other work and come back.
    /// Pings are answered with pongs before this returns.
    ///
    /// # Errors
    ///
    /// Returns [`WsSocketError::Eof`] when the peer disappears without a
    /// close frame, [`WsSocketError::Protocol`] on a malformed frame stream,
    /// and [`WsSocketError::Io`] on any other transport failure.
    pub fn poll(&mut self) -> Result<Option<WsMessage>, WsSocketError> {
        loop {
            if let Some(decoded) = self.take_decoded()? {
                match decoded {
                    Decoded::Message(message) => return Ok(Some(message)),
                    Decoded::Ping(payload) => {
                        self.write_frame(&coyoquil::Frame::Pong(&payload))?;
                    }
                    Decoded::Pong => {}
                }
                continue;
            }

            match self.transport.read(&mut self.read_buf) {
                Ok(0) => return Err(WsSocketError::Eof),
                Ok(n) => {
                    let chunk = &self.read_buf[..n];
                    self.decoder.push(chunk)?;
                }
                Err(e) if Self::is_timeout(&e) => return Ok(None),
                Err(e) => return Err(WsSocketError::Io(e)),
            }
        }
    }

    fn take_decoded(&mut self) -> Result<Option<Decoded>, WsSocketError> {
        Ok(match self.decoder.next_frame()? {
            Some(coyoquil::Frame::Text(text)) => {
                Some(Decoded::Message(WsMessage::Text(text.to_owned())))
            }
            Some(coyoquil::Frame::Binary(data)) => {
                Some(Decoded::Message(WsMessage::Binary(data.to_vec())))
            }
            Some(coyoquil::Frame::Close(reason)) => Some(Decoded::Message(WsMessage::Close(
                reason.and_then(|(_, body)| {
                    let text = String::from_utf8_lossy(body).into_owned();
                    (!text.is_empty()).then_some(text)
                }),
            ))),
            Some(coyoquil::Frame::Ping(payload)) => Some(Decoded::Ping(payload.to_vec())),
            Some(coyoquil::Frame::Pong(_)) => Some(Decoded::Pong),
            None => None,
        })
    }

    fn is_timeout(error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    }
}

/// What one decoded frame means to [`WsSocket::poll`].
enum Decoded {
    Message(WsMessage),
    Ping(Vec<u8>),
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-connection WebSocket server built on an independent RFC-6455
    /// implementation, so these tests prove interop instead of agreement
    /// between our encoder and our decoder.
    struct EchoServer {
        port: u16,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl EchoServer {
        fn start() -> Self {
            Self::spawn(Behaviour::Echo)
        }

        fn refusing() -> Self {
            Self::spawn(Behaviour::RefuseUpgrade)
        }

        fn pinging() -> Self {
            Self::spawn(Behaviour::Ping)
        }

        fn closing() -> Self {
            Self::spawn(Behaviour::CloseImmediately)
        }

        fn dawdling() -> Self {
            Self::spawn(Behaviour::SlowUpgrade)
        }

        fn spawn(behaviour: Behaviour) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let handle = std::thread::spawn(move || behaviour.serve(&listener));
            Self {
                port,
                handle: Some(handle),
            }
        }

        fn url(&self) -> crate::url::RelayUrl {
            crate::url::RelayUrl::parse(&format!("ws://127.0.0.1:{}", self.port)).unwrap()
        }

        fn client(&self) -> WsSocket {
            WsSocket::connect(
                &self.url(),
                &crate::tls::RelayTls::new().unwrap(),
                std::time::Duration::from_secs(5),
                std::time::Duration::from_millis(200),
            )
            .unwrap()
        }
    }

    impl Drop for EchoServer {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Behaviour {
        Echo,
        RefuseUpgrade,
        Ping,
        CloseImmediately,
        /// Waits before answering the upgrade, like a loaded relay.
        SlowUpgrade,
    }

    impl Behaviour {
        fn serve(self, listener: &std::net::TcpListener) {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            match self {
                Self::RefuseUpgrade => Self::refuse(stream),
                Self::Echo => Self::echo(stream),
                Self::Ping => Self::ping(stream),
                Self::CloseImmediately => Self::close(stream),
                Self::SlowUpgrade => Self::slow_upgrade(stream),
            }
        }

        /// The upgrade takes far longer than any sane IO pace.
        fn slow_upgrade(stream: std::net::TcpStream) {
            std::thread::sleep(std::time::Duration::from_millis(120));
            Self::echo(stream);
        }

        fn refuse(mut stream: std::net::TcpStream) {
            let mut scratch = [0_u8; 1024];
            let _ = stream.read(&mut scratch);
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }

        fn echo(stream: std::net::TcpStream) {
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            while let Ok(message) = ws.read() {
                match message {
                    tungstenite::Message::Text(text) => {
                        if ws.send(tungstenite::Message::Text(text)).is_err() {
                            return;
                        }
                    }
                    tungstenite::Message::Close(_) => return,
                    _ => {}
                }
            }
        }

        fn ping(stream: std::net::TcpStream) {
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            if ws
                .send(tungstenite::Message::Ping(b"beat".as_slice().into()))
                .is_err()
            {
                return;
            }
            while let Ok(message) = ws.read() {
                if let tungstenite::Message::Pong(payload) = message {
                    let _ = ws.send(tungstenite::Message::Text(
                        String::from_utf8_lossy(&payload).into_owned().into(),
                    ));
                    return;
                }
            }
        }

        fn close(stream: std::net::TcpStream) {
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            let _ = ws.send(tungstenite::Message::Close(Some(
                tungstenite::protocol::CloseFrame {
                    code: tungstenite::protocol::frame::coding::CloseCode::Normal,
                    reason: "bye".into(),
                },
            )));
            let _ = ws.flush();
            while ws.read().is_ok() {}
        }
    }

    struct Waited;

    impl Waited {
        fn message(socket: &mut WsSocket) -> WsMessage {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match socket.poll() {
                    Ok(Some(message)) => return message,
                    Ok(None) => {}
                    Err(e) => panic!("poll failed: {e}"),
                }
            }
            panic!("no message arrived before the deadline");
        }
    }

    #[test]
    fn a_handshake_completes_against_an_independent_server() {
        let server = EchoServer::start();
        let _socket = server.client();
    }

    // The IO pace is short, and a busy relay can take far longer than that
    // to answer an upgrade. The pace must therefore apply only after the
    // upgrade completes: `connect_timeout` covers the exchange itself.
    #[test]
    fn a_slow_relay_still_completes_its_upgrade() {
        let server = EchoServer::dawdling();
        let mut socket = WsSocket::connect(
            &server.url(),
            &crate::tls::RelayTls::new().unwrap(),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(5),
        )
        .expect("a relay slower than the IO pace must still connect");

        socket.send_text("[\"REQ\",\"sub\",{}]").unwrap();
        assert_eq!(
            Waited::message(&mut socket),
            WsMessage::Text("[\"REQ\",\"sub\",{}]".to_owned())
        );
    }

    #[test]
    fn a_text_frame_round_trips() {
        let server = EchoServer::start();
        let mut socket = server.client();
        socket.send_text("[\"REQ\",\"sub\",{}]").unwrap();
        assert_eq!(
            Waited::message(&mut socket),
            WsMessage::Text("[\"REQ\",\"sub\",{}]".to_owned())
        );
    }

    #[test]
    fn many_frames_arrive_in_order() {
        let server = EchoServer::start();
        let mut socket = server.client();
        for i in 0..16 {
            socket.send_text(&format!("msg-{i}")).unwrap();
        }
        for i in 0..16 {
            assert_eq!(
                Waited::message(&mut socket),
                WsMessage::Text(format!("msg-{i}"))
            );
        }
    }

    #[test]
    fn a_large_frame_survives_the_round_trip() {
        let server = EchoServer::start();
        let mut socket = server.client();
        let payload = "x".repeat(256 * 1024);
        socket.send_text(&payload).unwrap();
        assert_eq!(Waited::message(&mut socket), WsMessage::Text(payload));
    }

    #[test]
    fn a_ping_is_answered_with_a_pong() {
        let server = EchoServer::pinging();
        let mut socket = server.client();
        assert_eq!(
            Waited::message(&mut socket),
            WsMessage::Text("beat".to_owned())
        );
    }

    #[test]
    fn a_close_frame_surfaces_its_reason() {
        let server = EchoServer::closing();
        let mut socket = server.client();
        assert_eq!(
            Waited::message(&mut socket),
            WsMessage::Close(Some("bye".to_owned()))
        );
    }

    #[test]
    fn poll_returns_none_when_the_read_timeout_elapses() {
        let server = EchoServer::start();
        let mut socket = server.client();
        assert_eq!(socket.poll().unwrap(), None);
    }

    #[test]
    fn a_refused_upgrade_is_a_handshake_error() {
        let server = EchoServer::refusing();
        let error = WsSocket::connect(
            &server.url(),
            &crate::tls::RelayTls::new().unwrap(),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(error, WsSocketError::Handshake(_)));
        assert!(error.to_string().contains("upgrade failed"));
    }

    #[test]
    fn a_dead_port_is_an_io_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = crate::url::RelayUrl::parse(&format!("ws://127.0.0.1:{port}")).unwrap();
        let error = WsSocket::connect(
            &url,
            &crate::tls::RelayTls::new().unwrap(),
            std::time::Duration::from_millis(500),
            std::time::Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(error, WsSocketError::Io(_)));
    }

    #[test]
    fn an_unresolvable_host_reports_the_host() {
        let url = crate::url::RelayUrl::parse("ws://relay.invalid.invalid:80").unwrap();
        let error = WsSocket::connect(
            &url,
            &crate::tls::RelayTls::new().unwrap(),
            std::time::Duration::from_millis(500),
            std::time::Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(error, WsSocketError::Resolve(_)));
        assert!(error.to_string().contains("relay.invalid.invalid"));
    }
}
