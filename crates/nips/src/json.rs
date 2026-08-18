//! Backend-agnostic JSON dispatch shared across the NIP implementations.
//!
//! Mirrors `nostro2`'s own `bourne` / `serde` feature split: every NIP
//! module needs to serialize/parse small wire structs, and this single
//! entry point keeps that call-site free of `#[cfg]`.

pub struct NipJson;

impl NipJson {
    /// # Errors
    ///
    /// Returns [`crate::json::JsonError`] when serialization fails.
    #[cfg(feature = "bourne")]
    pub fn to_string<T: json_bourne::ToJson + ?Sized>(value: &T) -> Result<String, JsonError> {
        json_bourne::to_string(value).map_err(JsonError)
    }
    #[cfg(feature = "serde")]
    pub fn to_string<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
        serde_json::to_string(value).map_err(JsonError)
    }

    /// # Errors
    ///
    /// Returns [`crate::json::JsonError`] when parsing fails.
    #[cfg(feature = "bourne")]
    pub fn parse_str<T: for<'a> json_bourne::FromJson<'a>>(s: &str) -> Result<T, JsonError> {
        json_bourne::parse_str(s).map_err(JsonError)
    }
    #[cfg(feature = "serde")]
    pub fn parse_str<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, JsonError> {
        serde_json::from_str(s).map_err(JsonError)
    }
}

/// Opaque JSON error, wrapping whichever backend is active. Displays and
/// sources through to the underlying error either way.
#[derive(Debug)]
#[cfg(feature = "bourne")]
pub struct JsonError(json_bourne::Error);
#[derive(Debug)]
#[cfg(feature = "serde")]
pub struct JsonError(serde_json::Error);

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for JsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[cfg(feature = "bourne")]
impl From<json_bourne::Error> for JsonError {
    fn from(e: json_bourne::Error) -> Self {
        Self(e)
    }
}

#[cfg(feature = "serde")]
impl From<serde_json::Error> for JsonError {
    fn from(e: serde_json::Error) -> Self {
        Self(e)
    }
}
