//! T-AR-READ-1..20: the entry policy of R-AR-READ under both profiles.

use super::*;
use crate::launch::archive::tests_zipfx::{put16, Fx, FxEntry};

const LIMITS: ArchiveLimits = ArchiveLimits::PRODUCTION;

fn read_with(
    entries: Vec<FxEntry>,
    profile: Profile,
    limits: &ArchiveLimits,
) -> Result<usize, LaunchError> {
    let bytes = Fx::of(entries).build().bytes;
    read_archive(&bytes, profile, limits, &mut ReadStats::default())
        .map(|archive| archive.entries().len())
}

fn snapshot(entries: Vec<FxEntry>) -> Result<usize, LaunchError> {
    read_with(entries, Profile::Snapshot, &LIMITS)
}

fn upload(entries: Vec<FxEntry>) -> Result<usize, LaunchError> {
    read_with(entries, Profile::Upload, &LIMITS)
}

fn message(result: Result<usize, LaunchError>) -> String {
    match result {
        Err(LaunchError::Archive(message)) => message,
        other => panic!("expected an ArchiveError, got {other:?}"),
    }
}

#[track_caller]
fn assert_unsafe(name: &str, shown: &str) {
    for profile in [Profile::Upload, Profile::Snapshot] {
        let result = read_with(vec![FxEntry::stored(name, b"x")], profile, &LIMITS);
        assert_eq!(
            message(result),
            format!("Archive contains an unsafe entry path: {shown}"),
            "{name:?} {profile:?}"
        );
    }
}

#[test]
fn t_ar_read_01_parent_traversal() {
    assert_unsafe("../x", "'../x'");
    assert_unsafe("app/..", "'app/..'");
}

#[test]
fn t_ar_read_02_nested_traversal() {
    assert_unsafe("a/../../x", "'a/../../x'");
}

#[test]
fn t_ar_read_03_absolute_paths() {
    assert_unsafe("/etc/x", "'/etc/x'");
}

#[test]
fn t_ar_read_04_backslashes_anywhere() {
    assert_unsafe("\\x", "'\\\\x'");
    assert_unsafe("app\\x", "'app\\\\x'");
}

#[test]
fn t_ar_read_05_drive_prefixes() {
    assert_unsafe("C:x", "'C:x'");
    assert_unsafe("z:/x", "'z:/x'");
    assert_eq!(
        snapshot(vec![FxEntry::stored("app/C:x", b"x")]),
        Ok(1),
        "only a leading drive"
    );
}

#[test]
fn t_ar_read_06_backslash_traversal() {
    assert_unsafe("a\\..\\x", "'a\\\\..\\\\x'");
}

#[test]
fn t_ar_read_07_nul() {
    assert_unsafe("a\0b", "'a\\x00b'");
}

#[test]
fn t_ar_read_08_control_and_invisible_characters() {
    assert_unsafe("a\u{1}b", "'a\\x01b'");
    assert_unsafe("a\u{7f}", "'a\\x7f'");
    assert_unsafe("a\u{85}b", "'a\\x85b'");
    assert_unsafe("a\nb", "'a\\nb'");
    // Invisible format characters are not control characters: accepted
    // (an emoji ZWJ sequence is a legitimate name), but always shown escaped.
    assert_eq!(
        snapshot(vec![FxEntry::stored("app/a\u{202e}b\u{200d}", b"x")]),
        Ok(1)
    );
    assert_unsafe("a\u{202e}\u{1}", "'a\\u202e\\x01'");
}

#[test]
fn t_ar_read_09_dot_segments() {
    assert_unsafe("./a", "'./a'");
    assert_unsafe("a/./b", "'a/./b'");
    assert_unsafe(".", "'.'");
    assert_eq!(snapshot(vec![FxEntry::stored(".hidden/..x", b"x")]), Ok(1));
}

#[test]
fn t_ar_read_10_empty_segments() {
    assert_unsafe("a//b", "'a//b'");
    assert_unsafe("", "''");
    assert_unsafe("/a", "'/a'");
}

#[test]
fn t_ar_read_11_overlong_names_and_segments() {
    let segment = "s".repeat(255);
    assert_eq!(
        snapshot(vec![FxEntry::stored(&format!("a/{segment}"), b"x")]),
        Ok(1)
    );
    let long_segment = "s".repeat(256);
    let result = snapshot(vec![FxEntry::stored(&format!("a/{long_segment}"), b"x")]);
    assert!(message(result).starts_with("Archive contains an unsafe entry path: 'a/sss"));
    let deep = vec!["d"; 2048].join("/");
    assert_eq!(deep.len(), 4095);
    assert_eq!(snapshot(vec![FxEntry::stored(&deep, b"x")]), Ok(1));
    let deeper = format!("{deep}/e");
    assert_eq!(deeper.len(), 4097);
    let result = snapshot(vec![FxEntry::stored(&deeper, b"x")]);
    let text = message(result);
    assert!(
        text.starts_with("Archive contains an unsafe entry path: 'd/d/d"),
        "{text}"
    );
    assert!(
        text.len() < 400,
        "the shown name is bounded: {}",
        text.len()
    );
}

