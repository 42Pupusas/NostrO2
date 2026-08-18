//! Backend-agnostic JSON serialization for outbound relay frames.
//!
//! Mirrors `nostro2`'s own `bourne` / `serde` feature split so `relay.rs`
//! stays free of `#[cfg]` at its call sites. The module itself is private,
//! so these items are already crate-visible without `pub(crate)`.

pub struct RelayJson;

impl RelayJson {
    /// # Errors
    ///
    /// Returns [`crate::errors::NostrRelayError`] when serialization fails.
    #[cfg(feature = "bourne")]
    pub fn to_string<T: json_bourne::ToJson + ?Sized>(
        value: &T,
    ) -> Result<String, crate::errors::NostrRelayError> {
        Ok(json_bourne::to_string(value)?)
    }

    /// # Errors
    ///
    /// Returns [`crate::errors::NostrRelayError`] when serialization fails.
    #[cfg(feature = "serde")]
    pub fn to_string<T: serde::Serialize + ?Sized>(
        value: &T,
    ) -> Result<String, crate::errors::NostrRelayError> {
        Ok(serde_json::to_string(value)?)
    }

    #[cfg(all(test, feature = "bourne"))]
    pub const fn dummy_err() -> json_bourne::Error {
        json_bourne::Error::new(
            json_bourne::ErrorKind::ExpectedArray,
            json_bourne::Position { offset: 0 },
        )
    }

    #[cfg(all(test, feature = "serde"))]
    pub fn dummy_err() -> serde_json::Error {
        serde_json::from_str::<()>("!!!").unwrap_err()
    }
}
