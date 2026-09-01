//! TLS setup for relay sockets.
//!
//! The driver threads do blocking IO, so they need a synchronous TLS stream
//! rather than an async one. `RelayTls` builds one `rustls` client config per
//! process and wraps a plain `TcpStream` into a TLS stream on demand.
//!
//! The crypto provider is a build-time choice between the `rustls-ring` and
//! `rustls-aws-lc` features. Only [`RelayTls::provider`] knows which one is
//! active; every other line of this crate is free of that `#[cfg]`.
//!
//! Neither feature is mandatory. Under `rustls-custom-provider` this crate
//! links no provider of its own, and the caller supplies one: either through
//! [`RelayTls::with_provider`], through [`RelayTls::from_config`] for full
//! control of roots and client auth, or by installing a process-wide default
//! before the first connection. That is how a provider this crate does not
//! know about, such as `rustls-rustcrypto`, is used.

/// Why a TLS session cannot start.
#[derive(Debug)]
pub enum RelayTlsError {
    /// The configuration is invalid, or no crypto provider accepted it.
    Config(Box<rustls::Error>),
    /// The host is not a valid TLS server name.
    ServerName(String),
    /// The build links no crypto provider, and none was supplied.
    ///
    /// This is only reachable under `rustls-custom-provider`, which is the
    /// feature that says the caller brings its own.
    NoProvider,
}

impl std::fmt::Display for RelayTlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(e) => write!(f, "tls configuration error: {e}"),
            Self::ServerName(h) => write!(f, "invalid tls server name '{h}'"),
            Self::NoProvider => f.write_str(
                "no rustls crypto provider: build with `rustls-ring` or `rustls-aws-lc`, \
                 pass one to RelayTls::with_provider, or install a process default first",
            ),
        }
    }
}

impl std::error::Error for RelayTlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(e) => Some(e.as_ref()),
            Self::ServerName(_) | Self::NoProvider => None,
        }
    }
}

impl From<rustls::Error> for RelayTlsError {
    fn from(value: rustls::Error) -> Self {
        Self::Config(Box::new(value))
    }
}

/// A shared client configuration that turns TCP streams into TLS streams.
///
/// Cloning is cheap: the `rustls` configuration sits behind an `Arc` and is
/// read-only, so every driver thread shares one copy of the root store.
#[derive(Clone)]
pub struct RelayTls {
    config: std::sync::Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for RelayTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayTls").finish_non_exhaustive()
    }
}

impl RelayTls {
    /// Builds a client configuration that trusts the webpki root store.
    ///
    /// # Errors
    ///
    /// Returns [`RelayTlsError::Config`] when the selected crypto provider
    /// rejects the protocol versions.
    pub fn new() -> Result<Self, RelayTlsError> {
        Self::with_provider(Self::provider()?)
    }

    /// Builds a client configuration that trusts the webpki root store, using
    /// `provider` for cryptography.
    ///
    /// Use this to run a provider this crate does not link, such as
    /// `rustls-rustcrypto`. Build the crate with `default-features = false`
    /// and the `rustls-custom-provider` feature, then pass the provider here.
    ///
    /// # Example
    /// ```no_run
    /// use nostro2_relay::{DriverConfig, NostrRelay, RelayTls, RelayUrl};
    ///
    /// # fn example(provider: rustls::crypto::CryptoProvider)
    /// # -> Result<(), Box<dyn std::error::Error>> {
    /// let tls = RelayTls::with_provider(std::sync::Arc::new(provider))?;
    /// let url = RelayUrl::parse("wss://relay.example.com")?;
    /// let relay = NostrRelay::connect_blocking_config(DriverConfig::new(url).with_tls(tls))?;
    /// # let _ = relay;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RelayTlsError::Config`] when `provider` rejects the safe
    /// default protocol versions.
    pub fn with_provider(
        provider: std::sync::Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Self, RelayTlsError> {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self::from_config(config))
    }

    /// Wraps a client configuration the caller built.
    ///
    /// This is the widest door: it accepts any [`rustls::ClientConfig`], so
    /// the caller chooses the provider, the root store, client certificates,
    /// and the protocol versions. Build the configuration with the `rustls`
    /// this crate re-exports, so both agree on the version.
    #[must_use]
    pub fn from_config(config: rustls::ClientConfig) -> Self {
        Self::from_shared(std::sync::Arc::new(config))
    }

    /// Wraps a client configuration that is already shared.
    ///
    /// Use this to give several clients, or several pools, one root store
    /// and one session cache.
    #[must_use]
    pub const fn from_shared(config: std::sync::Arc<rustls::ClientConfig>) -> Self {
        Self { config }
    }

    /// The configuration these sessions use.
    #[must_use]
    pub const fn config(&self) -> &std::sync::Arc<rustls::ClientConfig> {
        &self.config
    }

