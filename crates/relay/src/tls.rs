//! TLS setup for relay sockets.
//!
//! The driver threads do blocking IO, so they need a synchronous TLS stream
//! rather than an async one. `RelayTls` builds one `rustls` client config per
//! process and wraps a plain `TcpStream` into a TLS stream on demand.
//!
//! The crypto provider is a build-time choice between the `rustls-ring` and
//! `rustls-aws-lc` features. Only [`RelayTls::provider`] knows which one is
//! active; every other line of this crate is free of that `#[cfg]`.

/// Why a TLS session cannot start.
#[derive(Debug)]
pub enum RelayTlsError {
    /// The configuration is invalid, or no crypto provider accepted it.
    Config(Box<rustls::Error>),
    /// The host is not a valid TLS server name.
    ServerName(String),
}

impl std::fmt::Display for RelayTlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(e) => write!(f, "tls configuration error: {e}"),
            Self::ServerName(h) => write!(f, "invalid tls server name '{h}'"),
        }
    }
}

impl std::error::Error for RelayTlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(e) => Some(e.as_ref()),
            Self::ServerName(_) => None,
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
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder_with_provider(Self::provider())
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config: std::sync::Arc::new(config),
        })
    }

    #[cfg(feature = "rustls-ring")]
    fn provider() -> std::sync::Arc<rustls::crypto::CryptoProvider> {
        std::sync::Arc::new(rustls::crypto::ring::default_provider())
    }

    #[cfg(all(feature = "rustls-aws-lc", not(feature = "rustls-ring")))]
    fn provider() -> std::sync::Arc<rustls::crypto::CryptoProvider> {
        std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    }

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
            RelayTls::new().expect("the selected provider builds a client config")
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
}
