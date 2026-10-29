//! The strict ZIP reader (R-AR-READ, R-ZIP-FRAME), one reader with two
//! profiles over an in-memory archive of at most 500 MiB.
//!
//! [`read_archive`] runs the container framing (R-ZIP-FRAME steps 1-7) over
//! the whole input, then the entry policy (names, types, declared limits,
//! duplicates and file/directory conflicts under both profiles; the single
//! top-level directory for [`Profile::Upload`], the
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

use std::collections::{BTreeSet, HashSet};

use miniz_oxide::inflate::core::{decompress, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;
use unicode_normalization::UnicodeNormalization;

use super::casefold::simple_fold_str;
use super::crc32::Crc32;
use super::vcs::check_snapshot_name;
use super::{entry_count_message, entry_size_message, ratio_message, total_message, ArchiveLimits};
use crate::launch::text::repr;
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

fn frame(code: &str) -> LaunchError {
    LaunchError::Archive(frame_message(code))
}

/// Signatures.
const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_ZIP64_EOCD: u32 = 0x0606_4b50;
const SIG_ZIP64_LOCATOR: u32 = 0x0706_4b50;
const SIG_DESCRIPTOR: u32 = 0x0807_4b50;
/// Highest version needed accepted (4.5, ZIP64).
const MAX_VERSION_NEEDED: u16 = 45;
/// Flags allowed: deflate options (1, 2), data descriptor (3), UTF-8 (11).
const ALLOWED_FLAGS: u16 = 0x0002 | 0x0004 | 0x0008 | 0x0800;
const FLAG_DESCRIPTOR: u16 = 0x0008;
const FLAG_UTF8: u16 = 0x0800;
/// The longest name and segment (R-AR-READ).
const MAX_NAME_BYTES: usize = 4096;
const MAX_SEGMENT_BYTES: usize = 255;
/// Output piece size of the decoder (a power of two for the wrapping buffer).
const DECODE_BUFFER: usize = 64 * 1024;

/// Bounds-checked little-endian reads over the whole input.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn slice(&self, start: u64, len: u64) -> Result<&'a [u8], LaunchError> {
        let end = start.checked_add(len).ok_or_else(|| frame(code::BOUNDS))?;
        if end > self.len() {
            return Err(frame(code::BOUNDS));
        }
        let start = usize::try_from(start).map_err(|_| frame(code::BOUNDS))?;
        let end = usize::try_from(end).map_err(|_| frame(code::BOUNDS))?;
        Ok(&self.0[start..end])
    }

    fn u16(&self, at: u64) -> Result<u16, LaunchError> {
        let bytes = self.slice(at, 2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&self, at: u64) -> Result<u32, LaunchError> {
        let bytes = self.slice(at, 4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&self, at: u64) -> Result<u64, LaunchError> {
        let bytes = self.slice(at, 8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(array))
    }
}

fn add(a: u64, b: u64) -> Result<u64, LaunchError> {
    a.checked_add(b).ok_or_else(|| frame(code::BOUNDS))
}

/// The effective end-record values.
struct Trailer {
    count: u64,
    cd_size: u64,
    cd_offset: u64,
    cd_end: u64,
    zip64: bool,
}

/// Step 1: the one end record ending exactly at the input.
fn find_eocd(bytes: &Bytes<'_>) -> Result<u64, LaunchError> {
    let n = bytes.len();
    if n < 22 {
        return Err(frame(code::EOCD_MISSING));
    }
    let window_start = n.saturating_sub(22 + 65_535);
    let mut found = None;
    for p in window_start..=n - 22 {
        if bytes.u32(p)? == SIG_EOCD && p + 22 + u64::from(bytes.u16(p + 20)?) == n {
            if found.is_some() {
                return Err(frame(code::EOCD_AMBIGUOUS));
            }
            found = Some(p);
        }
    }
    found.ok_or_else(|| frame(code::EOCD_MISSING))
}

/// Step 2: classic fields, and the ZIP64 records exactly when a classic
/// field holds its sentinel.
fn end_values(bytes: &Bytes<'_>, p: u64) -> Result<Trailer, LaunchError> {
    let disk = bytes.u16(p + 4)?;
    let cd_disk = bytes.u16(p + 6)?;
    let count_disk = bytes.u16(p + 8)?;
    let count = bytes.u16(p + 10)?;
    let cd_size = bytes.u32(p + 12)?;
    let cd_offset = bytes.u32(p + 16)?;
    if disk != 0 || cd_disk != 0 || count_disk != count {
        return Err(frame(code::MULTI_DISK));
    }
    let needs_zip64 = count == 0xFFFF || cd_size == u32::MAX || cd_offset == u32::MAX;
    if !needs_zip64 {
        return Ok(Trailer {
            count: u64::from(count),
            cd_size: u64::from(cd_size),
            cd_offset: u64::from(cd_offset),
            cd_end: p,
            zip64: false,
        });
    }
    let inconsistent = || frame(code::ZIP64_INCONSISTENT);
    let locator = p.checked_sub(20).ok_or_else(inconsistent)?;
    if bytes.u32(locator)? != SIG_ZIP64_LOCATOR {
        return Err(inconsistent());
    }
    if bytes.u32(locator + 4)? != 0 || bytes.u32(locator + 16)? != 1 {
        return Err(frame(code::MULTI_DISK));
    }
    let q = bytes.u64(locator + 8)?;
    if q.checked_add(56) != Some(locator) {
        return Err(inconsistent());
    }
    if bytes.u32(q)? != SIG_ZIP64_EOCD
        || bytes.u64(q + 4)? != 44
        || bytes.u16(q + 14)? > MAX_VERSION_NEEDED
    {
        return Err(inconsistent());
    }
    if bytes.u32(q + 16)? != 0 || bytes.u32(q + 20)? != 0 {
        return Err(frame(code::MULTI_DISK));
    }
    let on_disk = bytes.u64(q + 24)?;
    let total = bytes.u64(q + 32)?;
    if on_disk != total {
        return Err(frame(code::MULTI_DISK));
    }
    let size64 = bytes.u64(q + 40)?;
    let offset64 = bytes.u64(q + 48)?;
    let field_ok = |classic: u64, sentinel: u64, value: u64| {
        if value >= sentinel {
            classic == sentinel
        } else {
            classic == value
        }
    };
    if !field_ok(u64::from(count), 0xFFFF, total)
        || !field_ok(u64::from(cd_size), u64::from(u32::MAX), size64)
        || !field_ok(u64::from(cd_offset), u64::from(u32::MAX), offset64)
    {
        return Err(inconsistent());
    }
    Ok(Trailer {
        count: total,
        cd_size: size64,
        cd_offset: offset64,
        cd_end: q,
        zip64: true,
    })
}

/// Step 5: an extra-field region parsed exactly; the ZIP64 item's data.
fn parse_extra(region: &[u8]) -> Result<Option<&[u8]>, LaunchError> {
    let mut pos = 0_usize;
    let mut ids = Vec::new();
    let mut zip64 = None;
    while pos < region.len() {
        if region.len() - pos < 4 {
            return Err(frame(code::EXTRA_FIELD));
        }
        let id = u16::from_le_bytes([region[pos], region[pos + 1]]);
        let size = usize::from(u16::from_le_bytes([region[pos + 2], region[pos + 3]]));
        let start = pos + 4;
        if size > region.len() - start {
            return Err(frame(code::EXTRA_FIELD));
        }
        if id == 0x7075 {
            return Err(frame(code::EXTRA_FIELD));
        }
        ids.push(id);
        if id == 0x0001 {
            zip64 = Some(&region[start..start + size]);
        }
        pos = start + size;
    }
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(frame(code::EXTRA_FIELD));
    }
    Ok(zip64)
}

