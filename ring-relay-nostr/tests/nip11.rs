//! NIP-11 end-to-end test: GET / with application/nostr+json should return
//! the relay information document, not a WebSocket upgrade.

use ring_relay_nostr::{NostrRelay, RelayConfig, RelayInfo};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn spawn_relay(config: RelayConfig) -> (u16, ring_relay_nostr::ShutdownHandle) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    rx.recv().unwrap()
}

fn tcp(port: u16) -> TcpStream {
    for _ in 0..20 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            return s;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("tcp connect failed");
}

fn send_and_read(port: u16, request: &str) -> String {
    let mut sock = tcp(port);
    sock.write_all(request.as_bytes()).unwrap();
    sock.flush().unwrap();
    let mut buf = Vec::new();
    let _ = sock.read_to_end(&mut buf); // server closes on HTTP responses
    String::from_utf8_lossy(&buf).into_owned()
}

#[test]
fn nip11_serves_info_document() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let req = "GET / HTTP/1.1\r\nHost: localhost\r\nAccept: application/nostr+json\r\n\r\n";
    let resp = send_and_read(port, req);

    assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "got:\n{resp}");
    assert!(resp.contains("Content-Type: application/nostr+json"));

    // Body is after the blank line. Parse and spot-check.
    let body = resp
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .expect("body delimiter");
    let v: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
    assert_eq!(v["supported_nips"], serde_json::json!([1, 11]));

    shutdown.shutdown();
}

#[test]
fn get_without_nostr_accept_gets_404() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let req = "GET / HTTP/1.1\r\nHost: localhost\r\nAccept: text/html\r\n\r\n";
    let resp = send_and_read(port, req);

    assert!(
        resp.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "got:\n{resp}"
    );

    shutdown.shutdown();
}

#[test]
fn get_other_path_gets_404() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let req = "GET /not-here HTTP/1.1\r\nHost: localhost\r\nAccept: application/nostr+json\r\n\r\n";
    let resp = send_and_read(port, req);

    assert!(
        resp.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "got:\n{resp}"
    );

    shutdown.shutdown();
}

#[test]
fn ws_upgrade_still_works_with_http_handler_present() {
    use std::net::TcpStream as StdTcp;
    use tungstenite::{Message, client::IntoClientRequest};

    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let url = format!("ws://127.0.0.1:{port}");
    let req = url.into_client_request().unwrap();
    // Retry since the relay thread may not be accepting yet.
    let mut ws = None;
    for _ in 0..20 {
        match tungstenite::connect(req.clone()) {
            Ok((w, _)) => {
                ws = Some(w);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut ws = ws.expect("ws connect");

    ws.send(Message::Text(r#"["REQ","s1",{"kinds":[1]}]"#.into()))
        .unwrap();
    let msg = ws.read().unwrap();
    match msg {
        Message::Text(t) => assert!(t.contains("EOSE"), "got: {t}"),
        other => panic!("expected text, got {other:?}"),
    }

    drop(ws);
    let _ = StdTcp::connect(("127.0.0.1", port)); // tickle
    shutdown.shutdown();
}

#[test]
fn custom_info_document_served() {
    let mut info = RelayInfo::minimal();
    info.name = Some("Test Relay".into());
    info.description = Some("ephemeral".into());
    info.pubkey = Some("a".repeat(64));

    let config = RelayConfig {
        info: Some(info),
        ..RelayConfig::default()
    };
    let (port, shutdown) = spawn_relay(config);

    let req = "GET / HTTP/1.1\r\nHost: localhost\r\nAccept: application/nostr+json\r\n\r\n";
    let resp = send_and_read(port, req);

    let body = resp.split_once("\r\n\r\n").unwrap().1;
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["name"], "Test Relay");
    assert_eq!(v["description"], "ephemeral");
    assert_eq!(v["pubkey"], "a".repeat(64));

    shutdown.shutdown();
}
