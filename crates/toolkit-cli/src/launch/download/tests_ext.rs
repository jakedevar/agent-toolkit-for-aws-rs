//! `download-snapshot` end to end over `FakeStreamingTransport` and a fixed
//! resolver: T-PLAN-7 (planner refusals after the download, zero creation
//! calls) and T-EXT-1..14 (R-AR-EXT), plus T-STAGE-2's "no request" half.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, FixedResolver};

use super::*;
use crate::launch::archive::extract::NoExtractHook;
use crate::launch::archive::fsops::creation_calls;
use crate::launch::archive::plan::{budget_message, MAX_SNAPSHOT_NODES};
use crate::launch::archive::tests_snapshot::{names, partials};
use crate::launch::archive::tests_zipfx::{Fx, FxEntry};
use crate::launch::clock::SeededRandom;
use crate::launch::http::DOWNLOAD_CAP;
use crate::launch::tests_support::Scratch;

/// A presigned-style URL whose query carries a fake signature that must
/// never appear in any message.
pub const URL: &str =
    "https://snapshots.s3.us-east-1.amazonaws.com/l-1/snapshot.zip?X-Amz-Signature=FAKE_SIG_9f3a";

pub fn resolver() -> FixedResolver {
    FixedResolver::new().answer_any(&["52.216.10.20"])
}

pub fn ok_transport(body: &[u8]) -> FakeStreamingTransport {
    let length = body.len().to_string();
    FakeStreamingTransport::new().respond(
        200,
        &[("content-length", length.as_str())],
        body.chunks(64 * 1024).map(FakeChunk::data).collect(),
    )
}

/// The knobs a case changes.
pub struct Knobs<'a> {
    pub url: Option<&'a str>,
    pub budget: usize,
    pub cap: usize,
    pub hook: &'a mut dyn ExtractHook,
}

impl Default for Knobs<'_> {
    fn default() -> Self {
        Self {
            url: Some(URL),
            budget: MAX_SNAPSHOT_NODES,
            cap: DOWNLOAD_CAP,
            hook: Box::leak(Box::new(NoExtractHook)),
        }
    }
}

/// Run `download-snapshot` to `output` (or the default under `temp`).
pub fn run(
    output: Option<&Path>,
    temp: &Path,
    transport: &dyn StreamingTransport,
    knobs: Knobs<'_>,
) -> Result<Downloaded, LaunchError> {
    let url = knobs.url.map(str::to_string);
    let source = move || Ok(url.clone());
    let resolver = resolver();
    let random = SeededRandom::new(11);
    let mut deps = DownloadDeps {
        download_url: &source,
        transport,
        resolver: &resolver,
        random: &random,
        hook: knobs.hook,
        temp_dir: temp,
        limits: ArchiveLimits::PRODUCTION,
        node_budget: knobs.budget,
        download_cap: knobs.cap,
        deadline: Instant::now() + Duration::from_secs(DOWNLOAD_SECONDS),
    };
    download_snapshot(
        &DownloadRequest {
            launch_id: "l-1",
            output,
            region: "us-east-1",
        },
        &mut deps,
    )
}

/// Nothing at `out`, nothing staged next to it.
#[track_caller]
pub fn assert_nothing_created(parent: &Path, out: &Path) {
    assert!(std::fs::symlink_metadata(out).is_err(), "{out:?} exists");
    assert!(partials(parent).is_empty(), "{:?}", names(parent));
}

fn chain(top: &str, depth: usize) -> String {
    let mut name = top.to_string();
    for _ in 0..depth {
        name.push_str("/a");
    }
    name.push_str("/f");
    name
}

#[test]
fn t_plan_7_planner_refusals_after_the_download_create_nothing() {
    let injected: Vec<FxEntry> = (0..3)
        .map(|index| FxEntry::stored(&chain(&format!("t{index}"), 39), b"x"))
        .collect();
    let production: Vec<FxEntry> = (0..101)
        .map(|index| FxEntry::stored(&chain(&format!("u{index}"), 1990), b"x"))
        .collect();
    for (entries, budget) in [(injected, 100), (production, MAX_SNAPSHOT_NODES)] {
        let bytes = Fx::of(entries).build().bytes;
        let scratch = Scratch::new("plan7");
        let out = scratch.join("out");
        let transport = ok_transport(&bytes);
        let before = creation_calls();
        let result = run(
            Some(&out),
            scratch.path(),
            &transport,
            Knobs {
                budget,
                ..Knobs::default()
            },
        );
        assert_eq!(result, Err(LaunchError::Archive(budget_message(budget))));
        assert_eq!(transport.requests().len(), 1, "refused after the download");
        assert_eq!(creation_calls(), before, "zero mkdirat and openat(O_CREAT)");
        assert_nothing_created(scratch.path(), &out);
        assert!(names(scratch.path()).is_empty());
    }
}

