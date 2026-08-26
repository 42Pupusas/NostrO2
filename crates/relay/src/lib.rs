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
mod json;
mod pool;
mod reconnect;
mod relay;
mod socket;
mod tls;
mod url;
pub use nostro2;
pub use pool::NostrPool;
pub use driver::{DriverConfig, DriverEvent, DriverPorts, Handshake, RelayDriver};
pub use guard::{DriverGuard, Shutdown};
pub use reconnect::{ReconnectConfig, ReconnectSchedule};
pub use relay::NostrRelay;
pub use socket::{WsMessage, WsSocket, WsSocketError};
pub use tls::{RelayTls, RelayTlsError};
pub use url::{RelayScheme, RelayUrl, RelayUrlError};