fn le64(bytes: &[u8]) -> u64 {
    let mut array = [0_u8; 8];
    array.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(array)
}

/// One central record after steps 4 and 5.
struct Central {
    name_offset: u64,
    name_len: u64,
    host: u8,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u64,
    uncompressed: u64,
    local_offset: u64,
    external: u32,
    zip64_sizes: bool,
}

/// Step 4: exactly `count` records spanning the central directory.
fn central_records(
    bytes: &Bytes<'_>,
    end: &Trailer,
    stats: &mut ReadStats,
) -> Result<Vec<Central>, LaunchError> {
    let count = usize::try_from(end.count).map_err(|_| frame(code::BOUNDS))?;
    let mut records = Vec::with_capacity(count);
    let mut cursor = end.cd_offset;
    for _ in 0..count {
        if add(cursor, 46)? > end.cd_end || bytes.u32(cursor)? != SIG_CENTRAL {
            return Err(frame(code::CD_RECORD));
        }
        stats.central_records_read += 1;
        let made_by = bytes.u16(cursor + 4)?;
        let needed = bytes.u16(cursor + 6)?;
        let flags = bytes.u16(cursor + 8)?;
        let method = bytes.u16(cursor + 10)?;
        let crc = bytes.u32(cursor + 16)?;
        let compressed32 = bytes.u32(cursor + 20)?;
        let uncompressed32 = bytes.u32(cursor + 24)?;
        let name_len = u64::from(bytes.u16(cursor + 28)?);
        let extra_len = u64::from(bytes.u16(cursor + 30)?);
        let comment_len = u64::from(bytes.u16(cursor + 32)?);
        let disk = bytes.u16(cursor + 34)?;
        let external = bytes.u32(cursor + 38)?;
        let offset32 = bytes.u32(cursor + 42)?;
        let record_end = add(add(add(cursor, 46)?, name_len + extra_len)?, comment_len)?;
        if record_end > end.cd_end {
            return Err(frame(code::CD_RECORD));
        }
        let host = made_by.to_be_bytes()[0];
        if (host != 0 && host != 3) || needed > MAX_VERSION_NEEDED {
            return Err(frame(code::VERSION));
        }
        if flags & !ALLOWED_FLAGS != 0 || (method != 8 && flags & 0x0006 != 0) {
            return Err(frame(code::FLAGS));
        }
        if method != 0 && method != 8 {
            return Err(frame(code::METHOD));
        }
        if disk != 0 {
            return Err(frame(code::MULTI_DISK));
        }
        let extra = bytes.slice(cursor + 46 + name_len, extra_len)?;
        let zip64 = parse_extra(extra)?;
        let sentinels = [
            uncompressed32 == u32::MAX,
            compressed32 == u32::MAX,
            offset32 == u32::MAX,
        ];
        let wanted = sentinels.iter().filter(|set| **set).count();
        let mut values = match (wanted, zip64) {
            (0, None) => None,
            (wanted, Some(data)) if wanted > 0 && data.len() == 8 * wanted => {
                Some(data.chunks_exact(8).map(le64))
            }
            _ => return Err(frame(code::ZIP64_EXTRA)),
        };
        let mut resolve = |sentinel: bool, classic: u32| match (sentinel, values.as_mut()) {
            (true, Some(values)) => values.next().unwrap_or(u64::MAX),
            _ => u64::from(classic),
        };
        let uncompressed = resolve(sentinels[0], uncompressed32);
        let compressed = resolve(sentinels[1], compressed32);
        let local_offset = resolve(sentinels[2], offset32);
        if method == 0 && compressed != uncompressed {
            return Err(frame(code::STORED_SIZE));
        }
        records.push(Central {
            name_offset: cursor + 46,
            name_len,
            host,
            flags,
            method,
            crc,
            compressed,
            uncompressed,
            local_offset,
            external,
            zip64_sizes: sentinels[0] || sentinels[1],
        });
        cursor = record_end;
    }
    if cursor != end.cd_end {
        return Err(frame(code::CD_COUNT));
    }
    Ok(records)
}

