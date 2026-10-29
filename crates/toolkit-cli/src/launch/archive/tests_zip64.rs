//! T-ZIP-64-1..10: the strict ZIP64 subset (PM amendment seq 3, #5046) in
//! the writer and both reader profiles.

use super::plan::{plan, PlanStats, MAX_SNAPSHOT_NODES};
use super::tests_zipfx::{many_stored, put16, put32, put64, Built, EndFx, Fx, FxEntry};
use super::zip_read::{code, decode_entry, frame_message, read_archive, Profile, ReadStats};
use super::zip_write::ZipWriter;
use super::ArchiveLimits;
use crate::launch::LaunchError;

const LIMITS: ArchiveLimits = ArchiveLimits::PRODUCTION;

fn write(count: usize) -> Vec<u8> {
    let mut writer = ZipWriter::new(LIMITS);
    for index in 0..count {
        writer
            .add_file(&format!("app/{index}"), b"", 0o100_644, 0)
            .expect("within limits");
    }
    writer.finish().expect("finish").into_vec()
}

fn message(result: Result<impl std::fmt::Debug, LaunchError>) -> String {
    match result {
        Err(LaunchError::Archive(message)) => message,
        other => panic!("expected an ArchiveError, got {other:?}"),
    }
}

#[track_caller]
fn assert_code(bytes: &[u8], code: &str) {
    let result = read_archive(bytes, Profile::Snapshot, &LIMITS, &mut ReadStats::default());
    assert_eq!(message(result), frame_message(code));
}

fn eocd(bytes: &[u8]) -> usize {
    bytes.len() - 22
}

fn classic_count(bytes: &[u8]) -> u16 {
    let p = eocd(bytes);
    u16::from_le_bytes([bytes[p + 10], bytes[p + 11]])
}

fn has_zip64_records(bytes: &[u8]) -> bool {
    let p = eocd(bytes);
    p >= 20 && bytes[p - 20..p - 16] == 0x0706_4b50_u32.to_le_bytes()
}

fn round_trip(bytes: &[u8], count: usize) {
    for profile in [Profile::Upload, Profile::Snapshot] {
        let archive =
            read_archive(bytes, profile, &LIMITS, &mut ReadStats::default()).expect("reads");
        assert_eq!(archive.entries().len(), count, "{profile:?}");
        if profile == Profile::Snapshot {
            for entry in archive.entries() {
                decode_entry(&archive, entry, &mut |piece| {
                    assert!(piece.is_empty());
                    Ok(())
                })
                .expect("decodes");
            }
        }
    }
}

#[test]
fn t_zip_64_1_writer_at_65534_entries_writes_no_zip64() {
    let bytes = write(65_534);
    assert!(!has_zip64_records(&bytes));
    assert_eq!(classic_count(&bytes), 65_534);
    let archive =
        read_archive(&bytes, Profile::Upload, &LIMITS, &mut ReadStats::default()).expect("reads");
    assert!(!archive.uses_zip64());
}

#[test]
fn t_zip_64_2_writer_at_65535_entries_writes_the_zip64_records() {
    let bytes = write(65_535);
    assert!(has_zip64_records(&bytes));
    assert_eq!(classic_count(&bytes), 0xFFFF);
    let p = eocd(&bytes);
    let cd_size = u32::from_le_bytes(bytes[p + 12..p + 16].try_into().expect("4"));
    let cd_offset = u32::from_le_bytes(bytes[p + 16..p + 20].try_into().expect("4"));
    assert_ne!(cd_size, u32::MAX, "the real size, not a sentinel");
    assert_ne!(cd_offset, u32::MAX, "the real offset, not a sentinel");
    let record = p - 20 - 56;
    assert_eq!(bytes[record..record + 4], 0x0606_4b50_u32.to_le_bytes());
    assert_eq!(
        u64::from_le_bytes(bytes[record + 4..record + 12].try_into().expect("8")),
        44
    );
    assert_eq!(
        u64::from_le_bytes(bytes[record + 32..record + 40].try_into().expect("8")),
        65_535
    );
    let archive =
        read_archive(&bytes, Profile::Upload, &LIMITS, &mut ReadStats::default()).expect("reads");
    assert!(archive.uses_zip64());
    assert_eq!(archive.entries().len(), 65_535);
}

#[test]
fn t_zip_64_3_round_trips_at_65536_and_100000_entries() {
    for count in [65_536, 100_000] {
        let bytes = write(count);
        assert!(has_zip64_records(&bytes));
        round_trip(&bytes, count);
    }
}

#[test]
fn t_zip_64_4_entry_100001_is_refused_while_building() {
    let mut limits = LIMITS;
    limits.max_entries = 100_000;
    let mut writer = ZipWriter::new(limits);
    for index in 0..100_000 {
        writer
            .add_file(&format!("app/{index}"), b"", 0o100_644, 0)
            .expect("fits");
    }
    assert_eq!(
        message(writer.check_next("app/last", 0)),
        "Archive has 100001 entries, exceeding the 100000 entry limit."
    );
    assert_eq!(
        message(writer.add_file("app/last", b"", 0o100_644, 0)),
        "Archive has 100001 entries, exceeding the 100000 entry limit."
    );
    assert_eq!(writer.entry_count(), 100_000);
}

#[test]
fn t_zip_64_5_count_sentinel_without_locator() {
    let mut fx = Fx::of(vec![FxEntry::stored("a", b"1"), FxEntry::stored("b", b"2")]);
    fx.end = EndFx {
        zip64: false,
        count_sentinel: Some(true),
        ..EndFx::default()
    };
    assert_code(&fx.build().bytes, code::ZIP64_INCONSISTENT);
}

