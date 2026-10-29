//! Test-only ZIP fixtures built byte by byte (R-ZIP-FRAME's "all fixtures
//! are byte-built in the test"). [`Fx`] lays out entries, the central
//! directory and the end records with optional structural variations; the
//! tests then patch single fields through [`Built`]'s recorded offsets.

use crate::launch::archive::crc32::crc32;

/// Little-endian `u16` at `offset`.
pub fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

/// Little-endian `u32` at `offset`.
pub fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Little-endian `u64` at `offset`.
pub fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Raw deflate (level 6) of `data`.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(data, 6)
}

/// One extra-field item.
pub fn extra_item(id: u16, data: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    out.extend_from_slice(&u16::try_from(data.len()).expect("small").to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// One entry of a fixture.
#[derive(Clone, Debug)]
pub struct FxEntry {
    pub name: Vec<u8>,
    /// The bytes in the data span (already compressed for method 8).
    pub payload: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub uncompressed: u64,
    pub made_by: u16,
    pub needed: u16,
    pub external: u32,
    pub central_extra: Vec<u8>,
    pub local_extra: Vec<u8>,
    /// Write the central sizes and offset as sentinels with a ZIP64 extra.
    pub zip64_central: bool,
    /// Write the local sizes as sentinels with a local ZIP64 extra.
    pub zip64_local: bool,
    /// Bytes after the data span (a data descriptor).
    pub descriptor: Vec<u8>,
    /// Bytes inserted before this entry's local header.
    pub gap_before: Vec<u8>,
    pub comment: Vec<u8>,
}

impl FxEntry {
    /// A stored file.
    pub fn stored(name: &str, content: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            payload: content.to_vec(),
            method: 0,
            flags: 0x0800,
            crc: crc32(content),
            uncompressed: content.len() as u64,
            made_by: 0x0314,
            needed: 20,
            external: 0o100_644 << 16,
            central_extra: Vec::new(),
            local_extra: Vec::new(),
            zip64_central: false,
            zip64_local: false,
            descriptor: Vec::new(),
            gap_before: Vec::new(),
            comment: Vec::new(),
        }
    }

    /// A deflated file.
    pub fn deflated(name: &str, content: &[u8]) -> Self {
        Self {
            payload: deflate(content),
            method: 8,
            ..Self::stored(name, content)
        }
    }

    /// A stored directory entry.
    pub fn dir(name: &str) -> Self {
        Self {
            external: (0o040_755 << 16) | 0x10,
            ..Self::stored(name, b"")
        }
    }

    /// The compressed size.
    pub fn compressed(&self) -> u64 {
        self.payload.len() as u64
    }

    /// Use a data descriptor (flag bit 3), local CRC and sizes zero.
    pub fn with_descriptor(mut self) -> Self {
        self.flags |= 0x0008;
        let mut descriptor = 0x0807_4b50_u32.to_le_bytes().to_vec();
        descriptor.extend_from_slice(&self.crc.to_le_bytes());
        if self.zip64_central {
            descriptor.extend_from_slice(&self.compressed().to_le_bytes());
            descriptor.extend_from_slice(&self.uncompressed.to_le_bytes());
        } else {
            descriptor.extend_from_slice(
                &u32::try_from(self.compressed())
                    .expect("small")
                    .to_le_bytes(),
            );
            descriptor.extend_from_slice(
                &u32::try_from(self.uncompressed)
                    .expect("small")
                    .to_le_bytes(),
            );
        }
        self.descriptor = descriptor;
        self
    }
}

/// End-record choices.
#[derive(Clone, Debug, Default)]
pub struct EndFx {
    /// Write the ZIP64 record and locator.
    pub zip64: bool,
    /// Put the count sentinel in the classic record (default: when zip64).
    pub count_sentinel: Option<bool>,
    /// Put the size sentinel in the classic record.
    pub size_sentinel: bool,
    /// Put the offset sentinel in the classic record.
    pub offset_sentinel: bool,
    /// The archive comment.
    pub comment: Vec<u8>,
}

/// A whole fixture.
#[derive(Clone, Debug, Default)]
pub struct Fx {
    pub entries: Vec<FxEntry>,
    /// Bytes after the last entry, before the central directory.
    pub before_cd: Vec<u8>,
    /// Bytes after the central records, before the end records.
    pub after_cd: Vec<u8>,
    /// Extra central records duplicating entry `i` (pointing at its header).
    pub duplicate_central: Vec<usize>,
    /// Extra central records for `(entry, local offset)` pairs that have no
    /// local header of their own in the entry list.
    pub extra_central: Vec<(FxEntry, usize)>,
    pub end: EndFx,
}

/// A built fixture with the offsets the tests patch.
#[derive(Clone, Debug)]
pub struct Built {
    pub bytes: Vec<u8>,
    pub local: Vec<usize>,
    pub central: Vec<usize>,
    pub cd_offset: usize,
    pub cd_end: usize,
    pub zip64_record: Option<usize>,
    pub locator: Option<usize>,
    pub eocd: usize,
}

impl Fx {
    /// A fixture of `entries` with default end records.
    pub fn of(entries: Vec<FxEntry>) -> Self {
        Self {
            entries,
            ..Self::default()
        }
    }

    /// Lay the fixture out.
    #[allow(clippy::too_many_lines)]
    pub fn build(&self) -> Built {
        let mut bytes = Vec::new();
        let mut local = Vec::new();
        for entry in &self.entries {
            bytes.extend_from_slice(&entry.gap_before);
            local.push(bytes.len());
            let descriptor_used = entry.flags & 0x0008 != 0;
            bytes.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
            bytes.extend_from_slice(&entry.needed.to_le_bytes());
            bytes.extend_from_slice(&entry.flags.to_le_bytes());
            bytes.extend_from_slice(&entry.method.to_le_bytes());
            bytes.extend_from_slice(&0_u16.to_le_bytes());
            bytes.extend_from_slice(&0x0021_u16.to_le_bytes());
            let mut local_extra = entry.local_extra.clone();
            if descriptor_used {
                bytes.extend_from_slice(&[0; 12]);
            } else if entry.zip64_local {
                bytes.extend_from_slice(&entry.crc.to_le_bytes());
                bytes.extend_from_slice(&u32::MAX.to_le_bytes());
                bytes.extend_from_slice(&u32::MAX.to_le_bytes());
                let mut data = entry.uncompressed.to_le_bytes().to_vec();
                data.extend_from_slice(&entry.compressed().to_le_bytes());
                local_extra.extend_from_slice(&extra_item(1, &data));
            } else {
                bytes.extend_from_slice(&entry.crc.to_le_bytes());
                bytes.extend_from_slice(
                    &u32::try_from(entry.compressed())
                        .expect("small")
                        .to_le_bytes(),
                );
                bytes.extend_from_slice(
                    &u32::try_from(entry.uncompressed)
                        .expect("small")
                        .to_le_bytes(),
                );
            }
            bytes.extend_from_slice(&u16::try_from(entry.name.len()).expect("name").to_le_bytes());
            bytes.extend_from_slice(
                &u16::try_from(local_extra.len())
                    .expect("extra")
                    .to_le_bytes(),
            );
            bytes.extend_from_slice(&entry.name);
            bytes.extend_from_slice(&local_extra);
            bytes.extend_from_slice(&entry.payload);
            bytes.extend_from_slice(&entry.descriptor);
        }
        bytes.extend_from_slice(&self.before_cd);
        let cd_offset = bytes.len();
        let mut central = Vec::new();
        let mut order: Vec<(&FxEntry, usize)> = (0..self.entries.len())
            .chain(self.duplicate_central.iter().copied())
            .map(|index| (&self.entries[index], local[index]))
            .collect();
        order.extend(
            self.extra_central
                .iter()
                .map(|(entry, offset)| (entry, *offset)),
        );
        for (entry, local_offset) in order {
            central.push(bytes.len());
            let mut extra = Vec::new();
            bytes.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
            bytes.extend_from_slice(&entry.made_by.to_le_bytes());
            bytes.extend_from_slice(&entry.needed.to_le_bytes());
            bytes.extend_from_slice(&entry.flags.to_le_bytes());
            bytes.extend_from_slice(&entry.method.to_le_bytes());
            bytes.extend_from_slice(&0_u16.to_le_bytes());
            bytes.extend_from_slice(&0x0021_u16.to_le_bytes());
            bytes.extend_from_slice(&entry.crc.to_le_bytes());
            if entry.zip64_central {
                bytes.extend_from_slice(&u32::MAX.to_le_bytes());
                bytes.extend_from_slice(&u32::MAX.to_le_bytes());
                let mut data = entry.uncompressed.to_le_bytes().to_vec();
                data.extend_from_slice(&entry.compressed().to_le_bytes());
                data.extend_from_slice(&(local_offset as u64).to_le_bytes());
                extra.extend_from_slice(&extra_item(1, &data));
            } else {
                bytes.extend_from_slice(
                    &u32::try_from(entry.compressed())
                        .expect("small")
                        .to_le_bytes(),
                );
                bytes.extend_from_slice(
                    &u32::try_from(entry.uncompressed)
                        .expect("small")
                        .to_le_bytes(),
                );
            }
            extra.extend_from_slice(&entry.central_extra);
            bytes.extend_from_slice(&u16::try_from(entry.name.len()).expect("name").to_le_bytes());
            bytes.extend_from_slice(&u16::try_from(extra.len()).expect("extra").to_le_bytes());
            bytes.extend_from_slice(
                &u16::try_from(entry.comment.len())
                    .expect("comment")
                    .to_le_bytes(),
            );
            bytes.extend_from_slice(&0_u16.to_le_bytes());
            bytes.extend_from_slice(&0_u16.to_le_bytes());
            bytes.extend_from_slice(&entry.external.to_le_bytes());
            let offset = if entry.zip64_central {
                u32::MAX
            } else {
                u32::try_from(local_offset).expect("small")
            };
            bytes.extend_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&entry.name);
            bytes.extend_from_slice(&extra);
            bytes.extend_from_slice(&entry.comment);
        }
        bytes.extend_from_slice(&self.after_cd);
        let cd_end = bytes.len();
        let count = central.len() as u64;
        let cd_size = (cd_end - cd_offset) as u64;
        let mut zip64_record = None;
        let mut locator = None;
        if self.end.zip64 {
            zip64_record = Some(bytes.len());
            bytes.extend_from_slice(&0x0606_4b50_u32.to_le_bytes());
            bytes.extend_from_slice(&44_u64.to_le_bytes());
            bytes.extend_from_slice(&0x032d_u16.to_le_bytes());
            bytes.extend_from_slice(&45_u16.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&cd_size.to_le_bytes());
            bytes.extend_from_slice(&(cd_offset as u64).to_le_bytes());
            locator = Some(bytes.len());
            bytes.extend_from_slice(&0x0706_4b50_u32.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&(zip64_record.expect("set") as u64).to_le_bytes());
            bytes.extend_from_slice(&1_u32.to_le_bytes());
        }
        let eocd = bytes.len();
        let count_sentinel = self.end.count_sentinel.unwrap_or(self.end.zip64);
        let classic_count = if count_sentinel {
            0xFFFF
        } else {
            u16::try_from(count).unwrap_or(0xFFFF)
        };
        bytes.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&classic_count.to_le_bytes());
        bytes.extend_from_slice(&classic_count.to_le_bytes());
        let size = if self.end.size_sentinel {
            u32::MAX
        } else {
            u32::try_from(cd_size).expect("small")
        };
        let offset = if self.end.offset_sentinel {
            u32::MAX
        } else {
            u32::try_from(cd_offset).expect("small")
        };
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(
            &u16::try_from(self.end.comment.len())
                .expect("comment")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&self.end.comment);
        Built {
            bytes,
            local,
            central,
            cd_offset,
            cd_end,
            zip64_record,
            locator,
            eocd,
        }
    }
}

/// One `n`-entry archive of tiny stored files `d/<i>` (fast to build for
/// the ZIP64 count boundaries).
pub fn many_stored(count: usize, prefix: &str) -> Fx {
    let entries = (0..count)
        .map(|index| FxEntry::stored(&format!("{prefix}/{index}"), b""))
        .collect();
    let zip64 = count >= 0xFFFF;
    Fx {
        entries,
        end: EndFx {
            zip64,
            ..EndFx::default()
        },
        ..Fx::default()
    }
}

/// `len` bytes of deterministic, incompressible noise (a 64-bit LCG), so
/// declared-size fixtures stay under the 100:1 ratio rule.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state.to_be_bytes()[0]
        })
        .collect()
}