/// Step 6: the local header (and descriptor) of one record; returns the
/// data offset and the end of the entry's byte range.
fn local_entry(
    bytes: &Bytes<'_>,
    record: &Central,
    cd_offset: u64,
) -> Result<(u64, u64), LaunchError> {
    let o = record.local_offset;
    if add(o, 30)? > cd_offset || bytes.u32(o)? != SIG_LOCAL {
        return Err(frame(code::LOCAL_HEADER));
    }
    if bytes.u16(o + 4)? > MAX_VERSION_NEEDED {
        return Err(frame(code::VERSION));
    }
    let flags = bytes.u16(o + 6)?;
    let method = bytes.u16(o + 8)?;
    let crc = bytes.u32(o + 14)?;
    let compressed32 = bytes.u32(o + 18)?;
    let uncompressed32 = bytes.u32(o + 22)?;
    let name_len = u64::from(bytes.u16(o + 26)?);
    let extra_len = u64::from(bytes.u16(o + 28)?);
    let data_offset = add(add(o + 30, name_len)?, extra_len)?;
    if data_offset > cd_offset {
        return Err(frame(code::LOCAL_HEADER));
    }
    if flags != record.flags || method != record.method {
        return Err(frame(code::LOCAL_MISMATCH));
    }
    if bytes.slice(o + 30, name_len)? != bytes.slice(record.name_offset, record.name_len)? {
        return Err(frame(code::LOCAL_MISMATCH));
    }
    let zip64 = parse_extra(bytes.slice(o + 30 + name_len, extra_len)?)?;
    let both_sentinel = compressed32 == u32::MAX && uncompressed32 == u32::MAX;
    let (local_uncompressed, local_compressed) = match (both_sentinel, zip64) {
        (false, None) => (u64::from(uncompressed32), u64::from(compressed32)),
        (true, Some(data)) if data.len() == 16 => (le64(&data[..8]), le64(&data[8..])),
        _ => return Err(frame(code::ZIP64_EXTRA)),
    };
    let data_end = add(data_offset, record.compressed)?;
    if data_end > cd_offset {
        return Err(frame(code::BOUNDS));
    }
    if record.flags & FLAG_DESCRIPTOR == 0 {
        if crc != record.crc
            || local_compressed != record.compressed
            || local_uncompressed != record.uncompressed
        {
            return Err(frame(code::LOCAL_MISMATCH));
        }
        return Ok((data_offset, data_end));
    }
    if crc != 0 || local_compressed != 0 || local_uncompressed != 0 {
        return Err(frame(code::DESCRIPTOR));
    }
    let wide = record.zip64_sizes;
    let descriptor_len = if wide { 24 } else { 16 };
    let entry_end = add(data_end, descriptor_len)?;
    if entry_end > cd_offset {
        return Err(frame(code::DESCRIPTOR));
    }
    let (d_compressed, d_uncompressed) = if wide {
        (bytes.u64(data_end + 8)?, bytes.u64(data_end + 16)?)
    } else {
        (
            u64::from(bytes.u32(data_end + 8)?),
            u64::from(bytes.u32(data_end + 12)?),
        )
    };
    if bytes.u32(data_end)? != SIG_DESCRIPTOR
        || bytes.u32(data_end + 4)? != record.crc
        || d_compressed != record.compressed
        || d_uncompressed != record.uncompressed
    {
        return Err(frame(code::DESCRIPTOR));
    }
    Ok((data_offset, entry_end))
}

