//! Minimal bech32 encode/decode (BIP-173) with no external dependencies.
//!
//! Only supports the original bech32 checksum (not bech32m / BIP-350).
//! Sufficient for NIP-19 `nsec1…` / `npub1…` keys.

// ── Crypto struct ─────────────────────────────────────────────────

/// Minimal bech32 encoding/decoding (BIP-173).
///
/// Used indirectly via [`super::SignerBech32`] and
/// [`super::KeypairBech32`] — consumers should prefer those.
#[doc(hidden)]
pub struct Bech32Crypto;

impl Bech32Crypto {
    const CHARSET: &'static [u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

    fn charset_rev(c: u8) -> Option<u8> {
        // CHARSET has 32 entries; position is always < 32.
        #[allow(clippy::cast_possible_truncation)]
        Self::CHARSET.iter().position(|&ch| ch == c).map(|i| i as u8)
    }

    fn polymod(values: &[u8]) -> u32 {
        const GEN: [u32; 5] = [
            0x3b6a_57b2,
            0x2650_8e6d,
            0x1ea1_19fa,
            0x3d42_33dd,
            0x2a14_62b3,
        ];
        let mut chk: u32 = 1;
        for &v in values {
            let top = chk >> 25;
            chk = ((chk & 0x01ff_ffff) << 5) ^ u32::from(v);
            for (i, g) in GEN.iter().enumerate() {
                if (top >> i) & 1 == 1 {
                    chk ^= g;
                }
            }
        }
        chk
    }

    fn hrp_expand(hrp: &str) -> Vec<u8> {
        let mut v: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
        v.push(0);
        v.extend(hrp.bytes().map(|b| b & 0x1f));
        v
    }

    fn create_checksum(hrp: &str, data: &[u8]) -> [u8; 6] {
        let mut values = Self::hrp_expand(hrp);
        values.extend_from_slice(data);
        values.extend_from_slice(&[0; 6]);
        let pm = Self::polymod(&values) ^ 1;
        let mut ret = [0_u8; 6];
        for (i, byte) in ret.iter_mut().enumerate() {
            *byte = ((pm >> (5 * (5 - i))) & 0x1f) as u8;
        }
        ret
    }

    fn verify_checksum(hrp: &str, data: &[u8]) -> bool {
        let mut values = Self::hrp_expand(hrp);
        values.extend_from_slice(data);
        Self::polymod(&values) == 1
    }

    #[allow(clippy::cast_possible_truncation)]
    fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Vec<u8> {
        let mut acc: u32 = 0;
        let mut bits: u32 = 0;
        let max_v = (1_u32 << to) - 1;
        let mut ret = Vec::new();

        for &value in data {
            acc = (acc << from) | u32::from(value);
            bits += from;
            while bits >= to {
                bits -= to;
                ret.push(((acc >> bits) & max_v) as u8);
            }
        }

        if pad && bits > 0 {
            ret.push(((acc << (to - bits)) & max_v) as u8);
        }

        ret
    }

    // ── Public methods ────────────────────────────────────────────

    /// Encode `data` bytes as a bech32 string with the given human-readable part.
    ///
    /// # Errors
    /// Returns `Bech32Error::InvalidHrp` if `hrp` is empty or contains invalid chars.
    pub fn encode(hrp: &str, data: &[u8]) -> Result<String, Bech32Error> {
        if hrp.is_empty() || hrp.len() > 83 {
            return Err(Bech32Error::InvalidHrp);
        }
        if !hrp.bytes().all(|b| (33..=126).contains(&b)) {
            return Err(Bech32Error::InvalidHrp);
        }

        let base32 = Self::convert_bits(data, 8, 5, true);
        let checksum = Self::create_checksum(hrp, &base32);

        let mut result = String::with_capacity(hrp.len() + 1 + base32.len() + 6);
        result.push_str(hrp);
        result.push('1');
        for &b in &base32 {
            result.push(Self::CHARSET[b as usize] as char);
        }
        for &b in &checksum {
            result.push(Self::CHARSET[b as usize] as char);
        }
        Ok(result)
    }

