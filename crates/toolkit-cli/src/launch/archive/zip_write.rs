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

use miniz_oxide::deflate::core::{
    compress_to_output, create_comp_flags_from_zip_params, CompressorOxide, TDEFLFlush, TDEFLStatus,
};
use unicode_normalization::char::is_combining_mark;

use super::crc32::crc32;
use super::{
    archive_size_message, entry_count_message, entry_size_message, ratio_message, total_message,
    ArchiveLimits,
};
use crate::launch::buffer::{BufError, CappedBuf};
use crate::launch::text::repr;
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

/// Version made by: Unix (3), specification 2.0.
const VERSION_MADE_BY: u16 = 0x0314;
/// Version needed for deflate.
const VERSION_NEEDED: u16 = 20;
/// Flag bit 11: UTF-8 names.
const FLAG_UTF8: u16 = 0x0800;
/// Method 8: deflate.
const METHOD_DEFLATE: u16 = 8;
/// The raw deflate encoding of an empty input: one final fixed-Huffman block
/// holding only the end-of-block code.
const EMPTY_DEFLATE: [u8; 2] = [0x03, 0x00];

/// Builds one archive in memory.
pub struct ZipWriter {
    buf: CappedBuf,
    written: Vec<Written>,
    limits: ArchiveLimits,
    total_uncompressed: u64,
    compressor: Option<Box<CompressorOxide>>,
    compressor_used: bool,
}

impl ZipWriter {
    /// An empty archive under `limits`.
    #[must_use]
    pub fn new(limits: ArchiveLimits) -> Self {
        let cap = usize::try_from(limits.max_archive_bytes).unwrap_or(usize::MAX);
        Self {
            buf: CappedBuf::new(cap),
            written: Vec::new(),
            limits,
            total_uncompressed: 0,
            compressor: None,
            compressor_used: false,
        }
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
        let count = self.written.len() as u64 + 1;
        if count > self.limits.max_entries {
            return Err(LaunchError::Archive(entry_count_message(
                count,
                &self.limits,
            )));
        }
        if size > self.limits.max_entry_uncompressed {
            return Err(LaunchError::Archive(entry_size_message(
                &repr(name),
                size,
                &self.limits,
            )));
        }
        Ok(())
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
        let size = data.len() as u64;
        self.check_next(name, size)?;
        let Ok(name_len) = u16::try_from(name.len()) else {
            return Err(LaunchError::Archive(format!(
                "Archive contains an unsafe entry path: {}",
                repr(name)
            )));
        };
        let crc = crc32(data);
        let (dos_time, dos_date) = dos_datetime(mtime_unix);
        let local_offset = self.offset32(self.buf.len())?;
        let mut header = Vec::with_capacity(30 + name.len());
        header.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        header.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
        header.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        header.extend_from_slice(&METHOD_DEFLATE.to_le_bytes());
        header.extend_from_slice(&dos_time.to_le_bytes());
        header.extend_from_slice(&dos_date.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&[0; 8]);
        header.extend_from_slice(&name_len.to_le_bytes());
        header.extend_from_slice(&0_u16.to_le_bytes());
        header.extend_from_slice(name.as_bytes());
        self.push(&header)?;
        let data_start = self.buf.len();
        self.deflate(data)?;
        let compressed = (self.buf.len() - data_start) as u64;
        if compressed > 0 && size > self.limits.max_ratio.saturating_mul(compressed) {
            return Err(LaunchError::Archive(ratio_message(
                &repr(name),
                &self.limits,
            )));
        }
        self.total_uncompressed = self.total_uncompressed.saturating_add(size);
        if self.total_uncompressed > self.limits.max_total_uncompressed {
            return Err(LaunchError::Archive(total_message(&self.limits)));
        }
        let compressed = self.size32(compressed)?;
        let uncompressed = self.size32(size)?;
        let at = local_offset as usize;
        self.buf.patch(at + 18, &compressed.to_le_bytes());
        self.buf.patch(at + 22, &uncompressed.to_le_bytes());
        self.written.push(Written {
            local_offset,
            name_offset: at + 30,
            name_len,
            crc,
            compressed,
            uncompressed,
            time: dos_time,
            date: dos_date,
            mode: zip_mode(mode),
        });
        Ok(())
    }

