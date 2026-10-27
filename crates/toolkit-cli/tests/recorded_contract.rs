//! Workspace contract for slices that replay recorded AWS calls (#4727).
//!
//! Every slice without a live AWS client used to hand-write its recorded
//! client and its response accessors, and reviews kept finding the same
//! defects: a wrong-typed or `null` recorded member defaulted (#4712, #4724),
//! a `null` response list read as an empty discovery (#4713, #4725), a failure
//! step whose expected request was never compared (#4726). The rules now live
//! once in `toolkit_aws::recorded`; this test keeps the slices on them.
//!
//! A *recorded slice* is a top-level module of `crates/toolkit-cli/src` (the
//! directory and its `<name>.rs`) whose non-test code defines a
//! `RecordedClient` or uses `toolkit_aws::recorded`. For each one:
//!
//! 1. a slice that defines a `RecordedClient` uses `toolkit_aws::recorded`;
//! 2. it defines none of the shared helpers itself (`optional_member`,
//!    `optional_list`, `optional_object`, `optional_text`, `same_json`, ...);
//! 3. it reads no array or object member ad hoc: `Some(Json::Array(..))`,
//!    `Some(Json::Object(..))`, `x @ Json::Array(..)`, `x @ Json::Object(..)`
//!    and `as_array` / `as_object` are refused unless the line, or the comment
//!    block directly above it, carries the marker `recorded-shape:` with the
//!    reason (a required member that rejects every other shape, or a value
//!    that is not a response: a snapshot file, the command's own manifest).
//!
//! Test code (`tests.rs`, `tests/`) is out of scope. The README section
//! "Slices without a live AWS client" is the guide for slice authors.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The shared module every recorded slice reads through.
const SHARED_MODULE: &str = "toolkit_aws::recorded";
/// What makes a slice a recorded slice even before it uses the module.
const RECORDED_CLIENT: &str = "struct RecordedClient";
/// The allow-list marker; the text after it is the reason.
const MARKER: &str = "recorded-shape:";
/// Ad-hoc reads of an array or object member.
const AD_HOC: [&str; 6] = [
    "Some(Json::Array(",
    "Some(Json::Object(",
    "@ Json::Array(",
    "@ Json::Object(",
    "as_array",
    "as_object",
];
/// Helpers a recorded slice must take from the shared module, never redefine.
const SHARED_HELPERS: [&str; 8] = [
    "fn optional_member",
    "fn optional_list",
    "fn optional_object",
    "fn optional_object_list",
    "fn optional_string_list",
    "fn optional_text",
    "fn nullable_text",
    "fn same_json",
];

struct SourceFile {
    /// Path relative to `src`, with `/` separators.
    relative: String,
    text: String,
}

fn collect(dir: &Path, root: &Path, out: &mut Vec<SourceFile>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path
                .strip_prefix(root)
                .expect("under src")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            out.push(SourceFile { relative, text });
        }
    }
}

/// Every non-test source file of `crates/toolkit-cli/src`.
fn sources() -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.retain(|file| !is_test_file(&file.relative));
    files
}

fn is_test_file(relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    parts.last() == Some(&"tests.rs") || parts[..parts.len() - 1].contains(&"tests")
}

/// The top-level module a file belongs to: `ai_models/hub.rs` and
/// `ai_models.rs` are both `ai_models`.
fn slice_of(relative: &str) -> &str {
    let first = relative.split('/').next().unwrap_or(relative);
    first.strip_suffix(".rs").unwrap_or(first)
}

fn recorded_slices(files: &[SourceFile]) -> BTreeSet<&str> {
    files
        .iter()
        .filter(|file| file.text.contains(SHARED_MODULE) || file.text.contains(RECORDED_CLIENT))
        .map(|file| slice_of(&file.relative))
        .collect()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// The lines (1-based number, trimmed text) of `source` that read an array or
/// object member ad hoc without the allow-list marker.
fn ad_hoc_accesses(source: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if is_comment(line) || !AD_HOC.iter().any(|pattern| line.contains(pattern)) {
            continue;
        }
        let allowed = line.contains(MARKER)
            || lines[..index]
                .iter()
                .rev()
                .take_while(|above| is_comment(above))
                .any(|above| above.contains(MARKER));
        if !allowed {
            found.push((index + 1, line.trim().to_string()));
        }
    }
    found
}

/// The shared helpers `source` defines itself.
fn redefined_helpers(source: &str) -> Vec<&'static str> {
    SHARED_HELPERS
        .into_iter()
        .filter(|helper| {
            source.lines().any(|line| {
                !is_comment(line)
                    && line
                        .split_once(helper)
                        .is_some_and(|(_, rest)| rest.starts_with('(') || rest.starts_with('<'))
            })
        })
        .collect()
}

