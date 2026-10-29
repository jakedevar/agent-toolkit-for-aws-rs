//! T-ZIP-FRAME-1..34: the R-ZIP-FRAME invariant table, byte-built fixtures.

use super::*;
use crate::launch::archive::tests_zipfx::{
    deflate, extra_item, noise, put16, put32, put64, Built, EndFx, Fx, FxEntry,
};

const LIMITS: ArchiveLimits = ArchiveLimits::PRODUCTION;

fn read(bytes: &[u8]) -> Result<Archive<'_>, LaunchError> {
    read_archive(bytes, Profile::Snapshot, &LIMITS, &mut ReadStats::default())
}

fn message(result: Result<impl std::fmt::Debug, LaunchError>) -> String {
    match result {
        Err(LaunchError::Archive(message)) => message,
        other => panic!("expected an ArchiveError, got {other:?}"),
    }
}

#[track_caller]
fn assert_code(bytes: &[u8], code: &str) {
    assert_eq!(message(read(bytes)), frame_message(code));
}

/// Decode every entry, returning their contents.
fn decode_all(bytes: &[u8]) -> Result<Vec<Vec<u8>>, LaunchError> {
    let archive = read(bytes)?;
    let mut out = Vec::new();
    for entry in archive.entries() {
        let mut content = Vec::new();
        decode_entry(&archive, entry, &mut |piece| {
            content.extend_from_slice(piece);
            Ok(())
        })?;
        out.push(content);
    }
    Ok(out)
}

#[track_caller]
fn assert_decode_code(bytes: &[u8], code: &str) {
    assert!(
        read(bytes).is_ok(),
        "framing accepts it; only the decode refuses"
    );
    assert_eq!(message(decode_all(bytes)), frame_message(code));
}

const LONG_A: &str = "a-rather-long-entry-name-number-one.txt";
const LONG_B: &str = "a-rather-long-entry-name-number-two.txt";

fn two() -> Fx {
    Fx::of(vec![
        FxEntry::stored(LONG_A, b"hello"),
        FxEntry::deflated(LONG_B, b"hello hello hello hello hello"),
    ])
}

fn two_built() -> Built {
    two().build()
}

#[test]
fn baseline_fixture_reads_and_decodes() {
    let built = two_built();
    let archive = read(&built.bytes).expect("valid");
    assert_eq!(archive.entries().len(), 2);
    assert_eq!(archive.name(&archive.entries()[0]), LONG_A);
    assert!(!archive.uses_zip64());
    assert_eq!(
        decode_all(&built.bytes).expect("decodes"),
        vec![b"hello".to_vec(), b"hello hello hello hello hello".to_vec()]
    );
}

#[test]
fn t_zip_frame_01_short_input_and_missing_end_record() {
    assert_code(b"", code::EOCD_MISSING);
    assert_code(&[0x50, 0x4b, 0x05, 0x06], code::EOCD_MISSING);
    assert_code(&[0_u8; 21], code::EOCD_MISSING);
    let mut built = two_built();
    built.bytes[built.eocd] = b'X';
    assert_code(&built.bytes, code::EOCD_MISSING);
}

#[test]
fn t_zip_frame_02_end_record_beyond_the_search_window() {
    let mut fx = two();
    fx.end.comment = vec![b'c'; 65_535];
    let built = fx.build();
    read(&built.bytes).expect("the longest comment is inside the window");
    let mut bytes = two_built().bytes;
    bytes.extend_from_slice(&vec![0_u8; 65_536]);
    assert_code(&bytes, code::EOCD_MISSING);
}

#[test]
fn t_zip_frame_03_forged_end_record_in_the_comment() {
    let base = two_built();
    let forged = base.bytes[base.eocd..].to_vec();
    assert_eq!(forged.len(), 22);
    let mut fx = two();
    fx.end.comment = forged;
    assert_code(&fx.build().bytes, code::EOCD_AMBIGUOUS);
}

#[test]
fn t_zip_frame_04_comment_length_one_short_or_long() {
    let mut fx = two();
    fx.end.comment = b"comment".to_vec();
    let built = fx.build();
    for wrong in [6_u16, 8] {
        let mut bytes = built.bytes.clone();
        put16(&mut bytes, built.eocd + 20, wrong);
        assert_code(&bytes, code::EOCD_MISSING);
    }
}

#[test]
fn t_zip_frame_05_disk_fields() {
    let built = two_built();
    for (offset, value) in [(4, 1_u16), (6, 1), (8, 1)] {
        let mut bytes = built.bytes.clone();
        put16(&mut bytes, built.eocd + offset, value);
        assert_code(&bytes, code::MULTI_DISK);
    }
}