    /// Write the central directory and the end records.
    ///
    /// # Errors
    /// The archive-size message when they do not fit.
    pub fn finish(mut self) -> Result<CappedBuf, LaunchError> {
        let cd_offset = self.buf.len();
        let written = std::mem::take(&mut self.written);
        for entry in &written {
            let name = self.buf.as_slice()
                [entry.name_offset..entry.name_offset + usize::from(entry.name_len)]
                .to_vec();
            let mut record = Vec::with_capacity(46 + name.len());
            record.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
            record.extend_from_slice(&VERSION_MADE_BY.to_le_bytes());
            record.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
            record.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            record.extend_from_slice(&METHOD_DEFLATE.to_le_bytes());
            record.extend_from_slice(&entry.time.to_le_bytes());
            record.extend_from_slice(&entry.date.to_le_bytes());
            record.extend_from_slice(&entry.crc.to_le_bytes());
            record.extend_from_slice(&entry.compressed.to_le_bytes());
            record.extend_from_slice(&entry.uncompressed.to_le_bytes());
            record.extend_from_slice(&entry.name_len.to_le_bytes());
            record.extend_from_slice(&[0; 8]);
            record.extend_from_slice(&(entry.mode << 16).to_le_bytes());
            record.extend_from_slice(&entry.local_offset.to_le_bytes());
            record.extend_from_slice(&name);
            self.push(&record)?;
        }
        let cd_size = self.buf.len() - cd_offset;
        let count = written.len() as u64;
        let cd_offset32 = self.offset32(cd_offset)?;
        let cd_size32 = self.offset32(cd_size)?;
        let classic_count = if count >= ZIP64_ENTRY_THRESHOLD {
            let record_offset = self.buf.len() as u64;
            let mut end = Vec::with_capacity(76);
            end.extend_from_slice(&0x0606_4b50_u32.to_le_bytes());
            end.extend_from_slice(&44_u64.to_le_bytes());
            end.extend_from_slice(&0x032d_u16.to_le_bytes());
            end.extend_from_slice(&45_u16.to_le_bytes());
            end.extend_from_slice(&[0; 8]);
            end.extend_from_slice(&count.to_le_bytes());
            end.extend_from_slice(&count.to_le_bytes());
            end.extend_from_slice(&(cd_size as u64).to_le_bytes());
            end.extend_from_slice(&(cd_offset as u64).to_le_bytes());
            end.extend_from_slice(&0x0706_4b50_u32.to_le_bytes());
            end.extend_from_slice(&0_u32.to_le_bytes());
            end.extend_from_slice(&record_offset.to_le_bytes());
            end.extend_from_slice(&1_u32.to_le_bytes());
            self.push(&end)?;
            0xFFFF
        } else {
            u16::try_from(count).unwrap_or(0xFFFF)
        };
        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        eocd.extend_from_slice(&[0; 4]);
        eocd.extend_from_slice(&classic_count.to_le_bytes());
        eocd.extend_from_slice(&classic_count.to_le_bytes());
        eocd.extend_from_slice(&cd_size32.to_le_bytes());
        eocd.extend_from_slice(&cd_offset32.to_le_bytes());
        eocd.extend_from_slice(&0_u16.to_le_bytes());
        self.push(&eocd)?;
        Ok(self.buf)
    }

