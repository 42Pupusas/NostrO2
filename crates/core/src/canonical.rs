//! Minimal, JSON-backend-independent writer for the NIP-01 canonical
//! event serialization used to compute event IDs.
//!
//! The canonical form `[0, pubkey, created_at, kind, tags, content]` is a
//! fixed part of the Nostr protocol, not a serialization choice. Every
//! event ID must hash the exact same bytes no matter which JSON crate (if
//! any) a consumer picked for full note serialization elsewhere. Keeping
//! this trait free of both `json-bourne` and `serde` guarantees that.

/// Escape sequences for bytes 0x00-0x1F plus `"` and `\`, per RFC 8259
/// §7. `0` means the byte passes through verbatim; otherwise the value
/// is the ASCII char to emit after `\` (e.g. `b'n'` for `\n`).
const ESCAPE_TABLE: [u8; 256] = {
    let mut t = [0_u8; 256];
    t[b'"' as usize] = b'"';
    t[b'\\' as usize] = b'\\';
    t[b'\n' as usize] = b'n';
    t[b'\r' as usize] = b'r';
    t[b'\t' as usize] = b't';
    t[0x08] = b'b';
    t[0x0C] = b'f';
    t
};

const HEX_LOWER: [u8; 16] = *b"0123456789abcdef";

/// Output sink for the NIP-01 canonical event serialization.
///
/// Exposes only the primitives that shape ever needs — bytes, escaped
/// strings, and signed/unsigned 64-bit integers. No floats, no JSON
/// value tree, no dependency on any general-purpose JSON crate.
pub trait CanonicalWrite {
    type Error;

    /// Append a single ASCII byte. Used for structural punctuation.
    fn write_byte(&mut self, b: u8) -> Result<(), Self::Error>;

    /// Append a `&str` verbatim, with no escaping.
    #[inline]
    fn write_str_raw(&mut self, s: &str) -> Result<(), Self::Error> {
        for &b in s.as_bytes() {
            self.write_byte(b)?;
        }
        Ok(())
    }

    /// Append a JSON-quoted, escaped string (including the surrounding
    /// `"` characters).
    #[inline]
    fn write_escaped_str(&mut self, s: &str) -> Result<(), Self::Error> {
        self.write_byte(b'"')?;
        for &b in s.as_bytes() {
            let esc = ESCAPE_TABLE[b as usize];
            if esc != 0 {
                self.write_byte(b'\\')?;
                self.write_byte(esc)?;
            } else if b < 0x20 {
                self.write_byte(b'\\')?;
                self.write_byte(b'u')?;
                self.write_byte(b'0')?;
                self.write_byte(b'0')?;
                self.write_byte(HEX_LOWER[(b >> 4) as usize])?;
                self.write_byte(HEX_LOWER[(b & 0x0F) as usize])?;
            } else {
                self.write_byte(b)?;
            }
        }
        self.write_byte(b'"')
    }

    /// Write a signed 64-bit integer as a JSON number.
    #[inline]
    fn write_int_i64(&mut self, n: i64) -> Result<(), Self::Error> {
        self.write_str_raw(itoa::Buffer::new().format_i64(n))
    }

    /// Write an unsigned 64-bit integer as a JSON number.
    #[inline]
    fn write_int_u64(&mut self, n: u64) -> Result<(), Self::Error> {
        self.write_str_raw(itoa::Buffer::new().format_u64(n))
    }
}

/// Tiny stack-buffer integer formatter, avoiding a dependency on any
/// JSON crate (or `std::fmt`'s heavier machinery) for the two integer
/// widths the canonical form needs.
mod itoa {
    pub struct Buffer([u8; 20]);

    impl Buffer {
        #[must_use]
        pub const fn new() -> Self {
            Self([0_u8; 20])
        }

        pub fn format_u64(&mut self, mut mag: u64) -> &str {
            if mag == 0 {
                self.0[0] = b'0';
                return core::str::from_utf8(&self.0[..1]).unwrap_or_default();
            }
            let mut pos = self.0.len();
            while mag > 0 {
                pos -= 1;
                #[allow(clippy::cast_possible_truncation)]
                {
                    self.0[pos] = b'0' + (mag % 10) as u8;
                }
                mag /= 10;
            }
            core::str::from_utf8(&self.0[pos..]).unwrap_or_default()
        }

        pub fn format_i64(&mut self, n: i64) -> &str {
            if n >= 0 {
                #[allow(clippy::cast_sign_loss)]
                return self.format_u64(n as u64);
            }
            #[allow(clippy::cast_possible_truncation)]
            let mag = (i128::from(n)).unsigned_abs() as u64;
            let len = self.0.len();
            let digits_start = len - self.format_u64(mag).len();
            self.0[digits_start - 1] = b'-';
            core::str::from_utf8(&self.0[digits_start - 1..]).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CanonicalWrite;

    struct VecSink(Vec<u8>);
    impl CanonicalWrite for VecSink {
        type Error = core::convert::Infallible;
        fn write_byte(&mut self, b: u8) -> Result<(), Self::Error> {
            self.0.push(b);
            Ok(())
        }
    }

    fn as_str(sink: &VecSink) -> &str {
        core::str::from_utf8(&sink.0).unwrap()
    }

    #[test]
    fn integers_round_trip_through_display() {
        let cases = [0_i64, 1, -1, 42, -42, i64::MAX, i64::MIN, 9_999, -9_999];
        for n in cases {
            let mut sink = VecSink(Vec::new());
            sink.write_int_i64(n).unwrap();
            assert_eq!(as_str(&sink), n.to_string());
        }
    }

    #[test]
    fn unsigned_integers_round_trip_through_display() {
        let cases = [0_u64, 1, 42, u64::MAX];
        for n in cases {
            let mut sink = VecSink(Vec::new());
            sink.write_int_u64(n).unwrap();
            assert_eq!(as_str(&sink), n.to_string());
        }
    }

    #[test]
    fn escapes_control_chars_quotes_and_backslashes() {
        let mut sink = VecSink(Vec::new());
        sink.write_escaped_str("a\"b\\c\nd\te\rf\u{8}g\u{c}h\u{1}i")
            .unwrap();
        assert_eq!(as_str(&sink), r#""a\"b\\c\nd\te\rf\bg\fh\u0001i""#);
    }

    #[test]
    fn passes_through_plain_ascii_and_utf8() {
        let mut sink = VecSink(Vec::new());
        sink.write_escaped_str("hello 🦀").unwrap();
        assert_eq!(as_str(&sink), "\"hello 🦀\"");
    }
}
