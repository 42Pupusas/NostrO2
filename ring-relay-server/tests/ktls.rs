//! kTLS integration test: terminate TLS on the server via kernel offload,
//! then roundtrip a WebSocket text frame through the kTLS-armed fd.
//!
//! Skips at runtime when the host kernel doesn't have the `tls` module
//! loaded (setsockopt TCP_ULP fails with ENOENT otherwise).

#![cfg(feature = "ktls")]

use futures_util::{SinkExt, StreamExt};
use rcgen::{CertificateParams, KeyPair};
use ring_relay_server::{ClientMessage, ServerConfig, WsServer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Generate a self-signed cert + key valid for 127.0.0.1. Returns the cert
/// (for the client trust store) and a ready server config.
fn gen_tls() -> (CertificateDer<'static>, Arc<rustls::ServerConfig>) {
    let key_pair = KeyPair::generate().expect("keypair");
    let params = CertificateParams::new(vec!["127.0.0.1".into(), "localhost".into()])
        .expect("cert params");
    let cert = params.self_signed(&key_pair).expect("self sign");
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der: PrivateKeyDer<'static> = PrivateKeyDer::Pkcs8(key_pair.serialize_der().into());

    let mut server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("server cert");
    server_cfg.enable_secret_extraction = true;

    (cert_der, Arc::new(server_cfg))
}

fn client_tls_config(root: CertificateDer<'static>) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(root).expect("add root");
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Heuristic: check `/proc/modules` for a loaded `tls` entry. Linux-only.
fn ktls_available() -> bool {
    std::fs::read_to_string("/proc/modules")
        .map(|s| s.lines().any(|l| l.starts_with("tls ")))
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ktls_roundtrip() {
    if !ktls_available() {
        eprintln!("SKIP: kernel tls module not loaded (try: sudo modprobe tls)");
        return;
    }

    let (cert_for_client, tls_cfg) = gen_tls();

    let config = ServerConfig {
        tls: Some(tls_cfg),
        ..ServerConfig::default()
    };

    let mut server =
        WsServer::bind_with_config([127, 0, 0, 1], 0, 32, config).expect("bind tls server");
    let port = server.port();
    let sender = server.sender();

    // Drive the client concurrently with the server-side receive loop.
    let client_task = tokio::spawn(async move {
        let mut last_err = String::new();
        for _ in 0..20 {
            match connect(port, cert_for_client.clone()).await {
                Ok(mut ws) => {
                    ws.send(Message::Text("hello-ktls".into())).await.unwrap();
                    let resp = tokio::time::timeout(Duration::from_secs(2), ws.next())
                        .await
                        .expect("ws recv timeout")
                        .expect("stream ended")
                        .expect("ws error");
                    assert_eq!(resp, Message::Text("hello-ktls".into()));
                    return;
                }
                Err(e) => {
                    last_err = e;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        panic!("wss connect failed after retries: {last_err}");
    });

    eprintln!("waiting for Connected");
    let client_id = match server.recv() {
        ClientMessage::Connected { client_id, .. } => {
            eprintln!("got Connected fd={client_id}");
            client_id
        }
        other => panic!("expected Connected, got {other:?}"),
    };

    eprintln!("waiting for Text");
    match server.recv() {
        ClientMessage::Text { client_id: id, text } => {
            eprintln!("got Text: {text}");
            assert_eq!(id, client_id);
            assert_eq!(text, "hello-ktls");
            sender.send_text(client_id, text);
        }
        other => panic!("expected Text, got {other:?}"),
    }

    eprintln!("joining client");
    client_task.await.unwrap();
    eprintln!("done");
}

async fn connect(
    port: u16,
    cert: CertificateDer<'static>,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>, String>
{
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| format!("tcp connect: {e}"))?;
    let connector = tokio_tungstenite::Connector::Rustls(client_tls_config(cert));
    let url = format!("wss://127.0.0.1:{port}/");
    let req = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("req: {e}"))?;
    let (ws, _resp) =
        tokio_tungstenite::client_async_tls_with_config(req, tcp, None, Some(connector))
            .await
            .map_err(|e| format!("ws handshake: {e}"))?;
    Ok(ws)
}
