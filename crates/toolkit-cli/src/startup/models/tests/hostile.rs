//! Hostile-input tests (#4602): every diagnostic path of the command with
//! caller-derived text carrying ESC, BEL, CR, LF, U+202E and a fake secret
//! marker. A diagnostic is single-line, escaped, and never repeats a value of
//! the input.

use std::fs;

use toolkit_core::Json;

use super::cli::{recommend, INPUT, OPENAI_INPUT};
use super::{input_with, j, merged, plugin_root, recommend_input, HOSTILE};
use crate::startup::models::request::Input;
use crate::startup::scoring::fixtures::{exec, path_text, write, TempDir};

const MARKER: &str = "AKIAFAKESECRETMARKER";

/// The hostile text as a JSON string literal.
fn lit() -> String {
    Json::string(HOSTILE).to_compact_string()
}

/// A file name carrying the same controls (no `/`).
fn hostile_name(suffix: &str) -> String {
    format!("a\u{1b}[2J\r\nforged: ok\u{202e}\u{7}{MARKER}{suffix}")
}

/// No raw control or bidirectional character, and every line is a finding or
/// the one `error:` summary.
fn assert_safe(stderr: &str) {
    for raw in ['\u{1b}', '\u{7}', '\r', '\u{202e}', '\u{2028}'] {
        assert!(!stderr.contains(raw), "{raw:?} in {stderr:?}");
    }
    assert!(stderr.ends_with('\n'), "{stderr:?}");
    for line in stderr.lines() {
        assert!(
            line.starts_with("  - ") || line.starts_with("error: "),
            "forged or stray line {line:?} in {stderr:?}"
        );
    }
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("error: "))
            .count(),
        1,
        "{stderr:?}"
    );
    assert!(stderr
        .lines()
        .last()
        .is_some_and(|line| line.starts_with("error: ")));
}

fn run_input(document: &str) -> (u8, String, String) {
    let dir = TempDir::new("models-hostile");
    write(&dir, "input.json", document);
    recommend(&[path_text(&dir.join("input.json"))])
}

