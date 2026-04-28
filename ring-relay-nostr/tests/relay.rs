//! End-to-end integration tests for the ephemeral Nostr relay.
//!
//! Drives the real `NostrRelay` over a real WebSocket and verifies NIP-01 flow:
//! EVENT is OK'd, REQ yields EOSE, subsequent matching EVENTs fan out to
//! subscribers, CLOSE stops delivery, FIFO eviction kicks in at the sub cap.

use futures_util::{SinkExt, StreamExt};
use nostro2::{NostrNote, NostrSigner};
use nostro2_signer::K256Keypair;
use ring_relay_nostr::{NostrRelay, RelayConfig, StorageConfig};
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsClient = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// RAII handle: signals the relay to shut down on drop and waits for the
/// worker thread to actually exit. Tests must hold this for the full duration
/// of the test — letting it drop early (or never) leaves shard threads
/// running, which produces flaky timeouts under parallel cargo-test load
/// because dozens of leaked threads compete for the runtime's CPU.
struct RelayGuard {
    shutdown: Option<ring_relay_nostr::ShutdownHandle>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RelayGuard {
    fn shutdown(self) {
        // Explicit shutdown that joins; preferred over relying on Drop so
        // tests fail loudly if the relay thread hangs.
        drop(self);
    }
}

impl Drop for RelayGuard {
    fn drop(&mut self) {
        if let Some(s) = self.shutdown.take() {
            s.shutdown();
        }
        if let Some(h) = self.handle.take() {
            // If a test panicked we still want to clean up; ignore the result.
            let _ = h.join();
        }
    }
}

/// Spawn a relay on an ephemeral port. Returns the port and a guard that
/// shuts the relay down (and joins its worker thread) when dropped.
fn spawn_relay(config: RelayConfig) -> (u16, RelayGuard) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut relay = NostrRelay::bind([127, 0, 0, 1], 0, config).expect("bind relay");
        let port = relay.port();
        let shutdown = relay.shutdown_handle();
        tx.send((port, shutdown)).unwrap();
        relay.run();
    });
    let (port, shutdown) = rx.recv().unwrap();
    (
        port,
        RelayGuard {
            shutdown: Some(shutdown),
            handle: Some(handle),
        },
    )
}

async fn connect(port: u16) -> WsClient {
    // Tiny retry: the relay thread needs a moment to start accepting.
    for attempt in 0..20 {
        let url = format!("ws://127.0.0.1:{port}/");
        match connect_async(&url).await {
            Ok((ws, _)) => return ws,
            Err(_) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("connect failed: {e}"),
        }
    }
    unreachable!();
}

async fn send(ws: &mut WsClient, json: &str) {
    ws.send(Message::Text(json.to_string().into()))
        .await
        .unwrap();
}

/// Read the next text frame with a timeout so tests fail fast on regressions.
///
/// Pings/Pongs slip through silently; everything else is a real signal that
/// belongs to the test. The 5s timeout is intentionally generous because
/// `cargo test` defaults to running all tests in parallel — under heavy
/// load (14 tests × 2-4 tokio worker threads each on a modest box) any
/// individual frame can take noticeably longer than wall-clock 50ms.
async fn recv_text(ws: &mut WsClient) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout waiting for frame")
            .expect("stream ended")
            .expect("ws error");
        match msg {
            Message::Text(t) => return serde_json::from_str(&t).expect("relay response not JSON"),
            // Drop control frames; tungstenite may surface server-initiated
            // pings or our own queued pongs depending on timing.
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected text, got {other:?}"),
        }
    }
}

