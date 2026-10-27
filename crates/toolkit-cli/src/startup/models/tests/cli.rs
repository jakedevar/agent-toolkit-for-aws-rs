//! The `startup model-recommend` command: files, flags, exit statuses, the
//! stale-verification rule, schema findings and the router entry.

use std::fs;

use toolkit_core::{parse_json, Json};

use super::{get, j, plugin_root, text};
use crate::startup::models::command::check_schema_for_tests;
use crate::startup::scoring::dumps_indent2;
use crate::startup::scoring::fixtures::{exec, path_text, write, TempDir};

pub(super) const INPUT: &str = r#"{
  "schema_version": 2,
  "region": "us-east-1",
  "primary_unit": "support-agent",
  "workloads": [{
    "workload_id": "support-agent",
    "source": {"provider": "anthropic", "model_ids": ["claude-3-7-sonnet-latest"],
               "sdk": "anthropic", "api_surface": "messages", "source_paths": ["src/agent.py"]},
    "requirements": {"priority": "balanced", "critical_features": ["tool_use"]},
    "detected_features": []
  }]
}"#;

pub(super) const OPENAI_INPUT: &str = r#"{
  "schema_version": 2,
  "region": "us-east-2",
  "primary_unit": "chat-svc",
  "workloads": [{
    "workload_id": "chat-svc",
    "source": {"provider": "openai", "model_ids": ["gpt-5.4"], "sdk": "openai",
               "api_surface": "responses", "source_paths": ["src/app.py"]},
    "requirements": {"priority": "balanced", "critical_features": [], "api_continuity": "required"},
    "detected_features": []
  }]
}"#;

pub(super) fn cli(args: &[&str]) -> (u8, String, String) {
    exec(args)
}

pub(super) fn recommend(args: &[&str]) -> (u8, String, String) {
    let root = plugin_root();
    let mut full = vec!["startup", "model-recommend"];
    full.extend_from_slice(args);
    full.extend(["--plugin-root", path_text(&root)]);
    exec(&full)
}

#[test]
fn a_recommendation_is_written_beside_the_input() {
    let dir = TempDir::new("models-ok");
    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    let (status, stdout, stderr) = recommend(&[path_text(&input)]);
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(stdout, "RESULT=ok WORKLOADS=1 SCHEMA_VALIDATED=yes\n");
    assert_eq!(stderr, "");
    let written = fs::read_to_string(dir.join("model-recommendation.json")).expect("result");
    assert!(written.ends_with("}\n"));
    let result = parse_json(&written).expect("valid JSON");
    assert_eq!(dumps_indent2(&result) + "\n", written);
    let rec = get(get(&result, "workloads"), "support-agent");
    assert_eq!(text(rec, "primary_model"), "anthropic.claude-sonnet-5");
    assert_eq!(text(get(&result, "catalog"), "verified_at"), "2026-09-24");
    // The members appear in the order of the Python dict literals.
    let Json::Object(members) = &result else {
        panic!("result");
    };
    let keys: Vec<&str> = members.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "schema_version",
            "catalog",
            "catalog_provenance",
            "primary_unit",
            "workloads"
        ]
    );
    let Json::Object(rec_members) = rec else {
        panic!("rec");
    };
    let rec_keys: Vec<&str> = rec_members.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        rec_keys,
        [
            "workload_id",
            "provider_module",
            "decision_status",
            "source",
            "source_analysis",
            "feature_assessment",
            "primary_model",
            "model_identity",
            "api_path",
            "invocation_model_id",
            "decision_options",
            "alternatives",
            "rationale",
            "blocks",
            "tuning",
            "compatibility",
            "architecture_impacts",
            "migration_deltas",
            "evaluation",
            "rollout",
            "verification"
        ]
    );
    // Deterministic: a second run writes the same bytes.
    let (status, _, _) = recommend(&[path_text(&input)]);
    assert_eq!(status, 0);
    assert_eq!(
        fs::read_to_string(dir.join("model-recommendation.json")).expect("result"),
        written
    );
}