#[test]
fn the_known_recorded_slices_are_in_scope() {
    let files = sources();
    let slices = recorded_slices(&files);
    for slice in ["ai_models", "bedrock_agent"] {
        assert!(
            slices.contains(slice),
            "{slice} is no longer detected as a recorded slice: {slices:?}"
        );
    }
}

#[test]
fn every_recorded_client_uses_the_shared_module() {
    let files = sources();
    let mut offenders = Vec::new();
    for slice in recorded_slices(&files) {
        let in_slice = || {
            files
                .iter()
                .filter(|file| slice_of(&file.relative) == slice)
        };
        let defines_client = in_slice().any(|file| file.text.contains(RECORDED_CLIENT));
        let uses_module = in_slice().any(|file| file.text.contains(SHARED_MODULE));
        if defines_client && !uses_module {
            offenders.push(slice);
        }
    }
    assert!(
        offenders.is_empty(),
        "these slices define a RecordedClient without `{SHARED_MODULE}` (#4727): {offenders:?}. \
         Parse steps with `step_fields`, replay them with `Step`/`StepQueue` and read members \
         with the shared accessors; see the README section on slices without a live AWS client."
    );
}

#[test]
fn recorded_slices_define_no_helper_of_their_own() {
    let files = sources();
    let slices = recorded_slices(&files);
    let offenders: Vec<String> = files
        .iter()
        .filter(|file| slices.contains(slice_of(&file.relative)))
        .flat_map(|file| {
            redefined_helpers(&file.text)
                .into_iter()
                .map(move |helper| format!("{}: {helper}", file.relative))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "recorded slices must use the `{SHARED_MODULE}` helpers instead of their own copies \
         (#4727):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn recorded_slices_read_members_through_the_shared_helpers() {
    let files = sources();
    let slices = recorded_slices(&files);
    let offenders: Vec<String> = files
        .iter()
        .filter(|file| slices.contains(slice_of(&file.relative)))
        .flat_map(|file| {
            ad_hoc_accesses(&file.text)
                .into_iter()
                .map(move |(line, text)| format!("{}:{line}: {text}", file.relative))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "ad-hoc array/object member access in a recorded slice (#4727):\n{}\n\
         Read an optional response member with `{SHARED_MODULE}::optional_list`, \
         `optional_object` or `optional_text` (absent or [] is empty; null or another type is \
         an invalid response). If the value is not a response, or the member is required and \
         every other shape is rejected, put `// {MARKER} <reason>` on the line or directly above.",
        offenders.join("\n")
    );
}

#[test]
fn the_scanner_flags_ad_hoc_reads_and_honours_the_marker() {
    let source = "\
fn f(response: &Json) {
    if let Some(Json::Array(items)) = response.get(\"Items\") {}
    let x = value.as_array();
    match response.get(\"A\") { Some(a @ Json::Object(_)) => {} _ => {} }
    // recorded-shape: a required member, every other shape is rejected.
    let Some(Json::Object(m)) = response.get(\"M\") else { return };
    // recorded-shape: a comment block
    // that spans two lines.
    let Some(Json::Array(n)) = response.get(\"N\") else { return };
    let Some(Json::Array(o)) = response.get(\"O\") else { return }; // recorded-shape: inline
    // recorded-shape: only covers the next code line.
    let a = 1;
    let Some(Json::Object(p)) = response.get(\"P\") else { return };
    // Some(Json::Array(in a comment)) is not code.
    let items = optional_list(response, \"Items\")?;
    let built = Json::Array(Vec::new());
}
";
    let lines: Vec<usize> = ad_hoc_accesses(source)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    assert_eq!(lines, vec![2, 3, 4, 13]);
}

#[test]
fn the_scanner_finds_redefined_helpers_and_scopes_slices() {
    let source = "\
use toolkit_aws::recorded::optional_list;
fn optional_member<'j, T>() {}
pub(crate) fn same_json(a: &Json, b: &Json) -> bool { a == b }
// fn optional_text(...) in a comment is not a definition
fn optional_list_len() {}
";
    assert_eq!(
        redefined_helpers(source),
        vec!["fn optional_member", "fn same_json"]
    );
    assert_eq!(slice_of("ai_models/hub.rs"), "ai_models");
    assert_eq!(slice_of("bedrock_agent.rs"), "bedrock_agent");
    assert!(is_test_file("ai_models/tests/hub.rs"));
    assert!(is_test_file("bedrock_agent/tests.rs"));
    assert!(!is_test_file("ai_models/hub.rs"));
}
