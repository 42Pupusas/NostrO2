//! Relay URL parsing.
//!
//! The driver thread needs four things from a relay address: a host to
//! resolve, a port to dial, a TLS decision, and the request target for the
//! RFC-6455 upgrade. This module turns a user string into exactly those,
//! with no IO and no runtime.

/// The transport a relay URL asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelayScheme {
    /// Plain TCP.
    Ws,
    /// TCP with TLS.
    Wss,
}

impl RelayScheme {
    /// The port to dial when the URL omits one.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Ws => 80,
            Self::Wss => 443,
        }
    }

    /// Whether this scheme needs a TLS handshake.
    #[must_use]
    pub const fn is_secure(self) -> bool {
        matches!(self, Self::Wss)
    }

    /// The canonical scheme token, without the separator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ws => "ws",
            Self::Wss => "wss",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        if token.eq_ignore_ascii_case("ws") || token.eq_ignore_ascii_case("http") {
            Some(Self::Ws)
        } else if token.eq_ignore_ascii_case("wss") || token.eq_ignore_ascii_case("https") {
            Some(Self::Wss)
        } else {
            None
        }
    }
}

impl std::fmt::Display for RelayScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a relay URL is unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayUrlError {
    /// No `://` separator.
    MissingScheme,
    /// A scheme other than `ws`, `wss`, `http`, or `https`.
    UnknownScheme(String),
    /// The authority is empty.
    EmptyHost,
    /// The host holds a character no host may hold.
    InvalidHost(String),
    /// The port is absent, non-numeric, or above 65535.
    InvalidPort(String),
    /// An IPv6 authority without its closing bracket.
    UnclosedIpv6Host,
    /// A `user@host` authority, which relays do not use.
    UserInfoNotSupported,
}

impl std::fmt::Display for RelayUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingScheme => f.write_str("relay url needs a scheme, e.g. wss://relay.example.com"),
            Self::UnknownScheme(s) => write!(f, "unsupported relay scheme '{s}', expected ws or wss"),
            Self::EmptyHost => f.write_str("relay url has no host"),
            Self::InvalidHost(h) => write!(f, "invalid relay host '{h}'"),
            Self::InvalidPort(p) => write!(f, "invalid relay port '{p}'"),
            Self::UnclosedIpv6Host => f.write_str("relay url has an unclosed '[' in its host"),
            Self::UserInfoNotSupported => f.write_str("relay url must not carry user info"),
        }
    }
}

impl std::error::Error for RelayUrlError {}

/// A parsed relay address.
///
/// [`Self::host`] is the name to resolve and to present for TLS; it never
/// carries IPv6 brackets. [`Self::authority`] is the `Host:` header value and
/// carries them when the host is a literal IPv6 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayUrl {
    scheme: RelayScheme,
    host: String,
    port: u16,
    authority: String,
    target: String,
}

impl RelayUrl {
    /// Parses a relay address.
    ///
    /// `http` and `https` are accepted as aliases of `ws` and `wss`. A
    /// fragment is dropped; a query is kept in the request target.
    ///
    /// # Errors
    ///
    /// Returns [`RelayUrlError`] when the scheme, host, or port is unusable.
    pub fn parse(input: &str) -> Result<Self, RelayUrlError> {
        let trimmed = input.trim();
        let (scheme_token, rest) = trimmed
            .split_once("://")
            .ok_or(RelayUrlError::MissingScheme)?;
        let scheme = RelayScheme::from_token(scheme_token)
            .ok_or_else(|| RelayUrlError::UnknownScheme(scheme_token.to_owned()))?;

        let rest = rest.split('#').next().unwrap_or_default();
        let cut = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path_and_query) = rest.split_at(cut);

        if authority.contains('@') {
            return Err(RelayUrlError::UserInfoNotSupported);
        }

        let (host, port) = Self::split_authority(authority, scheme)?;
        let authority = if port == scheme.default_port() {
            Self::render_host(&host)
        } else {
            format!("{}:{port}", Self::render_host(&host))
        };
        let target = if path_and_query.is_empty() {
            "/".to_owned()
        } else {
            path_and_query.to_owned()
        };