#[test]
fn t_zip_frame_06_zero_entries() {
    assert_code(&Fx::of(Vec::new()).build().bytes, code::EMPTY);
}

#[test]
fn t_zip_frame_07_count_over_the_limit_reads_no_record() {
    let mut fx = two();
    fx.end.zip64 = true;
    let mut built = fx.build();
    let record = built.zip64_record.expect("zip64");
    put64(&mut built.bytes, record + 24, 100_001);
    put64(&mut built.bytes, record + 32, 100_001);
    let mut stats = ReadStats::default();
    let result = read_archive(&built.bytes, Profile::Snapshot, &LIMITS, &mut stats);
    assert_eq!(
        message(result),
        "Archive has 100001 entries, exceeding the 100000 entry limit."
    );
    assert_eq!(stats.central_records_read, 0);
}

#[test]
fn t_zip_frame_08_central_size_limits_before_any_record() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.eocd + 12, 64 * 1024 * 1024 + 1);
    let mut stats = ReadStats::default();
    let result = read_archive(&bytes, Profile::Snapshot, &LIMITS, &mut stats);
    assert_eq!(message(result), frame_message(code::LAYOUT));
    assert_eq!(stats.central_records_read, 0);
    // 2 records declared as 3 cannot fit 3 * 46 bytes when the names are short.
    let short = Fx::of(vec![FxEntry::stored("a", b"1"), FxEntry::stored("b", b"2")]).build();
    let mut bytes = short.bytes.clone();
    put16(&mut bytes, short.eocd + 8, 3);
    put16(&mut bytes, short.eocd + 10, 3);
    let mut stats = ReadStats::default();
    let result = read_archive(&bytes, Profile::Snapshot, &LIMITS, &mut stats);
    assert_eq!(message(result), frame_message(code::CD_COUNT));
    assert_eq!(stats.central_records_read, 0);
}

#[test]
fn t_zip_frame_09_offsets_past_the_input() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.eocd + 16, 0xFFFF_FFF0);
    assert_code(&bytes, code::LAYOUT);
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.eocd + 12, 0xFFFF_FFFF);
    put32(&mut bytes, built.eocd + 16, 0xFFFF_FFFF);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
}

#[test]
fn t_zip_frame_10_gap_between_central_directory_and_end() {
    let mut fx = two();
    fx.after_cd = vec![0; 4];
    let built = fx.build();
    // Counted in the central size: trailing bytes after the last record.
    assert_code(&built.bytes, code::CD_COUNT);
    // Not counted: the central directory does not end at the end record.
    let mut bytes = built.bytes.clone();
    let size = u32::try_from(built.cd_end - built.cd_offset - 4).expect("small");
    put32(&mut bytes, built.eocd + 12, size);
    assert_code(&bytes, code::LAYOUT);
}

#[test]
fn t_zip_frame_11_declared_count_larger_than_present() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.eocd + 8, 3);
    put16(&mut bytes, built.eocd + 10, 3);
    assert_code(&bytes, code::CD_RECORD);
}

#[test]
fn t_zip_frame_12_declared_count_smaller_than_present() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.eocd + 8, 1);
    put16(&mut bytes, built.eocd + 10, 1);
    assert_code(&bytes, code::CD_COUNT);
}

#[test]
fn t_zip_frame_13_variable_lengths_overrunning_the_central_directory() {
    let built = two_built();
    let last = built.central[1];
    for field in [28, 30, 32] {
        let mut bytes = built.bytes.clone();
        put16(&mut bytes, last + field, 200);
        assert_code(&bytes, code::CD_RECORD);
    }
}

#[test]
fn t_zip_frame_14_wrong_signature_inside_the_central_directory() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.central[1], 0x0201_4b51);
    assert_code(&bytes, code::CD_RECORD);
}

#[test]
fn t_zip_frame_15_host_and_version_needed() {
    let built = two_built();
    for made_by in [0x0A14_u16, 0x0B14, 0x1314] {
        let mut bytes = built.bytes.clone();
        put16(&mut bytes, built.central[0] + 4, made_by);
        assert_code(&bytes, code::VERSION);
    }
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.central[0] + 6, 46);
    assert_code(&bytes, code::VERSION);
    // MS-DOS host is accepted (the name decides the type).
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.central[0] + 4, 0x0014);
    put32(&mut bytes, built.central[0] + 38, 0);
    read(&bytes).expect("host 0");
}

