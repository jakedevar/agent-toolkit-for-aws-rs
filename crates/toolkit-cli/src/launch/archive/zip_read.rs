//! The strict ZIP reader (R-AR-READ, R-ZIP-FRAME), one reader with two
//! profiles over an in-memory archive of at most 500 MiB.
//!
//! [`read_archive`] runs the container framing (R-ZIP-FRAME steps 1-7) over
//! the whole input, then the entry policy (names, types, declared limits,
//! duplicates; the single top-level directory for [`Profile::Upload`], the
//! version-control denylist of R-AR-VCS for [`Profile::Snapshot`]). Nothing
//! is decoded. [`decode_entry`] is step 8, the exact consumption of one
//! entry's data, used by extraction (L6) and by the tests on writer output.
//!
//! Every framing failure is `The archive is not a valid ZIP file (<code>).`
//! with one of the fixed codes below; entry failures name the entry through
//! the shown (redacted, bounded, escaped) Python `repr`.
//!
//! All offsets and lengths are `u64` with checked sums; every slice is
//! bounds-checked first; nothing is allocated from a declared value before
//! the value has passed its limit.

use super::ArchiveLimits;
use crate::launch::LaunchError;

/// The framing failure codes of R-ZIP-FRAME.
pub mod code {
    /// Input shorter than 22 bytes, or no end record ending at the input.
    pub const EOCD_MISSING: &str = "eocd_missing";
    /// More than one end record ends exactly at the input.
    pub const EOCD_AMBIGUOUS: &str = "eocd_ambiguous";
    /// Any disk field other than a single disk.
    pub const MULTI_DISK: &str = "multi_disk";
    /// ZIP64 records missing, misplaced, malformed or inconsistent.
    pub const ZIP64_INCONSISTENT: &str = "zip64_inconsistent";
    /// Zero entries.
    pub const EMPTY: &str = "empty";
    /// A gap, a prepended stub or misplaced structures.
    pub const LAYOUT: &str = "layout";
    /// A central record that is truncated, overruns or has a bad signature.
    pub const CD_RECORD: &str = "cd_record";
    /// The declared count does not match the records.
    pub const CD_COUNT: &str = "cd_count";
    /// Unsupported host or version needed.
    pub const VERSION: &str = "version";
    /// A general-purpose flag outside the allowlist.
    pub const FLAGS: &str = "flags";
    /// A compression method other than stored or deflate.
    pub const METHOD: &str = "method";
    /// A stored entry whose two sizes differ.
    pub const STORED_SIZE: &str = "stored_size";
    /// An extra-field region that does not parse exactly.
    pub const EXTRA_FIELD: &str = "extra_field";
    /// A ZIP64 extra field present or absent against the sentinels.
    pub const ZIP64_EXTRA: &str = "zip64_extra";
    /// A local header that is missing, misplaced or overruns.
    pub const LOCAL_HEADER: &str = "local_header";
    /// A local header that differs from its central record.
    pub const LOCAL_MISMATCH: &str = "local_mismatch";
    /// A data descriptor that is missing or differs.
    pub const DESCRIPTOR: &str = "descriptor";
    /// Two entries sharing bytes.
    pub const OVERLAP: &str = "overlap";
    /// A deflate stream that ends before its compressed span does.
    pub const TRAILING_DATA: &str = "trailing_data";
    /// A deflate stream that needs more input than its span.
    pub const TRUNCATED_STREAM: &str = "truncated_stream";
    /// An invalid deflate stream.
    pub const DEFLATE: &str = "deflate";
    /// Decoded output longer or shorter than declared.
    pub const SIZE_MISMATCH: &str = "size_mismatch";
    /// Decoded output whose CRC-32 differs.
    pub const CRC_MISMATCH: &str = "crc_mismatch";
    /// An arithmetic overflow or an out-of-range offset.
    pub const BOUNDS: &str = "bounds";
    /// A directory entry whose name, mode, size or CRC disagree.
    pub const DIRECTORY: &str = "directory";
}

/// Which input the reader checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The writer's own bytes before upload: single top-level directory.
    Upload,
    /// A downloaded snapshot (hostile): no single-root rule, R-AR-VCS.
    Snapshot,
}

/// An entry's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory (name ends in exactly one `/`).
    Directory,
}

/// One validated entry; every range is inside the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Offset of the name bytes in the archive (the central copy).
    pub name_offset: usize,
    /// Length of the name in bytes, including a directory's final `/`.
    pub name_len: usize,
    /// File or directory.
    pub kind: EntryKind,
    /// 0 (stored) or 8 (deflate).
    pub method: u16,
    /// The general-purpose flags.
    pub flags: u16,
    /// Central CRC-32.
    pub crc: u32,
    /// Resolved compressed size.
    pub compressed: u64,
    /// Resolved uncompressed size.
    pub uncompressed: u64,
    /// Resolved local-header offset.
    pub local_offset: u64,
    /// Start of the data in the archive.
    pub data_offset: u64,
    /// The Unix mode for host 3, else `None`.
    pub unix_mode: Option<u32>,
}

/// A validated archive borrowing its bytes.
#[derive(Debug)]
pub struct Archive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
    zip64: bool,
}

impl<'a> Archive<'a> {
    /// The entries in central-directory order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The archive bytes.
    #[must_use]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Whether the ZIP64 end records were used.
    #[must_use]
    pub fn uses_zip64(&self) -> bool {
        self.zip64
    }

    /// An entry's name (validated UTF-8).
    #[must_use]
    pub fn name(&self, entry: &Entry) -> &'a str {
        let bytes = &self.data[entry.name_offset..entry.name_offset + entry.name_len];
        std::str::from_utf8(bytes).unwrap_or("")
    }
}

/// Counters for the "nothing read before the limit" assertions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReadStats {
    /// Central-directory records parsed.
    pub central_records_read: usize,
}

/// The fixed framing message.
#[must_use]
pub fn frame_message(code: &str) -> String {
    format!("The archive is not a valid ZIP file ({code}).")
}

/// Validate `data` under `profile` and `limits`.
///
/// # Errors
/// [`LaunchError::Archive`] with the first failure found.
pub fn read_archive<'a>(
    data: &'a [u8],
    profile: Profile,
    limits: &ArchiveLimits,
    stats: &mut ReadStats,
) -> Result<Archive<'a>, LaunchError> {
    let _ = (data, profile, limits, stats);
    todo!("L2: reader")
}

/// R-ZIP-FRAME step 8: decode `entry` exactly, passing the output to `sink`
/// in pieces of at most 64 KiB, checking the declared size as output is
/// produced and the CRC-32 at the end.
///
/// # Errors
/// `trailing_data`, `truncated_stream`, `deflate`, `size_mismatch` or
/// `crc_mismatch`; or the sink's own error.
pub fn decode_entry(
    archive: &Archive<'_>,
    entry: &Entry,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), LaunchError>,
) -> Result<(), LaunchError> {
    let _ = (archive, entry, sink);
    todo!("L2: decode")
}

#[cfg(test)]
mod tests_frame;
#[cfg(test)]
mod tests_policy;
