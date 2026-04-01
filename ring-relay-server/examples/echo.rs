//! Simple WebSocket echo server.
//!
//! Run: cargo run --example echo -p ring-relay-server
//! Test: websocat ws://127.0.0.1:9090

use ring_relay_server::{ClientMessage, WsServer};

fn main() {
    let port = 9090;
    println!("Starting WebSocket echo server on 0.0.0.0:{port}");

    let mut server = WsServer::bind([0, 0, 0, 0], port, 1024)
        .expect("failed to start server");

    let sender = server.sender();

    loop {
        match server.recv() {
            ClientMessage::Connected { client_id } => {
                println!("[+] Client {client_id} connected");
            }
            ClientMessage::Text { client_id, text } => {
                println!("[<] Client {client_id}: {text}");
                if let Err(e) = sender.send_text(client_id, text) {
                    eprintln!("[!] Failed to echo to {client_id}: {e}");
                }
            }
            ClientMessage::Binary { client_id, data } => {
                println!("[<] Client {client_id}: {} bytes binary", data.len());
                if let Err(e) = sender.send_binary(client_id, data) {
                    eprintln!("[!] Failed to echo to {client_id}: {e}");
                }
            }
            ClientMessage::Disconnected { client_id, reason } => {
                println!("[-] Client {client_id} disconnected: {reason:?}");
            }
        }
    }
}
