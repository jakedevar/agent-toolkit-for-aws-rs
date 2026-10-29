//! The ledger of source files skipped as secrets (#5135, part of #5082).
//!
//! The Python kept every skipped path and logged them all on one INFO line
//! (`Excluded N file(s) matching secret patterns: a, b, ...`). A tree with
//! one allowed file and any number of `.env.*` files would make that list
//! grow without bound while the archive stays tiny, so the ledger is
//! bounded by a [`SkippedBudget`]:
//!
//! - it retains the first skipped paths in walk order, at most
//!   `max_paths` of them and at most `max_bytes` bytes of path text (each
//!   path plus one separator byte), in a [`CappedBuf`] whose *allocation*
//!   never exceeds `max_bytes`;
//! - the first path that does not fit closes the ledger: it and every later
//!   path are only counted, never stored, so the retained list is always a
//!   prefix of the walk order;
//! - [`SkippedSecrets::summary`] names the retained paths and the count of
//!   the further skipped paths it does not list.
//!
//! Skipping never fails the build: a refused retention is a counted path.
//!
//! [`CappedBuf`]: crate::launch::buffer::CappedBuf

use crate::launch::buffer::CappedBuf;

/// How much of the skipped-path list is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedBudget {
    /// Most paths retained.
    pub max_paths: usize,
    /// Most bytes of retained path text, one separator byte per path
    /// included; also the ledger's allocation limit.
    pub max_bytes: usize,
}

impl SkippedBudget {
    /// The production budget: 1000 paths or 64 KiB.
    pub const PRODUCTION: Self = Self {
        max_paths: 1000,
        max_bytes: 64 * 1024,
    };
}

/// The bounded ledger.
#[derive(Debug)]
pub struct SkippedSecrets {
    names: CappedBuf,
    max_paths: usize,
    retained: usize,
    total: u64,
    closed: bool,
}

impl SkippedSecrets {
    /// An empty ledger. Allocates nothing.
    #[must_use]
    pub fn new(budget: SkippedBudget) -> Self {
        Self {
            names: CappedBuf::new(budget.max_bytes),
            max_paths: budget.max_paths,
            retained: 0,
            total: 0,
            closed: false,
        }
    }

    /// Count the skipped path `rel`, and keep it when it fits.
    pub fn record(&mut self, rel: &str) {
        let _ = rel;
        todo!("#5135 record")
    }

    /// Paths skipped in all.
    #[must_use]
    pub fn total(&self) -> u64 {
        todo!("#5135 total")
    }

    /// Paths retained.
    #[must_use]
    pub fn retained(&self) -> usize {
        todo!("#5135 retained")
    }

    /// Paths counted but not retained.
    #[must_use]
    pub fn omitted(&self) -> u64 {
        todo!("#5135 omitted")
    }

    /// The retained paths, in walk order.
    #[allow(unreachable_code)]
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        let (): () = todo!("#5135 paths");
        std::iter::empty()
    }

    /// The ledger's current allocation, in bytes.
    #[must_use]
    pub fn capacity(&self) -> usize {
        todo!("#5135 capacity")
    }

    /// The largest old-plus-new allocation a growth held at once.
    #[must_use]
    pub fn peak(&self) -> usize {
        todo!("#5135 peak")
    }

    /// The INFO line, or `None` when nothing was skipped:
    /// `Excluded N file(s) matching secret patterns: a, b` and, when some
    /// were not retained, `, and M more not listed`. Paths are shown escaped.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        todo!("#5135 summary")
    }
}

#[cfg(test)]
mod tests_skipped;
