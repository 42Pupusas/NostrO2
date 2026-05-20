#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
pub mod errors;
mod pool;
mod relay;
pub use nostro2;
pub use pool::NostrPool;
pub use relay::{NostrRelay, ReconnectConfig};
