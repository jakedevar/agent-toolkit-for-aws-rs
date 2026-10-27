//! Tests of `startup check-expected` (#4408): every committed golden tree
//! passes its program; for every program a deliberately invalid copy of the
//! fixture proves the check fails (AC3); malformed input fails closed; and the
//! failure report never carries a raw control character.

#![allow(clippy::semicolon_if_nothing_returned)] // closure tables of one-line edits

use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::Json;

use crate::startup::scoring::fixtures::{exec, j, path_text, TempDir};

mod azure;
mod capture;
mod decide_workshop;
mod repair;
mod roots;

/// The shipped plugin the expectation documents and golden trees live in.
/// One invalid-fixture case: a label, the damage, the message it must produce.
pub(super) type Case = (&'static str, fn(&mut Json), &'static str);

pub(super) fn plugin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

/// A committed golden tree: `fixtures/<set>/<tree>`.
pub(super) fn golden(set: &str, tree: &str) -> PathBuf {
    plugin_root().join("fixtures").join(set).join(tree)
}

/// Runs `startup check-expected KIND RUN [EXTRA...]` in process.
pub(super) fn check(kind: &str, run: &Path, extra: &[&str]) -> (u8, String, String) {
    let root = plugin_root();
    let mut args = vec![
        "startup",
        "check-expected",
        kind,
        path_text(run),
        "--plugin-root",
        path_text(&root),
    ];
    args.splice(4..4, extra.iter().copied());
    exec(&args)
}

/// The program passes: status 0 and a final `PASS` line.
pub(super) fn assert_passes(kind: &str, run: &Path) {
    let (status, stdout, stderr) = check(kind, run, &[]);
    assert_eq!(status, 0, "{kind} on {run:?}:\n{stdout}\n{stderr}");
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("PASS")),
        "{kind}: {stdout}"
    );
    assert!(stderr.is_empty(), "{kind}: {stderr}");
}

/// The program fails with exit status 1; returns its report.
pub(super) fn assert_fails(kind: &str, run: &Path) -> String {
    let (status, stdout, stderr) = check(kind, run, &[]);
    assert_eq!(status, 1, "{kind} must fail:\n{stdout}\n{stderr}");
    assert!(stdout.contains("FAIL"), "{kind}: {stdout}");
    assert!(!stdout.contains("PASS"), "{kind}: {stdout}");
    assert!(stderr.starts_with("error: "), "{kind}: {stderr}");
    assert_no_raw_controls(&stdout);
    assert_no_raw_controls(&stderr);
    stdout
}

/// The program fails and its report names `needle`.
pub(super) fn assert_fails_with(kind: &str, run: &Path, needle: &str) {
    let report = assert_fails(kind, run);
    assert!(
        report.contains(needle),
        "{kind} must report {needle:?}:\n{report}"
    );
}

pub(super) fn assert_no_raw_controls(text: &str) {
    for c in text.chars() {
        assert!(
            c == '\n' || !toolkit_core::is_unsafe_char(c),
            "raw U+{:04X} in {text:?}",
            u32::from(c)
        );
    }
}

fn copy_dir(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create copy target");
    for entry in fs::read_dir(source).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let to = target.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).expect("copy fixture file");
        }
    }
}

/// A scratch copy of a golden tree to damage; the guard removes it.
pub(super) struct Copy {
    _guard: TempDir,
    run: PathBuf,
}

impl Copy {
    pub(super) fn of(set: &str, tree: &str) -> Self {
        let guard = TempDir::new("check-expected");
        let run = guard.dir("run");
        copy_dir(&golden(set, tree), &run);
        Self { _guard: guard, run }
    }

    /// An empty run directory.
    pub(super) fn empty() -> Self {
        let guard = TempDir::new("check-expected");
        let run = guard.dir("run");
        Self { _guard: guard, run }
    }

    pub(super) fn path(&self) -> &Path {
        &self.run
    }

    pub(super) fn write(&self, relative: &str, text: &str) {
        let path = self.run.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        fs::write(path, text).expect("write fixture");
    }

    pub(super) fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.run.join(relative)).expect("read fixture")
    }

    pub(super) fn remove(&self, relative: &str) {
        fs::remove_file(self.run.join(relative)).expect("remove fixture file");
    }

    /// Edits a JSON document of the copy.
    pub(super) fn edit(&self, relative: &str, change: impl FnOnce(&mut Json)) {
        let mut doc = j(&self.read(relative));
        change(&mut doc);
        self.write(relative, &doc.to_compact_string());
    }
}

