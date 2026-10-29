//! A byte collector whose *allocation*, not only its length, is bounded
//! (#5082, applied to every L2 collector: the archive being built and Git's
//! captured output).
//!
//! `Vec` growth is amortized doubling: a vector whose length is kept at or
//! below a cap `C` can still reserve up to about `2 C`, and a moving
//! reallocation briefly holds the old and the new buffer. [`CappedBuf`]
//! therefore never calls the amortized growth path. It grows with
//! `try_reserve_exact` to `min(limit, max(needed, 2 * capacity, MIN_GROWTH))`,
//! so:
//!
//! - **steady state**: `capacity <= limit` at every point (asserted after
//!   every growth, and by the tests after every append);
//! - **transient peak** during a growth that moves the buffer: the old plus
//!   the new allocation, `old + new < 2 * limit`. [`CappedBuf::peak`] records
//!   the largest such sum seen, the allocator peak this collector can cause;
//! - **refusal**: an append that would pass `limit` is refused before any
//!   byte is copied or any memory reserved, and leaves the buffer unchanged;
//!   an allocation failure is reported, never an abort.
//!
//! The payload-length invariant (`len <= limit`) and the allocation account
//! (`capacity <= limit`, `peak < 2 * limit`) are separate numbers on purpose.

/// The smallest growth step, so small appends do not reallocate per call.
pub const MIN_GROWTH: usize = 4096;

/// Why an append was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufError {
    /// The append would make the length exceed the limit; `attempted` is
    /// the length it would have reached.
    Limit {
        /// Length the append would have reached.
        attempted: usize,
    },
    /// The allocator refused the reservation.
    Alloc,
}

/// A growable byte buffer with a hard length and allocation limit.
#[derive(Debug)]
pub struct CappedBuf {
    data: Vec<u8>,
    limit: usize,
    peak: usize,
    growths: usize,
}

impl CappedBuf {
    /// An empty buffer that may hold at most `limit` bytes. Allocates nothing.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
            peak: 0,
            growths: 0,
        }
    }

    /// Append `bytes`, or refuse without changing the buffer.
    ///
    /// # Errors
    /// [`BufError::Limit`] past the limit, [`BufError::Alloc`] when the
    /// reservation fails.
    pub fn extend(&mut self, bytes: &[u8]) -> Result<(), BufError> {
        let _ = bytes;
        todo!("L2: #5082 growth")
    }

    /// Overwrite `bytes.len()` bytes at `offset` (header fix-ups).
    ///
    /// # Panics
    /// When the range is not inside the current length (an internal bug).
    pub fn patch(&mut self, offset: usize, bytes: &[u8]) {
        self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether nothing was appended.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Current allocation, in bytes.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// The length and allocation limit.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The largest old-plus-new allocation a growth has held at once.
    #[must_use]
    pub fn peak(&self) -> usize {
        self.peak
    }

    /// How many times the allocation grew.
    #[must_use]
    pub fn growths(&self) -> usize {
        self.growths
    }

    /// The collected bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// The collected bytes, giving up the buffer.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }
}

#[cfg(test)]
mod tests;