fn signed_note(content: &str) -> NostrNote {
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note(content);
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).expect("sign");
    assert!(note.verify());
    note
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_is_ok_acked() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    let note = signed_note("hello");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], true);

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn req_yields_immediate_eose() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    send(&mut ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "EOSE");
    assert_eq!(resp[1], "s1");

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_fanout_to_matching_subscriber() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let mut sub_ws = connect(port).await;
    let mut pub_ws = connect(port).await;

    // Subscribe to kind 1.
    send(&mut sub_ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let eose = recv_text(&mut sub_ws).await;
    assert_eq!(eose[0], "EOSE");

    // Publish a matching event from the other connection.
    let note = signed_note("hi");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;

    // Publisher sees OK.
    let ok = recv_text(&mut pub_ws).await;
    assert_eq!(ok[0], "OK");
    assert_eq!(ok[2], true);

    // Subscriber sees the event.
    let evt = recv_text(&mut sub_ws).await;
    assert_eq!(evt[0], "EVENT");
    assert_eq!(evt[1], "s1");
    assert_eq!(evt[2]["id"].as_str().unwrap(), id);

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_matching_filter_does_not_receive() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let mut sub_ws = connect(port).await;
    let mut pub_ws = connect(port).await;

    // Subscribe to kind 7 only.
    send(&mut sub_ws, r#"["REQ","s1",{"kinds":[7]}]"#).await;
    let _ = recv_text(&mut sub_ws).await;

    // Publish kind 1.
    let note = signed_note("nope");
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;
    let _ok = recv_text(&mut pub_ws).await;

    // Subscriber should not receive; use a short timeout.
    let result = tokio::time::timeout(Duration::from_millis(200), sub_ws.next()).await;
    assert!(
        result.is_err(),
        "subscriber unexpectedly received: {result:?}"
    );

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_stops_fanout() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());

    let mut sub_ws = connect(port).await;
    let mut pub_ws = connect(port).await;

    send(&mut sub_ws, r#"["REQ","s1",{"kinds":[1]}]"#).await;
    let _ = recv_text(&mut sub_ws).await;

    send(&mut sub_ws, r#"["CLOSE","s1"]"#).await;
    // Give dispatcher a moment to process the CLOSE before publishing.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let note = signed_note("after close");
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;
    let _ok = recv_text(&mut pub_ws).await;

    let result = tokio::time::timeout(Duration::from_millis(200), sub_ws.next()).await;
    assert!(result.is_err(), "closed sub received: {result:?}");

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_fifo_eviction() {
    let mut config = RelayConfig::default();
    config.max_subs_per_conn = 2;
    let (port, shutdown) = spawn_relay(config);
    let mut ws = connect(port).await;

    send(&mut ws, r#"["REQ","a",{"kinds":[1]}]"#).await;
    let _ = recv_text(&mut ws).await; // EOSE a
    send(&mut ws, r#"["REQ","b",{"kinds":[1]}]"#).await;
    let _ = recv_text(&mut ws).await; // EOSE b

    // Third sub evicts "a".
    send(&mut ws, r#"["REQ","c",{"kinds":[1]}]"#).await;

    // Expect a CLOSED for "a" followed by EOSE for "c". Order: we emit CLOSED
    // *before* EOSE in `on_req`, so read in that order.
    let closed = recv_text(&mut ws).await;
    assert_eq!(closed[0], "CLOSED");
    assert_eq!(closed[1], "a");

    let eose = recv_text(&mut ws).await;
    assert_eq!(eose[0], "EOSE");
    assert_eq!(eose[1], "c");

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_event_rejected() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    // Signed by one key, pubkey swapped to a different one → signature fails.
    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("tampered");
    note.pubkey = kp.public_key();
    kp.sign_nostr_note(&mut note).unwrap();
    note.pubkey = "0".repeat(64); // corrupt the pubkey after signing

    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[2], false);

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_verb_gets_notice() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    send(&mut ws, r#"["AUTH","challenge"]"#).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "NOTICE");

    shutdown.shutdown(); // joins relay thread
}

/// Exercises cross-shard sub replication: with reader_shards=2, open several
/// subscriber connections (likely landing on both shards) and one publisher.
/// Every subscriber should receive every event regardless of which shard the
/// publisher's connection lives on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_shard_fanout() {
    use ring_relay_server::ShardConfig;

    let mut cfg = RelayConfig::default();
    cfg.shards = ShardConfig {
        reader_shards: 2,
        writer_shards: 2,
    };
    let (port, shutdown) = spawn_relay(cfg);

    // Open 4 subscribers; with fd % 2 routing at least one will land on a
    // different shard than the publisher, so the test always exercises the
    // replication path at N=2. If all happen to collide on one shard the
    // test still passes — but that's vanishingly unlikely with 4+1 fds.
    let mut subs = Vec::with_capacity(4);
    for i in 0..4 {
        let mut ws = connect(port).await;
        let req = format!(r#"["REQ","s{i}",{{"kinds":[1]}}]"#);
        send(&mut ws, &req).await;
        let eose = recv_text(&mut ws).await;
        assert_eq!(eose[0], "EOSE");
        subs.push(ws);
    }

    // Give replication a brief moment to propagate SubRepl::Add messages
    // across peer shards before we publish.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut pub_ws = connect(port).await;
    let note = signed_note("cross-shard");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;

    let ok = recv_text(&mut pub_ws).await;
    assert_eq!(ok[0], "OK");
    assert_eq!(ok[2], true);

    // Every subscriber must see the event, regardless of shard placement.
    for (i, sub) in subs.iter_mut().enumerate() {
        let evt = recv_text(sub).await;
        assert_eq!(evt[0], "EVENT", "sub {i} did not receive EVENT");
        assert_eq!(evt[1], format!("s{i}"));
        assert_eq!(evt[2]["id"].as_str().unwrap(), id);
    }

    shutdown.shutdown(); // joins relay thread
}

// --- validation / NIP-11 limit enforcement -------------------------------

/// Non-hex id must be rejected up front with `invalid: malformed id` — not
/// silently coerced to `[0; 32]` and pushed into the verify pool / storage
/// indexes. Regression guard: a previous version fell back to all-zero bytes
/// for any id that failed `decode_hex32`, which would then index distinct
/// events under a shared zero-key slot in the replaceable bucket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_id_is_rejected_with_message() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    let mut note = signed_note("hi");
    // Replace the id with a 64-char string that contains a non-hex char.
    // Length matches so naive validators wave it through.
    let mut id = note.id.clone().unwrap();
    id.replace_range(0..1, "z");
    note.id = Some(id.clone());

    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], false);
    assert!(
        resp[3]
            .as_str()
            .unwrap_or("")
            .contains("invalid: malformed id"),
        "got {:?}",
        resp[3]
    );

    shutdown.shutdown();
}

/// Wrong-length pubkey must be rejected with `invalid: malformed pubkey`.
/// Same regression guard as above but for the pubkey field, which feeds
/// the replaceable bucket's primary key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_pubkey_is_rejected_with_message() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    let mut note = signed_note("hi");
    let id = note.id.clone().unwrap();
    // Trim one hex digit. Now 63 chars, fails decode_hex32's length check.
    note.pubkey.pop();

    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], false);
    assert!(
        resp[3]
            .as_str()
            .unwrap_or("")
            .contains("invalid: malformed pubkey"),
        "got {:?}",
        resp[3]
    );

    shutdown.shutdown();
}