/// Step 7: the entries tile the data before the central directory.
fn check_tiling(ranges: &mut [(u64, u64)], cd_offset: u64) -> Result<(), LaunchError> {
    ranges.sort_unstable();
    let mut expected = 0_u64;
    for (start, end) in ranges.iter() {
        if *start < expected {
            return Err(frame(code::OVERLAP));
        }
        if *start > expected {
            return Err(frame(code::LAYOUT));
        }
        expected = *end;
    }
    if expected != cd_offset {
        return Err(frame(code::LAYOUT));
    }
    Ok(())
}

fn unsafe_path(name: &str) -> LaunchError {
    LaunchError::Archive(format!(
        "Archive contains an unsafe entry path: {}",
        repr(name)
    ))
}

/// R-AR-READ name rules; `name` keeps a directory's final `/`.
pub(crate) fn check_name(name: &str) -> Result<(), LaunchError> {
    let body = name.strip_suffix('/').unwrap_or(name);
    let drive = {
        let bytes = name.as_bytes();
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
    };
    let bad = name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.chars().any(char::is_control)
        || name.contains('\\')
        || name.starts_with('/')
        || drive
        || body.split('/').any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.len() > MAX_SEGMENT_BYTES
        });
    if bad {
        return Err(unsafe_path(name));
    }
    Ok(())
}

