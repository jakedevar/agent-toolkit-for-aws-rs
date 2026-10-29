//! The strict ZIP writer for the upload (R-AR-ZIP; ZIP64 per the PM
//! amendment, #5046).
//!
//! Layout, exactly the shape R-ZIP-FRAME requires of the reader:
//!
//! - per entry: a local header (signature, version needed 20, flag bit 11
//!   (UTF-8), method 8, DOS time and date, CRC-32, compressed and
//!   uncompressed sizes, name, no extra field) immediately followed by the
//!   raw deflate data (level 6, `miniz_oxide`). No data descriptor (flag
//!   bit 3 is never set) and no gap;
//! - the central directory: one record per entry in the same order, version
//!   made by Unix 2.0 (`0x0314`), `external_attr = mode << 16` with `mode`
//!   either `0o100644` or `0o100755`;
//! - only when the entry count is 65 535 or more: a ZIP64 end-of-central-
//!   directory record (44-byte body, version 4.5) and its locator, with the
//!   classic count fields set to the `0xFFFF` sentinel; the central size and
//!   offset always fit 32 bits (the archive is at most 500 MiB), so they are
//!   real in both records and no per-entry ZIP64 extra is ever written;
//! - the classic end-of-central-directory record with an empty comment.
//!
//! Limits are enforced while building, with the Python messages: the entry
//! count, the per-entry size (also before reading, from the walked size), the
//! per-entry ratio after compressing, the running uncompressed total and the
//! running archive size. The archive bytes live in a [`CappedBuf`] limited to
//! the archive cap (#5082): the allocation never exceeds 500 MiB and a growth
//! never holds more than old plus new below twice that.

use miniz_oxide::deflate::core::CompressorOxide;

use super::ArchiveLimits;
use crate::launch::buffer::CappedBuf;
use crate::launch::LaunchError;

/// The DOS timestamp floor, 1980-01-01 00:00:00 UTC.
pub const DOS_EPOCH_UNIX: i64 = 315_532_800;
/// The DOS timestamp ceiling, 2107-12-31 23:59:58 UTC.
pub const DOS_MAX_UNIX: i64 = 4_354_819_198;
/// The entry count from which the writer adds the ZIP64 records.
pub const ZIP64_ENTRY_THRESHOLD: u64 = 0xFFFF;

/// One central-directory record to be written.
#[derive(Debug, Clone, Copy)]
struct Written {
    local_offset: u32,
    name_offset: usize,
    name_len: u16,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    time: u16,
    date: u16,
    mode: u32,
}

/// Builds one archive in memory.
pub struct ZipWriter {
    buf: CappedBuf,
    written: Vec<Written>,
    limits: ArchiveLimits,
    total_uncompressed: u64,
    compressor: Option<Box<CompressorOxide>>,
}

impl ZipWriter {
    /// An empty archive under `limits`.
    #[must_use]
    pub fn new(limits: ArchiveLimits) -> Self {
        let _ = limits;
        todo!("L2: writer")
    }

    /// Entries added so far.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.written.len()
    }

    /// The archive buffer, for the #5082 allocation assertions.
    #[must_use]
    pub fn buffer(&self) -> &CappedBuf {
        &self.buf
    }

    /// Refuse an entry before its bytes are read: the entry count and the
    /// per-entry size from the walked size.
    ///
    /// # Errors
    /// The Python entry-count or per-entry size message.
    pub fn check_next(&self, name: &str, size: u64) -> Result<(), LaunchError> {
        let _ = (name, size);
        todo!("L2: pre-read limits")
    }

    /// Add one regular file.
    ///
    /// # Errors
    /// Any limit's Python message; the archive is then unusable.
    pub fn add_file(
        &mut self,
        name: &str,
        data: &[u8],
        mode: u32,
        mtime_unix: i64,
    ) -> Result<(), LaunchError> {
        let _ = (name, data, mode, mtime_unix);
        todo!("L2: add_file")
    }

    /// Write the central directory and the end records.
    ///
    /// # Errors
    /// The archive-size message when they do not fit.
    pub fn finish(self) -> Result<CappedBuf, LaunchError> {
        todo!("L2: finish")
    }
}

/// The DOS `(time, date)` of a Unix timestamp, in UTC, clamped to the DOS
/// range (the Python raised before 1980; recorded divergence D11).
#[must_use]
pub fn dos_datetime(mtime_unix: i64) -> (u16, u16) {
    let _ = mtime_unix;
    todo!("L2: dos time")
}

/// The archived mode of a source file: `0o100755` when the owner may
/// execute it, else `0o100644`; never setuid, setgid, sticky or
/// world-writable.
#[must_use]
pub fn zip_mode(mode: u32) -> u32 {
    let _ = mode;
    todo!("L2: mode")
}

/// Python `sanitize_root_name`: the last `/`- or `\`-separated segment of
/// the stripped name, leading dots removed, every character that is not a
/// Unicode word character, `.` or `-` replaced by `-`, `-` trimmed; `app`
/// when nothing is left.
#[must_use]
pub fn sanitize_root_name(name: &str) -> String {
    let _ = name;
    todo!("L2: sanitize_root_name")
}

/// Python `\w` for `str` patterns: `str.isalnum()` or `_`, approximated
/// without Unicode category tables as alphanumeric and not a combining mark.
#[must_use]
pub fn is_python_word_char(c: char) -> bool {
    let _ = c;
    todo!("L2: \\w")
}

#[cfg(test)]
mod tests_write;