#[test]
fn the_openai_key_order_matches_the_python_literals() {
    let dir = TempDir::new("models-openai");
    write(&dir, "input.json", OPENAI_INPUT);
    let input = dir.join("input.json");
    let output = dir.join("out.json");
    let (status, stdout, stderr) = recommend(&[path_text(&input), "--output", path_text(&output)]);
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(stdout, "RESULT=ok WORKLOADS=1 SCHEMA_VALIDATED=yes\n");
    let result = j(&fs::read_to_string(&output).expect("result"));
    let rec = get(get(&result, "workloads"), "chat-svc");
    let Json::Object(members) = rec else {
        panic!("rec");
    };
    let keys: Vec<&str> = members.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "workload_id",
            "provider_module",
            "decision_status",
            "source",
            "source_analysis",
            "feature_assessment",
            "primary_model",
            "model_identity",
            "api_path",
            "invocation_model_id",
            "decision_options",
            "alternatives",
            "rationale",
            "blocks",
            "tuning",
            "compatibility",
            "architecture_impacts",
            "additional_targets",
            "migration_deltas",
            "evaluation",
            "rollout",
            "verification"
        ]
    );
    assert_eq!(text(get(&result, "catalog"), "provider"), "openai");
    assert!(!dir.join("model-recommendation.json").exists());
}

#[test]
fn a_changed_invocation_contract_removes_the_stale_verification() {
    let dir = TempDir::new("models-stale");
    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    let verification = dir.join("model-verification.json");
    let result = dir.join("model-recommendation.json");

    // No previous result: the verification file is stale.
    write(&dir, "model-verification.json", "{}");
    assert_eq!(recommend(&[path_text(&input)]).0, 0);
    assert!(!verification.exists());

    // Same contract: the verification stays.
    write(&dir, "model-verification.json", "{}");
    assert_eq!(recommend(&[path_text(&input)]).0, 0);
    assert!(verification.exists());

    // A different model/path in the input removes it again.
    let changed = INPUT.replace(r#""priority": "balanced""#, r#""priority": "cost""#);
    write(&dir, "input.json", &changed);
    assert_eq!(recommend(&[path_text(&input)]).0, 0);
    assert!(!verification.exists());

    // A previous result that is not a result is a changed contract too.
    for previous in [
        "not json",
        "[]",
        r#"{"workloads": []}"#,
        r#"{"workloads": {"a": {}}}"#,
    ] {
        write(&dir, "model-recommendation.json", previous);
        write(&dir, "model-verification.json", "{}");
        assert_eq!(recommend(&[path_text(&input)]).0, 0, "{previous}");
        assert!(!verification.exists(), "{previous}");
    }

    // A change that does not touch the contract (key order) keeps it.
    let written = fs::read_to_string(&result).expect("result");
    let reordered = dumps_indent2(&j(&written));
    write(&dir, "model-recommendation.json", &reordered);
    write(&dir, "model-verification.json", "{}");
    assert_eq!(recommend(&[path_text(&input)]).0, 0);
    assert!(verification.exists());
}

#[test]
fn a_missing_input_is_a_usage_error() {
    let (status, stdout, stderr) = cli(&["startup", "model-recommend"]);
    assert_eq!(status, 2);
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("usage: toolkit startup model-recommend INPUT.json"),
        "{stderr}"
    );
    assert_eq!(cli(&["startup", "model-recommend", "a", "b"]).0, 2);
    assert_eq!(cli(&["startup", "model-recommend", "a", "--bogus"]).0, 2);
}

#[test]
fn flags_accept_both_spellings() {
    let dir = TempDir::new("models-flags");
    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    let output = dir.join("custom.json");
    let root = plugin_root();
    let (status, stdout, stderr) = exec(&[
        "startup",
        "model-recommend",
        path_text(&input),
        &format!("--output={}", path_text(&output)),
        &format!("--plugin-root={}", path_text(&root)),
    ]);
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(stdout, "RESULT=ok WORKLOADS=1 SCHEMA_VALIDATED=yes\n");
    assert!(output.exists());
}