/// Where the default destination goes, for the cases that use it.
pub fn default_outputs(temp: &Path) -> Vec<PathBuf> {
    names(temp)
        .into_iter()
        .filter(|name| name.starts_with("launch-snapshot-"))
        .map(|name| temp.join(name))
        .collect()
}

// ---------------------------------------------------------------------------
// T-EXT-1..4: every refusal class of the reader, end to end.
// ---------------------------------------------------------------------------

use crate::launch::archive::tests_snapshot::{mode, sample_bytes, sample_entries, tree};
use crate::launch::archive::tests_zipfx::{extra_item, noise, put16, put32, put64, EndFx};
use crate::launch::archive::zip_read::{decode_entry, read_archive, Profile, ReadStats};
use crate::launch::archive::plan::{plan, PlanStats};
use std::cell::Cell;

/// The reader, the planner and every entry's decode, without a file system:
/// the verdict `download-snapshot` must reproduce.
fn direct(bytes: &[u8]) -> Result<(), LaunchError> {
    let archive = read_archive(
        bytes,
        Profile::Snapshot,
        &ArchiveLimits::PRODUCTION,
        &mut ReadStats::default(),
    )?;
    plan(&archive, MAX_SNAPSHOT_NODES, &mut PlanStats::default())?;
    for entry in archive.entries() {
        decode_entry(&archive, entry, &mut |_| Ok(()))?;
    }
    Ok(())
}

fn with_ok(bad: FxEntry) -> Fx {
    Fx::of(vec![FxEntry::stored("ok.txt", b"ok"), bad])
}

/// Run one refused fixture end to end: the reader's own error, one
/// request, nothing at the destination, no staging left; for refusals that
/// happen before extraction, no creation call at all.
#[track_caller]
fn assert_refused_end_to_end(label: &str, bytes: &[u8], needle: &str) {
    let expected = direct(bytes).expect_err(label);
    assert!(
        expected.message().contains(needle),
        "{label}: {:?} lacks {needle:?}",
        expected.message()
    );
    let scratch = Scratch::new("ext-refusal");
    let out = scratch.join("out");
    let transport = ok_transport(bytes);
    let before = creation_calls();
    let error = run(Some(&out), scratch.path(), &transport, Knobs::default())
        .expect_err(label);
    assert_eq!(error, expected, "{label}");
    assert_eq!(transport.requests().len(), 1, "{label}");
    assert_nothing_created(scratch.path(), &out);
    assert!(names(scratch.path()).is_empty(), "{label}");
    let decode_time = ["trailing_data", "truncated_stream", "(deflate)", "size_mismatch", "crc_mismatch"]
        .iter()
        .any(|code| needle.contains(code));
    if !decode_time {
        assert_eq!(creation_calls(), before, "{label}: created before refusing");
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn t_ext_1_every_framing_refusal_creates_nothing() {
    let mut cases: Vec<(&str, Vec<u8>, &str)> = vec![("short", b"PK\x05\x06".to_vec(), "eocd_missing")];
    let base = Fx::of(sample_entries());
    let built = base.build();
    let mut forged = vec![0x50, 0x4b, 0x05, 0x06];
    forged.extend_from_slice(&[0; 18]);
    let ambiguous = Fx {
        end: EndFx {
            comment: forged,
            ..EndFx::default()
        },
        ..base.clone()
    };
    cases.push(("forged eocd", ambiguous.build().bytes, "eocd_ambiguous"));
    let mut bytes = built.bytes.clone();
    put16(&mut bytes, built.eocd + 4, 1);
    cases.push(("disk", bytes, "multi_disk"));
    cases.push(("empty", Fx::of(Vec::new()).build().bytes, "(empty)"));
    let gap = Fx {
        after_cd: vec![0; 4],
        ..base.clone()
    };
    cases.push(("gap", gap.build().bytes, "(cd_count)"));
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.central[1], 0x1234_5678);
    cases.push(("central signature", bytes, "cd_record"));
    let mut bytes = built.bytes.clone();
    let count = u16::try_from(built.central.len() - 1).unwrap();
    put16(&mut bytes, built.eocd + 8, count);
    put16(&mut bytes, built.eocd + 10, count);
    cases.push(("count", bytes, "cd_count"));
    let mut bytes = built.bytes.clone();
    bytes[built.central[0] + 5] = 1;
    cases.push(("host", bytes, "(version)"));
    let mut encrypted = FxEntry::stored("bad", b"x");
    encrypted.flags |= 0x0001;
    cases.push(("flags", with_ok(encrypted).build().bytes, "(flags)"));
    let mut method = FxEntry::stored("bad", b"x");
    method.method = 12;
    cases.push(("method", with_ok(method).build().bytes, "(method)"));
    let mut stored = FxEntry::stored("bad", b"abc");
    stored.uncompressed = 2;
    cases.push(("stored size", with_ok(stored).build().bytes, "stored_size"));
    let mut extra = FxEntry::stored("bad", b"x");
    extra.central_extra = vec![1, 2, 3];
    cases.push(("extra remainder", with_ok(extra).build().bytes, "extra_field"));
    let mut unicode_path = FxEntry::stored("bad", b"x");
    unicode_path.central_extra = extra_item(0x7075, b"\x01....other");
    cases.push(("unicode path", with_ok(unicode_path).build().bytes, "extra_field"));
    let mut zip64 = FxEntry::stored("bad", b"x");
    zip64.central_extra = extra_item(1, &[0; 8]);
    cases.push(("zip64 extra", with_ok(zip64).build().bytes, "zip64_extra"));
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.local[1], 0x0807_4b50);
    cases.push(("local signature", bytes, "local_header"));
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.local[0] + 14, 0xdead_beef);
    cases.push(("local crc", bytes, "local_mismatch"));
    let mut descriptor = FxEntry::stored("bad", b"x").with_descriptor();
    descriptor.descriptor[0] = 0;
    cases.push(("descriptor", with_ok(descriptor).build().bytes, "(descriptor)"));
    let shared = Fx {
        duplicate_central: vec![0],
        ..base.clone()
    };
    cases.push(("shared local entry", shared.build().bytes, "(overlap)"));
    let stub = Fx {
        entries: {
            let mut entries = sample_entries();
            entries[0].gap_before = b"MZ stub".to_vec();
            entries
        },
        ..Fx::default()
    };
    cases.push(("prepended stub", stub.build().bytes, "(layout)"));
    let mut padded = FxEntry::deflated("bad", &b"y".repeat(300));
    padded.payload.extend_from_slice(&[0; 3]);
    cases.push(("padding", with_ok(padded).build().bytes, "trailing_data"));
    let mut truncated = FxEntry::deflated("bad", &noise(4000, 3));
    truncated.payload.truncate(truncated.payload.len() - 5);
    cases.push(("truncated", with_ok(truncated).build().bytes, "truncated_stream"));
    let mut invalid = FxEntry::deflated("bad", b"x");
    invalid.payload = vec![0xff; 16];
    invalid.uncompressed = 16;
    cases.push(("invalid deflate", with_ok(invalid).build().bytes, "(deflate)"));
    let mut short = FxEntry::deflated("bad", &b"y".repeat(100));
    short.uncompressed = 101;
    cases.push(("short output", with_ok(short).build().bytes, "size_mismatch"));
    let mut directory = FxEntry::dir("bad/");
    directory.external = 0o100_644 << 16;
    cases.push(("directory mode", with_ok(directory).build().bytes, "(directory)"));
    for (label, bytes, needle) in cases {
        assert_refused_end_to_end(label, &bytes, needle);
    }
}

