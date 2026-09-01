//! A consumer that brings its own crypto provider.
//!
//! Under `rustls-custom-provider` this crate links no provider, so these
//! tests do what such a consumer does: build one, hand it over, and connect.
//! They also pin the diagnostic a consumer sees when it forgets.
//!
//! `rustls-rustcrypto` is the motivating case. It is not a dependency here,
//! because the paths under test never name a provider: they take whatever
//! `rustls::crypto::CryptoProvider` the caller passes. `ring` stands in for
//! that, so the test needs no extra crate to prove the door opens.

#![cfg(feature = "rustls-custom-provider")]

/// Builds providers and configurations the way a consumer would.
struct Consumer;

impl Consumer {
    fn provider() -> std::sync::Arc<rustls::crypto::CryptoProvider> {
        std::sync::Arc::new(rustls::crypto::ring::default_provider())
    }

    fn roots() -> rustls::RootCertStore {
        rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        }
    }

    fn client_config() -> rustls::ClientConfig {
        rustls::ClientConfig::builder_with_provider(Self::provider())
            .with_safe_default_protocol_versions()
            .expect("the provider supports the safe defaults")
            .with_root_certificates(Self::roots())
            .with_no_client_auth()
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
fn a_supplied_provider_opens_a_session() {
    let tls = nostro2_relay::RelayTls::with_provider(Consumer::provider())
        .expect("the supplied provider builds a client config");
    assert!(
        tls.connect("relay.example.com", Consumer::loopback_stream())
            .is_ok()
    );
}

#[test]
fn a_supplied_configuration_opens_a_session() {
    let tls = nostro2_relay::RelayTls::from_config(Consumer::client_config());
    assert!(
        tls.connect("relay.example.com", Consumer::loopback_stream())
            .is_ok()
    );
}

#[test]
fn a_supplied_configuration_reaches_the_driver() {
    let url = nostro2_relay::RelayUrl::parse("wss://relay.example.com").unwrap();
    let tls = nostro2_relay::RelayTls::from_config(Consumer::client_config());
    let config = nostro2_relay::DriverConfig::new(url).with_tls(tls.clone());

    let carried = config
        .tls()
        .expect("the configuration carries the session factory")
        .expect("a secure url keeps its configuration");
    assert!(std::sync::Arc::ptr_eq(carried.config(), tls.config()));
}

/// The whole point of the feature: a relay starts on a provider this crate
/// does not link, without ever calling [`nostro2_relay::RelayTls::new`].
#[test]
fn a_relay_starts_on_a_supplied_provider() {
    let url = nostro2_relay::RelayUrl::parse("wss://127.0.0.1:1").unwrap();
    let tls = nostro2_relay::RelayTls::from_config(Consumer::client_config());
    let config = nostro2_relay::DriverConfig::new(url)
        .with_tls(tls)
        .with_reconnect(nostro2_relay::ReconnectConfig::disabled());

    let relay = nostro2_relay::NostrRelay::with_driver_config(config)
        .expect("the supplied configuration is accepted");
    assert_eq!(relay.url().to_string(), "wss://127.0.0.1:1/");
}

/// A plaintext relay never starts a TLS session, so it must connect in a
/// build that links no provider and was given none.
#[test]
fn a_plain_relay_needs_no_provider() {
    let url = nostro2_relay::RelayUrl::parse("ws://127.0.0.1:1").unwrap();
    let config = nostro2_relay::DriverConfig::new(url);
    assert!(
        config
            .tls()
            .expect("a plain url never asks for a provider")
            .is_none()
    );
}

/// A consumer that supplies nothing gets a diagnostic naming its options,
/// not a panic from inside `rustls`.
///
/// This runs in its own process because installing a process-wide default
/// is irreversible, and the other tests in this file install one.
#[test]
fn a_missing_provider_is_a_readable_error() {
    if std::env::var_os("NOSTRO2_RELAY_NO_PROVIDER_CHILD").is_some() {
        let error = nostro2_relay::RelayTls::new()
            .expect_err("this build links no provider and none was installed");
        assert!(matches!(
            error,
            nostro2_relay::RelayTlsError::NoProvider
        ));

        let text = error.to_string();
        assert!(text.contains("rustls-ring"), "{text}");
        assert!(text.contains("with_provider"), "{text}");
        return;
    }

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("a_missing_provider_is_a_readable_error")
        .arg("--exact")
        .arg("--nocapture")
        .env("NOSTRO2_RELAY_NO_PROVIDER_CHILD", "1")
        .status()
        .expect("the test binary runs itself");
    assert!(status.success(), "the child process reported the error");
}
