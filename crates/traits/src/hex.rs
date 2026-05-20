//! Minimal hex encode/decode — replaces the `hex` crate.

const LUT: &[u8; 16] = b"0123456789abcdef";

pub trait Hexable {
    fn to_hex(&self) -> String;
}

impl Hexable for [u8] {
    fn to_hex(&self) -> String {
        let mut s = String::with_capacity(self.len() * 2);
        for &b in self {
            s.push(LUT[(b >> 4) as usize] as char);
            s.push(LUT[(b & 0x0f) as usize] as char);
        }
        s
    }
}

impl<const N: usize> Hexable for [u8; N] {
    fn to_hex(&self) -> String {
        self.as_slice().to_hex()
    }
}

pub trait FromHex {
    /// Decode hex into a new `Vec<u8>`.
    ///
    /// # Errors
    ///
    /// Returns [`HexError`] on odd length or non-hex characters.
    fn decode_hex(&self) -> Result<Vec<u8>, HexError>;

    /// Decode hex into an existing byte slice. The slice must be exactly
    /// half the length of the hex input.
    ///
    /// # Errors
    ///
    /// Returns [`HexError`] on length mismatch or non-hex characters.
    fn decode_hex_to_slice(&self, out: &mut [u8]) -> Result<(), HexError>;
}

impl FromHex for str {
    fn decode_hex(&self) -> Result<Vec<u8>, HexError> {
        let bytes = self.as_bytes();
        if bytes.len() % 2 != 0 {
            return Err(HexError::OddLength);
        }
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks_exact(2) {
            out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
        }
        Ok(out)
    }

    fn decode_hex_to_slice(&self, out: &mut [u8]) -> Result<(), HexError> {
        let input = self.as_bytes();
        if input.len() != out.len() * 2 {
            return Err(HexError::LengthMismatch);
        }
        for (i, pair) in input.chunks_exact(2).enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(())
    }
}

#[inline]
const fn nibble(b: u8) -> Result<u8, HexError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(HexError::InvalidChar(b)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexError {
    OddLength,
    LengthMismatch,
    InvalidChar(u8),
}

impl std::fmt::Display for HexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OddLength => f.write_str("odd-length hex string"),
            Self::LengthMismatch => f.write_str("hex/output length mismatch"),
            Self::InvalidChar(b) => write!(f, "invalid hex character: {}", *b as char),
        }
    }
}

impl std::error::Error for HexError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_roundtrip() {
        let bytes = [0xde_u8, 0xad, 0xbe, 0xef, 0x00, 0xff];
        assert_eq!(bytes.to_hex(), "deadbeef00ff");
    }

    #[test]
    fn decode_roundtrip() {
        let bytes = "deadbeef00ff".decode_hex().unwrap();
        assert_eq!(bytes, [0xde, 0xad, 0xbe, 0xef, 0x00, 0xff]);
        assert_eq!(bytes.to_hex(), "deadbeef00ff");
    }

    #[test]
    fn decode_to_slice_works() {
        let mut out = [0_u8; 3];
        "abcdef".decode_hex_to_slice(&mut out).unwrap();
        assert_eq!(out, [0xab, 0xcd, 0xef]);
    }

    #[test]
    fn decode_uppercase() {
        assert_eq!("ABCDEF".decode_hex().unwrap(), [0xab, 0xcd, 0xef]);
    }

    #[test]
    fn odd_length_fails() {
        assert!(matches!("abc".decode_hex(), Err(HexError::OddLength)));
    }

    #[test]
    fn invalid_char_fails() {
        assert!(matches!("zz".decode_hex(), Err(HexError::InvalidChar(b'z'))));
    }

    #[test]
    fn slice_length_mismatch() {
        let mut out = [0_u8; 2];
        assert!(matches!(
            "aabbcc".decode_hex_to_slice(&mut out),
            Err(HexError::LengthMismatch)
        ));
    }

    #[test]
    fn empty_input() {
        assert_eq!([].to_hex(), "");
        assert_eq!("".decode_hex().unwrap(), Vec::<u8>::new());
    }
}
