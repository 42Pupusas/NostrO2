#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
// A TLS backend is mandatory: `connect_async_with_config` and
// `MaybeTlsStream` are gated behind tokio-tungstenite's `connect`/`stream`
// features, which only the `rustls-*` features forward. Without one, the
// crate references items configured out of existence and fails to build
// with a wall of "cannot find" errors. Turn that into a single clear line.
#[cfg(not(any(feature = "rustls-ring", feature = "rustls-aws-lc")))]
compile_error!(
    "nostro2-relay needs a TLS backend; enable exactly one of `rustls-ring` (default) or `rustls-aws-lc`"
);

pub mod errors;
mod json;
mod pool;
mod reconnect;
mod relay;
mod socket;
mod task_guard;
mod tls;
mod url;
pub use nostro2;
pub use pool::NostrPool;
pub use reconnect::{ReconnectConfig, ReconnectSchedule};
pub use relay::NostrRelay;
pub use task_guard::TaskGuard;
pub use socket::{WsMessage, WsSocket, WsSocketError};
pub use tls::{RelayTls, RelayTlsError};
pub use url::{RelayScheme, RelayUrl, RelayUrlError};