/// Tampered `id` (valid hex, same length) must fail verification. Guards the
/// sha256(id) check against regressing to a length-only validator.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_id_is_rejected() {
    let (port, shutdown) = spawn_relay(RelayConfig::default());
    let mut ws = connect(port).await;

    let mut note = signed_note("hi");
    // Flip one hex character of the id. Still 64 hex chars, so it passes the
    // length check but not the sha256 recomputation.
    let mut id = note.id.clone().unwrap();
    let c = id.remove(0);
    let flipped = if c == '0' { '1' } else { '0' };
    id.insert(0, flipped);
    note.id = Some(id.clone());

    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], false);
    assert!(
        resp[3].as_str().unwrap_or("").contains("invalid"),
        "expected reason to start with 'invalid:', got {:?}",
        resp[3]
    );

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_frame_is_noticed() {
    let cfg = RelayConfig {
        max_message_length: Some(256),
        ..RelayConfig::default()
    };
    let (port, shutdown) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    // 1 KiB of garbage well over the 256-byte cap — relay must NOTICE and not
    // attempt to parse.
    let big = "x".repeat(1024);
    let frame = format!(r#"["EVENT",{{"content":"{big}"}}]"#);
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "NOTICE");
    assert!(
        resp[1]
            .as_str()
            .unwrap_or("")
            .contains("max_message_length"),
        "got {:?}",
        resp[1]
    );

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_content_is_rejected() {
    let cfg = RelayConfig {
        max_content_length: Some(16),
        max_message_length: Some(8 * 1024), // big enough to reach validator
        ..RelayConfig::default()
    };
    let (port, shutdown) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    let note = signed_note(&"a".repeat(128));
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], false);
    assert!(
        resp[3]
            .as_str()
            .unwrap_or("")
            .contains("max_content_length"),
        "got {:?}",
        resp[3]
    );

    shutdown.shutdown(); // joins relay thread
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_tags_is_rejected() {
    let cfg = RelayConfig {
        max_event_tags: Some(2),
        ..RelayConfig::default()
    };
    let (port, shutdown) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    let kp = K256Keypair::generate();
    let mut note = NostrNote::text_note("tagged");
    note.pubkey = kp.public_key();
    note.tags.add_custom_tag("t", "one");
    note.tags.add_custom_tag("t", "two");
    note.tags.add_custom_tag("t", "three");
    kp.sign_nostr_note(&mut note).expect("sign");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut ws, &frame).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "OK");
    assert_eq!(resp[1], id);
    assert_eq!(resp[2], false);
    assert!(
        resp[3].as_str().unwrap_or("").contains("too many tags"),
        "got {:?}",
        resp[3]
    );

    shutdown.shutdown(); // joins relay thread
}

