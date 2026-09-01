//! A WebSocket relay client for Nostr that depends on no async runtime.
//!
//! Each connection runs on its own thread and talks to the application
//! through lock-free rings, so the crate carries no executor of its own and
//! holds no lock on the data path. The `async` methods await ring futures,
//! which any executor can poll.
//!
//! # Without an executor
//!
//! Every operation has a blocking twin, so a service built on threads never
//! has to poll a future. A crate whose only constructor is `async` still
//! forces the caller to find a runtime, even though dialling a socket is
//! not an asynchronous act, so the pairs are complete rather than partial:
//!
//! | Async | Blocking |
//! |---|---|
//! | [`NostrRelay::new`] | [`NostrRelay::connect_blocking`] |
//! | [`NostrRelay::with_reconnect`] | [`NostrRelay::connect_blocking_with`] |
//! | [`NostrRelay::with_driver_config`] + await | [`NostrRelay::connect_blocking_config`] |
//! | [`NostrRelay::recv`] | [`NostrRelay::recv_blocking`] |
//! | [`NostrRelay::recv_event`] | [`NostrRelay::recv_event_blocking`] |
//! | [`NostrRelay::send_all`] | [`NostrRelay::send_all_blocking`] |
//! | [`NostrPool::recv`] | [`NostrPool::recv_blocking`] |
//! | [`NostrPool::recv_event`] | [`NostrPool::recv_event_blocking`] |
//!
//! `send`, `close`, and every constructor of [`NostrPool`] are already
//! synchronous: sending only pushes to a ring, so it never blocks.
//!
//! `tests/blocking_only.rs` exercises the whole crate without writing
//! `.await` once, so this parity cannot quietly lapse.
//!
//! The dependency list reflects this. The only async crate the library
//! links is `futures-core`, which has no dependencies of its own and
//! supplies the `Stream` trait for [`NostrRelay::send_all`]. A caller that
//! never touches the async surface still compiles nothing extra.
//!
//! # Choosing a crypto provider
//!
//! TLS is `rustls`, and the provider is a build-time choice:
//!
//! - `rustls-ring` (default) or `rustls-aws-lc` link a provider directly.
//! - `rustls-custom-provider` links none, and the caller supplies one.
//!
//! The third exists because a provider this crate does not know about, such
//! as `rustls-rustcrypto`, is still a valid choice. Pass it to
//! [`RelayTls::with_provider`], or build a whole [`rustls::ClientConfig`]
//! and pass that to [`RelayTls::from_config`] when the root store, the
//! client certificates, or the protocol versions also need to change. Give
//! the result to [`DriverConfig::with_tls`]; cloning a [`RelayTls`] shares
//! one root store across every relay in a pool.
//!
//! Build configurations against the [`rustls`] re-exported here, so the
//! types match the version the driver uses.
//!
//! A `ws://` relay starts no TLS session, so it needs no provider at all
//! and never builds a configuration.
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
// A crypto provider is mandatory, but it does not have to be one this crate
// links. Enable `rustls-ring` or `rustls-aws-lc` to get one built in, or
// `rustls-custom-provider` to promise you will supply your own through
// `RelayTls::with_provider`, `RelayTls::from_config`, or a process-wide
// default. Without any of the three, `rustls` would fail far from here on a
// missing provider, so say it plainly instead.
#[cfg(not(any(
    feature = "rustls-ring",
    feature = "rustls-aws-lc",
    feature = "rustls-custom-provider"
)))]
compile_error!(
    "nostro2-relay needs a rustls crypto provider: enable `rustls-ring` (default), `rustls-aws-lc`, or `rustls-custom-provider` to supply your own"
);

mod driver;
pub mod errors;
mod guard;
mod heartbeat;
mod json;
mod next;
mod pool;
mod pool_event;
mod reconnect;
mod relay;
mod session;
mod socket;
mod tls;
mod url;
mod verifier;
pub use nostro2;
/// The `rustls` this crate links.
///
/// Build custom [`RelayTls`] configurations against this re-export, so the
/// types match the version the driver uses.
pub use rustls;
pub use pool::NostrPool;
pub use pool_event::PoolEvent;
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