#[test]
fn t_ext_2_every_zip64_refusal_creates_nothing() {
    let zip64 = |end: EndFx| Fx {
        end,
        ..Fx::of(sample_entries())
    };
    let mut cases: Vec<(&str, Vec<u8>, &str)> = vec![(
        "count sentinel without locator",
        zip64(EndFx {
            count_sentinel: Some(true),
            ..EndFx::default()
        })
        .build()
        .bytes,
        "zip64_inconsistent",
    )];
    let good = zip64(EndFx {
        zip64: true,
        ..EndFx::default()
    })
    .build();
    let record = good.zip64_record.expect("record");
    let locator = good.locator.expect("locator");
    let mut bytes = good.bytes.clone();
    put64(&mut bytes, record + 4, 45);
    cases.push(("record size", bytes, "zip64_inconsistent"));
    let mut bytes = good.bytes.clone();
    put32(&mut bytes, locator + 4, 1);
    cases.push(("locator disk", bytes, "multi_disk"));
    let mut bytes = good.bytes.clone();
    put64(&mut bytes, record + 24, 100_001);
    put64(&mut bytes, record + 32, 100_001);
    cases.push(("count 100 001", bytes, "100001 entries"));
    let mut bytes = good.bytes.clone();
    put64(&mut bytes, record + 48, u64::MAX);
    cases.push(("u64::MAX offset", bytes, "not a valid ZIP file"));
    let fits = zip64(EndFx {
        zip64: true,
        count_sentinel: Some(false),
        ..EndFx::default()
    })
    .build();
    let mut bytes = fits.bytes.clone();
    let count = u16::try_from(fits.central.len() + 1).unwrap();
    put16(&mut bytes, fits.eocd + 8, count);
    put16(&mut bytes, fits.eocd + 10, count);
    cases.push(("classic differs", bytes, "not a valid ZIP file"));
    cases.push((
        "record without sentinel",
        fits.bytes.clone(),
        "(layout)",
    ));
    for (label, bytes, needle) in cases {
        assert_refused_end_to_end(label, &bytes, needle);
    }
}

