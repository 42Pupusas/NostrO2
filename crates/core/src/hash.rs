/// Zero-alloc adapter: feeds bourne `JsonWrite` output directly into SHA-256.
///
/// Used by both [`crate::NostrNote`] and [`crate::NostrNoteView`] to compute
/// canonical event IDs without allocating an intermediate string.
#[allow(clippy::redundant_pub_crate)]
pub(crate) struct Sha256Sink<'a>(pub(crate) &'a mut sha2::Sha256);

impl bourne::JsonWrite for Sha256Sink<'_> {
    type Error = core::convert::Infallible;

    #[inline]
    fn write_byte(&mut self, b: u8) -> Result<(), Self::Error> {
        use sha2::Digest as _;
        self.0.update([b]);
        Ok(())
    }

    #[inline]
    fn write_str_raw(&mut self, s: &str) -> Result<(), Self::Error> {
        use sha2::Digest as _;
        self.0.update(s.as_bytes());
        Ok(())
    }

    #[inline]
    fn write_float_f64(&mut self, f: f64) -> Result<(), Self::Error> {
        use sha2::Digest as _;
        use std::io::Write as _;
        let mut buf = [0_u8; 24];
        let n = write!(&mut buf[..], "{f}").map_or(0, |()| {
            buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
        });
        self.0.update(&buf[..n]);
        Ok(())
    }
}
