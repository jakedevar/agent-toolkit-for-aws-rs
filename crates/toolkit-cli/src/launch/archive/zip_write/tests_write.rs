//! T-AR-ZIP-1..8: the strict writer, checked through the reader.

use super::*;
use crate::launch::archive::crc32::crc32;
use crate::launch::archive::tests_zipfx::noise;
use crate::launch::archive::zip_read::{decode_entry, read_archive, Profile, ReadStats};
use crate::launch::archive::{archive_size_message, MIB};

const LIMITS: ArchiveLimits = ArchiveLimits::PRODUCTION;

fn message(result: Result<impl std::fmt::Debug, LaunchError>) -> String {
    match result {
        Err(LaunchError::Archive(message)) => message,
        other => panic!("expected an ArchiveError, got {other:?}"),
    }
}

fn sample_files() -> Vec<(String, Vec<u8>, u32)> {
    vec![
        (
            "app/README.md".to_string(),
            b"# hello\n".to_vec(),
            0o100_644,
        ),
        (
            "app/bin/run.sh".to_string(),
            b"#!/bin/sh\necho hi\n".repeat(50),
            0o100_755,
        ),
        (
            "app/data/noise.bin".to_string(),
            noise(70_000, 1),
            0o100_600,
        ),
        ("app/empty".to_string(), Vec::new(), 0o100_644),
        (
            "app/caf\u{e9}/\u{65e5}\u{672c}.txt".to_string(),
            b"unicode".to_vec(),
            0o100_644,
        ),
    ]
}

fn build(files: &[(String, Vec<u8>, u32)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(LIMITS);
    for (name, data, mode) in files {
        writer
            .add_file(name, data, *mode, 1_793_190_896)
            .expect("add");
    }
    writer.finish().expect("finish").into_vec()
}

#[test]
fn t_ar_zip_1_round_trip_through_the_reader() {
    let files = sample_files();
    let bytes = build(&files);
    for profile in [Profile::Upload, Profile::Snapshot] {
        let archive = read_archive(&bytes, profile, &LIMITS, &mut ReadStats::default())
            .expect("the writer's output passes the strict reader");
        assert_eq!(archive.entries().len(), files.len());
        for (entry, (name, data, _)) in archive.entries().iter().zip(&files) {
            assert_eq!(archive.name(entry), name);
            assert_eq!(entry.method, 8, "always deflate");
            let mut decoded = Vec::new();
            decode_entry(&archive, entry, &mut |piece| {
                decoded.extend_from_slice(piece);
                Ok(())
            })
            .expect("exact decode");
            assert_eq!(&decoded, data, "{name}");
        }
    }
}

#[test]
fn t_ar_zip_2_crc_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    let bytes = build(&[("app/check".to_string(), b"123456789".to_vec(), 0o100_644)]);
    let archive =
        read_archive(&bytes, Profile::Upload, &LIMITS, &mut ReadStats::default()).expect("reads");
    assert_eq!(archive.entries()[0].crc, 0xCBF4_3926);
    // The local header carries the same CRC (no data descriptor).
    assert_eq!(bytes[14..18], 0xCBF4_3926_u32.to_le_bytes());
}

#[test]
fn t_ar_zip_3_mode_mapping() {
    for (source, archived) in [
        (0o100_644, 0o100_644),
        (0o100_600, 0o100_644),
        (0o100_666, 0o100_644),
        (0o100_755, 0o100_755),
        (0o100_700, 0o100_755),
        (0o100_744, 0o100_755),
        (0o104_755, 0o100_755),
        (0o102_777, 0o100_755),
        (0o101_644, 0o100_644),
        (0o100_011, 0o100_644),
    ] {
        assert_eq!(zip_mode(source), archived, "{source:o}");
    }
    let bytes = build(&sample_files());
    let archive =
        read_archive(&bytes, Profile::Upload, &LIMITS, &mut ReadStats::default()).expect("reads");
    let modes: Vec<Option<u32>> = archive
        .entries()
        .iter()
        .map(|entry| entry.unix_mode)
        .collect();
    assert_eq!(
        modes,
        vec![
            Some(0o100_644),
            Some(0o100_755),
            Some(0o100_644),
            Some(0o100_644),
            Some(0o100_644)
        ]
    );
    let cd = archive.entries()[0].local_offset;
    assert_eq!(cd, 0);
}