fn small_zip64() -> Built {
    // A legitimate ZIP64 archive: 65 535 tiny entries (count sentinel).
    many_stored(65_535, "z").build()
}

#[test]
fn t_zip_64_6_misplaced_or_malformed_records() {
    let built = small_zip64();
    read_archive(
        &built.bytes,
        Profile::Snapshot,
        &LIMITS,
        &mut ReadStats::default(),
    )
    .expect("the base fixture is valid");
    let locator = built.locator.expect("locator");
    let record = built.zip64_record.expect("record");
    // Locator signature broken: no locator at p - 20.
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, locator, 0);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
    // The record does not end at the locator.
    for shift in [1_u64, 8] {
        let mut bytes = built.bytes.clone();
        put64(&mut bytes, locator + 8, record as u64 - shift);
        assert_code(&bytes, code::ZIP64_INCONSISTENT);
    }
    // Size of record not 44.
    let mut bytes = built.bytes.clone();
    put64(&mut bytes, record + 4, 45);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
    // Version needed above 4.5.
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, record + 14, 46);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
    // Wrong record signature.
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, record, 0x0606_4b51);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
}

#[test]
fn t_zip_64_7_sentinel_iff_out_of_range() {
    // A sentinel where the ZIP64 value fits (count 2).
    let mut fx = Fx::of(vec![FxEntry::stored("a", b"1"), FxEntry::stored("b", b"2")]);
    fx.end.zip64 = true;
    assert_code(&fx.build().bytes, code::ZIP64_INCONSISTENT);
    let built = small_zip64();
    let p = built.eocd;
    // Offset sentinel although the offset fits.
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, p + 16, u32::MAX);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
    // A real classic size that differs from the ZIP64 size.
    let mut bytes = built.bytes.clone();
    let size = u32::from_le_bytes(bytes[p + 12..p + 16].try_into().expect("4"));
    put32(&mut bytes, p + 12, size + 1);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
    // A classic count that is not the sentinel although the count does not fit.
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, p + 8, 0xFFFE);
    put16(&mut bytes, p + 10, 0xFFFE);
    put32(&mut bytes, p + 12, u32::MAX);
    assert_code(&bytes, code::ZIP64_INCONSISTENT);
}

#[test]
fn t_zip_64_8_disk_fields_of_the_zip64_records() {
    let built = small_zip64();
    let locator = built.locator.expect("locator");
    let record = built.zip64_record.expect("record");
    for (offset, value) in [(locator + 4, 1_u32), (locator + 16, 2), (locator + 16, 0)] {
        let mut bytes = built.bytes.clone();
        put32(&mut bytes, offset, value);
        assert_code(&bytes, code::MULTI_DISK);
    }
    for offset in [record + 16, record + 20] {
        let mut bytes = built.bytes.clone();
        put32(&mut bytes, offset, 1);
        assert_code(&bytes, code::MULTI_DISK);
    }
    let mut bytes = built.bytes.clone();
    put64(&mut bytes, record + 24, 65_534);
    assert_code(&bytes, code::MULTI_DISK);
}

#[test]
fn t_zip_64_9_limits_apply_to_the_zip64_values_before_parsing() {
    let built = small_zip64();
    let record = built.zip64_record.expect("record");
    let p = built.eocd;
    {
        let mut bytes = built.bytes.clone();
        put64(&mut bytes, record + 24, 100_001);
        put64(&mut bytes, record + 32, 100_001);
        let mut stats = ReadStats::default();
        let result = read_archive(&bytes, Profile::Snapshot, &LIMITS, &mut stats);
        assert_eq!(
            message(result),
            "Archive has 100001 entries, exceeding the 100000 entry limit."
        );
        assert_eq!(stats.central_records_read, 0);
    }
    // ZIP64 central size over 64 MiB (classic size set to the sentinel).
    let mut bytes = built.bytes.clone();
    put64(&mut bytes, record + 40, 64 * 1024 * 1024 + 1);
    put32(&mut bytes, p + 12, 64 * 1024 * 1024 + 1);
    let mut stats = ReadStats::default();
    let result = read_archive(&bytes, Profile::Snapshot, &LIMITS, &mut stats);
    assert_eq!(message(result), frame_message(code::LAYOUT));
    assert_eq!(stats.central_records_read, 0);
    // `u64::MAX` offset: checked arithmetic, not a wrap.
    let mut bytes = built.bytes.clone();
    put64(&mut bytes, record + 48, u64::MAX);
    put32(&mut bytes, p + 16, u32::MAX);
    assert_code(&bytes, code::BOUNDS);
}

#[test]
fn t_zip_64_10_zip64_records_without_a_sentinel_and_a_70000_entry_snapshot() {
    let mut fx = Fx::of(vec![FxEntry::stored("a", b"1"), FxEntry::stored("b", b"2")]);
    fx.end = EndFx {
        zip64: true,
        count_sentinel: Some(false),
        ..EndFx::default()
    };
    assert_code(&fx.build().bytes, code::LAYOUT);
    let built = many_stored(70_000, "snap").build();
    let archive = read_archive(
        &built.bytes,
        Profile::Snapshot,
        &LIMITS,
        &mut ReadStats::default(),
    )
    .expect("a 70 000-entry ZIP64 snapshot reads");
    assert!(archive.uses_zip64());
    for entry in archive.entries() {
        decode_entry(&archive, entry, &mut |_| Ok(())).expect("decodes");
    }
    let planned = plan(&archive, MAX_SNAPSHOT_NODES, &mut PlanStats::default()).expect("plans");
    assert_eq!((planned.files(), planned.directories()), (70_000, 1));
}