/// Replaces the member or element at `path` (object keys, array indexes).
pub(super) fn set(doc: &mut Json, path: &[&str], value: Json) {
    let (last, parents) = path.split_last().expect("a path");
    let mut node = doc;
    for segment in parents {
        node = step(node, segment);
    }
    match node {
        Json::Object(members) => match members.iter_mut().find(|(key, _)| key == last) {
            Some(slot) => slot.1 = value,
            None => members.push(((*last).to_string(), value)),
        },
        Json::Array(items) => {
            let index: usize = last.parse().expect("array index");
            items[index] = value;
        }
        other => panic!("cannot set {last} on {other:?}"),
    }
}

/// Removes the object member at `path`.
pub(super) fn remove(doc: &mut Json, path: &[&str]) {
    let (last, parents) = path.split_last().expect("a path");
    let mut node = doc;
    for segment in parents {
        node = step(node, segment);
    }
    let Json::Object(members) = node else {
        panic!("not an object at {path:?}");
    };
    let before = members.len();
    members.retain(|(key, _)| key != last);
    assert_ne!(before, members.len(), "no member {path:?} to remove");
}

fn step<'a>(node: &'a mut Json, segment: &str) -> &'a mut Json {
    match node {
        Json::Object(members) => {
            &mut members
                .iter_mut()
                .find(|(key, _)| key == segment)
                .unwrap_or_else(|| panic!("no member {segment}"))
                .1
        }
        Json::Array(items) => &mut items[segment.parse::<usize>().expect("array index")],
        other => panic!("cannot step into {other:?} at {segment}"),
    }
}

pub(super) fn text(value: &str) -> Json {
    Json::string(value)
}

/// Every `check_expected_*.py` the repository ships, as `set/file`.
fn shipped_programs() -> Vec<String> {
    let fixtures = plugin_root().join("fixtures");
    let mut found = Vec::new();
    for set in fs::read_dir(&fixtures).expect("fixtures dir").flatten() {
        if !set.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        for file in fs::read_dir(set.path()).expect("set dir").flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if name.starts_with("check_expected_") && name.to_lowercase().ends_with(".py") {
                found.push(format!("{}/{name}", set.file_name().to_string_lossy()));
            }
        }
    }
    found.sort();
    found
}

/// AC1: every shipped Python asserter is covered by a registered program, and
/// the program table says which.
#[test]
fn every_shipped_python_asserter_has_a_program() {
    let source = include_str!("mod.rs");
    let shipped = shipped_programs();
    assert_eq!(shipped.len(), 16, "{shipped:?}");
    for program in &shipped {
        assert!(
            source.contains(&format!("`{program}`")),
            "{program} is not in the program table of fixtures/mod.rs"
        );
    }
    assert_eq!(super::PROGRAMS.len(), shipped.len());
    let names: std::collections::BTreeSet<_> = super::PROGRAMS.iter().map(|p| p.name).collect();
    assert_eq!(names.len(), shipped.len(), "program names are unique");
}

#[test]
fn the_readme_documents_the_command_and_every_kind() {
    let readme = include_str!("../../../README.md");
    assert!(readme.contains("### startup check-expected (#4408)"));
    for program in super::PROGRAMS {
        assert!(
            readme.contains(program.name),
            "README lacks {}",
            program.name
        );
    }
}