#[test]
fn t_zip_frame_16_refused_flag_bits() {
    let built = two_built();
    for bit in [0_u16, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15] {
        let mut bytes = built.bytes.clone();
        put16(&mut bytes, built.central[1] + 8, 0x0800 | (1 << bit));
        assert_code(&bytes, code::FLAGS);
    }
    // Bits 1 and 2 are deflate options: allowed on method 8, refused on stored.
    for options in [0x0002_u16, 0x0004, 0x0006] {
        let mut fx = two();
        fx.entries[1].flags |= options;
        read(&fx.build().bytes).expect("deflate options on a deflate entry");
        let mut fx = two();
        fx.entries[0].flags |= options;
        assert_code(&fx.build().bytes, code::FLAGS);
    }
}

#[test]
fn t_zip_frame_17_methods_other_than_stored_and_deflate() {
    for method in [1_u16, 9, 12, 14, 93, 99] {
        let mut fx = two();
        fx.entries[1].method = method;
        assert_code(&fx.build().bytes, code::METHOD);
    }
}

#[test]
fn t_zip_frame_18_extra_field_region_framing() {
    let cases: [Vec<u8>; 5] = [
        vec![0x99],
        vec![0x99, 0x99, 0x01],
        {
            let mut item = extra_item(0x5455, &[1, 2]);
            put16(&mut item, 2, 10);
            item
        },
        [extra_item(0x5455, &[1]), extra_item(0x5455, &[2])].concat(),
        extra_item(0x7075, b"\x01\x00\x00\x00\x00other-name"),
    ];
    for extra in cases {
        let mut fx = two();
        fx.entries[0].central_extra = extra.clone();
        assert_code(&fx.build().bytes, code::EXTRA_FIELD);
        let mut fx = two();
        fx.entries[0].local_extra = extra;
        assert_code(&fx.build().bytes, code::EXTRA_FIELD);
    }
    // Unknown ids are skipped uninterpreted.
    let mut fx = two();
    fx.entries[0].central_extra =
        [extra_item(0x5455, &[1, 2, 3]), extra_item(0x000a, &[0; 8])].concat();
    fx.entries[0].local_extra = extra_item(0x5455, &[9]);
    read(&fx.build().bytes).expect("unknown ids skipped");
}

#[test]
fn t_zip_frame_19_central_zip64_extra_rules() {
    // A valid ZIP64 central extra (all three sentinels) is accepted.
    let mut fx = two();
    fx.entries[1].zip64_central = true;
    let built = fx.build();
    assert_eq!(
        decode_all(&built.bytes).expect("decodes")[1],
        b"hello hello hello hello hello"
    );
    // Sentinel without the extra.
    let plain = two_built();
    let mut bytes = plain.bytes.clone();
    put32(&mut bytes, plain.central[0] + 20, u32::MAX);
    assert_code(&bytes, code::ZIP64_EXTRA);
    // The extra without any sentinel.
    let mut fx = two();
    fx.entries[0].central_extra = extra_item(1, &5_u64.to_le_bytes());
    assert_code(&fx.build().bytes, code::ZIP64_EXTRA);
    // Wrong length: two sentinels, three values.
    let mut bytes = built.bytes.clone();
    let offset_field = built.central[1] + 42;
    put32(
        &mut bytes,
        offset_field,
        u32::try_from(built.local[1]).expect("small"),
    );
    assert_code(&bytes, code::ZIP64_EXTRA);
    // The disk-number sentinel: single disk only.
    let mut bytes = plain.bytes.clone();
    put16(&mut bytes, plain.central[0] + 34, 0xFFFF);
    assert_code(&bytes, code::MULTI_DISK);
}

#[test]
fn t_zip_frame_20_local_offset_outside_the_entry_area() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put32(
        &mut bytes,
        built.central[1] + 42,
        u32::try_from(built.cd_offset).expect("small"),
    );
    assert_code(&bytes, code::LOCAL_HEADER);
    let mut bytes = built.bytes.clone();
    put32(
        &mut bytes,
        built.central[1] + 42,
        u32::try_from(built.cd_offset - 10).expect("small"),
    );
    assert_code(&bytes, code::LOCAL_HEADER);
    // A ZIP64 offset whose `o + 30` overflows.
    let mut fx = two();
    fx.entries[1].zip64_central = true;
    let built = fx.build();
    let mut bytes = built.bytes.clone();
    let name_len = LONG_B.len();
    let offset_value = built.central[1] + 46 + name_len + 4 + 16;
    put64(&mut bytes, offset_value, u64::MAX - 10);
    assert_code(&bytes, code::BOUNDS);
}