        Ok(Self {
            scheme,
            host,
            port,
            authority,
            target,
        })
    }

    fn split_authority(
        authority: &str,
        scheme: RelayScheme,
    ) -> Result<(String, u16), RelayUrlError> {
        if authority.is_empty() {
            return Err(RelayUrlError::EmptyHost);
        }

        if let Some(stripped) = authority.strip_prefix('[') {
            let close = stripped
                .find(']')
                .ok_or(RelayUrlError::UnclosedIpv6Host)?;
            let host = &stripped[..close];
            let port = Self::parse_port(&stripped[close + 1..], scheme)?;
            return Ok((Self::validate_host(host)?, port));
        }

        match authority.rsplit_once(':') {
            Some((host, port)) => Ok((
                Self::validate_host(host)?,
                Self::parse_port(&format!(":{port}"), scheme)?,
            )),
            None => Ok((Self::validate_host(authority)?, scheme.default_port())),
        }
    }

    fn parse_port(suffix: &str, scheme: RelayScheme) -> Result<u16, RelayUrlError> {
        let Some(digits) = suffix.strip_prefix(':') else {
            return if suffix.is_empty() {
                Ok(scheme.default_port())
            } else {
                Err(RelayUrlError::InvalidPort(suffix.to_owned()))
            };
        };
        digits
            .parse()
            .map_err(|_| RelayUrlError::InvalidPort(digits.to_owned()))
    }

    fn validate_host(host: &str) -> Result<String, RelayUrlError> {
        if host.is_empty() {
            return Err(RelayUrlError::EmptyHost);
        }
        let forbidden = host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '?' | '#' | '@'));
        if forbidden {
            return Err(RelayUrlError::InvalidHost(host.to_owned()));
        }
        Ok(host.to_ascii_lowercase())
    }

    fn render_host(host: &str) -> String {
        if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        }
    }

    /// The transport this URL asks for.
    #[must_use]
    pub const fn scheme(&self) -> RelayScheme {
        self.scheme
    }

    /// The host to resolve, without IPv6 brackets.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port to dial.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The `Host:` header value.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// The request target of the upgrade, path plus query.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Whether the driver must run a TLS handshake.
    #[must_use]
    pub const fn is_secure(&self) -> bool {
        self.scheme.is_secure()
    }
}

impl std::fmt::Display for RelayUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://{}{}", self.scheme, self.authority, self.target)
    }
}

impl std::str::FromStr for RelayUrl {
    type Err = RelayUrlError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Parsed;

    impl Parsed {
        fn ok(input: &str) -> RelayUrl {
            RelayUrl::parse(input).unwrap_or_else(|e| panic!("{input} should parse, got {e}"))
        }

        fn err(input: &str) -> RelayUrlError {
            RelayUrl::parse(input).unwrap_err()
        }
    }

    #[test]
    fn secure_url_defaults_to_port_443() {
        let url = Parsed::ok("wss://relay.example.com");
        assert_eq!(url.scheme(), RelayScheme::Wss);
        assert_eq!(url.host(), "relay.example.com");
        assert_eq!(url.port(), 443);
        assert_eq!(url.target(), "/");
        assert_eq!(url.authority(), "relay.example.com");
        assert!(url.is_secure());
    }

    #[test]
    fn plain_url_defaults_to_port_80() {
        let url = Parsed::ok("ws://localhost");
        assert_eq!(url.port(), 80);
        assert!(!url.is_secure());
    }

    #[test]
    fn explicit_port_lands_in_the_authority() {
        let url = Parsed::ok("ws://127.0.0.1:7777");
        assert_eq!(url.host(), "127.0.0.1");
        assert_eq!(url.port(), 7777);
        assert_eq!(url.authority(), "127.0.0.1:7777");
        assert_eq!(url.to_string(), "ws://127.0.0.1:7777/");
    }

    #[test]
    fn default_port_stays_out_of_the_authority() {
        let url = Parsed::ok("wss://relay.example.com:443/nostr");
        assert_eq!(url.authority(), "relay.example.com");
        assert_eq!(url.target(), "/nostr");
    }

    #[test]
    fn query_survives_and_fragment_does_not() {
        let url = Parsed::ok("wss://relay.example.com/nostr?x=1#frag");
        assert_eq!(url.target(), "/nostr?x=1");
    }