    #[cfg(feature = "rustls-ring")]
    #[allow(clippy::unnecessary_wraps)]
    fn provider() -> Result<std::sync::Arc<rustls::crypto::CryptoProvider>, RelayTlsError> {
        Ok(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
    }

    #[cfg(all(feature = "rustls-aws-lc", not(feature = "rustls-ring")))]
    #[allow(clippy::unnecessary_wraps)]
    fn provider() -> Result<std::sync::Arc<rustls::crypto::CryptoProvider>, RelayTlsError> {
        Ok(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
    }

    /// Falls back to whatever the process installed.
    ///
    /// This build links no provider, so the only one available is the
    /// process-wide default. A caller that installed none gets
    /// [`RelayTlsError::NoProvider`] rather than a panic from deeper in
    /// `rustls`.
    #[cfg(not(any(feature = "rustls-ring", feature = "rustls-aws-lc")))]
    fn provider() -> Result<std::sync::Arc<rustls::crypto::CryptoProvider>, RelayTlsError> {
        rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .ok_or(RelayTlsError::NoProvider)
    }

    /// A configuration for the crate's own tests.
    ///
    /// Under `rustls-custom-provider` the library links no provider, so
    /// [`Self::new`] fails until a consumer installs one. The tests stand in
    /// for that consumer. Builds that link a provider install nothing.
    ///
    /// # Panics
    ///
    /// Panics when no provider can be built, which would mean the test
    /// harness itself is misconfigured.
    #[cfg(test)]
    pub(crate) fn testing() -> Self {
        Self::install_test_provider();
        Self::new().expect("a provider is available to the test harness")
    }

    #[cfg(all(test, not(any(feature = "rustls-ring", feature = "rustls-aws-lc"))))]
    fn install_test_provider() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::CryptoProvider::install_default(
                rustls::crypto::ring::default_provider(),
            );
        });
    }

    #[cfg(all(test, any(feature = "rustls-ring", feature = "rustls-aws-lc")))]
    const fn install_test_provider() {}

    /// Starts a TLS session over `stream` for `host`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayTlsError::ServerName`] when `host` is not a valid DNS
    /// name or IP literal, and [`RelayTlsError::Config`] when the session
    /// refuses to start.
    pub fn connect(
        &self,
        host: &str,
        stream: std::net::TcpStream,
    ) -> Result<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>, RelayTlsError>
    {
        let server_name = rustls::pki_types::ServerName::try_from(host)
            .map_err(|_| RelayTlsError::ServerName(host.to_owned()))?
            .to_owned();
        let connection = rustls::ClientConnection::new(self.config.clone(), server_name)?;
        Ok(rustls::StreamOwned::new(connection, stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture;

    impl Fixture {
        fn tls() -> RelayTls {
            RelayTls::testing()
        }

        fn loopback_stream() -> std::net::TcpStream {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let client = std::net::TcpStream::connect(addr).unwrap();
            std::mem::forget(listener.accept().unwrap().0);
            client
        }
    }

    #[test]
    fn a_config_builds_with_the_selected_provider() {
        let _tls = Fixture::tls();
    }

    #[test]
    fn clones_share_one_configuration() {
        let tls = Fixture::tls();
        let clone = tls.clone();
        assert!(std::sync::Arc::ptr_eq(&tls.config, &clone.config));
    }

    #[test]
    fn a_dns_name_starts_a_session() {
        let tls = Fixture::tls();
        assert!(
            tls.connect("relay.example.com", Fixture::loopback_stream())
                .is_ok()
        );
    }

    #[test]
    fn an_ip_literal_starts_a_session() {
        let tls = Fixture::tls();
        assert!(tls.connect("127.0.0.1", Fixture::loopback_stream()).is_ok());
    }

    #[test]
    fn a_malformed_host_is_rejected() {
        let tls = Fixture::tls();
        let error = tls
            .connect("not a host", Fixture::loopback_stream())
            .unwrap_err();
        assert!(matches!(error, RelayTlsError::ServerName(_)));
        assert!(error.to_string().contains("not a host"));
    }

    #[test]
    fn the_debug_form_hides_the_configuration() {
        assert_eq!(format!("{:?}", Fixture::tls()), "RelayTls { .. }");
    }

    /// The paths under test are provider-agnostic. `ring` stands in for
    /// whatever a caller brings, and is available to the tests in every
    /// build because it is a dev-dependency.
    #[test]
    fn a_caller_supplied_provider_builds_a_config() {
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let tls = RelayTls::with_provider(provider).expect("the provider builds a client config");
        assert!(tls.connect("relay.example.com", Fixture::loopback_stream()).is_ok());
    }

    #[test]
    fn a_caller_supplied_config_is_used_verbatim() {
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();

        let shared = std::sync::Arc::new(config);
        let tls = RelayTls::from_shared(shared.clone());
        assert!(std::sync::Arc::ptr_eq(tls.config(), &shared));
    }

    #[test]
    fn a_caller_supplied_config_still_opens_sessions() {
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
        .with_no_client_auth();

        let tls = RelayTls::from_config(config);
        assert!(tls.connect("relay.example.com", Fixture::loopback_stream()).is_ok());
    }

    #[test]
    fn a_driver_config_carries_a_caller_supplied_tls() {
        let url = crate::url::RelayUrl::parse("wss://relay.example.com").unwrap();
        let tls = Fixture::tls();
        let config = crate::driver::DriverConfig::new(url).with_tls(tls.clone());
        let carried = config
            .tls()
            .expect("the configuration carries a session factory")
            .expect("a secure url keeps its configuration");
        assert!(std::sync::Arc::ptr_eq(carried.config(), tls.config()));
    }

    #[test]
    fn a_secure_url_without_tls_builds_the_default() {
        RelayTls::install_test_provider();
        let url = crate::url::RelayUrl::parse("wss://relay.example.com").unwrap();
        let config = crate::driver::DriverConfig::new(url);
        assert!(config.tls.is_none());
        assert!(config.tls().expect("the default builds").is_some());
    }

    /// A plaintext relay starts no TLS session, so it must not demand a
    /// crypto provider. Without this, `ws://` would fail in a build that
    /// links none, even though it never encrypts anything.
    #[test]
    fn a_plain_url_needs_no_configuration_at_all() {
        let url = crate::url::RelayUrl::parse("ws://relay.example.com").unwrap();
        let config = crate::driver::DriverConfig::new(url);
        assert!(config.tls().expect("a plain url never fails").is_none());
    }
}