#[test]
fn t_zip_frame_21_wrong_local_signature() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.local[1], 0x0403_4b51);
    assert_code(&bytes, code::LOCAL_HEADER);
}

#[test]
fn t_zip_frame_22_local_name_method_or_flags_differ() {
    let built = two_built();
    let mut bytes = built.bytes.clone();
    bytes[built.local[0] + 30] = b'A';
    assert_code(&bytes, code::LOCAL_MISMATCH);
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.local[1] + 8, 0);
    assert_code(&bytes, code::LOCAL_MISMATCH);
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.local[1] + 6, 0x0802);
    assert_code(&bytes, code::LOCAL_MISMATCH);
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.local[1] + 4, 46);
    assert_code(&bytes, code::VERSION);
}

#[test]
fn t_zip_frame_23_local_crc_and_sizes_differ_without_descriptor() {
    let built = two_built();
    for field in [14, 18, 22] {
        let mut bytes = built.bytes.clone();
        let value = u32::from_le_bytes(
            bytes[built.local[1] + field..built.local[1] + field + 4]
                .try_into()
                .expect("4"),
        );
        put32(&mut bytes, built.local[1] + field, value ^ 1);
        assert_code(&bytes, code::LOCAL_MISMATCH);
    }
}

#[test]
fn t_zip_frame_24_local_zip64_extra_rules() {
    // A valid local ZIP64 extra (both local sizes sentinel) is accepted.
    let mut fx = two();
    fx.entries[1].zip64_local = true;
    assert_eq!(decode_all(&fx.build().bytes).expect("decodes").len(), 2);
    // A local ZIP64 extra carrying an offset (24 bytes).
    let mut fx = two();
    fx.entries[0].local_extra = extra_item(1, &[0; 24]);
    assert_code(&fx.build().bytes, code::ZIP64_EXTRA);
    // A local ZIP64 extra without the sentinels.
    let mut fx = two();
    let mut data = 5_u64.to_le_bytes().to_vec();
    data.extend_from_slice(&5_u64.to_le_bytes());
    fx.entries[0].local_extra = extra_item(1, &data);
    assert_code(&fx.build().bytes, code::ZIP64_EXTRA);
    // Sentinels with an 8-byte extra.
    let mut fx = two();
    fx.entries[1].zip64_local = true;
    let built = fx.build();
    let mut bytes = built.bytes.clone();
    let name_len = LONG_B.len();
    put16(&mut bytes, built.local[1] + 30 + name_len + 2, 8);
    assert!(read(&bytes).is_err());
}

#[test]
fn t_zip_frame_25_data_descriptors() {
    let mut fx = two();
    fx.entries[1] = fx.entries[1].clone().with_descriptor();
    let built = fx.build();
    let archive = read(&built.bytes).expect("a valid descriptor is accepted");
    assert_eq!(archive.entries()[1].compressed, fx.entries[1].compressed());
    assert_eq!(
        decode_all(&built.bytes).expect("decodes")[1],
        b"hello hello hello hello hello"
    );
    // Non-zero local fields with bit 3.
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.local[1] + 14, 1);
    assert_code(&bytes, code::DESCRIPTOR);
    // Missing signature (the unsigned form is refused).
    let descriptor = built.cd_offset - 16;
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, descriptor, 0);
    assert_code(&bytes, code::DESCRIPTOR);
    // Values that differ from the central record.
    for field in [4, 8, 12] {
        let mut bytes = built.bytes.clone();
        bytes[descriptor + field] ^= 1;
        assert_code(&bytes, code::DESCRIPTOR);
    }
    // 8-byte sizes on an entry without ZIP64 sizes.
    let mut fx = two();
    let entry = fx.entries[1].clone().with_descriptor();
    let mut wide = 0x0807_4b50_u32.to_le_bytes().to_vec();
    wide.extend_from_slice(&entry.crc.to_le_bytes());
    wide.extend_from_slice(&entry.compressed().to_le_bytes());
    wide.extend_from_slice(&entry.uncompressed.to_le_bytes());
    fx.entries[1] = FxEntry {
        descriptor: wide,
        ..entry
    };
    assert!(read(&fx.build().bytes).is_err());
}

#[test]
fn t_zip_frame_26_prepended_stub() {
    let mut fx = two();
    fx.entries[0].gap_before = b"#!/bin/sh\nexit 0\n".to_vec();
    assert_code(&fx.build().bytes, code::LAYOUT);
}

