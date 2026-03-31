//! High-throughput local Nostr relay for benchmarking.
//!
//! Listens on 12 ports (9900-9911). On any REQ, blasts N pre-serialized
//! events as fast as possible using buffered writes, then sends EOSE.
//!
//! Usage: cargo run -p relay-client --example local_server --release

use nostro2_signer::NostrKeypair;
use std::io::Write;
use std::net::{TcpListener, TcpStream};

const BASE_PORT: u16 = 9900;
const NUM_RELAYS: usize = 24;
const NUM_EVENTS: usize = 500_000;

fn main() {
    let keypair = NostrKeypair::new();
    println!("Generating {NUM_EVENTS} unique signed events per relay ({NUM_RELAYS} relays)...");

    let mut handles = Vec::new();
    for i in 0..NUM_RELAYS {
        let port = BASE_PORT + i as u16;
        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
            panic!("failed to bind {addr}: {e}");
        });
        println!("Listening on ws://{addr}");

        // Pre-generate and pre-serialize events as WebSocket frames
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

        let eose = "[\"EOSE\",\"bench\"]".to_string();
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
    // Increase TCP send buffer for throughput
    stream.set_nodelay(true)?;

    let mut ws = tungstenite::accept(stream)?;

    // Wait for a REQ message
    loop {
        let msg = ws.read()?;
        match msg {
            tungstenite::Message::Text(text) => {
                if text.contains("\"REQ\"") {
                    println!(
                        "[{peer}] REQ received, sending {n} events...",
                        n = events.len()
                    );
                    let start = std::time::Instant::now();

                    // Batch-send: write frames without flushing each one
                    for event in events {
                        ws.write(tungstenite::Message::Text(event.clone().into()))?;
                    }
                    ws.write(tungstenite::Message::Text(eose.to_string().into()))?;
                    ws.flush()?;

                    let elapsed = start.elapsed();
                    let rate = events.len() as f64 / elapsed.as_secs_f64();
                    println!(
                        "[{peer}] sent {n} events in {elapsed:.2?} ({rate:.0} events/s)",
                        n = events.len()
                    );

                    // Sink any further messages
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
