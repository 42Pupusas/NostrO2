use ring_relay_server::{ClientMessage, WsServer};
use tungstenite::{Message, client::IntoClientRequest, stream::MaybeTlsStream};
use std::net::TcpStream;

fn connect(port: u16) -> tungstenite::WebSocket<MaybeTlsStream<TcpStream>> {
    let url = format!("ws://127.0.0.1:{port}");
    let req = url.into_client_request().unwrap();
    let (ws, _resp) = tungstenite::connect(req).expect("WS connect failed");
    ws
}

#[test]
fn connect_and_receive_connected_event() {
    let mut server = WsServer::bind([127, 0, 0, 1], 0, 64).unwrap();
    let port = server.port();

    let _client = connect(port);

    match server.recv() {
        ClientMessage::Connected { client_id } => {
            assert!(client_id > 0, "client_id should be a valid fd");
        }
        other => panic!("expected Connected, got {other:?}"),
    }
}

#[test]
fn echo_text_roundtrip() {
    let mut server = WsServer::bind([127, 0, 0, 1], 0, 64).unwrap();
    let port = server.port();
    let sender = server.sender();

    let mut client = connect(port);

    // Wait for Connected event
    let client_id = match server.recv() {
        ClientMessage::Connected { client_id } => client_id,
        other => panic!("expected Connected, got {other:?}"),
    };

    // Client sends a message
    client.send(Message::Text("hello".into())).unwrap();

    // Server receives it
    match server.recv() {
        ClientMessage::Text { client_id: id, text } => {
            assert_eq!(id, client_id);
            assert_eq!(text, "hello");
        }
        other => panic!("expected Text, got {other:?}"),
    }

    // Server echoes it back
    sender.send_text(client_id, "hello".to_string()).unwrap();

    // Client receives the echo
    let msg = client.read().unwrap();
    assert_eq!(msg, Message::Text("hello".into()));
}

#[test]
fn multiple_clients() {
    let mut server = WsServer::bind([127, 0, 0, 1], 0, 64).unwrap();
    let port = server.port();
    let sender = server.sender();

    let mut client1 = connect(port);
    let id1 = match server.recv() {
        ClientMessage::Connected { client_id } => client_id,
        other => panic!("expected Connected, got {other:?}"),
    };

    let mut client2 = connect(port);
    let id2 = match server.recv() {
        ClientMessage::Connected { client_id } => client_id,
        other => panic!("expected Connected, got {other:?}"),
    };

    assert_ne!(id1, id2);

    // Send different messages to each client
    sender.send_text(id1, "for-client-1".to_string()).unwrap();
    sender.send_text(id2, "for-client-2".to_string()).unwrap();

    let msg1 = client1.read().unwrap();
    let msg2 = client2.read().unwrap();

    assert_eq!(msg1, Message::Text("for-client-1".into()));
    assert_eq!(msg2, Message::Text("for-client-2".into()));
}

#[test]
fn broadcast() {
    let mut server = WsServer::bind([127, 0, 0, 1], 0, 64).unwrap();
    let port = server.port();
    let sender = server.sender();

    let mut client1 = connect(port);
    let _ = server.recv(); // Connected

    let mut client2 = connect(port);
    let _ = server.recv(); // Connected

    sender.broadcast("everyone".to_string()).unwrap();

    let msg1 = client1.read().unwrap();
    let msg2 = client2.read().unwrap();

    assert_eq!(msg1, Message::Text("everyone".into()));
    assert_eq!(msg2, Message::Text("everyone".into()));
}

#[test]
fn client_disconnect_produces_event() {
    let mut server = WsServer::bind([127, 0, 0, 1], 0, 64).unwrap();
    let port = server.port();

    let client = connect(port);
    let client_id = match server.recv() {
        ClientMessage::Connected { client_id } => client_id,
        other => panic!("expected Connected, got {other:?}"),
    };

    // Drop the client — should produce a Disconnected event
    drop(client);

    match server.recv() {
        ClientMessage::Disconnected { client_id: id, .. } => {
            assert_eq!(id, client_id);
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
}
