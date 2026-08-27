//! A WebSocket relay client for Nostr that depends on no async runtime.
//!
//! Each connection runs on its own thread and talks to the application
//! through lock-free rings, so the crate carries no executor of its own and
//! holds no lock on the data path. The `async` methods await ring futures,
//! which any executor can poll.
//!
//! # Long-lived services
//!
//! The intended user is a daemon that holds a pool open for weeks and
//! reconnects through every network fault. Three guarantees serve that use,
//! each covered by tests in `tests/liveness.rs`:
//!
//! - **A dead connection is detected.** TCP never reports a peer that stops
//!   answering, so [`Heartbeat`] pings a quiet connection and drops one that
//!   does not reply. Without this a half-open socket stalls a reader
//!   forever, and reconnection never starts.
//! - **A stalled write never freezes the driver.** A relay that accepts the
//!   connection but stops reading it fills both receive windows. One thread
//!   owns the socket, so a blocking write would stop reads too. Writes are
//!   bounded by `DriverConfig::write_timeout`.
//! - **A reconnect restores the subscriptions.** A subscription lives on the
//!   relay, which forgets it when the connection drops. [`Session`] records
//!   the open filters and the driver replays them on the new connection, so
//!   a service does not go silent while looking connected.
//! - **A reader is always released.** Every way a driver can end, including
//!   a spent retry budget, an explicit close, or a panic on the IO thread,
//!   ends the stream rather than leaving a reader parked.
//! - **Reconnecting leaks nothing.** Sockets and threads stay flat across
//!   thousands of reconnects.
//!
//! # Panics
//!
//! The library panics only when the operating system refuses to spawn a
//! thread, which no caller can recover from. Every other failure is a
//! [`errors::NostrRelayError`] or a logged warning. A relay that sends junk,
//! a forged note, an unparseable URL, or a frame that does not decode never
//! brings down the process.

#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
// A TLS backend is mandatory: `rustls` needs exactly one crypto provider,
// and without a `rustls-*` feature it compiles with none, failing on a
// missing provider rather than anything readable. Turn that into a single
// clear line.
#[cfg(not(any(feature = "rustls-ring", feature = "rustls-aws-lc")))]
compile_error!(
    "nostro2-relay needs a TLS backend; enable exactly one of `rustls-ring` (default) or `rustls-aws-lc`"
);

mod driver;
pub mod errors;
mod guard;
mod heartbeat;
mod json;
mod pool;
mod reconnect;
mod relay;
mod session;
mod socket;
mod tls;
mod url;
mod verifier;
pub use nostro2;
pub use pool::NostrPool;
pub use driver::{DriverConfig, DriverEvent, DriverPorts, Handshake, RelayDriver};
pub use guard::{DriverGuard, Shutdown};
pub use heartbeat::{Heartbeat, HeartbeatConfig, Liveness};
pub use reconnect::{ReconnectConfig, ReconnectSchedule};
pub use relay::NostrRelay;
pub use session::Session;
pub use socket::{WsMessage, WsSocket, WsSocketError};
pub use tls::{RelayTls, RelayTlsError};
pub use url::{RelayScheme, RelayUrl, RelayUrlError};
pub use verifier::{NoteVerifier, Verdict, VerifyPolicy};