/// The comparison key of R-AR-READ's duplicate rule: NFC, then Unicode
/// simple case folding (CaseFolding.txt C and S, [`simple_fold_str`]; #5132),
/// then NFC again, with the directory marker removed. `ß` and `ẞ` share a key;
/// `ß` and `ss`, and `ı` and `i`, do not.
fn fold_key(name: &str) -> String {
    let body = name.strip_suffix('/').unwrap_or(name);
    let nfc: String = body.nfc().collect();
    simple_fold_str(&nfc).nfc().collect()
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
    let bytes = Bytes(data);
    let p = find_eocd(&bytes)?;
    let end = end_values(&bytes, p)?;
    if end.count == 0 {
        return Err(frame(code::EMPTY));
    }
    if end.count > limits.max_entries {
        return Err(LaunchError::Archive(entry_count_message(end.count, limits)));
    }
    if end.cd_size > limits.max_central_directory {
        return Err(frame(code::LAYOUT));
    }
    if end.cd_size < 46 * end.count {
        return Err(frame(code::CD_COUNT));
    }
    if add(end.cd_offset, end.cd_size)? != end.cd_end {
        return Err(frame(code::LAYOUT));
    }
    let records = central_records(&bytes, &end, stats)?;
    let mut ranges = Vec::with_capacity(records.len());
    let mut entries = Vec::with_capacity(records.len());
    for record in &records {
        let (data_offset, entry_end) = local_entry(&bytes, record, end.cd_offset)?;
        ranges.push((record.local_offset, entry_end));
        entries.push((record, data_offset));
    }
    check_tiling(&mut ranges, end.cd_offset)?;
    let mut out = Vec::with_capacity(entries.len());
    let mut total = 0_u64;
    for (record, data_offset) in entries {
        out.push(check_entry(
            &bytes,
            record,
            data_offset,
            profile,
            limits,
            &mut total,
        )?);
    }
    let archive = Archive {
        data,
        entries: out,
        zip64: end.zip64,
    };
    if profile == Profile::Upload {
        check_single_root(&archive)?;
    }
    // R-AR-READ duplicates and file/directory conflicts under both profiles (#5504).
    check_collisions(&archive)?;
    Ok(archive)
}