    fn size_error(limits: &ArchiveLimits, error: BufError) -> LaunchError {
        match error {
            BufError::Limit { attempted } => {
                LaunchError::Archive(archive_size_message(attempted as u64, limits))
            }
            BufError::Alloc => {
                LaunchError::Archive("There is not enough memory to build the archive.".to_string())
            }
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), LaunchError> {
        self.buf
            .extend(bytes)
            .map_err(|error| Self::size_error(&self.limits, error))
    }

    /// An offset or size that must fit a 32-bit field (always true under
    /// the 500 MiB cap; an injected larger cap is refused, not wrapped).
    fn offset32(&self, value: usize) -> Result<u32, LaunchError> {
        self.size32(value as u64)
    }

    fn size32(&self, value: u64) -> Result<u32, LaunchError> {
        u32::try_from(value)
            .ok()
            .filter(|value| *value != u32::MAX)
            .ok_or_else(|| LaunchError::Archive(archive_size_message(value, &self.limits)))
    }

    /// Raw deflate (level 6) of `data`, written straight into the archive
    /// buffer, so no per-entry copy is held.
    fn deflate(&mut self, data: &[u8]) -> Result<(), LaunchError> {
        if data.is_empty() {
            return self.push(&EMPTY_DEFLATE);
        }
        let Self {
            buf,
            limits,
            compressor,
            compressor_used,
            ..
        } = self;
        let compressor = compressor.get_or_insert_with(|| {
            Box::new(CompressorOxide::new(create_comp_flags_from_zip_params(
                6, -15, 0,
            )))
        });
        if *compressor_used {
            compressor.reset();
        }
        *compressor_used = true;
        let mut failure = None;
        let (status, consumed) = compress_to_output(compressor, data, TDEFLFlush::Finish, |out| {
            match buf.extend(out) {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        });
        if let Some(error) = failure {
            return Err(Self::size_error(limits, error));
        }
        if status != TDEFLStatus::Done || consumed != data.len() {
            return Err(LaunchError::Archive(
                "The archive could not be compressed.".to_string(),
            ));
        }
        Ok(())
    }
}

/// The DOS `(time, date)` of a Unix timestamp, in UTC, clamped to the DOS
/// range (the Python raised before 1980; recorded divergence D11).
#[must_use]
pub fn dos_datetime(mtime_unix: i64) -> (u16, u16) {
    let clamped = mtime_unix.clamp(DOS_EPOCH_UNIX, DOS_MAX_UNIX);
    let days = clamped.div_euclid(86_400);
    let seconds = clamped.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let field = |value: i64| u16::try_from(value).unwrap_or(0);
    let time =
        (field(seconds / 3600) << 11) | (field(seconds / 60 % 60) << 5) | field(seconds % 60 / 2);
    let date = (field(year - 1980) << 9) | (field(month) << 5) | field(day);
    (time, date)
}

/// The archived mode of a source file: `0o100755` when the owner may
/// execute it, else `0o100644`; never setuid, setgid, sticky or
/// world-writable.
#[must_use]
pub fn zip_mode(mode: u32) -> u32 {
    if mode & 0o100 != 0 {
        0o100_755
    } else {
        0o100_644
    }
}

/// Python `sanitize_root_name`: the last `/`- or `\`-separated segment of
/// the stripped name, leading dots removed, every character that is not a
/// Unicode word character, `.` or `-` replaced by `-`, `-` trimmed; `app`
/// when nothing is left.
#[must_use]
pub fn sanitize_root_name(name: &str) -> String {
    let base = name
        .trim()
        .split(['\\', '/'])
        .rfind(|segment| !segment.is_empty())
        .unwrap_or("")
        .trim()
        .trim_start_matches('.');
    let cleaned: String = base
        .chars()
        .map(|c| {
            if is_python_word_char(c) || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        "app".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Python `\w` for `str` patterns: `str.isalnum()` or `_`, approximated
/// without Unicode category tables as alphanumeric and not a combining mark.
#[must_use]
pub fn is_python_word_char(c: char) -> bool {
    c == '_' || (c.is_alphanumeric() && !is_combining_mark(c))
}

#[cfg(test)]
mod tests_write;