#[test]
fn usage_errors_are_exit_two_and_name_the_choices() {
    let run = Copy::empty();
    for args in [
        vec!["startup", "check-expected"],
        vec![
            "startup",
            "check-expected",
            "no-such-kind",
            path_text(run.path()),
        ],
        vec!["startup", "check-expected", "heroku-decide"],
        vec![
            "startup",
            "check-expected",
            "heroku-decide",
            path_text(run.path()),
            path_text(run.path()),
        ],
    ] {
        let (status, stdout, stderr) = exec(&args);
        assert_eq!(status, 2, "{args:?}: {stdout}{stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
    }
    let (_, _, stderr) = exec(&[
        "startup",
        "check-expected",
        "no-such-kind",
        path_text(run.path()),
    ]);
    assert!(stderr.contains("heroku-decide"), "{stderr}");
    // A flag without its value, and an unknown flag, are usage errors too.
    let (status, ..) = exec(&[
        "startup",
        "check-expected",
        "heroku-decide",
        "--plugin-root",
    ]);
    assert_eq!(status, 2);
    let (status, ..) = exec(&[
        "startup",
        "check-expected",
        "heroku-decide",
        path_text(run.path()),
        "--nope",
    ]);
    assert_eq!(status, 2);
}

#[test]
fn an_empty_or_missing_run_directory_fails_closed_for_every_program() {
    let empty = Copy::empty();
    let missing = empty.path().join("does-not-exist");
    for program in super::PROGRAMS {
        for run in [empty.path(), missing.as_path()] {
            let report = assert_fails(program.name, run);
            assert!(
                report.contains("missing") || report.contains("neither"),
                "{}: {report}",
                program.name
            );
        }
    }
}

/// Hostile text in the run directory name, in the documents (invalid JSON) and
/// in values that a check echoes never reaches the report raw.
#[test]
fn hostile_run_directories_and_values_are_escaped_in_every_report() {
    let hostile = "\u{1b}[31mFORGED\u{7}\r\u{202e}gpj.exe\nerror: injected line";
    let scratch = TempDir::new("check-expected-hostile");
    let run = scratch.dir("run\u{1b}[31m\u{202e}\nx");
    // Every document name any program reads holds valid JSON whose strings are
    // hostile, so the echoing branches run.
    let names = [
        ".phase-status.json",
        "preferences.json",
        "aws-design.json",
        "estimation-infra.json",
        "heroku-resource-inventory.json",
        "gcp-resource-inventory.json",
        "gcp-resource-clusters.json",
        "azure-resource-inventory.json",
        "azure-resource-clusters.json",
        "scenarios/index.json",
    ];
    let doc = Json::object(vec![
        ("current_phase", text(hostile)),
        ("run_mode", text(hostile)),
        ("phases", Json::object(vec![("generate", text(hostile))])),
        (
            "services",
            Json::Array(vec![Json::object(vec![("aws_service", text(hostile))])]),
        ),
        (
            "resources",
            Json::Array(vec![Json::object(vec![("azure_id", text(hostile))])]),
        ),
        (
            "metadata",
            Json::object(vec![("confidence", text(hostile))]),
        ),
        ("clarify_status", text(hostile)),
        ("scenarios", Json::Array(Vec::new())),
    ]);
    for name in names {
        let path = run.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        fs::write(path, doc.to_compact_string()).expect("write document");
    }
    for program in super::PROGRAMS {
        let (status, stdout, stderr) = check(program.name, &run, &[]);
        assert_eq!(status, 1, "{}: {stdout}{stderr}", program.name);
        assert_no_raw_controls(&stdout);
        assert_no_raw_controls(&stderr);
        assert!(
            !stdout
                .lines()
                .any(|line| line.starts_with("error: injected")),
            "{}: forged line in {stdout:?}",
            program.name
        );
        assert!(
            !stderr
                .lines()
                .any(|line| line.starts_with("error: injected")),
            "{}: forged line in {stderr:?}",
            program.name
        );
    }
}

/// Payload text of an invalid document is never echoed: the parse failure
/// names the file and the position, not the content.
#[test]
fn an_invalid_document_is_reported_by_name_and_position_not_content() {
    let secret = "FAKE-SECRET-4408-AKIAZZZZZZZZZZZZZZZZ";
    let run = Copy::empty();
    run.write(
        ".phase-status.json",
        &format!("{{\"owning_skill\": \"{secret}\u{1b}\", \"k\": [1, 2,\n"),
    );
    let report = assert_fails("heroku-decide", run.path());
    assert!(
        report.contains(".phase-status.json is not valid JSON"),
        "{report}"
    );
    assert!(!report.contains(secret), "{report}");
    assert!(!report.contains("FAKE-SECRET"), "{report}");
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_run_directory_is_refused_not_followed() {
    use std::os::unix::fs::symlink;
    let run = Copy::of("heroku-decision-gate", "after-decide-complete");
    let real = run.path().join("real-status.json");
    fs::rename(run.path().join(".phase-status.json"), &real).expect("move status");
    symlink(&real, run.path().join(".phase-status.json")).expect("link");
    let report = assert_fails("heroku-decide", run.path());
    assert!(
        report.contains("symbolic links are not followed"),
        "{report}"
    );

    // A linked directory component is refused as well.
    let scratch = TempDir::new("check-expected-linkdir");
    let outside = scratch.dir("outside");
    copy_dir(&golden("heroku-workshop", "after-arm64-reprice"), &outside);
    let linked = scratch.join("linked");
    symlink(&outside, &linked).expect("link dir");
    fs::rename(outside.join("scenarios"), outside.join("scenarios-real")).expect("move scenarios");
    symlink(outside.join("scenarios-real"), outside.join("scenarios")).expect("link scenarios");
    let report = assert_fails("heroku-workshop", &outside);
    assert!(
        report.contains("symbolic links are not followed"),
        "{report}"
    );
}

/// A malformed expectation document fails the check instead of passing it.
#[test]
fn a_malformed_expectation_document_fails_closed() {
    let scratch = TempDir::new("check-expected-spec");
    let root = scratch.dir("plugin");
    let set = root.join("fixtures/heroku-workshop");
    copy_dir(&golden("heroku-workshop", "seed"), &set.join("seed"));
    fs::write(
        set.join("expected-workshop.json"),
        r#"{"min_scenarios": "two"}"#,
    )
    .expect("write spec");
    let run = Copy::of("heroku-workshop", "after-arm64-reprice");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "check-expected",
        "heroku-workshop",
        path_text(run.path()),
        "--plugin-root",
        path_text(&root),
    ]);
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(
        stdout.contains("expectation document is malformed"),
        "{stdout}"
    );

    // Not JSON at all, and absent.
    fs::write(set.join("expected-workshop.json"), "{not json").expect("write spec");
    let (status, stdout, _) = exec(&[
        "startup",
        "check-expected",
        "heroku-workshop",
        path_text(run.path()),
        "--plugin-root",
        path_text(&root),
    ]);
    assert_eq!(status, 1);
    assert!(
        stdout.contains("expected-workshop.json is not valid JSON"),
        "{stdout}"
    );
    fs::remove_file(set.join("expected-workshop.json")).expect("remove spec");
    let (status, stdout, _) = exec(&[
        "startup",
        "check-expected",
        "heroku-workshop",
        path_text(run.path()),
        "--plugin-root",
        path_text(&root),
    ]);
    assert_eq!(status, 1);
    assert!(
        stdout.contains("missing expected-workshop.json"),
        "{stdout}"
    );
}