    /// Decode a bech32 string, returning `(hrp, data_bytes)`.
    ///
    /// # Errors
    /// Returns an error if the string is malformed, has mixed case, or has an
    /// invalid checksum.
    pub fn decode(s: &str) -> Result<(String, Vec<u8>), Bech32Error> {
        let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
        let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
        if has_upper && has_lower {
            return Err(Bech32Error::MixedCase);
        }

        let lower = s.to_ascii_lowercase();
        let sep = lower.rfind('1').ok_or(Bech32Error::NoSeparator)?;
        let hrp = &lower[..sep];
        let data_part = &lower[sep + 1..];

        if hrp.is_empty() || data_part.len() < 6 {
            return Err(Bech32Error::InvalidLength);
        }

        let mut data5 = Vec::with_capacity(data_part.len());
        for c in data_part.bytes() {
            data5.push(Self::charset_rev(c).ok_or(Bech32Error::InvalidChar(c as char))?);
        }

        if !Self::verify_checksum(hrp, &data5) {
            return Err(Bech32Error::InvalidChecksum);
        }

        let payload = &data5[..data5.len() - 6];
        let bytes = Self::convert_bits(payload, 5, 8, false);
        Ok((hrp.to_string(), bytes))
    }
}

// ── Error type ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bech32Error {
    InvalidChar(char),
    InvalidLength,
    InvalidChecksum,
    InvalidHrp,
    MixedCase,
    NoSeparator,
}

impl std::fmt::Display for Bech32Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidChar(c) => write!(f, "invalid bech32 character: {c}"),
            Self::InvalidLength => f.write_str("invalid bech32 length"),
            Self::InvalidChecksum => f.write_str("invalid bech32 checksum"),
            Self::InvalidHrp => f.write_str("invalid human-readable part"),
            Self::MixedCase => f.write_str("mixed case in bech32 string"),
            Self::NoSeparator => f.write_str("no separator found in bech32 string"),
        }
    }
}

impl std::error::Error for Bech32Error {}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_nsec() {
        let secret = [0xab_u8; 32];
        let encoded = Bech32Crypto::encode("nsec", &secret).unwrap();
        assert!(encoded.starts_with("nsec1"));
        let (hrp, decoded) = Bech32Crypto::decode(&encoded).unwrap();
        assert_eq!(hrp, "nsec");
        assert_eq!(decoded, secret);
    }

    #[test]
    fn round_trip_npub() {
        let pubkey = [0xcd_u8; 32];
        let encoded = Bech32Crypto::encode("npub", &pubkey).unwrap();
        assert!(encoded.starts_with("npub1"));
        let (hrp, decoded) = Bech32Crypto::decode(&encoded).unwrap();
        assert_eq!(hrp, "npub");
        assert_eq!(decoded, pubkey);
    }

    #[test]
    fn round_trip_varied_bytes() {
        let data: Vec<u8> = (0..32).collect();
        let encoded = Bech32Crypto::encode("nsec", &data).unwrap();
        let (_, decoded) = Bech32Crypto::decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn rejects_mixed_case() {
        let encoded = Bech32Crypto::encode("nsec", &[0; 32]).unwrap();
        let mixed = encoded[..5].to_uppercase() + &encoded[5..];
        assert!(matches!(Bech32Crypto::decode(&mixed), Err(Bech32Error::MixedCase)));
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut encoded = Bech32Crypto::encode("nsec", &[0; 32]).unwrap();
        let last = encoded.pop().unwrap();
        let replacement = if last == 'q' { 'p' } else { 'q' };
        encoded.push(replacement);
        assert!(matches!(
            Bech32Crypto::decode(&encoded),
            Err(Bech32Error::InvalidChecksum)
        ));
    }

    #[test]
    fn rejects_no_separator() {
        assert!(matches!(
            Bech32Crypto::decode("noseparator"),
            Err(Bech32Error::NoSeparator)
        ));
    }

    #[test]
    fn rejects_empty_hrp() {
        assert!(matches!(Bech32Crypto::encode("", &[0; 32]), Err(Bech32Error::InvalidHrp)));
    }

    #[test]
    fn accepts_uppercase_input() {
        let encoded = Bech32Crypto::encode("nsec", &[0xff; 32]).unwrap();
        let upper = encoded.to_uppercase();
        let (hrp, decoded) = Bech32Crypto::decode(&upper).unwrap();
        assert_eq!(hrp, "nsec");
        assert_eq!(decoded, vec![0xff; 32]);
    }

    #[test]
    fn error_display_all_variants() {
        let cases = [
            Bech32Error::InvalidChar('x'),
            Bech32Error::InvalidLength,
            Bech32Error::InvalidChecksum,
            Bech32Error::InvalidHrp,
            Bech32Error::MixedCase,
            Bech32Error::NoSeparator,
        ];
        for err in &cases {
            assert!(!format!("{err}").is_empty());
        }
    }

    #[test]
    fn compatible_with_bip173_test_vector() {
        let (hrp, data) = Bech32Crypto::decode("a12uel5l").unwrap();
        assert_eq!(hrp, "a");
        assert!(data.is_empty());
    }
}