#[test]
fn hostile_values_in_enumerated_fields_are_never_echoed() {
    let hostile = lit();
    let cases = [
        (
            r#""priority": "balanced""#,
            format!(r#""priority": {hostile}"#),
        ),
        (
            r#""provider": "anthropic""#,
            format!(r#""provider": {hostile}"#),
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            format!(r#""critical_features": ["tool_use"], "preferred_api_path": {hostile}"#),
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            format!(r#""critical_features": ["tool_use"], "data_residency": {hostile}"#),
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            format!(r#""critical_features": ["tool_use"], "cris_geography": {hostile}"#),
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            format!(r#""critical_features": ["tool_use"], "governance": [{hostile}]"#),
        ),
        (
            r#""detected_features": []"#,
            format!(r#""detected_features": [{hostile}]"#),
        ),
        (
            r#""detected_features": []"#,
            format!(r#""detected_features": [], "feature_status": {{"tool_use": {hostile}}}"#),
        ),
        (
            r#""schema_version": 2"#,
            format!(r#""schema_version": {hostile}"#),
        ),
        (
            r#""workload_id": "support-agent""#,
            format!(r#""workload_id": {hostile}"#),
        ),
        (
            r#""sdk": "anthropic""#,
            format!(r#""sdk": {{"x": {hostile}}}"#),
        ),
        (
            r#""region": "us-east-1""#,
            format!(r#""region": [{hostile}]"#),
        ),
    ];
    for (from, to) in cases {
        let document = INPUT.replace(from, &to);
        assert_ne!(document, INPUT, "{from}");
        let (status, stdout, stderr) = run_input(&document);
        assert_eq!(status, 1, "{to}: {stderr}");
        assert_eq!(stdout, "");
        assert_safe(&stderr);
        assert!(!stderr.contains(MARKER), "{to}: {stderr}");
        assert!(!stderr.contains("forged"), "{to}: {stderr}");
    }
}

#[test]
fn hostile_property_names_are_escaped_and_bounded() {
    let hostile = lit();
    let cases = [
        // additionalProperties findings print the name through safe_quoted.
        (
            r#""schema_version": 2"#,
            format!(r#""schema_version": 2, {hostile}: 1"#),
            true,
        ),
        (
            r#""detected_features": []"#,
            format!(r#""detected_features": [], {hostile}: 1"#),
            true,
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            format!(r#""critical_features": ["tool_use"], {hostile}: true"#),
            true,
        ),
        (
            r#""sdk": "anthropic""#,
            format!(r#""sdk": "anthropic", {hostile}: 1"#),
            true,
        ),
        // propertyNames findings name the object, never the key.
        (
            r#""detected_features": []"#,
            format!(r#""detected_features": [], "feature_status": {{{hostile}: "absent"}}"#),
            false,
        ),
    ];
    for (from, to, may_show_escaped_name) in cases {
        let document = INPUT.replace(from, &to);
        assert_ne!(document, INPUT, "{from}");
        let (status, _, stderr) = run_input(&document);
        assert_eq!(status, 1, "{to}: {stderr}");
        assert_safe(&stderr);
        if may_show_escaped_name {
            assert!(stderr.contains("\\x1b"), "the name is escaped: {stderr}");
        } else {
            assert!(!stderr.contains(MARKER), "{stderr}");
        }
    }
    // A very long name is bounded.
    let long = "k".repeat(5000);
    let (_, _, stderr) = run_input(&INPUT.replace(
        r#""schema_version": 2"#,
        &format!(r#""schema_version": 2, "{long}": 1"#),
    ));
    assert!(stderr.len() < 1500, "{}", stderr.len());
    assert_safe(&stderr);
}

#[test]
fn hostile_input_and_catalog_file_names_are_escaped() {
    let dir = TempDir::new("models-hostile-names");
    let missing = dir.join(hostile_name(".json"));
    let (status, stdout, stderr) = recommend(&[path_text(&missing)]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_safe(&stderr);
    assert!(stderr.contains("\\x1b"), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");

    // An existing but invalid input names its path in the summary.
    let named = dir.join(hostile_name("-input.json"));
    fs::write(&named, "{}").expect("write");
    let (status, _, stderr) = recommend(&[path_text(&named)]);
    assert_eq!(status, 1);
    assert_safe(&stderr);
    assert!(stderr.contains("does not satisfy the model-recommendation-input schema"));

    // Invalid JSON.
    fs::write(&named, "{\"a\": \"\u{1b}\"").expect("write");
    let (status, _, stderr) = recommend(&[path_text(&named)]);
    assert_eq!(status, 1);
    assert_safe(&stderr);

    // A hostile catalog path, for each of the two catalogs.
    write(&dir, "input.json", INPUT);
    write(&dir, "oai.json", OPENAI_INPUT);
    for (input, flag) in [
        ("input.json", "--catalog"),
        ("oai.json", "--openai-catalog"),
    ] {
        let (status, _, stderr) = recommend(&[
            path_text(&dir.join(input)),
            flag,
            path_text(&dir.join(hostile_name("-catalog.json"))),
        ]);
        assert_eq!(status, 1, "{flag}");
        assert_safe(&stderr);
        assert!(stderr.contains("\\x1b"), "{stderr}");
    }
}

#[test]
fn hostile_catalog_contents_are_escaped_and_not_echoed() {
    let dir = TempDir::new("models-hostile-catalog");
    write(&dir, "input.json", INPUT);
    let input = path_text(&dir.join("input.json")).to_string();
    let hostile = lit();
    let header = r#""schema_version": 2, "provider": "x", "verified_at": "d", "verified_region": "r", "paths": {}"#;
    let key_cases = [
        // A hostile model key with a missing field.
        format!(r#"{{{header}, "models": {{{hostile}: {{"display_name": "M"}}}}}}"#),
        // A hostile model key with a mistyped field.
        format!(
            r#"{{{header}, "models": {{{hostile}: {{"display_name": 1, "family": "f", "version": "1",
            "context_window": 1, "output_token_ceiling": 1, "capabilities": [], "paths": {{}}}}}}}}"#
        ),
        // A hostile path key.
        format!(
            r#"{{{header}, "models": {{"m": {{"display_name": "M", "family": "f", "version": "1",
            "context_window": 1, "output_token_ceiling": 1, "capabilities": [],
            "paths": {{{hostile}: {{"available": true}}}}}}}}}}"#
        ),
    ];
    let value_cases = [
        // A repeated hostile key is refused by the strict parser without naming it.
        format!(r#"{{{header}, "models": {{{hostile}: {{}}, {hostile}: {{}}}}}}"#),
        // A hostile value where a limit belongs.
        format!(
            r#"{{{header}, "models": {{"m": {{"display_name": "M", "family": "f", "version": "1",
            "context_window": {hostile}, "output_token_ceiling": 1, "capabilities": [], "paths": {{}}}}}}}}"#
        ),
        // A hostile value for a required string.
        format!(
            r#"{{"schema_version": 2, "provider": {{"x": {hostile}}}, "verified_at": "d",
            "verified_region": "r", "paths": {{}}, "models": {{}}}}"#
        ),
        // A hostile element in a list.
        format!(
            r#"{{{header}, "models": {{"m": {{"display_name": "M", "family": "f", "version": "1",
            "context_window": 1, "output_token_ceiling": 1, "capabilities": [{hostile}, 3],
            "paths": {{}}}}}}}}"#
        ),
    ];
    let run = |catalog: &str| {
        write(&dir, "catalog.json", catalog);
        let (status, stdout, stderr) =
            recommend(&[&input, "--catalog", path_text(&dir.join("catalog.json"))]);
        assert_eq!(status, 1, "{catalog}");
        assert_eq!(stdout, "");
        assert_safe(&stderr);
        assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
        stderr
    };
    for catalog in &key_cases {
        // A name of the file is data: it appears, escaped, on the one line.
        let stderr = run(catalog);
        assert!(stderr.contains("\\x1b[31m"), "{stderr}");
    }
    for catalog in &value_cases {
        // A value of the file is never repeated.
        let stderr = run(catalog);
        assert!(!stderr.contains(MARKER), "{stderr}");
        assert!(!stderr.contains("forged: ok"), "{stderr}");
    }
}

