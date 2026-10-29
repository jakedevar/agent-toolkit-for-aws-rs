//! CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`), table-driven, as
//! ZIP uses it (R-AR-ZIP).

/// A running CRC-32.
#[derive(Debug, Clone, Copy)]
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A fresh CRC.
    #[must_use]
    pub fn new() -> Self {
        Self { state: u32::MAX }
    }

    /// Feed `bytes`.
    pub fn update(&mut self, bytes: &[u8]) {
        let _ = bytes;
        todo!("L2: crc32")
    }

    /// The CRC of everything fed so far.
    #[must_use]
    pub fn finish(&self) -> u32 {
        !self.state
    }
}

/// The CRC-32 of `bytes`.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value_and_incremental_updates() {
        // The standard check value of CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
        let mut crc = Crc32::new();
        for piece in [&b"1234"[..], b"", b"56789"] {
            crc.update(piece);
        }
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }
}