#[test]
fn t_ar_read_12_duplicates_by_case() {
    let result = snapshot(vec![
        FxEntry::stored("App/README.md", b"1"),
        FxEntry::stored("app/readme.MD", b"2"),
    ]);
    assert_eq!(
        message(result),
        "Archive contains entries whose names collide after case folding or Unicode \
         normalisation: 'app/readme.MD'"
    );
    // Exact duplicates too, and the Greek final sigma folds like sigma.
    assert!(snapshot(vec![FxEntry::stored("a", b"1"), FxEntry::stored("a", b"2")]).is_err());
    assert!(snapshot(vec![
        FxEntry::stored("\u{3c3}", b"1"),
        FxEntry::stored("\u{3c2}", b"2")
    ])
    .is_err());
    assert!(snapshot(vec![
        FxEntry::stored("K", b"1"),
        FxEntry::stored("\u{212a}", b"2")
    ])
    .is_err());
}

#[test]
fn t_ar_read_13_duplicates_by_nfc() {
    let composed = "caf\u{e9}.txt";
    let decomposed = "cafe\u{301}.txt";
    let result = snapshot(vec![
        FxEntry::stored(composed, b"1"),
        FxEntry::stored(decomposed, b"2"),
    ]);
    assert!(message(result).starts_with("Archive contains entries whose names collide"));
    // A directory and a file of the same name, in either order.
    assert!(snapshot(vec![FxEntry::dir("x/"), FxEntry::stored("X", b"1")]).is_err());
}

#[test]
fn t_ar_read_14_symlink_entries() {
    let mut link = FxEntry::stored("app/link", b"/etc/passwd");
    link.external = 0o120_777 << 16;
    for profile in [Profile::Upload, Profile::Snapshot] {
        assert_eq!(
            message(read_with(vec![link.clone()], profile, &LIMITS)),
            "Archive contains a symlink entry: 'app/link'"
        );
    }
}

#[test]
fn t_ar_read_15_special_file_modes() {
    for (mode, label) in [
        (0o010_644_u32, "fifo"),
        (0o020_644, "char"),
        (0o060_644, "block"),
        (0o140_644, "socket"),
        (0o170_644, "unknown"),
    ] {
        let mut entry = FxEntry::stored("app/special", b"");
        entry.external = mode << 16;
        assert_eq!(
            message(snapshot(vec![entry])),
            "Archive contains an entry that is not a regular file or directory: 'app/special'",
            "{label}"
        );
    }
    // Setuid and sticky bits on a regular file are not a type; they are
    // never restored (L6) but do not refuse the archive.
    let mut entry = FxEntry::stored("app/tool", b"x");
    entry.external = 0o104_755 << 16;
    assert_eq!(snapshot(vec![entry]), Ok(1));
}

#[test]
fn t_ar_read_16_declared_size_bombs() {
    // Per entry: the declared uncompressed size, checked before any data is read.
    let mut limits = LIMITS;
    limits.max_entry_uncompressed = 4;
    let mut entry = FxEntry::stored("app/big", b"12345");
    entry.crc = 0;
    let result = read_with(vec![entry], Profile::Snapshot, &limits);
    assert_eq!(
        message(result),
        "Archive entry 'app/big' is 0 MiB uncompressed, exceeding the 0 MiB per-entry limit."
    );
    // The production message shape, from a declared 150 MiB stored entry: the
    // stored-size rule would fire too, so use a deflate entry whose declared
    // size passes the ratio rule against a padded span.
    let mut bomb = FxEntry::deflated("app/bomb", b"");
    bomb.payload = vec![0; 2 * 1024 * 1024];
    bomb.uncompressed = 150 * 1024 * 1024;
    assert_eq!(
        message(snapshot(vec![bomb])),
        "Archive entry 'app/bomb' is 150 MiB uncompressed, exceeding the 100 MiB per-entry limit."
    );
    // Running total.
    let mut limits = LIMITS;
    limits.max_total_uncompressed = 9;
    let entries = vec![
        FxEntry::stored("app/a", b"12345"),
        FxEntry::stored("app/b", b"12345"),
    ];
    assert_eq!(
        message(read_with(entries, Profile::Upload, &limits)),
        "Archive decompresses to over 0 GiB, exceeding the decompressed-size limit."
    );
}