#[test]
fn unreadable_and_invalid_inputs_are_one_line_errors() {
    let dir = TempDir::new("models-bad-input");
    let missing = dir.join("absent.json");
    let (status, stdout, stderr) = recommend(&[path_text(&missing)]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(stderr.starts_with("error: cannot read "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");

    write(&dir, "broken.json", "{\"a\": ");
    let (status, _, stderr) = recommend(&[path_text(&dir.join("broken.json"))]);
    assert_eq!(status, 1);
    assert!(stderr.contains("invalid JSON"), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

#[test]
fn schema_findings_name_the_field_and_the_rule() {
    let dir = TempDir::new("models-schema");
    let bad = INPUT
        .replace(r#""region": "us-east-1","#, "")
        .replace(r#""priority": "balanced""#, r#""priority": "fastest""#)
        .replace(
            r#""workload_id": "support-agent""#,
            r#""workload_id": "Support Agent""#,
        )
        .replace(
            r#""detected_features": []"#,
            r#""detected_features": ["nonsense"], "extra": 1"#,
        );
    write(&dir, "input.json", &bad);
    let (status, stdout, stderr) = recommend(&[path_text(&dir.join("input.json"))]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .all(|line| line.starts_with("  - $")),
        "{stderr}"
    );
    assert!(
        lines.last().is_some_and(|last| last.starts_with("error: ")
            && last.ends_with("does not satisfy the model-recommendation-input schema")),
        "{stderr}"
    );
    assert!(
        stderr.contains("'region' is a required property"),
        "{stderr}"
    );
    assert!(
        stderr.contains("$.workloads[0].requirements.priority: a string value is not one of"),
        "{stderr}"
    );
    assert!(
        stderr.contains("$.workloads[0].workload_id: a string value does not match"),
        "{stderr}"
    );
    assert!(
        stderr.contains("$.workloads[0].detected_features[0]"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Additional properties are not allowed ('extra' was unexpected)"),
        "{stderr}"
    );
    // The offending values are never repeated.
    for value in ["fastest", "Support Agent", "nonsense"] {
        assert!(!stderr.contains(value), "{value} in {stderr}");
    }
    assert!(!dir.join("model-recommendation.json").exists());
}

#[test]
fn many_findings_are_summarized() {
    let dir = TempDir::new("models-many");
    let features: Vec<String> = (0..40).map(|_| "\"nonsense\"".to_string()).collect();
    let bad = INPUT.replace(
        r#""detected_features": []"#,
        &format!(r#""detected_features": [{}]"#, features.join(",")),
    );
    write(&dir, "input.json", &bad);
    let (status, _, stderr) = recommend(&[path_text(&dir.join("input.json"))]);
    assert_eq!(status, 1);
    assert!(stderr.contains("  - ... and "), "{stderr}");
    assert!(stderr.lines().count() <= 23, "{stderr}");
}

#[test]
fn the_output_contract_is_checked_before_anything_is_written() {
    let dir = TempDir::new("models-contract");
    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    assert_eq!(recommend(&[path_text(&input)]).0, 0);
    let mut written =
        j(&fs::read_to_string(dir.join("model-recommendation.json")).expect("result"));
    // A recommended workload without its model identity breaks the contract.
    let Json::Object(top) = &mut written else {
        panic!("result");
    };
    let Json::Object(workloads) = &mut top.iter_mut().find(|(k, _)| k == "workloads").expect("w").1
    else {
        panic!("workloads");
    };
    let Json::Object(rec) = &mut workloads[0].1 else {
        panic!("rec");
    };
    rec.iter_mut()
        .find(|(k, _)| k == "model_identity")
        .expect("identity")
        .1 = Json::Null;
    let findings = check_schema_for_tests(&written);
    assert!(!findings.is_empty());
    assert!(
        findings.iter().all(|line| line.starts_with("  - $")),
        "{findings:?}"
    );
}

#[test]
fn catalogs_can_be_named_and_the_openai_catalog_is_lazy() {
    let dir = TempDir::new("models-catalogs");
    write(&dir, "input.json", INPUT);
    write(&dir, "openai.json", INPUT); // never read for an Anthropic-only input
    let input = dir.join("input.json");
    let missing = dir.join("absent.json");
    // Lazy: an Anthropic-only run never touches the OpenAI catalog.
    let (status, _, stderr) =
        recommend(&[path_text(&input), "--openai-catalog", path_text(&missing)]);
    assert_eq!(status, 0, "{stderr}");
    // An OpenAI workload needs it.
    write(&dir, "oai-input.json", OPENAI_INPUT);
    let (status, _, stderr) = recommend(&[
        path_text(&dir.join("oai-input.json")),
        "--openai-catalog",
        path_text(&missing),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.starts_with("error: cannot read "), "{stderr}");
    // The Anthropic catalog is always needed.
    let (status, _, stderr) = recommend(&[path_text(&input), "--catalog", path_text(&missing)]);
    assert_eq!(status, 1);
    assert!(stderr.starts_with("error: cannot read "), "{stderr}");
    // Without a plugin root the dated files are not found.
    let (status, _, stderr) = exec(&[
        "startup",
        "model-recommend",
        path_text(&input),
        "--plugin-root",
        path_text(&dir),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("cannot read"), "{stderr}");
}

#[test]
fn a_catalog_that_cannot_satisfy_a_workload_is_an_error() {
    let dir = TempDir::new("models-nocandidate");
    let catalog = r#"{"schema_version": 2, "provider": "anthropic", "verified_at": "2026-01-01",
        "verified_region": "us-east-1", "paths": {}, "models": {}}"#;
    write(&dir, "catalog.json", catalog);
    write(&dir, "input.json", INPUT);
    let (status, _, stderr) = recommend(&[
        path_text(&dir.join("input.json")),
        "--catalog",
        path_text(&dir.join("catalog.json")),
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        "error: catalog has no model/path candidate satisfying workload support-agent requirements\n"
    );
}

#[test]
fn a_malformed_catalog_names_the_missing_field() {
    let dir = TempDir::new("models-badcatalog");
    write(&dir, "input.json", INPUT);
    let input = path_text(&dir.join("input.json")).to_string();
    let cases = [
        (
            r#"{"schema_version": 2}"#,
            "missing required keys provider, verified_at, verified_region, paths, models",
        ),
        ("[]", "the catalog must be a JSON object"),
        (
            r#"{"schema_version": 2, "provider": "x", "verified_at": "d", "verified_region": "r",
                "paths": {}, "models": {"m": {"display_name": "M"}}}"#,
            "m missing family",
        ),
        (
            r#"{"schema_version": 2, "provider": "x", "verified_at": "d", "verified_region": "r",
                "paths": {}, "models": {"m": {"display_name": "M", "family": "f", "version": "1",
                "context_window": 0, "output_token_ceiling": 1, "capabilities": [], "paths": {}}}}"#,
            "model m context_window must be a positive int or \"unknown\"",
        ),
        (
            r#"{"schema_version": 2, "provider": "x", "verified_at": "d", "verified_region": "r",
                "paths": {}, "models": {"m": {"display_name": "M", "family": "f", "version": "1",
                "context_window": 1, "output_token_ceiling": 1, "capabilities": [],
                "paths": {"p": {"available": true}}}}}"#,
            "model m path p lacks \"model_id\"",
        ),
        (
            r#"{"schema_version": 2, "provider": "x", "verified_at": "d", "verified_region": "r",
                "paths": {}, "models": {"m": {"display_name": "M", "family": "f", "version": "1",
                "context_window": 1, "output_token_ceiling": 1, "capabilities": "tool_use", "paths": {}}}}"#,
            "model m \"capabilities\" must be an array",
        ),
        (
            r#"{"schema_version": 2, "provider": "x", "verified_at": "d", "verified_region": "r",
                "paths": {}, "models": {"m": {"display_name": "M", "family": "f", "version": 5,
                "context_window": 1, "output_token_ceiling": 1, "capabilities": [], "paths": {}}}}"#,
            "model m \"version\" must be a string",
        ),
    ];
    for (catalog, expected) in cases {
        write(&dir, "catalog.json", catalog);
        let (status, stdout, stderr) =
            recommend(&[&input, "--catalog", path_text(&dir.join("catalog.json"))]);
        assert_eq!(status, 1, "{catalog}");
        assert_eq!(stdout, "");
        assert!(stderr.contains(expected), "{expected} not in {stderr}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
    }
}

#[test]
fn the_openai_catalog_requires_a_generation() {
    let dir = TempDir::new("models-generation");
    write(&dir, "oai-input.json", OPENAI_INPUT);
    write(
        &dir,
        "catalog.json",
        r#"{"schema_version": 2, "provider": "openai", "verified_at": "d", "verified_region": "r",
            "paths": {}, "models": {"m": {"display_name": "M", "family": "f", "version": "1",
            "context_window": 1, "output_token_ceiling": 1, "capabilities": [], "paths": {}}}}"#,
    );
    let (status, _, stderr) = recommend(&[
        path_text(&dir.join("oai-input.json")),
        "--openai-catalog",
        path_text(&dir.join("catalog.json")),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("m missing generation"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn links_are_refused_for_every_validator_owned_read_and_write() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("models-links");
    let outside = TempDir::new("models-links-outside");
    write(&outside, "input.json", INPUT);
    write(&outside, "out.json", "{}");
    symlink(outside.join("input.json"), dir.join("linked-input.json")).expect("link");
    let (status, _, stderr) = recommend(&[path_text(&dir.join("linked-input.json"))]);
    assert_eq!(status, 1);
    assert!(stderr.contains("symbolic link"), "{stderr}");

    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    // A linked catalog.
    symlink(
        plugin_root()
            .join("skills/agent-advisor/references/models/anthropic-bedrock-2026-09-24.json"),
        dir.join("linked-catalog.json"),
    )
    .expect("link");
    let (status, _, stderr) = recommend(&[
        path_text(&input),
        "--catalog",
        path_text(&dir.join("linked-catalog.json")),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("symbolic link"), "{stderr}");

    // A linked output is never written through.
    symlink(outside.join("out.json"), dir.join("linked-out.json")).expect("link");
    let (status, stdout, stderr) = recommend(&[
        path_text(&input),
        "--output",
        path_text(&dir.join("linked-out.json")),
    ]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(stderr.contains("cannot write "), "{stderr}");
    assert_eq!(
        fs::read_to_string(outside.join("out.json")).expect("untouched"),
        "{}"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_previous_result_is_not_trusted() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("models-prevlink");
    let outside = TempDir::new("models-prevlink-outside");
    write(&dir, "input.json", INPUT);
    let input = dir.join("input.json");
    assert_eq!(recommend(&[path_text(&input)]).0, 0);
    // Move the real result away and link to it: the link is not read, so the
    // contract counts as changed and the verification goes; the write is refused.
    fs::rename(
        dir.join("model-recommendation.json"),
        outside.join("real.json"),
    )
    .expect("move");
    symlink(
        outside.join("real.json"),
        dir.join("model-recommendation.json"),
    )
    .expect("link");
    write(&dir, "model-verification.json", "{}");
    let (status, _, stderr) = recommend(&[path_text(&input)]);
    assert_eq!(status, 1);
    assert!(stderr.contains("cannot write "), "{stderr}");
    assert!(!dir.join("model-verification.json").exists());
}

#[test]
fn the_command_is_registered_once_in_the_router_the_readme_and_the_migration_notes() {
    let lib = include_str!("../../../lib.rs");
    assert_eq!(
        lib.matches(r#"path: &["startup", "model-recommend"]"#)
            .count(),
        1
    );
    assert_eq!(
        lib.matches("  toolkit startup model-recommend INPUT")
            .count(),
        1
    );
    assert_eq!(lib.matches("  startup model-recommend INPUT").count(), 1);
    let spec_start = lib
        .find(r#"path: &["startup", "model-recommend"]"#)
        .expect("spec");
    let spec = &lib[spec_start..spec_start + 400];
    assert!(spec.contains(
        r#"value_flags: &["--output", "--catalog", "--openai-catalog", "--plugin-root"]"#
    ));
    assert!(spec.contains("strict_values: false"));
    assert!(spec.contains("max_positionals: 1"));
    assert!(spec.contains("passthrough: false"));
    let readme = include_str!("../../../../README.md");
    assert_eq!(
        readme
            .matches("### startup model-recommend (#4401)")
            .count(),
        1
    );
    let doc = include_str!("../../../../../../docs/migration/startup-models.md");
    assert!(doc.contains("## Deliberate differences"));
    assert!(doc.contains("## Test mapping"));
}

#[test]
fn phase_documents_name_the_default_catalogs() {
    // test_current_catalog_readers_match_engine_defaults: the documents that
    // tell the model which catalog to load must name the engine's defaults.
    let skill = crate::startup::scoring::fixtures::real_skill();
    let pattern = fancy_regex::Regex::new(r"(?:anthropic|openai)-bedrock-\d{4}-\d{2}-\d{2}\.json")
        .expect("pattern");
    let expected: std::collections::BTreeSet<String> = [
        crate::startup::models::DEFAULT_CATALOG,
        crate::startup::models::OPENAI_CATALOG,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    for relative in [
        "references/phases/model-recommend/model-recommend.md",
        "references/phases/model-recommend/model-recommend-assemble.md",
        "references/decision-refs/model-selection.md",
    ] {
        let body = fs::read_to_string(skill.join(relative)).expect("phase document");
        let named: std::collections::BTreeSet<String> = pattern
            .find_iter(&body)
            .flatten()
            .map(|found| found.as_str().to_string())
            .collect();
        assert_eq!(named, expected, "{relative}");
        if let Some(rest) = body.strip_prefix("---\n") {
            let frontmatter = rest.split("---").next().unwrap_or("");
            let loaded: std::collections::BTreeSet<String> = pattern
                .find_iter(frontmatter)
                .flatten()
                .map(|found| found.as_str().to_string())
                .collect();
            assert_eq!(loaded, expected, "{relative} frontmatter");
        }
    }
}

#[test]
fn the_schema_keywords_of_the_contract_are_all_enforced() {
    let dir = TempDir::new("models-keywords");
    let cases = [
        (
            r#""critical_features": ["tool_use"]"#,
            r#""critical_features": ["tool_use", "tool_use"]"#,
            "$.workloads[0].requirements.critical_features: an array value violates the schema keyword 'uniqueItems'",
        ),
        (
            r#""schema_version": 2"#,
            r#""schema_version": 3"#,
            "$.schema_version: 2 was expected",
        ),
        (
            r#""region": "us-east-1""#,
            r#""region": """#,
            "$.region: a string value should be non-empty",
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            r#""critical_features": ["tool_use"], "min_context_tokens": 0"#,
            "$.workloads[0].requirements.min_context_tokens: an integer value is less than the minimum of 1",
        ),
        (
            r#""critical_features": ["tool_use"]"#,
            r#""critical_features": ["tool_use"], "min_context_tokens": 1.5"#,
            "$.workloads[0].requirements.min_context_tokens: a number value is not of type 'integer'",
        ),
    ];
    for (from, to, expected) in cases {
        write(&dir, "input.json", &INPUT.replace(from, to));
        let (status, _, stderr) = recommend(&[path_text(&dir.join("input.json"))]);
        assert_eq!(status, 1, "{to}");
        assert!(stderr.contains(expected), "{expected} not in {stderr}");
    }
    let (status, _, stderr) = {
        let empty = INPUT.replace(
            &INPUT[INPUT.find(r#""workloads""#).expect("workloads")..],
            r#""workloads": []}"#,
        );
        write(&dir, "input.json", &empty);
        recommend(&[path_text(&dir.join("input.json"))])
    };
    assert_eq!(status, 1);
    assert!(stderr.contains("$.workloads: an array value violates the schema keyword 'minItems'"), "{stderr}");
    // An integral float is an integer for the contract, as in jsonschema.
    write(
        &dir,
        "input.json",
        &INPUT.replace(
            r#""critical_features": ["tool_use"]"#,
            r#""critical_features": ["tool_use"], "min_context_tokens": 1000.0"#,
        ),
    );
    assert_eq!(recommend(&[path_text(&dir.join("input.json"))]).0, 0);
}