#[test]
fn t_ext_3_every_entry_policy_refusal_creates_nothing() {
    let mut cases: Vec<(String, Vec<u8>, &str)> = Vec::new();
    let long_segment = "a".repeat(256);
    let long_name = vec!["b".repeat(255); 17].join("/");
    for name in [
        "../x",
        "a/../../x",
        "/etc/x",
        "\\x",
        "C:x",
        "a\\..\\x",
        "a\u{0}b",
        "a\u{7}b",
        "./x",
        "a/./b",
        "a//b",
        long_segment.as_str(),
        long_name.as_str(),
    ] {
        let bytes = with_ok(FxEntry::stored(name, b"x")).build().bytes;
        cases.push((format!("name {name:?}"), bytes, "unsafe entry path"));
    }
    for (first, second) in [("Readme.TXT", "readme.txt"), ("e\u{301}", "\u{e9}")] {
        let bytes = Fx::of(vec![
            FxEntry::stored(first, b"1"),
            FxEntry::stored(second, b"2"),
        ])
        .build()
        .bytes;
        cases.push((format!("duplicate {first:?}"), bytes, "collide"));
    }
    for (kind, mode) in [("symlink", 0o120_777), ("fifo", 0o010_644), ("device", 0o020_644)] {
        let mut entry = FxEntry::stored("special", b"x");
        entry.external = mode << 16;
        let needle = if kind == "symlink" { "symlink entry" } else { "not a regular file" };
        cases.push((kind.to_string(), with_ok(entry).build().bytes, needle));
    }
    let mut declared = FxEntry::deflated("big", &noise(2000, 5));
    declared.uncompressed = 101 * 1024 * 1024;
    cases.push(("declared size".into(), with_ok(declared).build().bytes, "per-entry limit"));
    let mut ratio = FxEntry::deflated("ratio", &noise(100, 6));
    ratio.uncompressed = ratio.compressed() * 100 + 1;
    cases.push(("ratio".into(), with_ok(ratio).build().bytes, "compression-ratio"));
    let mut cp437 = FxEntry::stored("x", b"x");
    cp437.name = vec![0xff, b'x'];
    cp437.flags = 0;
    cases.push(("cp437".into(), with_ok(cp437).build().bytes, "not valid UTF-8"));
    for (label, bytes, needle) in cases {
        assert_refused_end_to_end(&label, &bytes, needle);
    }
}

#[test]
fn t_ext_4_every_version_control_refusal_creates_nothing() {
    for name in [
        ".git/config",
        "sub/.git",
        "sub/.git/HEAD",
        ".GIT/config",
        ".Git/hooks/x",
        ".g\u{200c}it/config",
        ".git./x",
        ".git /x",
        "git~1/config",
        ".git::$DATA/x",
        ".hg/hgrc",
        ".svn/x",
        ".bzr/x",
        "_darcs/x",
        "CVS/Entries",
        ".jj/x",
    ] {
        let bytes = with_ok(FxEntry::stored(name, b"payload")).build().bytes;
        assert_refused_end_to_end(name, &bytes, "version-control metadata");
    }
}

// ---------------------------------------------------------------------------
// T-EXT-5..8: success, existing destination, late failures.
// ---------------------------------------------------------------------------

/// Records the extraction's write progress and the state at publication.
#[derive(Default)]
struct Watch {
    max_written: u64,
    out: Option<std::path::PathBuf>,
    out_existed_before_publish: Option<bool>,
}

impl ExtractHook for Watch {
    fn before(&mut self, point: crate::launch::archive::extract::HookPoint) {
        if point == crate::launch::archive::extract::HookPoint::BeforePublish {
            self.out_existed_before_publish =
                self.out.as_ref().map(|out| std::fs::symlink_metadata(out).is_ok());
        }
    }

    fn written(&mut self, _node: u32, total: u64) {
        self.max_written = self.max_written.max(total);
    }
}