/// Cross-shard live fan-out **with storage enabled**. The persistence layer
/// changes the REQ flow (reader pool sends EOSE after a historical scan
/// instead of inline), but live fan-out for matching EVENTs must still cross
/// shards. Regression guard: an earlier comment in `on_req` falsely claimed
/// storage mode skipped sub replication. If anyone refactors based on that
/// comment, this test will fail loudly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_shard_fanout_with_storage() {
    use ring_relay_server::ShardConfig;

    let dir = tempfile::tempdir().expect("tempdir");
    let storage = StorageConfig {
        data_dir: dir.path().to_path_buf(),
        ephemeral_slots: 64,
        replaceable_slots: 16,
        parameterized_slots: 16,
        max_payload: 16 * 1024,
        reader_threads: 1,
        write_ring_capacity: 64,
        req_ring_capacity: 16,
        fsync_interval_ms: Some(10),
        ..StorageConfig::default()
    };
    let mut cfg = RelayConfig::default();
    cfg.shards = ShardConfig {
        reader_shards: 2,
        writer_shards: 2,
    };
    cfg.storage = Some(storage);
    let (port, shutdown) = spawn_relay(cfg);

    // Open 4 subscribers and drain each one's EOSE (which now arrives from
    // the reader pool after the historical scan, not inline).
    let mut subs = Vec::with_capacity(4);
    for i in 0..4 {
        let mut ws = connect(port).await;
        let req = format!(r#"["REQ","s{i}",{{"kinds":[1]}}]"#);
        send(&mut ws, &req).await;
        let eose = recv_text(&mut ws).await;
        assert_eq!(eose[0], "EOSE", "sub {i} expected EOSE, got {eose:?}");
        subs.push(ws);
    }

    // Let SubRepl::Add propagate to peer shards.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut pub_ws = connect(port).await;
    let note = signed_note("cross-shard-storage");
    let id = note.id.clone().unwrap();
    let frame = format!(r#"["EVENT",{}]"#, serde_json::to_string(&note).unwrap());
    send(&mut pub_ws, &frame).await;

    let ok = recv_text(&mut pub_ws).await;
    assert_eq!(ok[0], "OK");
    assert_eq!(ok[2], true);

    for (i, sub) in subs.iter_mut().enumerate() {
        let evt = recv_text(sub).await;
        assert_eq!(evt[0], "EVENT", "sub {i} did not receive EVENT");
        assert_eq!(evt[1], format!("s{i}"));
        assert_eq!(evt[2]["id"].as_str().unwrap(), id);
    }

    shutdown.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_subid_req_is_closed() {
    let cfg = RelayConfig {
        max_subid_length: Some(4),
        ..RelayConfig::default()
    };
    let (port, shutdown) = spawn_relay(cfg);
    let mut ws = connect(port).await;

    let sub_id = "s".repeat(32);
    send(&mut ws, &format!(r#"["REQ","{sub_id}",{{"kinds":[1]}}]"#)).await;

    let resp = recv_text(&mut ws).await;
    assert_eq!(resp[0], "CLOSED");
    assert_eq!(resp[1], sub_id);
    assert!(
        resp[2].as_str().unwrap_or("").contains("max_subid_length"),
        "got {:?}",
        resp[2]
    );

    shutdown.shutdown(); // joins relay thread
}