#[test]
fn t_zip_frame_27_gap_between_entries() {
    let mut fx = two();
    fx.entries[1].gap_before = vec![0; 3];
    assert_code(&fx.build().bytes, code::LAYOUT);
}

#[test]
fn t_zip_frame_28_shared_and_overlapping_entries() {
    let mut fx = two();
    fx.duplicate_central = vec![0];
    assert_code(&fx.build().bytes, code::OVERLAP);
    // The "quoted" overlap: B's local header sits inside A's stored data.
    let inner = Fx::of(vec![FxEntry::stored("b", b"x")]).build();
    let b_local = inner.bytes[..inner.cd_offset].to_vec();
    let outer_entry = FxEntry::stored("a", &b_local);
    let mut fx = Fx::of(vec![outer_entry]);
    fx.extra_central = vec![(FxEntry::stored("b", b"x"), 30 + 1)];
    assert_code(&fx.build().bytes, code::OVERLAP);
}

#[test]
fn t_zip_frame_29_last_entry_ending_before_the_central_directory() {
    let mut fx = two();
    fx.before_cd = b"hidden".to_vec();
    assert_code(&fx.build().bytes, code::LAYOUT);
}

#[test]
fn t_zip_frame_30_stored_sizes_differ() {
    let mut fx = two();
    fx.entries[0].uncompressed = 6;
    assert_code(&fx.build().bytes, code::STORED_SIZE);
}

#[test]
fn t_zip_frame_31_deflate_ending_early_with_padding() {
    let content = vec![b'z'; 1000];
    let mut entry = FxEntry::deflated("padded.txt", &content);
    entry.payload.extend_from_slice(&[0; 64]);
    assert_decode_code(&Fx::of(vec![entry]).build().bytes, code::TRAILING_DATA);
}

#[test]
fn t_zip_frame_32_deflate_truncated() {
    let content: Vec<u8> = (0..4000_u32)
        .map(|i| u8::try_from(i % 251).expect("byte"))
        .collect();
    let mut entry = FxEntry::deflated("cut.txt", &content);
    let cut = entry.payload.len() - 3;
    entry.payload.truncate(cut);
    assert_decode_code(&Fx::of(vec![entry]).build().bytes, code::TRUNCATED_STREAM);
    let mut entry = FxEntry::deflated("garbage.txt", &content);
    entry.payload = vec![0xff; 40];
    assert!(decode_all(&Fx::of(vec![entry]).build().bytes).is_err());
}

#[test]
fn t_zip_frame_33_output_size_and_crc() {
    let content = noise(5000, 33);
    // One byte longer than declared: decoding stops at declared + 1.
    let mut entry = FxEntry::deflated("long.txt", &content);
    entry.uncompressed = 4999;
    let bytes = Fx::of(vec![entry]).build().bytes;
    let archive = read(&bytes).expect("framing");
    let mut delivered = 0_usize;
    let result = decode_entry(&archive, &archive.entries()[0], &mut |piece| {
        delivered += piece.len();
        Ok(())
    });
    assert_eq!(message(result), frame_message(code::SIZE_MISMATCH));
    assert!(
        delivered <= 4999,
        "never more than the declared size reaches the sink"
    );
    // Shorter than declared.
    let mut entry = FxEntry::deflated("short.txt", &content);
    entry.uncompressed = 5001;
    assert_decode_code(&Fx::of(vec![entry]).build().bytes, code::SIZE_MISMATCH);
    // CRC mismatch, deflate and stored.
    let mut entry = FxEntry::deflated("crc.txt", &content);
    entry.crc ^= 1;
    assert_decode_code(&Fx::of(vec![entry]).build().bytes, code::CRC_MISMATCH);
    let mut entry = FxEntry::stored("crc.bin", b"abc");
    entry.crc ^= 1;
    assert_decode_code(&Fx::of(vec![entry]).build().bytes, code::CRC_MISMATCH);
    // The sink sees pieces of at most 64 KiB.
    let big = noise(300_000, 34);
    let bytes = Fx::of(vec![FxEntry::deflated("big.bin", &big)])
        .build()
        .bytes;
    let archive = read(&bytes).expect("framing");
    let mut largest = 0;
    let mut total = 0;
    decode_entry(&archive, &archive.entries()[0], &mut |piece| {
        largest = largest.max(piece.len());
        total += piece.len();
        Ok(())
    })
    .expect("decodes");
    assert_eq!(total, 300_000);
    assert_eq!(
        crate::launch::archive::crc32::crc32(&big),
        archive.entries()[0].crc
    );
    assert!(largest <= 64 * 1024);
}

