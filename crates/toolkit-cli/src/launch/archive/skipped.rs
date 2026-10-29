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

use std::fmt::Write as _;

use crate::launch::buffer::CappedBuf;
use crate::launch::text::shown;

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
        self.total = self.total.saturating_add(1);
        if self.closed {
            return;
        }
        // One separator byte per path; a file name never holds NUL.
        if self.retained >= self.max_paths
            || self.names.extend_parts(&[rel.as_bytes(), &[0]]).is_err()
        {
            // Past the count or byte cap, or refused by the allocator: this
            // and every later path are counted only.
            self.closed = true;
            return;
        }
        self.retained += 1;
    }

    /// Paths skipped in all.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Paths retained.
    #[must_use]
    pub fn retained(&self) -> usize {
        self.retained
    }

    /// Paths counted but not retained.
    #[must_use]
    pub fn omitted(&self) -> u64 {
        self.total - self.retained as u64
    }

    /// The retained paths, in walk order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.names
            .as_slice()
            .split(|byte| *byte == 0)
            .take(self.retained)
            .map(|path| std::str::from_utf8(path).unwrap_or_default())
    }

    /// The ledger's current allocation, in bytes.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.names.capacity()
    }

    /// The largest old-plus-new allocation a growth held at once.
    #[must_use]
    pub fn peak(&self) -> usize {
        self.names.peak()
    }

    /// The INFO line, or `None` when nothing was skipped:
    /// `Excluded N file(s) matching secret patterns: a, b` and, when some
    /// were not retained, `, and M more not listed` (`M not listed` when
    /// none was). Paths are shown escaped ([`shown`]).
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        if self.total == 0 {
            return None;
        }
        let mut out = format!("Excluded {} file(s) matching secret patterns: ", self.total);
        for (index, path) in self.paths().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&shown(path));
        }
        let omitted = self.omitted();
        if omitted > 0 {
            let _ = if self.retained == 0 {
                write!(out, "{omitted} not listed")
            } else {
                write!(out, ", and {omitted} more not listed")
            };
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests_skipped;