    #[test]
    fn query_without_path_keeps_its_own_target() {
        let url = Parsed::ok("wss://relay.example.com?x=1");
        assert_eq!(url.host(), "relay.example.com");
        assert_eq!(url.target(), "?x=1");
    }

    #[test]
    fn scheme_and_host_are_case_insensitive() {
        let url = Parsed::ok("WSS://Relay.Example.COM/Path");
        assert_eq!(url.scheme(), RelayScheme::Wss);
        assert_eq!(url.host(), "relay.example.com");
        assert_eq!(url.target(), "/Path");
    }

    #[test]
    fn http_schemes_alias_the_websocket_ones() {
        assert_eq!(Parsed::ok("http://a.example").scheme(), RelayScheme::Ws);
        assert_eq!(Parsed::ok("https://a.example").scheme(), RelayScheme::Wss);
        assert_eq!(Parsed::ok("https://a.example").port(), 443);
    }

    #[test]
    fn ipv6_host_drops_brackets_and_authority_keeps_them() {
        let url = Parsed::ok("ws://[::1]:7777/x");
        assert_eq!(url.host(), "::1");
        assert_eq!(url.port(), 7777);
        assert_eq!(url.authority(), "[::1]:7777");
        assert_eq!(url.target(), "/x");
    }

    #[test]
    fn ipv6_host_without_port_uses_the_default() {
        let url = Parsed::ok("wss://[2001:db8::1]/");
        assert_eq!(url.host(), "2001:db8::1");
        assert_eq!(url.port(), 443);
        assert_eq!(url.authority(), "[2001:db8::1]");
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(Parsed::ok("  wss://relay.example.com  ").port(), 443);
    }

    #[test]
    fn display_round_trips_through_parse() {
        let url = Parsed::ok("wss://relay.example.com/nostr?a=b");
        assert_eq!(Parsed::ok(&url.to_string()), url);
    }

    #[test]
    fn from_str_matches_parse() {
        let parsed: RelayUrl = "ws://localhost:8080".parse().unwrap();
        assert_eq!(parsed, Parsed::ok("ws://localhost:8080"));
    }

    #[test]
    fn a_missing_scheme_is_an_error() {
        assert_eq!(Parsed::err("relay.example.com"), RelayUrlError::MissingScheme);
    }

    #[test]
    fn an_unknown_scheme_names_itself() {
        assert_eq!(
            Parsed::err("ftp://relay.example.com"),
            RelayUrlError::UnknownScheme("ftp".to_owned())
        );
    }

    #[test]
    fn an_empty_host_is_an_error() {
        assert_eq!(Parsed::err("wss:///path"), RelayUrlError::EmptyHost);
        assert_eq!(Parsed::err("wss://:443"), RelayUrlError::EmptyHost);
    }

    #[test]
    fn a_non_numeric_port_is_an_error() {
        assert_eq!(
            Parsed::err("wss://relay.example.com:abc"),
            RelayUrlError::InvalidPort("abc".to_owned())
        );
    }

    #[test]
    fn a_port_above_the_u16_range_is_an_error() {
        assert_eq!(
            Parsed::err("wss://relay.example.com:70000"),
            RelayUrlError::InvalidPort("70000".to_owned())
        );
    }

    #[test]
    fn an_unclosed_ipv6_host_is_an_error() {
        assert_eq!(Parsed::err("ws://[::1:7777"), RelayUrlError::UnclosedIpv6Host);
    }

    #[test]
    fn user_info_is_rejected() {
        assert_eq!(
            Parsed::err("wss://user@relay.example.com"),
            RelayUrlError::UserInfoNotSupported
        );
    }

    #[test]
    fn a_host_with_whitespace_is_an_error() {
        assert!(matches!(
            Parsed::err("wss://relay example.com"),
            RelayUrlError::InvalidHost(_)
        ));
    }

    #[test]
    fn errors_carry_readable_text() {
        assert!(
            RelayUrlError::MissingScheme
                .to_string()
                .contains("needs a scheme")
        );
        assert!(
            RelayUrlError::InvalidPort("x".to_owned())
                .to_string()
                .contains('x')
        );
    }
}