#[test]
fn t_ar_read_17_ratio_bombs() {
    let mut bomb = FxEntry::deflated("app/zeros", &vec![0_u8; 1_000_000]);
    assert!(bomb.uncompressed > 100 * bomb.compressed());
    bomb.crc = crate::launch::archive::crc32::crc32(&vec![0_u8; 1_000_000]);
    assert_eq!(
        message(snapshot(vec![bomb])),
        "Archive entry 'app/zeros' exceeds the 100:1 compression-ratio limit."
    );
    // Exactly 100:1 passes.
    let mut edge = FxEntry::deflated("app/edge", b"");
    edge.payload = vec![0; 10];
    edge.uncompressed = 1000;
    let bytes = Fx::of(vec![edge]).build().bytes;
    assert!(read_archive(
        &bytes,
        Profile::Snapshot,
        &LIMITS,
        &mut ReadStats::default()
    )
    .is_ok());
    // Compressed size zero requires uncompressed zero.
    let mut empty_span = FxEntry::deflated("app/none", b"");
    empty_span.payload = Vec::new();
    empty_span.uncompressed = 1;
    assert_eq!(
        message(snapshot(vec![empty_span])),
        "Archive entry 'app/none' exceeds the 100:1 compression-ratio limit."
    );
}

#[test]
fn t_ar_read_18_upload_requires_one_top_level_directory() {
    assert_eq!(
        upload(vec![
            FxEntry::stored("app/a", b"1"),
            FxEntry::stored("app/b/c", b"2")
        ]),
        Ok(2)
    );
    assert_eq!(
        message(upload(vec![
            FxEntry::stored("app/a", b"1"),
            FxEntry::stored("other/b", b"2")
        ])),
        "Archive must contain a single top-level directory; found 2."
    );
    assert_eq!(
        message(upload(vec![
            FxEntry::stored("app/a", b"1"),
            FxEntry::stored("loose.txt", b"2"),
            FxEntry::stored("x/y", b"3")
        ])),
        "Archive must contain a single top-level directory; found 3."
    );
}

#[test]
fn t_ar_read_19_snapshot_has_no_single_root_rule() {
    assert_eq!(
        snapshot(vec![
            FxEntry::stored("README.md", b"1"),
            FxEntry::stored("src/main.rs", b"2"),
            FxEntry::dir("docs/"),
        ]),
        Ok(3)
    );
    // ... but the version-control denylist applies to it only.
    assert!(snapshot(vec![FxEntry::stored(".git/config", b"x")]).is_err());
    assert_eq!(upload(vec![FxEntry::stored("app/.gitignore", b"x")]), Ok(1));
}

#[test]
fn t_ar_read_20_names_must_be_utf8_and_are_shown_redacted() {
    // Flag bit 11 clear: only pure ASCII is accepted (CP437 is refused).
    let mut cp437 = FxEntry::stored("app/caf\u{e9}", b"x");
    cp437.flags = 0;
    assert_eq!(
        message(snapshot(vec![cp437])),
        "Archive contains an entry name that is not valid UTF-8."
    );
    let mut ascii = FxEntry::stored("app/plain", b"x");
    ascii.flags = 0;
    assert_eq!(snapshot(vec![ascii]), Ok(1));
    // Flag bit 11 set with invalid UTF-8.
    let mut invalid = FxEntry::stored("app/x", b"x");
    invalid.name = b"app/\xff\xfe".to_vec();
    assert_eq!(
        message(snapshot(vec![invalid])),
        "Archive contains an entry name that is not valid UTF-8."
    );
    // A credential-shaped name never reaches the message.
    let name = "../AKIAIOSFODNN7EXAMPLE";
    let text = message(snapshot(vec![FxEntry::stored(name, b"x")]));
    assert!(
        text.starts_with("Archive contains an unsafe entry path: "),
        "{text}"
    );
    assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"), "{text}");
    // The entry-count message from a classic count over an injected limit.
    let mut limits = LIMITS;
    limits.max_entries = 1;
    let bytes = Fx::of(vec![
        FxEntry::stored("app/a", b"1"),
        FxEntry::stored("app/b", b"2"),
    ])
    .build()
    .bytes;
    let mut stats = ReadStats::default();
    let result = read_archive(&bytes, Profile::Upload, &limits, &mut stats);
    assert!(matches!(
        result,
        Err(LaunchError::Archive(ref text)) if text == "Archive has 2 entries, exceeding the 1 entry limit."
    ));
    assert_eq!(stats.central_records_read, 0);
    let _ = put16;
}
