//! The archive boundary (design section 5, slice L2).
//!
//! - [`select`] + [`git`] + [`fsops`]: the upload source walk (R-AR-SEL,
//!   R-AR-OPEN, R-AR-GIT), Unix only (D16);
//! - [`zip_write`]: the strict writer (R-AR-ZIP, ZIP64 per #5046);
//! - [`zip_read`]: the strict reader, both profiles (R-AR-READ,
//!   R-ZIP-FRAME), including the exact deflate decode of step 8;
//! - [`vcs`]: the version-control name policy of snapshots (R-AR-VCS);
//! - [`plan`]: the materialisation plan and node budget (R-AR-PLAN);
//! - [`casefold`]: Unicode simple case folding (generated table, #5132);
//! - [`skipped`]: the bounded ledger of skipped secret paths (#5135);
//! - [`crc32`]: CRC-32 (IEEE).
//!
//! Extraction, staging, apply and the Git metadata discovery (R-AR-EXT,
//! R-AR-STAGE, R-AR-APPLY, R-AR-META) are slice L6.

pub mod casefold;
pub mod crc32;
#[cfg(unix)]
pub mod extract;
#[cfg(unix)]
pub mod fsops;
#[cfg(unix)]
pub mod nav;
#[cfg(unix)]
pub mod git;
pub mod plan;
#[cfg(unix)]
pub mod select;
pub mod skipped;
pub mod vcs;
pub mod zip_read;
pub mod zip_write;

/// One MiB.
pub const MIB: u64 = 1024 * 1024;
/// One GiB.
pub const GIB: u64 = 1024 * MIB;

/// Archive limits (`launch_config.py:72-78`), shared by the writer and both
/// reader profiles. Tests inject smaller values; production uses
/// [`ArchiveLimits::PRODUCTION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct ArchiveLimits {
    /// Largest archive, compressed (500 MiB).
    pub max_archive_bytes: u64,
    /// Most entries (100 000).
    pub max_entries: u64,
    /// Largest total uncompressed size (2 GiB).
    pub max_total_uncompressed: u64,
    /// Largest uncompressed entry (100 MiB).
    pub max_entry_uncompressed: u64,
    /// Largest uncompressed : compressed ratio per entry (100).
    pub max_ratio: u64,
    /// Largest central directory accepted by the reader (64 MiB, R-ZIP-FRAME
    /// step 3).
    pub max_central_directory: u64,
}

impl ArchiveLimits {
    /// The Python limits.
    pub const PRODUCTION: Self = Self {
        max_archive_bytes: 500 * MIB,
        max_entries: 100_000,
        max_total_uncompressed: 2 * GIB,
        max_entry_uncompressed: 100 * MIB,
        max_ratio: 100,
        max_central_directory: 64 * MIB,
    };
}

/// The Python entry-count message.
#[must_use]
pub fn entry_count_message(count: u64, limits: &ArchiveLimits) -> String {
    format!(
        "Archive has {count} entries, exceeding the {} entry limit.",
        limits.max_entries
    )
}

/// The Python per-entry size message (`name` already shown through
/// [`super::text::repr`]).
#[must_use]
pub fn entry_size_message(quoted_name: &str, size: u64, limits: &ArchiveLimits) -> String {
    format!(
        "Archive entry {quoted_name} is {} MiB uncompressed, exceeding the {} MiB per-entry \
         limit.",
        size / MIB,
        limits.max_entry_uncompressed / MIB
    )
}

/// The Python ratio message.
#[must_use]
pub fn ratio_message(quoted_name: &str, limits: &ArchiveLimits) -> String {
    format!(
        "Archive entry {quoted_name} exceeds the {}:1 compression-ratio limit.",
        limits.max_ratio
    )
}

/// The Python total-size message.
#[must_use]
pub fn total_message(limits: &ArchiveLimits) -> String {
    format!(
        "Archive decompresses to over {} GiB, exceeding the decompressed-size limit.",
        limits.max_total_uncompressed / GIB
    )
}

/// The Python archive-size message.
#[must_use]
pub fn archive_size_message(size: u64, limits: &ArchiveLimits) -> String {
    format!(
        "Archive is {} MiB, exceeding the {} MiB limit. Remove large files or build artifacts \
         and try again.",
        size / MIB,
        limits.max_archive_bytes / MIB
    )
}

#[cfg(test)]
mod tests_zip64;
#[cfg(all(test, unix))]
pub mod tests_snapshot;
#[cfg(test)]
pub mod tests_zipfx;
