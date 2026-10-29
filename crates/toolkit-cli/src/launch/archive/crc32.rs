//! CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`), table-driven, as
//! ZIP uses it (R-AR-ZIP).

/// The byte-wise lookup table of the reflected polynomial.
#[allow(clippy::cast_possible_truncation)]
const TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

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
        let mut state = self.state;
        for byte in bytes {
            state = TABLE[usize::from(state.to_le_bytes()[0] ^ byte)] ^ (state >> 8);
        }
        self.state = state;
    }

    /// The CRC of everything fed so far.
    #[must_use]
    pub fn finish(self) -> u32 {
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