#[test]
fn t_zip_frame_34_directory_syntax_and_modes() {
    // Accepted: stored and deflate-empty directories, with files below.
    let mut deflate_dir = FxEntry::dir("b/");
    deflate_dir.method = 8;
    deflate_dir.payload = deflate(b"");
    let fx = Fx::of(vec![
        FxEntry::dir("a/"),
        deflate_dir,
        FxEntry::stored("a/x.txt", b"x"),
        FxEntry::deflated("b/y.txt", b"yyyy"),
    ]);
    let built = fx.build();
    let archive = read(&built.bytes).expect("valid directories");
    let kinds: Vec<EntryKind> = archive.entries().iter().map(|entry| entry.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EntryKind::Directory,
            EntryKind::Directory,
            EntryKind::File,
            EntryKind::File
        ]
    );
    assert_eq!(
        decode_all(&built.bytes).expect("decodes"),
        vec![Vec::new(), Vec::new(), b"x".to_vec(), b"yyyy".to_vec()]
    );
    // Refused names.
    for name in ["a//", "/", "//"] {
        let bytes = Fx::of(vec![FxEntry::dir(name)]).build().bytes;
        assert!(
            message(read(&bytes)).starts_with("Archive contains an unsafe entry path: "),
            "{name}"
        );
    }
    // A directory with content, or a non-zero CRC.
    let mut sized = FxEntry::stored("d/", b"data");
    sized.external = (0o040_755 << 16) | 0x10;
    assert_code(&Fx::of(vec![sized]).build().bytes, code::DIRECTORY);
    let mut crc = FxEntry::dir("d/");
    crc.crc = 1;
    assert_code(&Fx::of(vec![crc]).build().bytes, code::DIRECTORY);
    // S_IFDIR without `/`, S_IFREG with `/`, DOS directory bit without `/`.
    let mut no_slash = FxEntry::stored("d", b"");
    no_slash.external = 0o040_755 << 16;
    assert_code(&Fx::of(vec![no_slash]).build().bytes, code::DIRECTORY);
    let mut reg_slash = FxEntry::stored("d/", b"");
    reg_slash.external = 0o100_644 << 16;
    assert_code(&Fx::of(vec![reg_slash]).build().bytes, code::DIRECTORY);
    let mut dos = FxEntry::stored("d", b"");
    dos.made_by = 0x0014;
    dos.external = 0x10;
    assert_code(&Fx::of(vec![dos]).build().bytes, code::DIRECTORY);
    // Host 0 with a trailing `/` is a directory; zero type bits too.
    let mut dos_dir = FxEntry::dir("e/");
    dos_dir.made_by = 0x0014;
    dos_dir.external = 0x10;
    let mut untyped = FxEntry::dir("f/");
    untyped.external = 0;
    let archive_bytes = Fx::of(vec![dos_dir, untyped]).build().bytes;
    let archive = read(&archive_bytes).expect("name decides");
    assert!(archive
        .entries()
        .iter()
        .all(|entry| entry.kind == EntryKind::Directory));
    // File/directory conflicts.
    let conflict = Fx::of(vec![
        FxEntry::stored("a", b"1"),
        FxEntry::stored("a/b", b"2"),
    ])
    .build();
    assert!(message(read(&conflict.bytes)).contains("'a"));
    let dup = Fx::of(vec![FxEntry::stored("a", b"1"), FxEntry::dir("a/")]).build();
    assert!(read(&dup.bytes).is_err());
    let reversed = Fx::of(vec![
        FxEntry::stored("a/b", b"2"),
        FxEntry::stored("a", b"1"),
    ])
    .build();
    assert!(read(&reversed.bytes).is_err());
}

#[test]
fn every_byte_belongs_to_one_structure() {
    // The exact-span rule: the fixture builder's own output tiles the input.
    let built = two_built();
    let archive = read(&built.bytes).expect("valid");
    let first = &archive.entries()[0];
    assert_eq!(first.local_offset, 0);
    assert_eq!(first.data_offset, 30 + LONG_A.len() as u64);
    let second = &archive.entries()[1];
    assert_eq!(second.local_offset, first.data_offset + first.compressed);
    assert_eq!(
        second.data_offset + second.compressed,
        built.cd_offset as u64
    );
    let _ = EndFx::default();
}