#[test]
fn t_ar_zip_4_dos_time_utc_and_clamped() {
    // 2026-10-28 12:34:56 UTC.
    assert_eq!(dos_datetime(1_793_190_896), (25_692, 23_900));
    // Odd seconds round down to the 2-second DOS resolution.
    assert_eq!(dos_datetime(1_793_190_897), (25_692, 23_900));
    let floor = (0, (1 << 5) | 1);
    assert_eq!(dos_datetime(DOS_EPOCH_UNIX), floor);
    assert_eq!(dos_datetime(0), floor, "pre-1980 is clamped, not refused");
    assert_eq!(dos_datetime(i64::MIN), floor);
    let ceiling = ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31);
    assert_eq!(dos_datetime(DOS_MAX_UNIX), ceiling);
    assert_eq!(dos_datetime(i64::MAX), ceiling);
    // A leap day.
    assert_eq!(dos_datetime(951_782_400), (0, (20 << 9) | (2 << 5) | 29));
    let mut writer = ZipWriter::new(LIMITS);
    writer
        .add_file("app/old", b"x", 0o100_644, -1)
        .expect("pre-1980 accepted");
    let bytes = writer.finish().expect("finish").into_vec();
    assert_eq!(bytes[10..14], [0, 0, 0x21, 0], "local time and date");
}

#[test]
fn t_ar_zip_5_layout_has_no_descriptors_extras_or_gaps() {
    let files = sample_files();
    let bytes = build(&files);
    let archive = read_archive(&bytes, Profile::Upload, &LIMITS, &mut ReadStats::default())
        .expect("exact tiling is part of the reader's framing");
    let mut expected_offset = 0_u64;
    for (entry, (name, _, _)) in archive.entries().iter().zip(&files) {
        assert_eq!(entry.local_offset, expected_offset, "{name}: no gap");
        let at = usize::try_from(entry.local_offset).expect("small");
        assert_eq!(bytes[at..at + 4], 0x0403_4b50_u32.to_le_bytes());
        assert_eq!(
            u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]),
            20,
            "version needed"
        );
        assert_eq!(entry.flags, 0x0800, "UTF-8 flag only, never bit 3");
        assert_eq!(
            u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]),
            0,
            "no local extra"
        );
        assert_eq!(
            entry.data_offset,
            entry.local_offset + 30 + name.len() as u64
        );
        expected_offset = entry.data_offset + entry.compressed;
    }
    // Central records: version made by Unix 2.0, no extra, no comment.
    let p = bytes.len() - 22;
    assert_eq!(bytes[p..p + 4], 0x0605_4b50_u32.to_le_bytes());
    assert_eq!(bytes[p + 20..p + 22], [0, 0], "no archive comment");
    let cd = usize::try_from(expected_offset).expect("small");
    assert_eq!(
        u32::from_le_bytes(bytes[p + 16..p + 20].try_into().expect("4")),
        u32::try_from(cd).expect("small")
    );
    assert_eq!(u16::from_le_bytes([bytes[cd + 4], bytes[cd + 5]]), 0x0314);
    assert_eq!(u16::from_le_bytes([bytes[cd + 30], bytes[cd + 31]]), 0);
    assert_eq!(u16::from_le_bytes([bytes[cd + 32], bytes[cd + 33]]), 0);
}

#[test]
fn t_ar_zip_6_limits_with_the_python_messages() {
    // Entry count.
    let mut limits = LIMITS;
    limits.max_entries = 2;
    let mut writer = ZipWriter::new(limits);
    writer.add_file("app/1", b"1", 0o100_644, 0).expect("1");
    writer.add_file("app/2", b"2", 0o100_644, 0).expect("2");
    assert_eq!(
        message(writer.add_file("app/3", b"3", 0o100_644, 0)),
        "Archive has 3 entries, exceeding the 2 entry limit."
    );
    // Per entry, before reading (walked size) and on the bytes.
    let writer = ZipWriter::new(LIMITS);
    assert_eq!(
        message(writer.check_next("app/huge.bin", 150 * MIB)),
        "Archive entry 'app/huge.bin' is 150 MiB uncompressed, exceeding the 100 MiB per-entry \
         limit."
    );
    writer
        .check_next("app/ok.bin", 100 * MIB)
        .expect("exactly the limit");
    let mut limits = LIMITS;
    limits.max_entry_uncompressed = 10;
    let mut writer = ZipWriter::new(limits);
    assert_eq!(
        message(writer.add_file("app/eleven", &[b'x'; 11], 0o100_644, 0)),
        "Archive entry 'app/eleven' is 0 MiB uncompressed, exceeding the 0 MiB per-entry limit."
    );
    // Ratio, after compressing that entry.
    let mut writer = ZipWriter::new(LIMITS);
    assert_eq!(
        message(writer.add_file("app/zeros", &vec![0_u8; 2_000_000], 0o100_644, 0)),
        "Archive entry 'app/zeros' exceeds the 100:1 compression-ratio limit."
    );
    // Running total.
    let mut limits = LIMITS;
    limits.max_total_uncompressed = 15;
    let mut writer = ZipWriter::new(limits);
    writer
        .add_file("app/a", &noise(10, 2), 0o100_644, 0)
        .expect("10");
    assert_eq!(
        message(writer.add_file("app/b", &noise(10, 3), 0o100_644, 0)),
        "Archive decompresses to over 0 GiB, exceeding the decompressed-size limit."
    );
    // Running archive size.
    let mut limits = LIMITS;
    limits.max_archive_bytes = 1000;
    let mut writer = ZipWriter::new(limits);
    assert_eq!(
        message(writer.add_file("app/noise", &noise(2000, 4), 0o100_644, 0)),
        "Archive is 0 MiB, exceeding the 0 MiB limit. Remove large files or build artifacts and \
         try again."
    );
    assert_eq!(
        archive_size_message(600 * MIB + 5, &LIMITS),
        "Archive is 600 MiB, exceeding the 500 MiB limit. Remove large files or build artifacts \
         and try again."
    );
    // The central directory and end records count against the cap too.
    let mut limits = LIMITS;
    limits.max_archive_bytes = 60;
    let mut writer = ZipWriter::new(limits);
    writer
        .add_file("app/x", b"", 0o100_644, 0)
        .expect("local entry fits");
    assert!(message(writer.finish()).starts_with("Archive is 0 MiB, exceeding the 0 MiB limit."));
}

