//! Tests for `toolkit startup artifacts` (#4399): the migrated cases of
//! `tests/test_validate_artifacts.py`, a positive and a negative fixture for
//! every reader, matcher and check, and runs against the real plugin tree.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::Json;

use super::contracts::{artifact_name, build_shape_contract, parse_block, scan_doc, Env};
use super::jsonc::{remove_trailing_commas, replace_lone_ellipsis, strip_jsonc};
use super::patterns::{
    enum_from_comment, enum_from_string, fnmatch, heading_matches, heading_token, is_placeholder,
    mentions_joined_set,
};
use super::pyjson::{dumps, loads};
use super::schema::{unsupported_keywords, Context, SubsetValidator};
use super::shape::{
    build_node, mark_allowed_anywhere, merge_example_at, validate_shape, walk_path, KeyComments,
    Node,
};
use super::{apply_baseline, Code, Finding};
use crate::run;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-artifacts-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture dir");
    fs::write(path, content).expect("write fixture");
}

fn real_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy target");
    for entry in fs::read_dir(from).expect("read copy source") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

/// Runs the CLI in-process; returns (status, stdout, stderr).
fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = run(&args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn text(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

fn json(source: &str) -> Json {
    loads(source).expect("valid fixture JSON")
}

/// The `(code, path)` pairs of the `errors` array of a `--json` report.
fn error_pairs(stdout: &str) -> Vec<(String, String)> {
    let report = json(stdout);
    let Some(Json::Array(errors)) = report.get("errors") else {
        panic!("no errors array in {stdout}");
    };
    errors
        .iter()
        .map(|e| {
            (
                e.get("code")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                e.get("path")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect()
}

// ---- Python-compatible JSON -------------------------------------------------

#[test]
fn loads_keeps_first_position_and_last_value_of_a_duplicate_key() {
    let value = json(r#"{"a": 1, "b": 2, "a": 3}"#);
    assert_eq!(dumps(&value, None, false), r#"{"a": 3, "b": 2}"#);
}

#[test]
fn loads_accepts_python_constants_and_rejects_the_rest() {
    assert!(matches!(json("NaN"), Json::Float(n) if n.is_nan()));
    assert!(matches!(json("-Infinity"), Json::Float(n) if n == f64::NEG_INFINITY));
    assert_eq!(
        json("[1, 2.5, -0, 1e2]"),
        Json::Array(vec![
            Json::Int(1),
            Json::Float(2.5),
            Json::Int(0),
            Json::Float(100.0),
        ])
    );
    let cases = [
        ("", "Expecting value"),
        ("{", "Expecting property name enclosed in double quotes"),
        (r#"{"a" 1}"#, "Expecting ':' delimiter"),
        (r#"{"a": 1 "b": 2}"#, "Expecting ',' delimiter"),
        (
            r#"{"a": 1,}"#,
            "Expecting property name enclosed in double quotes",
        ),
        ("[1,]", "Expecting value"),
        ("[1 2]", "Expecting ',' delimiter"),
        ("1 2", "Extra data"),
        ("01", "Extra data"),
        (r#""abc"#, "Unterminated string starting at"),
        ("\"a\nb\"", "Invalid control character at"),
        (r#""\q""#, "Invalid \\escape"),
        (r#""\u12G4""#, "Invalid \\uXXXX escape"),
        (
            "\u{feff}{}",
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
        ),
        ("tru", "Expecting value"),
    ];
    for (source, message) in cases {
        let error = loads(source).expect_err(source);
        assert_eq!(error.msg, message, "{source:?}");
    }
}

#[test]
fn loads_reports_python_line_and_column() {
    let error = loads("{\n  \"a\": 1\n  \"b\": 2\n}").expect_err("missing comma");
    assert_eq!((error.lineno, error.colno), (3, 3));
    assert_eq!(
        error.to_string(),
        "Expecting ',' delimiter: line 3 column 3 (char 13)"
    );
}

#[test]
fn loads_decodes_surrogate_pairs_and_replaces_a_lone_surrogate() {
    assert_eq!(json(r#""😀""#), Json::string("\u{1f600}"));
    assert_eq!(json(r#""\ud800x""#), Json::string("\u{fffd}x"));
}

#[test]
fn loads_refuses_runaway_nesting() {
    let deep = format!("{}{}", "[".repeat(600), "]".repeat(600));
    assert_eq!(
        loads(&deep).expect_err("too deep").msg,
        "maximum recursion depth exceeded"
    );
}

#[test]
fn dumps_matches_python_formatting() {
    let value = json(r#"{"b": [1, 2.0, "é\n"], "a": {}, "c": []}"#);
    assert_eq!(
        dumps(&value, None, false),
        r#"{"b": [1, 2.0, "é\n"], "a": {}, "c": []}"#
    );
    assert_eq!(
        dumps(&value, None, true),
        r#"{"a": {}, "b": [1, 2.0, "é\n"], "c": []}"#
    );
    assert_eq!(
        dumps(&json(r#"{"a": [1, {"b": null}]}"#), Some(2), false),
        "{\n  \"a\": [\n    1,\n    {\n      \"b\": null\n    }\n  ]\n}"
    );
    assert_eq!(dumps(&Json::string("\u{1f600}"), None, false), r#""😀""#);
}

// ---- JSONC stripping ----------------------------------------------------------

fn comment_at(stripped: &super::jsonc::Stripped, line: usize) -> Option<&str> {
    stripped
        .comments
        .iter()
        .find(|(l, _)| *l == line)
        .map(|(_, c)| c.as_str())
}

#[test]
fn strip_jsonc_removes_comments_outside_strings_only() {
    let stripped =
        strip_jsonc("{\n  \"url\": \"https://x/y\", // keep the url\n  \"n\": 1, // REQUIRED\n}");
    assert_eq!(
        json(&stripped.json),
        json(r#"{"url": "https://x/y", "n": 1}"#)
    );
    assert_eq!(comment_at(&stripped, 1), Some("keep the url"));
    assert_eq!(stripped.keys.get(&1).map(String::as_str), Some("url"));
    assert_eq!(comment_at(&stripped, 2), Some("REQUIRED"));
    assert_eq!(stripped.keys.get(&2).map(String::as_str), Some("n"));
}

#[test]
fn strip_jsonc_handles_ellipsis_and_bool_alternation() {
    let stripped = strip_jsonc(r#"{"a": ["Q1", ...], "b": true|false}"#);
    assert_eq!(
        json(&stripped.json),
        json(r#"{"a": ["Q1", "..."], "b": true}"#)
    );
    // Neither rewrite touches quoted or dotted text.
    let kept = strip_jsonc(r#"{"a": "...", "b": "x....y", "c": "true|false"}"#);
    assert_eq!(
        kept.json,
        r#"{"a": "...", "b": "x....y", "c": "true|false"}"#
    );
}

#[test]
fn strip_jsonc_binds_a_trailing_comment_to_the_enclosing_field() {
    let stripped =
        strip_jsonc("{\n  \"call_sites\": [{ \"file\": \"app.py\", \"line\": 1 }], // REQUIRED\n}");
    assert_eq!(comment_at(&stripped, 1), Some("REQUIRED"));
    assert_eq!(
        stripped.keys.get(&1).map(String::as_str),
        Some("call_sites")
    );
}

#[test]
fn strip_jsonc_binds_to_the_last_of_several_flat_sibling_keys() {
    let stripped = strip_jsonc("{\n  \"a\": 1, \"b\": 2 // REQUIRED\n}");
    assert_eq!(stripped.keys.get(&1).map(String::as_str), Some("b"));
}

#[test]
fn strip_jsonc_keeps_the_pending_key_across_whitespace_before_the_colon() {
    for separator in [" ", "\t", "  "] {
        let stripped = strip_jsonc(&format!(
            "{{\n  \"priority\"{separator}: \"balanced\", // REQUIRED\n}}"
        ));
        assert_eq!(comment_at(&stripped, 1), Some("REQUIRED"));
        assert_eq!(stripped.keys.get(&1).map(String::as_str), Some("priority"));
    }
}

#[test]
fn strip_jsonc_drops_a_pending_key_followed_by_a_real_token() {
    let stripped = strip_jsonc("{\n  \"a\" junk \"b\": 1 // REQUIRED\n}");
    assert_eq!(stripped.keys.get(&1).map(String::as_str), Some("b"));
}

#[test]
fn strip_jsonc_keeps_an_open_value_key_one_level_shallower() {
    let stripped = strip_jsonc("{\n  \"workloads\": [ // REQUIRED\n    1\n  ]\n}");
    assert_eq!(stripped.keys.get(&1).map(String::as_str), Some("workloads"));
}

#[test]
fn trailing_commas_are_removed_before_a_closing_bracket_only() {
    assert_eq!(remove_trailing_commas("[1, 2, ]"), "[1, 2 ]");
    assert_eq!(remove_trailing_commas("{\"a\": 1,\n}"), "{\"a\": 1\n}");
    assert_eq!(remove_trailing_commas("[1, 2]"), "[1, 2]");
}

#[test]
fn a_lone_ellipsis_line_becomes_a_member() {
    assert_eq!(
        replace_lone_ellipsis("{\n  \"a\": 1,\n  \"...\"\n}"),
        "{\n  \"a\": 1,\n  \"...\": \"...\"\n}"
    );
    assert_eq!(
        replace_lone_ellipsis("{\n  \"...\",\n  \"b\": 1\n}"),
        "{\n  \"...\": \"...\"\n  \"b\": 1\n}"
    );
    // Text before the token on its line, or after it, is not a lone ellipsis.
    assert_eq!(replace_lone_ellipsis("{ \"...\" }"), "{ \"...\" }");
    assert_eq!(
        replace_lone_ellipsis("[\n  \"...\" x\n]"),
        "[\n  \"...\" x\n]"
    );
}

// ---- pattern ports -----------------------------------------------------------

#[test]
fn placeholders_and_enum_tokens_follow_the_python_patterns() {
    assert!(is_placeholder("<ISO 8601>"));
    assert!(is_placeholder("<>"));
    assert!(!is_placeholder("<a\nb>"));
    assert!(!is_placeholder("<unterminated"));
    assert_eq!(
        enum_from_string("a | b-c|d.e"),
        Some(
            ["a", "b-c", "d.e"]
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        )
    );
    assert_eq!(enum_from_string("only one"), None);
    assert_eq!(enum_from_string("has space|b"), None);
}

#[test]
fn comment_enums_cover_bare_and_quoted_members() {
    let set = |items: &[&str]| -> Option<BTreeSet<String>> {
        Some(items.iter().map(|s| (*s).to_string()).collect())
    };
    assert_eq!(
        enum_from_comment("network | data | compute"),
        set(&["network", "data", "compute"])
    );
    assert_eq!(
        enum_from_comment("\"fast_path\" | \"wizard\" — which flow"),
        set(&["fast_path", "wizard"])
    );
    assert_eq!(
        enum_from_comment("\"full\" | \"simplified_live\" (live-only)"),
        set(&["full", "simplified_live"])
    );
    assert_eq!(enum_from_comment("see: `a` | `b`"), set(&["a", "b"]));
    assert_eq!(
        enum_from_comment("\"not an enum\" | \"also prose here\""),
        None
    );
    assert_eq!(enum_from_comment("just prose"), None);
    assert_eq!(enum_from_comment("a | b trailing words"), None);
    // The longest token is tried first; a shorter one is used when only it can
    // be followed by the tail.
    assert_eq!(enum_from_comment("a | b- (x)"), set(&["a", "b-"]));
    assert_eq!(enum_from_comment("a | b- end"), set(&["a", "b"]));
}

#[test]
fn joined_set_qualifier_is_recognised_in_both_spellings() {
    assert!(mentions_joined_set(
        "terraform | live (or a \"+\"-joined set)"
    ));
    assert!(mentions_joined_set("members joined with +"));
    assert!(mentions_joined_set("A + JOINED set"));
    assert!(!mentions_joined_set("table | rubric"));
    assert!(!mentions_joined_set("joined by comma"));
}

#[test]
fn heading_tokens_and_matches() {
    assert_eq!(heading_token("`services[]`").as_deref(), Some("services[]"));
    assert_eq!(heading_token("`a.b` (REQUIRED)").as_deref(), Some("a.b"));
    assert_eq!(heading_token("metadata notes").as_deref(), Some("metadata"));
    assert_eq!(heading_token("Contract: details"), None);
    assert_eq!(heading_token("``").as_deref(), None);
    assert!(heading_matches(
        "`graviton` block (added to each compute service in `aws-design.json`)",
        "graviton"
    ));
    assert!(!heading_matches(
        "`graviton_profile` (emitted by Discover, one entry per compute service)",
        "graviton"
    ));
    assert!(heading_matches(
        "`graviton_profile` (emitted by Discover)",
        "graviton_profile"
    ));
    assert!(heading_matches("Shape", "Shape"));
    assert!(!heading_matches("Shapes", "Shape"));
    assert!(heading_matches(
        "Cost tiers (`projected_costs` / `cost_comparison`)",
        "Cost tiers"
    ));
}

#[test]
fn fnmatch_follows_python_including_bracket_classes() {
    assert!(fnmatch("resources[3].id", "resources[[]*[]].id"));
    assert!(!fnmatch("resources.id", "resources[[]*[]].id"));
    assert!(fnmatch(
        "fixtures/gcp-a/x-gcp-resource-inventory.json",
        "fixtures/gcp-*/*gcp-resource-inventory.json"
    ));
    assert!(fnmatch("a/b/c", "*"));
    assert!(fnmatch("abc", "a?c"));
    assert!(!fnmatch("abd", "a?c"));
    assert!(fnmatch("b", "[a-c]"));
    assert!(!fnmatch("d", "[a-c]"));
    assert!(fnmatch("d", "[!a-c]"));
    assert!(fnmatch("x[", "x["));
    assert!(!fnmatch("AB", "ab"));
}

#[test]
fn artifact_names_normalise_scenario_numbers() {
    let name = |p: &str| artifact_name(Path::new(p));
    assert_eq!(
        name("run/scenarios/scenario-001.preferences.json"),
        "scenarios/scenario-NNN.preferences.json"
    );
    assert_eq!(name("run/scenario-12.x.json"), "scenario-NNN.x.json");
    assert_eq!(name("run/preferences.json"), "preferences.json");
    assert_eq!(name("run/scenarios/other.json"), "scenarios/other.json");
}

// ---- template inference --------------------------------------------------------

fn node_of(example: &str) -> Node {
    let stripped = strip_jsonc(example);
    let mut comments = KeyComments::new();
    for (line, comment) in &stripped.comments {
        if let Some(key) = stripped.keys.get(line).filter(|k| !k.is_empty()) {
            comments
                .entry(key.clone())
                .or_default()
                .push(comment.clone());
        }
    }
    build_node(&json(&stripped.json), &comments, "test")
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn enums_come_from_pipe_strings_and_comments() {
    let n = node_of(
        "{\"status\": \"full|reduced\", \"tier\": \"compute\", // network | data | compute\n}",
    );
    assert_eq!(
        n.keys["status"].enum_values,
        Some(set(&["full", "reduced"]))
    );
    assert_eq!(
        n.keys["tier"].enum_values,
        Some(set(&["network", "data", "compute"]))
    );
    let quoted = node_of("{\"mode\": \"wizard\", // \"fast_path\" | \"wizard\" — which flow\n \"clustering_mode\": \"full\", // \"full\" | \"simplified_live\" (live-only)\n}");
    assert_eq!(
        quoted.keys["mode"].enum_values,
        Some(set(&["fast_path", "wizard"]))
    );
    assert_eq!(
        quoted.keys["clustering_mode"].enum_values,
        Some(set(&["full", "simplified_live"]))
    );
    let prose = node_of("{\"note\": \"x\", // \"not an enum\" | \"also prose here\"\n}");
    assert_eq!(prose.keys["note"].enum_values, None);
}

#[test]
fn a_comment_enum_that_contradicts_the_example_is_ignored() {
    let n = node_of("{\"mode\": \"other\", // a | b\n}");
    assert_eq!(n.keys["mode"].enum_values, None);
}

#[test]
fn required_comments_and_placeholders() {
    let n = node_of("{\"id\": \"<uuid>\", // REQUIRED\n \"n\": \"<Q13 value>\"}");
    assert!(n.required.contains("id"));
    assert!(n.keys["n"].is_any());
    let conditional = node_of("{\"a\": 1, // required when x\n \"b\": 1, // ALWAYS present\n \"c\": 1, // NOT REQUIRED\n}");
    assert_eq!(conditional.required, set(&["b"]));
}

#[test]
fn nullable_comments_add_null() {
    let n = node_of("{\"a\": \"x\", // string or null\n \"b\": \"y\", // nullable\n \"c\": \"z\"}");
    assert!(n.keys["a"].kinds.contains("null"));
    assert!(n.keys["b"].kinds.contains("null"));
    assert!(!n.keys["c"].kinds.contains("null"));
}

#[test]
fn a_placeholder_key_makes_the_object_open_with_an_any_key_template() {
    let n = node_of("{\"<key>\": {\"value\": 1, \"chosen_by\": \"user|default\"}}");
    assert!(n.wildcard);
    let template = n.any_key.expect("template");
    assert_eq!(
        template.keys.keys().cloned().collect::<BTreeSet<_>>(),
        set(&["value", "chosen_by"])
    );
}

// ---- shape validation ------------------------------------------------------------

fn findings_for(example: &str, instance: &str, allowed: &[&str]) -> BTreeSet<(String, String)> {
    let mut n = node_of(example);
    if !allowed.is_empty() {
        mark_allowed_anywhere(&mut n, &set(allowed));
    }
    check(&n, instance)
}

fn check(n: &Node, instance: &str) -> BTreeSet<(String, String)> {
    let mut out = Vec::new();
    validate_shape(n, &json(instance), "", "art.json", "contract", &mut out);
    out.into_iter()
        .map(|f| (f.code.as_str().to_string(), f.path))
        .collect()
}

fn pair(code: &str, path: &str) -> (String, String) {
    (code.to_string(), path.to_string())
}

#[test]
fn unknown_keys_are_reported_but_underscore_keys_are_not() {
    let found = findings_for(
        r#"{"a": 1, "b": "x"}"#,
        r#"{"a": 1, "b": "x", "c": 2, "_comment": "ok"}"#,
        &[],
    );
    assert_eq!(found, BTreeSet::from([pair("UNKNOWN_KEY", "c")]));
}

#[test]
fn missing_required_type_mismatch_and_enum_violation() {
    let example = "{\"a\": 1, // REQUIRED\n \"s\": \"full|reduced\", \"arr\": [{\"k\": true}]}";
    let found = findings_for(example, r#"{"s": "partial", "arr": [{"k": "no"}]}"#, &[]);
    assert!(found.contains(&pair("MISSING_REQUIRED", "a")));
    assert!(found.contains(&pair("ENUM_VIOLATION", "s")));
    assert!(found.contains(&pair("TYPE_MISMATCH", "arr[0].k")));
}

#[test]
fn messages_name_the_kinds_and_the_allowed_values() {
    let n = node_of(r#"{"a": 1, "s": "full|reduced"}"#);
    let mut out = Vec::new();
    validate_shape(
        &n,
        &json(r#"{"a": "x", "s": "z"}"#),
        "",
        "art.json",
        "contract",
        &mut out,
    );
    let messages: Vec<&str> = out.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "is string, contract example is number",
            "'z' not in {full, reduced}"
        ]
    );
}

#[test]
fn null_is_tolerated_and_open_objects_accept_anything() {
    let found = findings_for(
        r#"{"a": 1, "cfg": {"...": "..."}}"#,
        r#"{"a": null, "cfg": {"anything": [1, 2]}}"#,
        &[],
    );
    assert!(found.is_empty());
}

#[test]
fn an_any_key_template_applies_to_named_children() {
    let example = r#"{"rows": {"<key>": {"value": "x", "chosen_by": "user|default"}}}"#;
    let found = findings_for(
        example,
        r#"{"rows": {"q1": {"value": "y", "chosen_by": "user", "extra": 1}}}"#,
        &[],
    );
    assert_eq!(
        found,
        BTreeSet::from([pair("UNKNOWN_KEY", "rows.q1.extra")])
    );
}

#[test]
fn an_empty_array_item_is_replaced_when_a_concrete_shape_arrives() {
    let mut seed = node_of(r#"{"services": [], "workloads": []}"#);
    seed.merge(&node_of(
        "{\"services\": [{\"routing_provenance\": \"table\", // REQUIRED\n \"n\": 1}]}",
    ));
    let item = seed.keys["services"].item.as_deref().expect("item");
    assert!(!item.is_any() && item.kinds.contains("object"));
    assert!(item.required.contains("routing_provenance"));
    seed.merge(&node_of(r#"{"services": []}"#));
    assert!(!seed.keys["services"]
        .item
        .as_deref()
        .expect("item")
        .is_any());

    let mut root = node_of(r#"{"services": [], "workloads": []}"#);
    root.merge(&node_of("{\"services\": [{\"routing_provenance\": \"table\", // REQUIRED\n \"n\": 1}], \"workloads\": [{\"workload_id\": \"wl\"}]}"));
    assert!(check(&root, r#"{"services": [], "workloads": []}"#).is_empty());
    assert!(check(&root, r#"{"services": [{"routing_provenance": "table", "n": 1}], "workloads": [{"workload_id": "wl"}]}"#).is_empty());
    assert!(check(&root, r#"{"services": [null]}"#).is_empty());
    for scalar in [r#""not-an-object""#, "17", "false", "[]", r#"["nested"]"#] {
        let found = check(
            &root,
            &format!(r#"{{"services": [{scalar}], "workloads": []}}"#),
        );
        assert!(
            found.contains(&pair("TYPE_MISMATCH", "services[0]")),
            "{scalar}"
        );
    }
    assert!(check(&root, r#"{"services": [], "workloads": [42]}"#)
        .contains(&pair("TYPE_MISMATCH", "workloads[0]")));
    assert!(check(&root, r#"{"services": [{}], "workloads": []}"#)
        .contains(&pair("MISSING_REQUIRED", "services[0].routing_provenance")));
    let placeholder = node_of(r#"{"ids": ["<azure_id>"]}"#);
    assert!(placeholder.keys["ids"]
        .item
        .as_deref()
        .expect("item")
        .is_any());
    assert!(check(&placeholder, r#"{"ids": [1, "x", false]}"#).is_empty());
}

#[test]
fn allowed_anywhere_vocabulary_is_accepted_at_every_level() {
    let example = r#"{"g": {"r": {"value": 1}}}"#;
    let instance = r#"{"g": {"r": {"value": 1, "source": "live"}}}"#;
    assert_eq!(
        findings_for(example, instance, &[]),
        BTreeSet::from([pair("UNKNOWN_KEY", "g.r.source")])
    );
    assert!(findings_for(example, instance, &["source"]).is_empty());
}

#[test]
fn a_declared_enum_survives_a_merge_with_a_concrete_example() {
    let mut declared = node_of(r#"{"confidence": "full|reduced"}"#);
    declared.merge(&node_of(r#"{"confidence": "full"}"#));
    assert_eq!(
        declared.keys["confidence"].enum_values,
        Some(set(&["full", "reduced"]))
    );
    let mut example = node_of(r#"{"confidence": "full"}"#);
    example.merge(&node_of(r#"{"confidence": "full|reduced"}"#));
    assert_eq!(
        example.keys["confidence"].enum_values,
        Some(set(&["full", "reduced"]))
    );
    declared.merge(&node_of(r#"{"confidence": "partial"}"#));
    assert_eq!(
        declared.keys["confidence"].enum_values,
        Some(set(&["full", "reduced", "partial"]))
    );
    declared.merge(&node_of(
        r#"{"confidence": "Using cached prices from 2026-03-04 (±5%)"}"#,
    ));
    assert_eq!(declared.keys["confidence"].enum_values, None);
}

#[test]
fn merged_children_are_copies_so_a_named_refinement_does_not_leak() {
    let mut root = node_of(
        r#"{"design_constraints": {"<key>": {"value": "<v>", "chosen_by": "user|default|extracted"}}}"#,
    );
    let target = walk_path(&mut root, "design_constraints.cpu_architecture");
    target.merge(&node_of(r#"{"value": "graviton", "chosen_by": "default"}"#));
    let generic = root.keys["design_constraints"]
        .any_key
        .as_deref()
        .expect("template")
        .keys["chosen_by"]
        .enum_values
        .clone();
    assert_eq!(generic, Some(set(&["user", "default", "extracted"])));
    let found = check(
        &root,
        r#"{"design_constraints": {"target_region": {"value": "us-east-1", "chosen_by": "invalid-origin"}, "cpu_architecture": {"value": "graviton", "chosen_by": "bogus"}}}"#,
    );
    assert_eq!(
        found,
        BTreeSet::from([
            pair(
                "ENUM_VIOLATION",
                "design_constraints.target_region.chosen_by"
            ),
            pair(
                "ENUM_VIOLATION",
                "design_constraints.cpu_architecture.chosen_by"
            ),
        ])
    );
}

#[test]
fn a_joined_set_allows_plus_joined_members_but_not_unknown_ones() {
    let example = "{\"source\": \"terraform\", // terraform | bicep | arm | live | rdfa | billing (or a \"+\"-joined set)\n \"prov\": \"table\", // table | rubric\n}";
    let n = node_of(example);
    assert_eq!(n.keys["source"].enum_join.as_deref(), Some("+"));
    assert_eq!(n.keys["prov"].enum_join, None);
    assert!(findings_for(
        example,
        r#"{"source": "live+terraform", "prov": "table"}"#,
        &[]
    )
    .is_empty());
    assert!(findings_for(example, r#"{"source": "terraform", "prov": "table"}"#, &[]).is_empty());
    assert_eq!(
        findings_for(
            example,
            r#"{"source": "terraform+bogus", "prov": "table"}"#,
            &[]
        ),
        BTreeSet::from([pair("ENUM_VIOLATION", "source")])
    );
    assert_eq!(
        findings_for(
            example,
            r#"{"source": "live", "prov": "table+rubric"}"#,
            &[]
        ),
        BTreeSet::from([pair("ENUM_VIOLATION", "prov")])
    );
}

#[test]
fn a_wrapped_array_example_merges_at_the_array_level() {
    let mut root = node_of(r#"{"metadata": {"a": 1}}"#);
    let wrapped = json(
        r#"{"workloads": [{"workload_id": "wl_1", "structured_output": false, "call_sites": [{"file": "a", "line": 1}]}]}"#,
    );
    merge_example_at(&mut root, "workloads[]", &wrapped, &KeyComments::new(), "t");
    assert_eq!(root.keys["workloads"].kinds, set(&["array"]));
    let item = root.keys["workloads"].item.as_deref().expect("item");
    assert_eq!(
        item.keys.keys().cloned().collect::<BTreeSet<_>>(),
        set(&["workload_id", "structured_output", "call_sites"])
    );
    merge_example_at(
        &mut root,
        "workloads[]",
        &json(r#"{"model_id": "m"}"#),
        &KeyComments::new(),
        "t",
    );
    assert!(root.keys["workloads"]
        .item
        .as_deref()
        .expect("item")
        .keys
        .contains_key("model_id"));
    let found = check(
        &root,
        r#"{"metadata": {"a": 1}, "workloads": [{"workload_id": "x", "model_name": "m", "structured_output": "yes", "call_sites": "a.ts"}]}"#,
    );
    assert_eq!(
        found,
        BTreeSet::from([
            pair("UNKNOWN_KEY", "workloads[0].model_name"),
            pair("TYPE_MISMATCH", "workloads[0].structured_output"),
            pair("TYPE_MISMATCH", "workloads[0].call_sites"),
        ])
    );
    assert!(check(&root, r#"{"metadata": {"a": 1}, "workloads": []}"#).is_empty());
}

// ---- contract documents ----------------------------------------------------------------

#[test]
fn documents_are_scanned_into_blocks_and_headings() {
    let doc = "# Title\n\n## thing.json\n\n```json\n{\"a\": 1}\n```\n\n```python\nignored\n```\n\n```JSONC\n{\"b\": 2}\n```\n";
    let (blocks, headings) = scan_doc(doc);
    assert_eq!(headings, ["Title", "thing.json"]);
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        (blocks[0].heading.as_str(), blocks[0].line),
        ("thing.json", 5)
    );
    assert_eq!(blocks[1].text, "{\"b\": 2}");
}

#[test]
fn parse_block_reads_objects_arrays_and_fragments() {
    let parse = |text: &str| {
        let block = super::contracts::Block {
            heading: "h".to_string(),
            text: text.to_string(),
            line: 7,
        };
        let mut findings = Vec::new();
        let parsed = parse_block(&block, "doc.md", &mut findings);
        (parsed.map(|(v, _)| v), findings)
    };
    assert_eq!(parse("{\"a\": 1,\n}").0, Some(json(r#"{"a": 1}"#)));
    assert_eq!(parse("[1, 2, ...]").0, Some(json(r#"[1, 2, "..."]"#)));
    assert_eq!(
        parse("\"a\": 1,\n\"b\": {\"c\": 2}").0,
        Some(json(r#"{"a": 1, "b": {"c": 2}}"#))
    );
    assert_eq!(
        parse("{\n  \"a\": 1,\n  \"...\"\n}").0,
        Some(json(r#"{"a": 1, "...": "..."}"#))
    );
    let (value, findings) = parse("{\"a\": }");
    assert_eq!(value, None);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].code, Code::SchemaParse);
    assert_eq!(findings[0].path, "L7");
    assert_eq!(
        findings[0].message,
        "example block under 'h' is not valid JSON/JSONC: Expecting value (block line 1)"
    );
}

#[test]
fn a_comment_on_its_own_line_binds_to_the_key_that_opened_the_object() {
    let block = super::contracts::Block {
        heading: "h".to_string(),
        text: "{\n  \"clarify\": {\n    // REQUIRED\n    \"verdict\": \"x\"\n  }\n}".to_string(),
        line: 1,
    };
    let mut findings = Vec::new();
    let (_, comments) = parse_block(&block, "doc.md", &mut findings).expect("parsed");
    assert_eq!(
        comments.get("clarify").map(Vec::as_slice),
        Some(&["REQUIRED".to_string()][..])
    );
}

fn plugin_with_doc(label: &str, doc: &str, manifest: &str) -> TempDir {
    let root = TempDir::new(label);
    write(&root, "schema.md", doc);
    write(&root, "scripts/artifact-contracts.json", manifest);
    root
}

#[test]
fn a_required_heading_with_a_table_body_is_enforced() {
    let doc = "# Contract\n\n## thing.json\n\n```json\n{\"metadata\": {\"x\": 1}, \"apps\": [], \"opt\": {}}\n```\n\n### `metadata` (REQUIRED)\n\n| Field | Type |\n|---|---|\n| `x` | number |\n\n### `apps[]` (REQUIRED)\n\n| Field | Type |\n|---|---|\n\n### `opt` (OPTIONAL — present when available)\n\n| Field | Type |\n|---|---|\n";
    let root = plugin_with_doc("required-table", doc, "{}");
    let mut env = Env::new(&root);
    let mut findings = Vec::new();
    let spec = json(r#"{"shape": "schema.md", "root_heading": "thing.json"}"#);
    let contract = build_shape_contract(&mut env, &spec, &mut findings)
        .expect("built")
        .expect("contract");
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(contract.root.required, set(&["metadata", "apps"]));
}

#[test]
fn shape_contract_failures_are_schema_parse_findings() {
    let root = plugin_with_doc(
        "shape-failures",
        "# T\n\n## other\n\n```json\n[1]\n```\n",
        "{}",
    );
    let mut env = Env::new(&root);
    for (spec, message) in [
        (r#"{"shape": "missing.md"}"#, "contract document not found"),
        (
            r#"{"shape": "schema.md", "root_heading": "thing.json"}"#,
            "no JSON block found under heading 'thing.json'",
        ),
        (
            r#"{"shape": "schema.md", "root_heading": "other"}"#,
            "root example block is not a JSON object",
        ),
        (
            r#"{"shape": "schema.md", "root_heading": "other", "sections": {"@L99": "x"}}"#,
            "root example block is not a JSON object",
        ),
    ] {
        let mut findings = Vec::new();
        let built =
            build_shape_contract(&mut env, &json(spec), &mut findings).expect("no hard error");
        assert!(built.is_none(), "{spec}");
        assert_eq!(findings.len(), 1, "{spec}");
        assert_eq!(
            (findings[0].code, findings[0].message.as_str()),
            (Code::SchemaParse, message)
        );
    }
}

#[test]
fn a_manifest_anchor_that_misses_a_block_is_reported() {
    let doc = "## thing.json\n\n```json\n{\"a\": 1}\n```\n";
    let root = plugin_with_doc("anchor", doc, "{}");
    let mut env = Env::new(&root);
    let mut findings = Vec::new();
    let spec =
        json(r#"{"shape": "schema.md", "root_heading": "thing.json", "sections": {"@L40": "a"}}"#);
    let built = build_shape_contract(&mut env, &spec, &mut findings).expect("built");
    assert!(built.is_some());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].path, "@L40");
    assert!(findings[0]
        .message
        .starts_with("manifest section anchor @L40 does not point at a JSON block start"));
}

#[test]
fn manifest_sections_rules_ignores_and_extras_refine_the_shape() {
    let doc = "## thing.json\n\n```json\n{\"resources\": [{\"id\": 1}], \"other\": {}}\n```\n\n### `cfg` config\n\n```json\n{\"port\": 80}\n```\n\n### Skipped\n\n```json\n{\"bad\": true}\n```\n";
    let extra = "## extras\n\n```json\n{\"added\": \"x\"}\n```\n";
    let root = plugin_with_doc("refine", doc, "{}");
    write(&root, "extra.md", extra);
    let mut env = Env::new(&root);
    let mut findings = Vec::new();
    let spec = json(
        r#"{"shape": "schema.md", "root_heading": "thing.json",
            "section_rules": [{"heading_regex": "^`[a-z_]+` config$", "path": "resources[].config"}],
            "ignore_headings": ["Skipped"],
            "extra_shapes": [{"shape": "extra.md", "heading": "extras", "path": "other"}],
            "required": ["resources"], "open_paths": ["other"], "allowed_anywhere": ["note"]}"#,
    );
    let contract = build_shape_contract(&mut env, &spec, &mut findings)
        .expect("built")
        .expect("contract");
    assert!(findings.is_empty(), "{findings:?}");
    let root_node = &contract.root;
    assert!(root_node.required.contains("resources"));
    let config = &root_node.keys["resources"]
        .item
        .as_deref()
        .expect("item")
        .keys["config"];
    assert!(config.keys.contains_key("port"));
    assert!(root_node.keys["other"].wildcard);
    assert!(root_node.keys["other"].keys.contains_key("added"));
    assert!(!root_node.keys.contains_key("bad"));
    assert!(root_node.keys["resources"]
        .item
        .as_deref()
        .expect("item")
        .keys
        .contains_key("note"));
    assert!(contract.label.ends_with("schema.md § thing.json"));
}

// ---- JSON Schema subset ------------------------------------------------------------------

fn schema_findings(schema: &str, value: &str) -> Vec<(String, String)> {
    let schema = json(schema);
    let mut validator = SubsetValidator::default();
    let mut out = Vec::new();
    let context = Context {
        artifact: "a.json",
        contract: "s",
    };
    validator
        .validate(&schema, &json(value), "", &context, &mut out, &schema)
        .expect("evaluated");
    out.into_iter()
        .map(|f: Finding| (f.path, f.message))
        .collect()
}

const SUBSET: &str = r#"{
  "type": "object", "required": ["phase"], "additionalProperties": false,
  "properties": {
    "phase": {"const": "estimate"},
    "tier": {"type": "string", "enum": ["small", "medium"]},
    "items": {"type": "array", "items": {"type": "integer", "minimum": 0}},
    "id": {"type": "string", "pattern": "^[a-z]+$"}
  }
}"#;

#[test]
fn the_json_schema_subset_reports_each_violation_with_python_wording() {
    let found = schema_findings(
        SUBSET,
        r#"{"phase": "design", "tier": "huge", "items": [-1, "x"], "id": "A1", "zzz": 1}"#,
    );
    assert_eq!(
        found,
        [
            ("phase", "'design' != const 'estimate'"),
            ("tier", "'huge' not in enum ['small', 'medium']"),
            ("items[0]", "-1 < minimum 0"),
            ("items[1]", "is string, schema type is integer"),
            ("id", "'A1' does not match pattern ^[a-z]+$"),
            ("zzz", "additionalProperties is false"),
        ]
        .map(|(p, m)| (p.to_string(), m.to_string()))
    );
    assert!(schema_findings(
        SUBSET,
        r#"{"phase": "estimate", "tier": "small", "items": [1], "id": "ab"}"#
    )
    .is_empty());
}

#[test]
fn numeric_string_object_and_array_limits() {
    let schema = r#"{"type": "object", "minProperties": 1, "maxProperties": 2,
        "properties": {
          "n": {"type": "number", "exclusiveMinimum": 0, "maximum": 10},
          "m": {"type": "number", "exclusiveMaximum": 1.5},
          "s": {"type": "string", "minLength": 2, "maxLength": 3},
          "a": {"type": "array", "minItems": 1, "maxItems": 2, "uniqueItems": true}
        }}"#;
    let found = schema_findings(schema, r#"{"n": 0, "m": 1.5}"#);
    assert_eq!(
        found,
        [
            ("n", "0 <= exclusiveMinimum 0"),
            ("m", "1.5 >= exclusiveMaximum 1.5")
        ]
        .map(|(p, m)| (p.to_string(), m.to_string()))
    );
    let found = schema_findings(schema, r#"{"n": 11, "s": "a", "a": [1, 1, 2]}"#);
    assert_eq!(
        found,
        [
            ("$", "3 properties > maxProperties 2"),
            ("n", "11 > maximum 10"),
            ("s", "length 1 < minLength 2"),
            ("a", "3 items > maxItems 2"),
            ("a", "items are not unique (uniqueItems)"),
        ]
        .map(|(p, m)| (p.to_string(), m.to_string()))
    );
    assert_eq!(
        schema_findings(schema, "{}"),
        [(
            "$".to_string(),
            "0 properties < minProperties 1".to_string()
        )]
    );
    // 1 and 1.0 and true are three different items for uniqueItems.
    assert!(schema_findings(r#"{"uniqueItems": true}"#, "[1, 1.0, true]").is_empty());
    assert_eq!(
        schema_findings(
            r#"{"uniqueItems": true}"#,
            r#"[{"a": 1, "b": 2}, {"b": 2, "a": 1}]"#
        )
        .len(),
        1
    );
}

#[test]
fn refs_combinators_and_type_lists() {
    let schema = r##"{
      "definitions": {"small": {"type": "integer", "maximum": 5}},
      "properties": {
        "r": {"$ref": "#/definitions/small"},
        "gone": {"$ref": "#/definitions/missing"},
        "remote": {"$ref": "https://example.test/x"},
        "t": {"type": ["string", "null"]},
        "any": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
        "one": {"oneOf": [{"type": "integer"}, {"minimum": 0}]},
        "no": {"not": {"type": "string"}},
        "all": {"allOf": [{"type": "string"}, {"minLength": 3}]}
      }}"##;
    let found = schema_findings(
        schema,
        r#"{"r": 9, "gone": 1, "remote": 1, "t": 3, "any": true, "one": 4, "no": "s", "all": "ab"}"#,
    );
    assert_eq!(
        found,
        [
            ("r", "9 > maximum 5"),
            ("t", "is number, schema type is ['string', 'null']"),
            ("any", "matches 0 of 2 anyOf branches"),
            ("one", "matches 2 of 2 oneOf branches"),
            ("no", "matches a `not` schema"),
            ("all", "length 2 < minLength 3"),
        ]
        .map(|(p, m)| (p.to_string(), m.to_string()))
    );
    assert!(schema_findings(
        schema,
        r#"{"r": 4, "t": null, "any": 1, "one": -1, "no": 1, "all": "abc"}"#
    )
    .is_empty());
    // Integer-valued floats are not integers; booleans are not numbers.
    assert_eq!(schema_findings(r#"{"type": "integer"}"#, "1.0").len(), 1);
    assert_eq!(schema_findings(r#"{"type": "number"}"#, "true").len(), 1);
    assert!(schema_findings(r#"{"type": "number"}"#, "1").is_empty());
}

#[test]
fn schema_patterns_use_the_bounded_engine_and_refuse_invalid_ones() {
    let schema = json(r#"{"type": "string", "pattern": "^(?!(?:both|unknown)$)[a-z][a-z0-9_]*$"}"#);
    let mut validator = SubsetValidator::default();
    let context = Context {
        artifact: "a",
        contract: "c",
    };
    for (value, ok) in [("abc", true), ("both", false), ("a_1", true), ("1a", false)] {
        let mut out = Vec::new();
        validator
            .validate(
                &schema,
                &Json::string(value),
                "",
                &context,
                &mut out,
                &schema,
            )
            .expect("evaluated");
        assert_eq!(out.is_empty(), ok, "{value}");
    }
    let broken = json(r#"{"pattern": "("}"#);
    let mut out = Vec::new();
    let error = validator
        .validate(&broken, &Json::string("x"), "", &context, &mut out, &broken)
        .expect_err("invalid pattern");
    assert!(error.starts_with("pattern '('"), "{error}");
}

#[test]
fn a_ref_cycle_is_refused_instead_of_recursing_forever() {
    let schema =
        json(r##"{"definitions": {"a": {"$ref": "#/definitions/a"}}, "$ref": "#/definitions/a"}"##);
    let mut validator = SubsetValidator::default();
    let context = Context {
        artifact: "a",
        contract: "c",
    };
    let error = validator
        .validate(
            &schema,
            &Json::Int(1),
            "",
            &context,
            &mut Vec::new(),
            &schema,
        )
        .expect_err("cycle");
    assert!(error.contains("too deep"), "{error}");
}

#[test]
fn unsupported_keywords_are_listed_with_their_pointer() {
    let schema = json(
        r#"{"type": "object",
            "properties": {"a": {"type": "string", "contentEncoding": "base64", "format": "date"}},
            "dependencies": {"a": ["b"]},
            "patternProperties": {"^x": {"type": "string"}},
            "allOf": [{"if": {}}],
            "items": {"unevaluatedProperties": false}}"#,
    );
    let found = unsupported_keywords(&schema, "#");
    let has =
        |pointer: &str, keyword: &str| found.contains(&(pointer.to_string(), keyword.to_string()));
    assert!(has("#/properties/a", "contentEncoding"));
    assert!(has("#", "dependencies"));
    assert!(has("#", "patternProperties"));
    assert!(has("#/allOf/0", "if"));
    assert!(has("#/items", "unevaluatedProperties"));
    assert!(!found.iter().any(|(_, k)| k == "format" || k == "type"));
}

#[test]
fn every_keyword_of_the_two_real_schemas_is_enforced_or_an_annotation() {
    for name in [
        "skills/shared/state/phase-status.schema.json",
        "skills/shared/estimate/estimation-infra.schema.json",
    ] {
        let schema = json(&fs::read_to_string(real_plugin().join(name)).expect("schema file"));
        assert_eq!(
            unsupported_keywords(&schema, "#"),
            Vec::<(String, String)>::new(),
            "{name}"
        );
    }
}

// ---- baseline -------------------------------------------------------------------------------

#[test]
fn the_baseline_marks_matches_and_reports_stale_entries() {
    let mut f1 = Finding::new(
        Code::UnknownKey,
        "fixtures/x/a.json",
        "resources[3].id",
        "m".to_string(),
        "",
    );
    let f2 = Finding::new(
        Code::UnknownKey,
        "fixtures/x/a.json",
        "other",
        "m".to_string(),
        "",
    );
    let baseline = json(
        r#"{"entries": [
            {"code": "UNKNOWN_KEY", "artifact": "fixtures/x/*", "path": "resources[[]*[]].id", "reason": "r"},
            {"code": "UNKNOWN_KEY", "artifact": "fixtures/x/*", "path": "never.matches", "reason": "r"}]}"#,
    );
    let mut all = vec![f1.clone(), f2];
    let stale = apply_baseline(&mut all, &baseline);
    assert!(all[0].baselined && all[0].baseline_reason == "r");
    assert!(!all[1].baselined);
    assert_eq!(stale.len(), 1);
    assert_eq!(
        stale[0].get("path").and_then(Json::as_str),
        Some("never.matches")
    );
    // The code must match too.
    f1.code = Code::TypeMismatch;
    let mut other = vec![f1];
    assert_eq!(apply_baseline(&mut other, &baseline).len(), 2);
}

// ---- the command ------------------------------------------------------------------------------

fn artifacts(args: &[&str]) -> (u8, String, String) {
    let mut all = vec!["startup", "artifacts"];
    all.extend_from_slice(args);
    exec(&all)
}

fn plugin_arg() -> String {
    text(&real_plugin()).to_string()
}

#[test]
fn self_check_parses_every_contract_document_of_the_real_plugin() {
    let plugin = plugin_arg();
    let (status, stdout, stderr) = artifacts(&["--self-check", "--plugin-root", &plugin]);
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("PASS — 0 artifact(s) checked, 0 error(s), 0 baselined, 0 info"),
        "{stdout}"
    );
    assert_eq!(stderr, "");
}

#[test]
fn the_fixture_goldens_pass_with_the_shipped_baseline_and_no_stale_entry() {
    let plugin = plugin_arg();
    let (status, stdout, stderr) = artifacts(&["--fixtures", "--json", "--plugin-root", &plugin]);
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    let report = json(&stdout);
    assert!(matches!(report.get("checked_artifacts"), Some(Json::Int(n)) if *n > 0));
    assert_eq!(report.get("errors"), Some(&Json::Array(Vec::new())));
    assert_eq!(
        report.get("stale_baseline_entries"),
        Some(&Json::Array(Vec::new()))
    );
    assert!(matches!(report.get("baselined"), Some(Json::Array(items)) if !items.is_empty()));

    let (status, stdout, _) = artifacts(&["--fixtures", "--plugin-root", &plugin]);
    assert_eq!(status, 0);
    assert!(
        stdout.contains("baselined (known, explained, not failing): "),
        "{stdout}"
    );
    assert!(stdout.contains("\nPASS — "), "{stdout}");
}

#[test]
fn an_off_contract_key_in_a_copied_golden_fails_with_its_path() {
    let plugin = real_plugin();
    let run = TempDir::new("injected");
    copy_dir(
        &plugin.join("fixtures/azure-iac-terraform/after-discover"),
        &run,
    );
    let path = run.join("azure-resource-inventory.json");
    let mut data = json(&fs::read_to_string(&path).expect("golden"));
    let Json::Object(top) = &mut data else {
        panic!("object")
    };
    let Some((_, Json::Array(resources))) = top.iter_mut().find(|(k, _)| k == "resources") else {
        panic!("resources")
    };
    let Json::Object(first) = &mut resources[0] else {
        panic!("resource")
    };
    first.push(("invented_by_a_phase_file".to_string(), Json::Bool(true)));
    fs::write(&path, dumps(&data, None, false)).expect("write mutated artifact");

    let plugin = plugin_arg();
    let (status, stdout, stderr) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "azure-to-aws",
        "--no-baseline",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1, "{stdout}");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(error_pairs(&stdout).contains(&pair(
        "UNKNOWN_KEY",
        "resources[0].invented_by_a_phase_file"
    )));
}

#[test]
fn the_unmodified_golden_run_passes_without_a_baseline() {
    let plugin = plugin_arg();
    let run = real_plugin().join("fixtures/azure-iac-terraform/after-discover");
    let (status, stdout, stderr) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "azure-to-aws",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert!(error_pairs(&stdout).is_empty());
}

#[test]
fn a_stale_baseline_entry_fails_the_gate() {
    let plugin = plugin_arg();
    let baseline = TempDir::new("stale");
    write(
        &baseline,
        "baseline.json",
        r#"{"entries": [{"code": "UNKNOWN_KEY", "artifact": "*", "path": "never.matches.anything", "reason": "stale"}]}"#,
    );
    let run = real_plugin().join("fixtures/azure-iac-terraform/after-discover");
    let file = baseline.join("baseline.json");
    let (status, stdout, _) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "azure-to-aws",
        "--baseline",
        text(&file),
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1);
    assert!(error_pairs(&stdout)
        .iter()
        .any(|(code, _)| code == "STALE_BASELINE"));
    let report = json(&stdout);
    let Some(Json::Array(stale)) = report.get("stale_baseline_entries") else {
        panic!("stale")
    };
    assert_eq!(
        stale[0].get("path").and_then(Json::as_str),
        Some("never.matches.anything")
    );
    let Some(Json::Array(errors)) = report.get("errors") else {
        panic!("errors")
    };
    assert_eq!(
        errors[0].get("message").and_then(Json::as_str),
        Some(
            r#"entry matched no finding — remove it, or fix its code/artifact/path: {"code": "UNKNOWN_KEY", "artifact": "*", "path": "never.matches.anything", "reason": "stale"}"#
        )
    );
    // --no-baseline ignores the file.
    let (status, _, _) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "azure-to-aws",
        "--baseline",
        text(&file),
        "--no-baseline",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 0);
}

#[test]
fn a_real_schema_violation_fails() {
    let run = TempDir::new("schema-violation");
    copy_dir(
        &real_plugin().join("fixtures/azure-iac-terraform/after-estimate"),
        &run,
    );
    let path = run.join("estimation-infra.json");
    let mut data = json(&fs::read_to_string(&path).expect("golden"));
    let Json::Object(top) = &mut data else {
        panic!("object")
    };
    let slot = top
        .iter_mut()
        .find(|(k, _)| k == "complexity_tier")
        .expect("tier");
    slot.1 = Json::string("enormous");
    fs::write(&path, dumps(&data, None, false)).expect("write mutated artifact");
    let plugin = plugin_arg();
    let (status, stdout, _) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "azure-to-aws",
        "--no-baseline",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1);
    assert!(error_pairs(&stdout).contains(&pair("SCHEMA_VIOLATION", "complexity_tier")));
}

#[test]
fn empty_phases_fail_min_properties_and_a_valid_status_passes() {
    let plugin = plugin_arg();
    let run = TempDir::new("phases");
    write(
        &run,
        ".phase-status.json",
        r#"{"migration_id": "x", "last_updated": "2026-01-01T00:00:00Z", "phases": {}}"#,
    );
    let (status, stdout, _) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "gcp-to-aws",
        "--no-baseline",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1);
    let report = json(&stdout);
    let Some(Json::Array(errors)) = report.get("errors") else {
        panic!("errors")
    };
    assert!(errors.iter().any(|e| e.get("code").and_then(Json::as_str)
        == Some("SCHEMA_VIOLATION")
        && e.get("path").and_then(Json::as_str) == Some("phases")
        && e.get("message")
            .and_then(Json::as_str)
            .is_some_and(|m| m.contains("minProperties"))));

    let valid = TempDir::new("phases-valid");
    fs::copy(
        real_plugin().join("fixtures/gcp-decision-gate/after-decide-complete/.phase-status.json"),
        valid.join(".phase-status.json"),
    )
    .expect("copy status");
    let (status, stdout, _) = artifacts(&[
        "--run-dir",
        text(&valid),
        "--skill",
        "gcp-to-aws",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 0, "{stdout}");
    assert!(error_pairs(&stdout).is_empty());
}

#[test]
fn a_neutral_run_skips_provider_artifacts_and_expected_snapshots() {
    let manifest = real_plugin().join("scripts/artifact-contracts.json");
    let manifest = json(&fs::read_to_string(manifest).expect("manifest"));
    let run = TempDir::new("neutral");
    write(&run, "gcp-resource-inventory.json", "{}");
    write(&run, "expected-artifacts.json", "not json");
    write(&run, "clarify-answers.json", "not json");
    write(&run, "live-capture/inner.json", "not json");
    let mut env = Env::new(&real_plugin());
    let mut findings = Vec::new();
    let checked =
        super::validate_run_dir(&mut env, &run, None, &manifest, &mut findings).expect("validated");
    assert_eq!(checked, 0);
    assert!(findings.is_empty());
    let names: Vec<String> = super::find_artifacts(&run)
        .expect("listed")
        .iter()
        .map(|p| p.file_name().expect("name").to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["gcp-resource-inventory.json"]);
}

#[test]
fn target_only_ai_contracts_do_not_validate_current_skill_outputs() {
    let manifest = json(
        &fs::read_to_string(real_plugin().join("scripts/artifact-contracts.json"))
            .expect("manifest"),
    );
    for skill in ["gcp-to-aws", "azure-to-aws"] {
        for artifact in ["estimation-ai.json", "generation-ai.json"] {
            let run = TempDir::new("target-only");
            write(&run, artifact, r#"{"phase": "estimate"}"#);
            let mut env = Env::new(&real_plugin());
            let mut findings = Vec::new();
            let checked =
                super::validate_run_dir(&mut env, &run, Some(skill), &manifest, &mut findings)
                    .expect("validated");
            assert_eq!(checked, 0, "{skill} {artifact}");
            assert_eq!(findings.len(), 1);
            assert_eq!(
                (findings[0].code, findings[0].path.as_str()),
                (Code::NoContract, "")
            );
            assert_eq!(
                findings[0].message,
                format!("no contract covers '{artifact}' for skill {skill}")
            );
        }
    }
}

#[test]
fn an_unparseable_artifact_is_a_type_mismatch_with_the_python_message() {
    let plugin = plugin_arg();
    let run = TempDir::new("broken-json");
    write(&run, ".phase-status.json", "{\"a\": }");
    let (status, stdout, _) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "gcp-to-aws",
        "--json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1);
    let report = json(&stdout);
    let Some(Json::Array(errors)) = report.get("errors") else {
        panic!("errors")
    };
    assert_eq!(
        errors[0].get("message").and_then(Json::as_str),
        Some("artifact is not valid JSON: Expecting value")
    );
    assert_eq!(errors[0].get("path").and_then(Json::as_str), Some("$"));
    assert_eq!(report.get("checked_artifacts"), Some(&Json::Int(0)));
}

fn custom_plugin(label: &str, doc: &str) -> TempDir {
    plugin_with_doc(
        label,
        doc,
        r#"{"skills": {"gcp-to-aws": {"thing.json": {"shape": "schema.md", "root_heading": "thing.json"}}}}"#,
    )
}

#[test]
fn a_required_inline_array_field_cannot_be_omitted() {
    let doc = "## thing.json\n\n```jsonc\n{\n  \"priority\": \"balanced\", // REQUIRED\n  \"call_sites\": [{ \"file\": \"app.py\", \"line\": 1 }], // REQUIRED\n  \"note\": \"x\"\n}\n```\n";
    let plugin = custom_plugin("inline-required", doc);
    for (body, missing) in [
        (r#"{"priority": "p", "note": "n"}"#, "call_sites"),
        (r#"{"call_sites": [], "note": "n"}"#, "priority"),
    ] {
        let run = TempDir::new("inline-run");
        write(&run, "thing.json", body);
        let (status, stdout, _) = artifacts(&[
            "--run-dir",
            text(&run),
            "--skill",
            "gcp-to-aws",
            "--json",
            "--plugin-root",
            text(&plugin),
        ]);
        assert_eq!(status, 1, "{body}");
        assert_eq!(
            error_pairs(&stdout),
            [pair("MISSING_REQUIRED", missing)],
            "{body}"
        );
    }
    let run = TempDir::new("inline-ok");
    write(
        &run,
        "thing.json",
        r#"{"priority": "p", "call_sites": [{"file": "a", "line": 2}], "note": "n"}"#,
    );
    let (status, _, _) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "gcp-to-aws",
        "--plugin-root",
        text(&plugin),
    ]);
    assert_eq!(status, 0);
}

#[test]
fn route_shapes_select_the_shape_by_the_artifact_marker() {
    let doc = "## default.json\n\n```json\n{\"mode\": \"a\", \"x\": 1}\n```\n\n## routed.json\n\n```json\n{\"mode\": \"ai-only\", \"y\": 1}\n```\n";
    let plugin = plugin_with_doc(
        "routes",
        doc,
        r#"{"skills": {"gcp-to-aws": {"thing.json": {
            "shape": "schema.md", "root_heading": "default.json",
            "route_shapes": [{"shape": "schema.md", "root_heading": "routed.json", "when_path": "mode", "when_value": "ai-only"}]}}}}"#,
    );
    let run_with = |body: &str| {
        let run = TempDir::new("routes-run");
        write(&run, "thing.json", body);
        let (_, stdout, _) = artifacts(&[
            "--run-dir",
            text(&run),
            "--skill",
            "gcp-to-aws",
            "--json",
            "--plugin-root",
            text(&plugin),
        ]);
        error_pairs(&stdout)
    };
    assert!(run_with(r#"{"mode": "a", "x": 2}"#).is_empty());
    assert!(run_with(r#"{"mode": "ai-only", "y": 2}"#).is_empty());
    assert_eq!(
        run_with(r#"{"mode": "ai-only", "x": 2}"#),
        [pair("UNKNOWN_KEY", "x")]
    );
}

#[test]
fn text_output_lists_errors_infos_and_the_summary() {
    let doc = "## thing.json\n\n```json\n{\"a\": 1}\n```\n";
    let plugin = custom_plugin("text-output", doc);
    let run = TempDir::new("text-run");
    write(&run, "thing.json", r#"{"a": 1, "extra": 2}"#);
    write(&run, "unrelated.json", "{}");
    let (status, stdout, stderr) = artifacts(&[
        "--run-dir",
        text(&run),
        "--skill",
        "gcp-to-aws",
        "--plugin-root",
        text(&plugin),
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        stdout,
        format!(
            "ERROR  UNKNOWN_KEY       {rel} :: extra\n       key is not in the contract's example shape\n       contract: schema.md § thing.json\ninfo   NO_CONTRACT       {other} :: no contract covers 'unrelated.json' for skill gcp-to-aws\n\nFAIL — 1 artifact(s) checked, 1 error(s), 0 baselined, 1 info\n",
            rel = fs::canonicalize(run.join("thing.json")).expect("canonical").display(),
            other = fs::canonicalize(run.join("unrelated.json")).expect("canonical").display(),
        )
    );
    assert_eq!(stderr, "error: 1 artifact contract problem(s)\n");
}

#[test]
fn usage_errors_exit_two() {
    let plugin = plugin_arg();
    let dir = TempDir::new("usage");
    for args in [
        vec![],
        vec!["--self-check", "--fixtures"],
        vec!["--run-dir", text(&dir)],
        vec!["--run-dir", text(&dir), "--skill", "bogus"],
        vec!["--run-dir", "/nonexistent/run-dir", "--skill", "gcp-to-aws"],
    ] {
        let mut all = args.clone();
        all.extend(["--plugin-root", plugin.as_str()]);
        let (status, stdout, stderr) = artifacts(&all);
        assert_eq!(status, 2, "{args:?}");
        assert_eq!(stdout, "");
        assert!(stderr.starts_with("error: "), "{stderr}");
    }
}

#[test]
fn unreadable_inputs_and_symlinks_are_runtime_errors() {
    let plugin = plugin_arg();
    let (status, _, stderr) = artifacts(&[
        "--self-check",
        "--manifest",
        "/nonexistent/manifest.json",
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("cannot read /nonexistent/manifest.json"),
        "{stderr}"
    );

    let broken = TempDir::new("broken-manifest");
    write(&broken, "m.json", "{\"skills\": ");
    let manifest = broken.join("m.json");
    let (status, _, stderr) = artifacts(&[
        "--self-check",
        "--manifest",
        text(&manifest),
        "--plugin-root",
        &plugin,
    ]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("Expecting value: line 1 column 12 (char 11)"),
        "{stderr}"
    );

    #[cfg(unix)]
    {
        let run = TempDir::new("symlink-run");
        write(&run, "real.json", "{}");
        std::os::unix::fs::symlink(run.join("real.json"), run.join("link.json")).expect("symlink");
        let (status, _, stderr) = artifacts(&[
            "--run-dir",
            text(&run),
            "--skill",
            "gcp-to-aws",
            "--plugin-root",
            &plugin,
        ]);
        assert_eq!(status, 1);
        assert!(
            stderr.contains("symbolic links are not followed"),
            "{stderr}"
        );
    }
}

#[test]
fn a_broken_contract_document_is_reported_by_self_check() {
    let plugin = plugin_with_doc(
        "broken-contract",
        "## thing.json\n\n```json\n{\"a\": }\n```\n",
        r#"{"skills": {"gcp-to-aws": {"thing.json": {"shape": "schema.md", "root_heading": "thing.json"}}}}"#,
    );
    let (status, stdout, _) =
        artifacts(&["--self-check", "--json", "--plugin-root", text(&plugin)]);
    assert_eq!(status, 1);
    assert_eq!(error_pairs(&stdout), [pair("SCHEMA_PARSE", "L3")]);
}

#[test]
fn find_artifacts_sorts_by_path_components() {
    let run = TempDir::new("sorted");
    for rel in [
        "a-b/c.json",
        "a/z.json",
        "a/b.json",
        "B.json",
        "node_modules/x.json",
        "a/notes.txt",
    ] {
        write(&run, rel, "{}");
    }
    let names: Vec<String> = super::find_artifacts(&run)
        .expect("listed")
        .iter()
        .map(|p| p.strip_prefix(&*run).expect("inside").display().to_string())
        .collect();
    assert_eq!(names, ["B.json", "a/b.json", "a/z.json", "a-b/c.json"]);
}