/// The entry policy of one record.
fn check_entry(
    bytes: &Bytes<'_>,
    record: &Central,
    data_offset: u64,
    profile: Profile,
    limits: &ArchiveLimits,
    total: &mut u64,
) -> Result<Entry, LaunchError> {
    let raw = bytes.slice(record.name_offset, record.name_len)?;
    let not_utf8 = || {
        LaunchError::Archive("Archive contains an entry name that is not valid UTF-8.".to_string())
    };
    if record.flags & FLAG_UTF8 == 0 && !raw.is_ascii() {
        return Err(not_utf8());
    }
    let name = std::str::from_utf8(raw).map_err(|_| not_utf8())?;
    check_name(name)?;
    if profile == Profile::Snapshot {
        check_snapshot_name(name)?;
    }
    let is_dir_name = name.ends_with('/');
    let unix = (record.host == 3).then_some(record.external >> 16);
    let file_type = unix.map_or(0, |mode| mode & 0o170_000);
    match file_type {
        0 => {
            if record.external & 0x10 != 0 && !is_dir_name {
                return Err(frame(code::DIRECTORY));
            }
        }
        0o100_000 if is_dir_name => return Err(frame(code::DIRECTORY)),
        0o040_000 if !is_dir_name => return Err(frame(code::DIRECTORY)),
        0o100_000 | 0o040_000 => {}
        0o120_000 => {
            return Err(LaunchError::Archive(format!(
                "Archive contains a symlink entry: {}",
                repr(name)
            )))
        }
        _ => {
            return Err(LaunchError::Archive(format!(
                "Archive contains an entry that is not a regular file or directory: {}",
                repr(name)
            )))
        }
    }
    let kind = if is_dir_name {
        if record.uncompressed != 0
            || record.crc != 0
            || (record.method == 0 && record.compressed != 0)
        {
            return Err(frame(code::DIRECTORY));
        }
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    if record.uncompressed > limits.max_entry_uncompressed {
        return Err(LaunchError::Archive(entry_size_message(
            &repr(name),
            record.uncompressed,
            limits,
        )));
    }
    let ratio_exceeded = if record.compressed == 0 {
        record.uncompressed != 0
    } else {
        record.uncompressed > limits.max_ratio.saturating_mul(record.compressed)
    };
    if ratio_exceeded {
        return Err(LaunchError::Archive(ratio_message(&repr(name), limits)));
    }
    *total = total.saturating_add(record.uncompressed);
    if *total > limits.max_total_uncompressed {
        return Err(LaunchError::Archive(total_message(limits)));
    }
    Ok(Entry {
        name_offset: usize::try_from(record.name_offset).map_err(|_| frame(code::BOUNDS))?,
        name_len: raw.len(),
        kind,
        method: record.method,
        flags: record.flags,
        crc: record.crc,
        compressed: record.compressed,
        uncompressed: record.uncompressed,
        local_offset: record.local_offset,
        data_offset,
        unix_mode: unix,
    })
}

/// Upload profile: one top-level directory (Python `validate_archive_bytes`).
fn check_single_root(archive: &Archive<'_>) -> Result<(), LaunchError> {
    let roots: HashSet<&str> = archive
        .entries()
        .iter()
        .filter_map(|entry| archive.name(entry).split('/').next())
        .filter(|top| !top.is_empty())
        .collect();
    if roots.len() > 1 {
        return Err(LaunchError::Archive(format!(
            "Archive must contain a single top-level directory; found {}.",
            roots.len()
        )));
    }
    Ok(())
}

/// Both profiles: no two entries equal after NFC and case folding, and no
/// file whose folded name is a proper path prefix of another entry.
fn check_collisions(archive: &Archive<'_>) -> Result<(), LaunchError> {
    let mut keys = BTreeSet::new();
    let mut files = Vec::new();
    for entry in archive.entries() {
        let name = archive.name(entry);
        let key = fold_key(name);
        if !keys.insert(key.clone()) {
            return Err(LaunchError::Archive(format!(
                "Archive contains entries whose names collide after case folding or Unicode \
                 normalisation: {}",
                repr(name)
            )));
        }
        if entry.kind == EntryKind::File {
            files.push((key, name));
        }
    }
    for (key, name) in files {
        let prefix = format!("{key}/");
        if keys
            .range(prefix.clone()..)
            .next()
            .is_some_and(|next| next.starts_with(&prefix))
        {
            return Err(LaunchError::Archive(format!(
                "Archive contains a file that another entry uses as a directory: {}",
                repr(name)
            )));
        }
    }
    Ok(())
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
    let span = Bytes(archive.data).slice(entry.data_offset, entry.compressed)?;
    let mut crc = Crc32::new();
    let mut produced_total = 0_u64;
    let mut emit = |piece: &[u8]| -> Result<(), LaunchError> {
        let next = produced_total + piece.len() as u64;
        if next > entry.uncompressed {
            return Err(frame(code::SIZE_MISMATCH));
        }
        produced_total = next;
        crc.update(piece);
        sink(piece)
    };
    match entry.method {
        0 => {
            for piece in span.chunks(DECODE_BUFFER) {
                emit(piece)?;
            }
        }
        _ => inflate_exact(span, &mut emit)?,
    }
    if produced_total != entry.uncompressed {
        return Err(frame(code::SIZE_MISMATCH));
    }
    if crc.finish() != entry.crc {
        return Err(frame(code::CRC_MISMATCH));
    }
    Ok(())
}

/// Raw inflate of exactly `span` through a 64 KiB wrapping buffer.
fn inflate_exact(
    span: &[u8],
    emit: &mut dyn FnMut(&[u8]) -> Result<(), LaunchError>,
) -> Result<(), LaunchError> {
    let mut decompressor = Box::<DecompressorOxide>::default();
    let mut out = vec![0_u8; DECODE_BUFFER];
    let mut in_pos = 0_usize;
    let mut out_pos = 0_usize;
    loop {
        let (status, consumed, produced) =
            decompress(&mut decompressor, &span[in_pos..], &mut out, out_pos, 0);
        in_pos += consumed;
        if produced > 0 {
            emit(&out[out_pos..out_pos + produced])?;
        }
        out_pos = (out_pos + produced) & (DECODE_BUFFER - 1);
        match status {
            TINFLStatus::Done => break,
            TINFLStatus::HasMoreOutput => {}
            TINFLStatus::NeedsMoreInput | TINFLStatus::FailedCannotMakeProgress => {
                return Err(frame(code::TRUNCATED_STREAM))
            }
            _ => return Err(frame(code::DEFLATE)),
        }
    }
    if in_pos != span.len() {
        return Err(frame(code::TRAILING_DATA));
    }
    Ok(())
}

#[cfg(test)]
mod tests_frame;
#[cfg(test)]
mod tests_policy;