#[test]
fn t_ar_zip_6b_archive_buffer_allocation_stays_under_the_cap() {
    // #5082: an injected small, non-power-of-two cap; successive small
    // entries cross every growth boundary; capacity never exceeds the cap,
    // and the refused entry leaves the allocation unchanged.
    let mut limits = LIMITS;
    limits.max_archive_bytes = 10_007;
    let mut writer = ZipWriter::new(limits);
    let mut index = 0_u64;
    loop {
        let before = writer.buffer().capacity();
        let result = writer.add_file(&format!("app/{index}"), &noise(97, index), 0o100_644, 0);
        assert!(
            writer.buffer().capacity() <= 10_007,
            "capacity {}",
            writer.buffer().capacity()
        );
        assert!(writer.buffer().peak() < 2 * 10_007);
        if result.is_err() {
            assert!(message(result).starts_with("Archive is 0 MiB"));
            assert!(
                writer.buffer().capacity() >= before,
                "never shrinks mid-build"
            );
            break;
        }
        index += 1;
    }
    assert!(index > 10, "several entries fit before the cap: {index}");
    assert!(
        writer.buffer().growths() >= 2,
        "growth boundaries were crossed"
    );
}

#[test]
fn t_ar_zip_7_sanitize_root_name_parity_table() {
    for (input, expected) in [
        ("my-app", "my-app"),
        ("/path/to/My App/", "My-App"),
        ("C:\\Users\\me\\proj", "proj"),
        ("  .hidden  ", "hidden"),
        ("...", "app"),
        ("", "app"),
        ("///", "app"),
        ("a b@c", "a-b-c"),
        ("--x--", "x"),
        ("a.b_c", "a.b_c"),
        ("caf\u{e9}", "caf\u{e9}"),
        ("\u{c5}ngstr\u{f6}m", "\u{c5}ngstr\u{f6}m"),
        ("\u{65e5}\u{672c}\u{8a9e}", "\u{65e5}\u{672c}\u{8a9e}"),
        ("e\u{301}", "e"),
        ("e\u{301}x", "e-x"),
        ("\u{663}\u{664}", "\u{663}\u{664}"),
        ("x\u{b2}", "x\u{b2}"),
        ("\u{216b}", "\u{216b}"),
        ("tab\there", "tab-here"),
        ("emoji\u{1f600}", "emoji"),
        ("..env", "env"),
    ] {
        assert_eq!(sanitize_root_name(input), expected, "{input:?}");
    }
}

#[test]
fn t_ar_zip_8_output_is_deterministic() {
    let files = sample_files();
    assert_eq!(build(&files), build(&files));
    let mut writer = ZipWriter::new(LIMITS);
    writer
        .add_file("app/empty", b"", 0o100_644, 0)
        .expect("empty");
    let bytes = writer.finish().expect("finish").into_vec();
    let archive = read_archive(
        &bytes,
        Profile::Snapshot,
        &LIMITS,
        &mut ReadStats::default(),
    )
    .expect("reads");
    let entry = &archive.entries()[0];
    assert_eq!((entry.method, entry.uncompressed), (8, 0));
    assert!(
        entry.compressed > 0,
        "an empty deflate stream still has bytes"
    );
    decode_entry(&archive, entry, &mut |piece| {
        assert!(piece.is_empty());
        Ok(())
    })
    .expect("decodes to nothing");
}