#[test]
fn t_ext_5_valid_snapshot_lands_atomically_with_exact_modes() {
    let temp = Scratch::new("ext5-tmp");
    let transport = ok_transport(&sample_bytes());
    let mut watch = Watch::default();
    let downloaded = run(
        None,
        temp.path(),
        &transport,
        Knobs {
            hook: &mut watch,
            ..Knobs::default()
        },
    )
    .expect("downloads");
    let outputs = default_outputs(temp.path());
    assert_eq!(outputs.len(), 1);
    let out = &outputs[0];
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("launch-snapshot-l-1-"), "{name}");
    assert_eq!(name.len(), "launch-snapshot-l-1-".len() + 16);
    assert_eq!(
        downloaded,
        Downloaded {
            output_dir: out.clone(),
            files: 4,
            directories: 4,
            bytes: 9 + 13 * 40 + 17 + 19,
            region: "us-east-1".to_string(),
        }
    );
    assert_eq!(
        tree(out),
        crate::launch::archive::extract::tests_stage::sample_tree()
    );
    assert_eq!(mode(out), 0o700);
    assert_eq!(mode(&out.join("bin/run.sh")), 0o755);
    assert_eq!(mode(&out.join("src/main.rs")), 0o644);
    assert_eq!(mode(&out.join("src/lib")), 0o755);
    assert!(partials(temp.path()).is_empty());
    let request = &transport.requests()[0];
    assert_eq!(request.method, toolkit_aws::transport::Method::Get);
    assert_eq!(request.url, URL);
    assert_eq!(
        render_downloaded(&downloaded),
        format!(
            "{{\"outputDir\":{},\"files\":4,\"directories\":4,\"bytes\":{},\"region\":\"us-east-1\"}}",
            serde_json::to_string(&out.to_string_lossy()).unwrap(),
            9 + 13 * 40 + 17 + 19
        )
    );
    // Atomic: with an explicit output, nothing exists at the name until
    // the exclusive rename.
    let scratch = Scratch::new("ext5-explicit");
    let explicit = scratch.join("out");
    let mut watch = Watch {
        out: Some(explicit.clone()),
        ..Watch::default()
    };
    run(
        Some(&explicit),
        scratch.path(),
        &ok_transport(&sample_bytes()),
        Knobs {
            hook: &mut watch,
            ..Knobs::default()
        },
    )
    .expect("downloads");
    assert_eq!(watch.out_existed_before_publish, Some(false));
    assert_eq!(names(scratch.path()), vec!["out".to_string()]);
}

#[test]
fn t_ext_6_existing_destination_is_refused_before_the_download() {
    let scratch = Scratch::new("ext6");
    let out = scratch.join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("mine"), b"mine").unwrap();
    let transport = ok_transport(&sample_bytes());
    let asked = Cell::new(0);
    let source = || {
        asked.set(asked.get() + 1);
        Ok(Some(URL.to_string()))
    };
    let resolver = resolver();
    let random = SeededRandom::new(1);
    let mut hook = NoExtractHook;
    let mut deps = DownloadDeps {
        download_url: &source,
        transport: &transport,
        resolver: &resolver,
        random: &random,
        hook: &mut hook,
        temp_dir: scratch.path(),
        limits: ArchiveLimits::PRODUCTION,
        node_budget: MAX_SNAPSHOT_NODES,
        download_cap: DOWNLOAD_CAP,
        deadline: Instant::now() + Duration::from_secs(DOWNLOAD_SECONDS),
    };
    let request = DownloadRequest {
        launch_id: "l-1",
        output: Some(&out),
        region: "us-east-1",
    };
    let error = download_snapshot(&request, &mut deps).expect_err("exists");
    assert_eq!(
        error.message(),
        format!("Output directory already exists: {}.", out.display())
    );
    assert_eq!(asked.get(), 0, "no GetLaunch");
    assert!(transport.requests().is_empty(), "no download");
    assert_eq!(names(&out), vec!["mine".to_string()]);
    assert!(partials(scratch.path()).is_empty());
}

#[test]
fn t_stage_2_writable_parent_refused_before_any_request() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("stage2-e2e");
    let parent = scratch.join("shared");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
    let transport = ok_transport(&sample_bytes());
    let error = run(
        Some(&parent.join("out")),
        scratch.path(),
        &transport,
        Knobs::default(),
    )
    .expect_err("refused");
    assert!(error.message().starts_with("The parent of the output directory can be modified"));
    assert!(transport.requests().is_empty(), "the transport fake records none");
    assert!(names(&parent).is_empty());
}

#[test]
fn t_ext_7_crc_failure_on_the_last_entry_cleans_up_descriptor_relative() {
    let mut entries = sample_entries();
    let mut last = FxEntry::stored("zz/last.txt", b"last entry");
    last.crc ^= 0x55;
    entries.push(last);
    let bytes = Fx::of(entries).build().bytes;
    let scratch = Scratch::new("ext7");
    let out = scratch.join("out");
    let before = creation_calls();
    let error = run(Some(&out), scratch.path(), &ok_transport(&bytes), Knobs::default())
        .expect_err("crc");
    assert_eq!(
        error.message(),
        "The archive is not a valid ZIP file (crc_mismatch)."
    );
    assert_eq!(creation_calls() - before, 2 + 10, "staging, tree and every node");
    assert_nothing_created(scratch.path(), &out);
    assert!(names(scratch.path()).is_empty());
}

#[test]
fn t_ext_8_size_bomb_stops_at_the_first_byte_past_the_declared_size() {
    let declared = 100_000;
    let mut bomb = FxEntry::deflated("bomb.bin", &noise(150_000, 8));
    bomb.uncompressed = declared;
    let bytes = with_ok(bomb).build().bytes;
    let scratch = Scratch::new("ext8");
    let out = scratch.join("out");
    let mut watch = Watch::default();
    let error = run(
        Some(&out),
        scratch.path(),
        &ok_transport(&bytes),
        Knobs {
            hook: &mut watch,
            ..Knobs::default()
        },
    )
    .expect_err("bomb");
    assert_eq!(
        error.message(),
        "The archive is not a valid ZIP file (size_mismatch)."
    );
    assert!(watch.max_written <= declared, "{}", watch.max_written);
    assert_nothing_created(scratch.path(), &out);
}