/// The node at `path` (object keys, array indexes), mutably.
pub(super) fn step_mut<'a>(doc: &'a mut Json, path: &[&str]) -> &'a mut Json {
    let mut node = doc;
    for segment in path {
        node = step(node, segment);
    }
    node
}

#[test]
fn an_anchored_path_keeps_its_root_and_refuses_one_outside_it() {
    use super::Anchored;
    let root = TempDir::new("anchored-root");
    fs::create_dir_all(root.join("a/b")).expect("tree");
    let derived = Anchored::root(root.to_path_buf()).join("a").join("b");
    assert_eq!(derived.check(&derived.join("file.json")), Ok(()));
    // An absolute name replaces the joined path; it is not below the anchor.
    let escaped = derived.join("/etc/hostname");
    let error = escaped.check(&escaped).expect_err("outside its anchor");
    assert!(error.contains("not below"), "{error}");
}

#[test]
fn the_design_reader_names_every_malformed_shape() {
    let ledger = super::ledger::Ledger::default();
    assert!(ledger.design_shape(&j(r#"{"name": "no services"}"#), "d.json"));
    assert!(ledger.design_shape(&j(r#"{"services": [{"service_id": "x"}]}"#), "d.json"));
    assert!(ledger.fails().is_empty());
    for (doc, needle) in [
        ("[]", "d.json is list, not an object"),
        (
            r#"{"services": null}"#,
            "d.json services is NoneType, not a list",
        ),
        (
            r#"{"services": [{}, null, 1]}"#,
            "d.json services[1] is NoneType, not an object",
        ),
        (
            r#"{"services": [{}, null, 1]}"#,
            "d.json services[2] is int, not an object",
        ),
    ] {
        let ledger = super::ledger::Ledger::default();
        assert!(!ledger.design_shape(&j(doc), "d.json"), "{doc}");
        assert!(
            ledger.fails().iter().any(|fail| fail == needle),
            "{doc}: {:?}",
            ledger.fails()
        );
    }
}
