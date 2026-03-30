//! Minimal local Nostr relay for benchmarking.
//!
//! Listens on ws://127.0.0.1:9999. On any REQ subscription, immediately blasts
//! N pre-generated signed events, sends EOSE, then sinks any incoming messages.
//!
//! Usage: cargo run -p nostro2-ring-relay --example local_server
//! Then point Caddy or clients at ws://127.0.0.1:9999

use nostro2_signer::NostrKeypair;
use std::net::{TcpListener, TcpStream};

const BASE_PORT: u16 = 9900;
const NUM_RELAYS: usize = 12;
const NUM_EVENTS: usize = 100_000;

fn main() {
    let keypair = NostrKeypair::new();
    let eose = "[\"EOSE\",\"bench\"]".to_string();
    println!("Generating {NUM_EVENTS} unique signed events per relay ({NUM_RELAYS} relays)...");

    // Spawn a listener on each port, each with its own unique events
    let mut handles = Vec::new();
    for i in 0..NUM_RELAYS {
        let port = BASE_PORT + i as u16;
        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
            panic!("failed to bind {addr}: {e}");
        });
        println!("Listening on ws://{addr}");

        // Generate unique events per relay so dedup caches don't filter them
        let events: Vec<String> = (0..NUM_EVENTS)
            .map(|j| {
                let mut note = nostro2::NostrNote {
                    content: format!("Relay {i} event {j}: {}", "x".repeat(100)),
                    kind: 1,
                    ..Default::default()
                };
                keypair.sign_note(&mut note).unwrap();
                let note_json = serde_json::to_string(&note).unwrap();
                format!("[\"EVENT\",\"bench\",{note_json}]")
            })
            .collect();
        let eose = eose.clone();
        handles.push(std::thread::spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[:{port}] accept error: {e}");
                        continue;
                    }
                };
                let events = events.clone();
                let eose = eose.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, &events, &eose) {
                        eprintln!("[:{port}] connection error: {e}");
                    }
                });
            }
        }));
    }
    println!("\n{NUM_RELAYS} relay servers ready. Waiting for connections...\n");

    for h in handles {
        let _ = h.join();
    }
}

fn handle_connection(
    stream: TcpStream,
    events: &[String],
    eose: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let peer = stream.peer_addr()?;
    println!("[{peer}] connected");

    let mut ws = tungstenite::accept(stream)?;
    println!("[{peer}] websocket handshake complete");

    // Wait for a REQ message
    loop {
        let msg = ws.read()?;
        match msg {
            tungstenite::Message::Text(text) => {
                if text.contains("\"REQ\"") {
                    println!("[{peer}] received REQ, sending {n} events...", n = events.len());
                    let start = std::time::Instant::now();

                    for event in events {
                        ws.send(tungstenite::Message::Text(event.clone().into()))?;
                    }
                    ws.send(tungstenite::Message::Text(eose.to_string().into()))?;

                    let elapsed = start.elapsed();
                    let rate = events.len() as f64 / elapsed.as_secs_f64();
                    println!("[{peer}] sent {n} events in {elapsed:.2?} ({rate:.0} events/s)",
                        n = events.len());

                    // Sink any further messages (echoes, etc.)
                    loop {
                        match ws.read() {
                            Ok(tungstenite::Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    break;
                }
            }
            tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }

    println!("[{peer}] disconnected");
    Ok(())
}