// ---------------------------------------------------------------------------
// T-EXT-9..14: the download.
// ---------------------------------------------------------------------------

/// Run with `url` and `transport`; the error, nothing created.
fn refused_download(url: &str, transport: &dyn StreamingTransport, cap: usize) -> LaunchError {
    let scratch = Scratch::new("ext-dl");
    let out = scratch.join("out");
    let before = creation_calls();
    let error = run(
        Some(&out),
        scratch.path(),
        transport,
        Knobs {
            url: Some(url),
            cap,
            ..Knobs::default()
        },
    )
    .expect_err("refused");
    assert_eq!(creation_calls(), before);
    assert_nothing_created(scratch.path(), &out);
    assert!(names(scratch.path()).is_empty());
    assert_eq!(error, LaunchError::Download(error.message().to_string()));
    error
}

#[test]
fn t_ext_9_non_https_url_is_refused_without_a_request() {
    for url in [
        "http://snapshots.s3.us-east-1.amazonaws.com/x.zip",
        "ftp://snapshots.s3.us-east-1.amazonaws.com/x.zip",
        "snapshots.s3.us-east-1.amazonaws.com/x.zip",
    ] {
        let transport = ok_transport(&sample_bytes());
        let error = refused_download(url, &transport, DOWNLOAD_CAP);
        assert_eq!(error.message(), "The snapshot download URL must use https.");
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn t_ext_10_userinfo_is_refused_without_a_request() {
    for url in [
        "https://user:FAKE_PW@snapshots.s3.us-east-1.amazonaws.com/x.zip",
        "https://user@snapshots.s3.us-east-1.amazonaws.com/x.zip",
    ] {
        let transport = ok_transport(&sample_bytes());
        let error = refused_download(url, &transport, DOWNLOAD_CAP);
        assert_eq!(
            error.message(),
            "The snapshot download URL must not contain user information."
        );
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn t_ext_11_disallowed_or_non_public_hosts_are_refused_without_a_request() {
    for url in [
        "https://evil.example.com/x.zip",
        "https://amazonaws.com.evil.example/x.zip",
        "https://xamazonaws.com/x.zip",
        "https://[::1]/x.zip",
        "https://127.0.0.1/x.zip",
        "https://snapshots.s3.us-east-1.amazonaws.com:0/x.zip",
        "https://snapshots.s3.us-east-1.amazonaws.com/a b.zip",
    ] {
        let transport = ok_transport(&sample_bytes());
        let error = refused_download(url, &transport, DOWNLOAD_CAP);
        assert!(
            error.message().starts_with("The snapshot download URL"),
            "{url}: {error:?}"
        );
        assert!(transport.requests().is_empty(), "{url}");
    }
    // A host on the allowlist whose answers are not all public.
    let scratch = Scratch::new("ext11-dns");
    let transport = ok_transport(&sample_bytes());
    let private = FixedResolver::new().answer_any(&["52.216.10.20", "10.0.0.7"]);
    let source = || Ok(Some(URL.to_string()));
    let random = SeededRandom::new(1);
    let mut hook = NoExtractHook;
    let mut deps = DownloadDeps {
        download_url: &source,
        transport: &transport,
        resolver: &private,
        random: &random,
        hook: &mut hook,
        temp_dir: scratch.path(),
        limits: ArchiveLimits::PRODUCTION,
        node_budget: MAX_SNAPSHOT_NODES,
        download_cap: DOWNLOAD_CAP,
        deadline: Instant::now() + Duration::from_secs(DOWNLOAD_SECONDS),
    };
    let out = scratch.join("out");
    let error = download_snapshot(
        &DownloadRequest {
            launch_id: "l-1",
            output: Some(&out),
            region: "us-east-1",
        },
        &mut deps,
    )
    .expect_err("non-public");
    assert_eq!(
        error.message(),
        "The snapshot download URL's host does not resolve only to public addresses."
    );
    assert!(transport.requests().is_empty());
    assert_nothing_created(scratch.path(), &out);
}

#[test]
fn t_ext_12_redirects_and_error_statuses_are_errors_naming_only_the_status() {
    for status in [301, 302, 307, 308, 403, 404, 500] {
        let transport = FakeStreamingTransport::new().respond(
            status,
            &[("location", "https://elsewhere.example/x"), ("content-length", "5")],
            vec![FakeChunk::data(b"hello")],
        );
        let error = refused_download(URL, &transport, DOWNLOAD_CAP);
        assert_eq!(
            error.message(),
            format!("The snapshot download failed with HTTP status {status}.")
        );
        assert_eq!(transport.requests().len(), 1, "no redirect followed");
        assert_eq!(transport.frames_pulled(), 0, "zero-read");
    }
}

/// An endless body without `Content-Length` (the R-API-BODY generating
/// double): frames of `frame` bytes forever, through the shared
/// `ChunkLimiter`; records whether the stream was dropped.
struct Endless {
    frame: usize,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pulled: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

struct EndlessStream {
    limiter: toolkit_aws::stream::ChunkLimiter,
    headers: toolkit_aws::transport::Headers,
    frame: usize,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pulled: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for EndlessStream {
    fn drop(&mut self) {
        self.dropped.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl toolkit_aws::stream::ResponseStream for EndlessStream {
    fn status(&self) -> u16 {
        200
    }
    fn headers(&self) -> &toolkit_aws::transport::Headers {
        &self.headers
    }
    fn narrow_body_limit(&mut self, cap: u64) {
        self.limiter.narrow(cap);
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, toolkit_aws::stream::StreamError> {
        let (frame, pulled) = (self.frame, self.pulled.clone());
        self.limiter.next(|| {
            pulled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(vec![b'z'; frame]))
        })
    }
}

impl StreamingTransport for Endless {
    fn open<'a>(
        &'a self,
        _request: &toolkit_aws::transport::HttpRequest,
        limits: &toolkit_aws::stream::StreamLimits,
    ) -> Result<Box<dyn toolkit_aws::stream::ResponseStream + 'a>, toolkit_aws::AwsError> {
        Ok(Box::new(EndlessStream {
            limiter: toolkit_aws::stream::ChunkLimiter::new(*limits),
            headers: toolkit_aws::transport::Headers::new(),
            frame: self.frame,
            dropped: self.dropped.clone(),
            pulled: self.pulled.clone(),
        }))
    }
}

fn endless(frame: usize) -> Endless {
    Endless {
        frame,
        dropped: std::sync::Arc::default(),
        pulled: std::sync::Arc::default(),
    }
}

/// The #5082 injected cap: small and not a power of two.
const SMALL_CAP: usize = 100_003;

#[test]
fn t_ext_13_oversize_length_and_endless_bodies_stop_at_the_cap() {
    // Declared Content-Length over the production cap: refused before the
    // first pull.
    let over = (DOWNLOAD_CAP + 1).to_string();
    let transport = FakeStreamingTransport::new().respond(
        200,
        &[("content-length", over.as_str())],
        vec![FakeChunk::data(b"PK")],
    );
    let error = refused_download(URL, &transport, DOWNLOAD_CAP);
    assert_eq!(error.message(), "The snapshot download exceeds the 500 MiB limit.");
    assert_eq!(transport.frames_pulled(), 0);
    // The same against the injected cap.
    let over = (SMALL_CAP + 1).to_string();
    let transport = FakeStreamingTransport::new().respond(
        200,
        &[("content-length", over.as_str())],
        vec![FakeChunk::data(b"PK")],
    );
    let error = refused_download(URL, &transport, SMALL_CAP);
    assert_eq!(
        error.message(),
        "The snapshot download exceeds the 100003 bytes limit."
    );
    assert_eq!(transport.frames_pulled(), 0);
    // No Content-Length and an endless body: the first frame past the cap
    // is refused and the stream dropped; at most the cap was ever pulled.
    let generator = endless(4_097);
    let error = refused_download(URL, &generator, SMALL_CAP);
    assert_eq!(
        error.message(),
        "The snapshot download exceeds the 100003 bytes limit."
    );
    assert!(generator.dropped.load(std::sync::atomic::Ordering::SeqCst));
    let pulled = generator.pulled.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(pulled, SMALL_CAP / 4_097 + 1, "stopped at the first frame past the cap");
    // A misleading Content-Length (smaller than the body): the first extra
    // byte is an invalid response.
    let body = sample_bytes();
    let transport = FakeStreamingTransport::new().respond(
        200,
        &[("content-length", "100")],
        body.chunks(4_097).map(FakeChunk::data).collect(),
    );
    let error = refused_download(URL, &transport, SMALL_CAP);
    assert_eq!(
        error.message(),
        "The snapshot download returned an invalid response."
    );
}

#[test]
fn t_ext_13_download_collector_allocation_stays_within_the_injected_cap() {
    // #5082: unknown length, small successive chunks crossing growth
    // boundaries, a non-power-of-two cap. Capacity never passes the cap and
    // the transient allocator peak (old + new buffer during a moving growth)
    // stays below twice the cap.
    let body: Vec<u8> = noise(SMALL_CAP, 13);
    let transport = FakeStreamingTransport::new().respond(
        200,
        &[],
        body.chunks(4_097).map(FakeChunk::data).collect(),
    );
    let request = toolkit_aws::transport::HttpRequest {
        method: toolkit_aws::transport::Method::Get,
        url: URL.to_string(),
        headers: toolkit_aws::transport::Headers::new(),
        body: Vec::new(),
        timeouts: toolkit_aws::transport::Timeouts::default(),
    };
    let response = crate::launch::http::collect_with_ceiling(
        &transport,
        &request,
        crate::launch::http::Operation::Download,
        Instant::now() + Duration::from_secs(DOWNLOAD_SECONDS),
        SMALL_CAP,
    )
    .expect("exactly at the cap");
    assert_eq!(response.body, body);
    assert!(response.collector_capacity <= SMALL_CAP, "{}", response.collector_capacity);
    assert!(response.collector_peak < 2 * SMALL_CAP, "{}", response.collector_peak);
    assert_eq!(transport.open_limits()[0].max_body_bytes(), SMALL_CAP as u64);
    // One byte more is refused and nothing is returned.
    let mut over = body.clone();
    over.push(0);
    let transport = FakeStreamingTransport::new().respond(
        200,
        &[],
        over.chunks(4_097).map(FakeChunk::data).collect(),
    );
    let refused = crate::launch::http::collect_with_ceiling(
        &transport,
        &request,
        crate::launch::http::Operation::Download,
        Instant::now() + Duration::from_secs(DOWNLOAD_SECONDS),
        SMALL_CAP,
    );
    assert_eq!(refused.unwrap_err(), crate::launch::http::BodyError::TooLarge);
    // The production policy: the download row's ceiling is 500 MiB.
    assert_eq!(DOWNLOAD_CAP, 500 * 1024 * 1024);
}

#[test]
fn t_ext_14_no_error_names_the_url_and_no_archive_file_is_written() {
    let secret_parts = ["FAKE_SIG_9f3a", "snapshots.s3", "X-Amz-Signature", "FAKE_PW"];
    let mut messages = Vec::new();
    for url in [
        "http://snapshots.s3.us-east-1.amazonaws.com/x?X-Amz-Signature=FAKE_SIG_9f3a",
        "https://u:FAKE_PW@snapshots.s3.us-east-1.amazonaws.com/x?X-Amz-Signature=FAKE_SIG_9f3a",
        "https://snapshots.s3.evil.example/x?X-Amz-Signature=FAKE_SIG_9f3a",
    ] {
        messages.push(refused_download(url, &ok_transport(b""), DOWNLOAD_CAP));
    }
    for status in [302, 404, 503] {
        let transport = FakeStreamingTransport::new().respond(status, &[], Vec::new());
        messages.push(refused_download(URL, &transport, DOWNLOAD_CAP));
    }
    messages.push(refused_download(URL, &endless(4_097), SMALL_CAP));
    let failing = FakeStreamingTransport::new().open_error(toolkit_aws::AwsError::timeout(toolkit_aws::error::TimeoutPhase::Deadline, URL));
    messages.push(refused_download(URL, &failing, DOWNLOAD_CAP));
    let failing = FakeStreamingTransport::new().respond(
        200,
        &[],
        vec![FakeChunk::Fail(toolkit_aws::AwsError::connect(URL))],
    );
    messages.push(refused_download(URL, &failing, DOWNLOAD_CAP));
    for error in &messages {
        for part in secret_parts {
            assert!(!error.message().contains(part), "{error:?} names {part}");
        }
    }
    // No temporary archive file: the only creations of a successful run are
    // the staging directory, its tree and the plan's nodes, and the
    // temporary directory stays empty.
    let temp = Scratch::new("ext14-tmp");
    let scratch = Scratch::new("ext14");
    let out = scratch.join("out");
    let before = creation_calls();
    run(
        Some(&out),
        temp.path(),
        &ok_transport(&sample_bytes()),
        Knobs::default(),
    )
    .expect("downloads");
    assert_eq!(creation_calls() - before, 2 + 8);
    assert!(names(temp.path()).is_empty());
    assert_eq!(names(scratch.path()), vec!["out".to_string()]);
    // The source never writes an archive file or names a temporary path.
    let source = include_str!("../download.rs");
    for forbidden in ["File::create", "OpenOptions", "temp_dir()", "fs::write"] {
        assert!(!source.contains(forbidden), "download.rs uses {forbidden}");
    }
}

#[test]
fn missing_download_url_is_the_python_message_and_creates_nothing() {
    let scratch = Scratch::new("ext-nourl");
    let out = scratch.join("out");
    let transport = ok_transport(&sample_bytes());
    let error = run(
        Some(&out),
        scratch.path(),
        &transport,
        Knobs {
            url: None,
            ..Knobs::default()
        },
    )
    .expect_err("no url");
    assert_eq!(error.message(), NO_URL_MESSAGE);
    assert!(transport.requests().is_empty());
    assert_nothing_created(scratch.path(), &out);
}
