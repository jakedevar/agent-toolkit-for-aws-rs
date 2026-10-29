//! Seams for randomness (design 7.1 `launch/clock.rs`).
//!
//! L0-L2 need only [`Random`]: the private temporary names of the state files
//! and of the Git ignore-rule metadata. L3 adds the PKCE verifier, the state
//! and the `clientToken` values through the same trait, and L4 the `Clock`
//! and `Exit` seams of the callback helper.

use std::cell::Cell;
use std::fmt::Write as _;

use super::LaunchError;

/// A source of unpredictable bytes.
pub trait Random {
    /// Fill `buf` completely.
    ///
    /// # Errors
    /// [`LaunchError::Config`] when the source fails (the OS CSPRNG is
    /// unavailable); callers never fall back to a weaker source.
    fn fill(&self, buf: &mut [u8]) -> Result<(), LaunchError>;
}

/// The operating system CSPRNG (`getrandom`).
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), LaunchError> {
        getrandom::fill(buf).map_err(|_| {
            LaunchError::Config(
                "The operating system random number generator is unavailable.".to_string(),
            )
        })
    }
}

/// `bytes` random bytes as lower-case hexadecimal (`2 * bytes` characters).
///
/// # Errors
/// The [`Random`] failure.
pub fn random_hex(random: &dyn Random, bytes: usize) -> Result<String, LaunchError> {
    let mut buf = vec![0_u8; bytes];
    random.fill(&mut buf)?;
    let mut hex = String::with_capacity(2 * bytes);
    for byte in buf {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// A deterministic test source: an `xorshift64*` stream from a seed. Never
/// used outside tests; it exists so temporary names are reproducible.
#[derive(Debug)]
pub struct SeededRandom(Cell<u64>);

impl SeededRandom {
    /// A stream starting from `seed` (0 is replaced by a fixed constant).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(Cell::new(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        }))
    }
}

impl Random for SeededRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), LaunchError> {
        for byte in buf {
            let mut x = self.0.get();
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0.set(x);
            *byte = x.wrapping_mul(0x2545_f491_4f6c_dd1d).to_be_bytes()[0];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_random_fills_and_hex_has_the_requested_length() {
        let mut first = [0_u8; 32];
        let mut second = [0_u8; 32];
        OsRandom.fill(&mut first).expect("csprng");
        OsRandom.fill(&mut second).expect("csprng");
        assert_ne!(first, second);
        let hex = random_hex(&OsRandom, 8).expect("hex");
        assert_eq!(hex.len(), 16);
        assert!(hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let seeded = random_hex(&SeededRandom::new(7), 8).expect("hex");
        assert_eq!(seeded, random_hex(&SeededRandom::new(7), 8).expect("hex"));
    }
}