#[test]
fn a_hostile_primary_unit_is_never_echoed() {
    let hostile = lit();
    let document = INPUT.replace(
        r#""primary_unit": "support-agent""#,
        &format!(r#""primary_unit": {hostile}"#),
    );
    let (status, stdout, stderr) = run_input(&document);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "error: primary_unit does not name a workload_id of the input\n"
    );
}

#[test]
fn hostile_region_and_free_text_stay_data() {
    let hostile = lit();
    let document = INPUT
        .replace(
            r#""region": "us-east-1""#,
            &format!(r#""region": {hostile}"#),
        )
        .replace(r#""sdk": "anthropic""#, &format!(r#""sdk": {hostile}"#))
        .replace(
            r#""critical_features": ["tool_use"]"#,
            &format!(r#""critical_features": ["tool_use", {hostile}]"#),
        );
    let dir = TempDir::new("models-hostile-data");
    write(&dir, "input.json", &document);
    let input = dir.join("input.json");
    let (status, stdout, stderr) = recommend(&[path_text(&input)]);
    // The region is in no catalog list, so Mantle Messages (no region list)
    // stays available only for sources without one; the run either recommends
    // or reports a plain error, never a raw control character.
    assert_eq!(stdout.is_empty(), status != 0);
    if status == 0 {
        let written = fs::read_to_string(dir.join("model-recommendation.json")).expect("result");
        for raw in ['\u{1b}', '\u{7}', '\r', '\u{202e}'] {
            assert!(!written.contains(raw), "{raw:?}");
        }
        assert!(
            written.contains("\\u001b"),
            "the JSON writer escapes the controls"
        );
    } else {
        assert_safe(&stderr);
        assert!(!stderr.contains("forged: ok"), "{stderr}");
    }
}

#[test]
fn engine_errors_escape_a_hostile_workload_id() {
    // The schema restricts workload ids, so the engine is driven directly.
    let workload = merged(
        r#"{"workload_id": "x", "source": {"provider": "anthropic", "model_ids": [],
            "sdk": "s", "api_surface": "messages", "source_paths": []},
            "requirements": {"priority": "balanced", "critical_features": [],
            "min_context_tokens": 9000000}, "detected_features": []}"#,
        "{}",
    );
    let Json::Object(mut members) = workload else {
        panic!("workload");
    };
    members[0].1 = Json::string(HOSTILE);
    let hostile_workload = Json::Object(members);
    let mut document = input_with(&hostile_workload, "us-east-1");
    let Json::Object(top) = &mut document else {
        panic!("input");
    };
    top.iter_mut()
        .find(|(k, _)| k == "primary_unit")
        .expect("unit")
        .1 = Json::string(HOSTILE);

    let message = recommend_input(&document, false)
        .expect_err("no candidate")
        .to_string();
    assert!(message.contains("\\x1b[31m"), "{message}");
    assert_safe(&format!("error: {message}\n"));

    // A duplicate hostile id.
    let Json::Object(top) = &mut document else {
        panic!("input");
    };
    let workloads = &mut top.iter_mut().find(|(k, _)| k == "workloads").expect("w").1;
    let Json::Array(items) = workloads else {
        panic!("workloads");
    };
    let mut repaired = items[0].clone();
    let Json::Object(fields) = &mut repaired else {
        panic!("workload");
    };
    fields
        .iter_mut()
        .find(|(k, _)| k == "requirements")
        .expect("requirements")
        .1 = j(r#"{"priority": "balanced", "critical_features": []}"#);
    *items = vec![repaired.clone(), repaired];
    let message = recommend_input(&document, false)
        .expect_err("duplicate")
        .to_string();
    assert!(message.starts_with("duplicate workload_id: "), "{message}");
    assert_safe(&format!("error: {message}\n"));
    assert!(Input::from_json(&document).is_ok());
}

#[cfg(unix)]
#[test]
fn write_and_link_diagnostics_escape_hostile_output_paths() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("models-hostile-out");
    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    // An output below a directory that does not exist.
    let output = dir.join(hostile_name("-dir")).join("out.json");
    let (status, stdout, stderr) = recommend(&[path_text(&input), "--output", path_text(&output)]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_safe(&stderr);
    assert!(stderr.starts_with("error: cannot write "), "{stderr}");
    assert!(stderr.contains("\\x1b"), "{stderr}");

    // A linked output named hostilely.
    let target = dir.join("target.json");
    fs::write(&target, "{}").expect("write");
    symlink(&target, dir.join(hostile_name("-link.json"))).expect("link");
    let (status, _, stderr) = recommend(&[
        path_text(&input),
        "--output",
        path_text(&dir.join(hostile_name("-link.json"))),
    ]);
    assert_eq!(status, 1);
    assert_safe(&stderr);
    assert_eq!(fs::read_to_string(&target).expect("untouched"), "{}");

    // A linked input named hostilely.
    symlink(&input, dir.join(hostile_name("-linked-input.json"))).expect("link");
    let (status, _, stderr) =
        recommend(&[path_text(&dir.join(hostile_name("-linked-input.json")))]);
    assert_eq!(status, 1);
    assert_safe(&stderr);
    assert!(stderr.contains("symbolic link"), "{stderr}");
}

#[test]
fn a_hostile_plugin_root_is_escaped_in_the_catalog_diagnostic() {
    let dir = TempDir::new("models-hostile-root");
    write(&dir, "input.json", INPUT);
    let root = dir.join(hostile_name("-plugin"));
    let (status, _, stderr) = exec(&[
        "startup",
        "model-recommend",
        path_text(&dir.join("input.json")),
        "--plugin-root",
        path_text(&root),
    ]);
    assert_eq!(status, 1);
    assert_safe(&stderr);
    assert!(stderr.contains("\\x1b"), "{stderr}");
    // The real root still works.
    let real = plugin_root();
    let (status, _, _) = exec(&[
        "startup",
        "model-recommend",
        path_text(&dir.join("input.json")),
        "--plugin-root",
        path_text(&real),
    ]);
    assert_eq!(status, 0);
}
