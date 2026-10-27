//! Offline, deterministic tests for the `toolkit startup` commands (#4399).
//!
//! Every rule of the three Python scripts has a positive and a negative
//! fixture here, plus the migrated Python unit tests
//! (`tests/test_model_id_lint.py`) and runs against the real plugin tree.
//! Fixtures live in unique temporary directories removed on drop. Retired
//! model IDs used by the fixtures are invented (`acme.*`) so the real
//! plugin lint never sees them.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::model_ids::{
    canonicalize, has_fabricated_id, ids_in_backticks, is_model_id, rebuild_dual_ids, suffix,
    IdMatcher,
};
use super::pricing::{default_catalog_name, positive_rate, starts_with_active, strip_region};
use super::text::{fold, is_space, is_word, Pattern, Tok};
use crate::run;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-startup-{label}-{}-{}",
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

fn path_text(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}

fn real_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

// ---- pattern matcher -----------------------------------------------------

#[test]
fn folding_matches_python_ignorecase_equivalences() {
    assert_eq!(fold('A'), 'a');
    assert_eq!(fold('\u{130}'), 'i');
    assert_eq!(fold('\u{131}'), 'i');
    assert_eq!(fold('\u{17f}'), 's');
    assert_eq!(fold('\u{212a}'), 'k');
    assert_eq!(fold('7'), '7');
    assert_eq!(fold('$'), '$');
}

#[test]
fn whitespace_and_word_classes_follow_python_str() {
    for c in [
        ' ', '\t', '\n', '\u{b}', '\u{c}', '\u{1c}', '\u{a0}', '\u{2003}',
    ] {
        assert!(is_space(c), "{c:?}");
    }
    for c in ['a', 'Z', '0', '_', 'é', '日'] {
        assert!(is_word(c), "{c:?}");
    }
    for c in ['-', '.', '$', '`', ' '] {
        assert!(!is_word(c), "{c:?}");
    }
    assert!(!is_space('x'));
}

const GREEDY: Pattern = Pattern::new("greedy", &[Tok::Space0, Tok::Lit(" x")]);

const PHRASE: Pattern = Pattern::new(
    "phrase",
    &[
        Tok::Boundary,
        Tok::Lit("big"),
        Tok::Space1,
        Tok::Lit("cat"),
        Tok::Space0,
        Tok::Lit("s"),
        Tok::Boundary,
    ],
);

#[test]
fn patterns_search_case_insensitively_with_word_boundaries() {
    assert_eq!(PHRASE.source(), "phrase");
    for hit in [
        "big cat s",
        "BIG\t\n CATS",
        "a big cat s.",
        "see: Big  Cat\u{a0}s!",
    ] {
        assert!(PHRASE.is_match(hit), "{hit:?}");
    }
    for miss in [
        "",
        "bigcat s",
        "big cats2",
        "abig cats",
        "big cat",
        "big dog s",
    ] {
        assert!(!PHRASE.is_match(miss), "{miss:?}");
    }
}

#[test]
fn optional_and_class_tokens_backtrack_like_a_regular_expression() {
    const LINK: Pattern = Pattern::new(
        "link",
        &[
            Tok::Lit("a"),
            Tok::Opt('s'),
            Tok::Opt('/'),
            Tok::OneOf("\"'"),
        ],
    );
    for hit in ["a\"", "as'", "aS/\"", "a/'"] {
        assert!(LINK.is_match(hit), "{hit:?}");
    }
    for miss in ["a", "as", "a//\"", "a\u{201c}"] {
        assert!(!LINK.is_match(miss), "{miss:?}");
    }
    // `\s*` may give back characters the next literal needs.
    assert!(GREEDY.is_match("   x"));
    assert!(!GREEDY.is_match("x"));
}

// ---- model-pricing -------------------------------------------------------

const CATALOG_NAME: &str = "cat.json";
const CACHE_HEADER: &str =
    "| Model | Model ID | Provider | Input $/1M | Output $/1M | Context | Tier | Status |\n\
| --- | --- | --- | --- | --- | --- | --- | --- |\n";

fn pricing_row(model_id: &str, input: &str, output: &str, status: &str) -> String {
    format!("| Name | {model_id} | Anthropic | {input} | {output} | 1M | tier | {status} |\n")
}

fn catalog_json(ids: &[&str]) -> String {
    let models: Vec<String> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            format!(r#""m{index}":{{"paths":{{"runtime_converse":{{"model_id":"{id}"}}}}}}"#)
        })
        .collect();
    format!(r#"{{"models":{{{}}}}}"#, models.join(","))
}

fn write_selector(root: &Path, line: &str) {
    write(
        root,
        "skills/agent-advisor/scripts/model_recommendation.py",
        &format!("import os\nMODELS_DIR = 1\n{line}\nOTHER = 2\n"),
    );
}

fn selector_line() -> String {
    format!("DEFAULT_CATALOG = MODELS_DIR / \"{CATALOG_NAME}\"")
}

fn write_catalog(root: &Path, contents: &str) {
    write(
        root,
        &format!("skills/agent-advisor/references/models/{CATALOG_NAME}"),
        contents,
    );
}

fn write_cache(root: &Path, cloud: &str, rows: &str) {
    write(
        root,
        &format!("skills/{cloud}/references/vendored/ai/bedrock-pricing-cache.md"),
        &format!("# Pricing\n\n{CACHE_HEADER}{rows}"),
    );
}

const SONNET: &str = "anthropic.claude-sonnet-5";
const OPUS: &str = "anthropic.claude-opus-5-5";

fn priced_rows() -> String {
    format!(
        "{}{}",
        pricing_row(SONNET, "2.00", "10.00", "active ($2/$10)"),
        pricing_row(&format!("global.{OPUS}"), "4.00", "20.00", "Active"),
    )
}

/// A plugin tree where both candidates are priced in both clouds.
fn pricing_fixture(label: &str) -> TempDir {
    let root = TempDir::new(label);
    write_selector(&root, &selector_line());
    write_catalog(&root, &catalog_json(&[SONNET, OPUS]));
    write_cache(&root, "gcp-to-aws", &priced_rows());
    write_cache(&root, "azure-to-aws", &priced_rows());
    root
}

fn pricing(root: &Path) -> (u8, String, String) {
    exec(&["startup", "model-pricing", "--plugin-root", path_text(root)])
}

const PRICING_OK: &str =
    "Model pricing coverage: all Anthropic candidates have numeric Active cache rows.\n";

#[test]
fn pricing_passes_when_every_candidate_has_an_active_numeric_row() {
    let root = pricing_fixture("pricing-ok");
    let (status, stdout, stderr) = pricing(&root);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (0, PRICING_OK, "")
    );
}

#[test]
fn pricing_accepts_the_equals_form_and_defaults_to_the_current_directory() {
    let root = pricing_fixture("pricing-equals");
    let flag = format!("--plugin-root={}", path_text(&root));
    let (status, stdout, _) = exec(&["startup", "model-pricing", &flag]);
    assert_eq!((status, stdout.as_str()), (0, PRICING_OK));
    // The default root is `.`, which is the crate directory under `cargo test`
    // and has no plugin files: a clean runtime error, not a panic.
    let (status, stdout, stderr) = exec(&["startup", "model-pricing"]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(stderr.starts_with("error: cannot read "), "{stderr}");
}

#[test]
fn pricing_reports_missing_candidates_per_cloud_in_order() {
    let root = pricing_fixture("pricing-missing");
    write_catalog(&root, &catalog_json(&[SONNET, "anthropic.claude-fable-9"]));
    let (status, stdout, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "gcp-to-aws: candidate anthropic.claude-fable-9 has no numeric Active price row.\n\
         azure-to-aws: candidate anthropic.claude-fable-9 has no numeric Active price row.\n\
         error: 2 model pricing problem(s)\n"
    );
}

#[test]
fn pricing_requires_the_row_in_each_cloud_separately() {
    let root = pricing_fixture("pricing-one-cloud");
    write_cache(
        &root,
        "azure-to-aws",
        &pricing_row(SONNET, "2.00", "10.00", "active"),
    );
    let (status, _, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        format!(
            "azure-to-aws: candidate {OPUS} has no numeric Active price row.\n\
             error: 1 model pricing problem(s)\n"
        )
    );
}

#[test]
fn pricing_status_must_start_with_the_word_active() {
    for (status_cell, priced) in [
        ("active", true),
        ("ACTIVE (verified 2026-09-24)", true),
        ("Active; no Batch", true),
        ("aCtIvE,", true),
        ("activated", false),
        ("active_soon", false),
        ("inactive", false),
        ("restricted (Active only for covered models)", false),
        ("legacy", false),
        ("", false),
        ("acti", false),
    ] {
        let root = pricing_fixture("pricing-status");
        let mut rows = pricing_row(SONNET, "2.00", "10.00", "active");
        rows.push_str(&pricing_row(OPUS, "4.00", "20.00", status_cell));
        write_cache(&root, "gcp-to-aws", &rows);
        let (code, _, stderr) = pricing(&root);
        assert_eq!(code == 0, priced, "{status_cell:?}: {stderr}");
        if !priced {
            assert!(
                stderr.contains(&format!("gcp-to-aws: candidate {OPUS} has no numeric")),
                "{status_cell:?}: {stderr}"
            );
        }
    }
}

#[test]
fn pricing_rates_must_be_finite_and_positive_numbers() {
    for (rate, ok) in [
        ("2.00", true),
        ("0.0001", true),
        (".5", true),
        ("5.", true),
        ("+5", true),
        ("1e1", true),
        ("1_0.5", true),
        ("0", false),
        ("0.0", false),
        ("-1", false),
        ("-0.5", false),
        ("1__0", false),
        ("_1", false),
        ("1_", false),
        ("1_.5", false),
        ("nan", false),
        ("inf", false),
        ("-inf", false),
        ("Infinity", false),
        ("1e999", false),
        ("_unverified_", false),
        ("n/a", false),
        ("$2", false),
        ("", false),
        ("1 0", false),
    ] {
        assert_eq!(positive_rate(rate), ok, "{rate:?}");
    }
}

#[test]
fn pricing_both_rate_columns_are_required_and_other_columns_ignored() {
    for (input, output, priced) in [
        ("2.00", "10.00", true),
        ("2.00", "_unverified_", false),
        ("n/a", "10.00", false),
        ("0", "10.00", false),
    ] {
        let root = pricing_fixture("pricing-columns");
        let mut rows = pricing_row(SONNET, "2.00", "10.00", "active");
        // Context and tier cells are never read.
        rows.push_str(&pricing_row(OPUS, input, output, "active"));
        write_cache(&root, "gcp-to-aws", &rows);
        assert_eq!(pricing(&root).0 == 0, priced, "{input} {output}");
    }
}

#[test]
fn pricing_region_prefixes_are_stripped_once_and_only_at_the_start() {
    for (id, expected) in [
        ("us.anthropic.x", "anthropic.x"),
        ("eu.anthropic.x", "anthropic.x"),
        ("au.anthropic.x", "anthropic.x"),
        ("jp.anthropic.x", "anthropic.x"),
        ("global.anthropic.x", "anthropic.x"),
        ("us.us.anthropic.x", "us.anthropic.x"),
        ("anthropic.us.x", "anthropic.us.x"),
        ("usa.anthropic.x", "usa.anthropic.x"),
        ("US.anthropic.x", "US.anthropic.x"),
        ("anthropic.x", "anthropic.x"),
    ] {
        assert_eq!(strip_region(id), expected, "{id}");
    }
    let root = pricing_fixture("pricing-region");
    let rows = format!(
        "{}{}",
        pricing_row(&format!("us.{SONNET}"), "2.00", "10.00", "active"),
        pricing_row(&format!("jp.{OPUS}"), "4.00", "20.00", "active"),
    );
    write_cache(&root, "gcp-to-aws", &rows);
    assert_eq!(pricing(&root).0, 0);
}

#[test]
fn pricing_ignores_rows_that_are_not_complete_table_lines() {
    let root = pricing_fixture("pricing-shape");
    let rows = format!(
        "{}  {}| short | row |\ntext | not | a | row | at all | x | y | active\n",
        pricing_row(SONNET, "2.00", "10.00", "active"),
        // Indented rows are not table rows for this scanner.
        pricing_row(OPUS, "4.00", "20.00", "active"),
    );
    write_cache(&root, "gcp-to-aws", &rows);
    let (status, _, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert!(
        stderr.contains(&format!("gcp-to-aws: candidate {OPUS}")),
        "{stderr}"
    );
}

#[test]
fn pricing_reads_crlf_caches_and_catalogs() {
    let root = pricing_fixture("pricing-crlf");
    let crlf = priced_rows().replace('\n', "\r\n");
    write_cache(&root, "gcp-to-aws", &crlf);
    write_selector(&root, &format!("{}\r", selector_line()));
    assert_eq!(pricing(&root).0, 0);
}

#[test]
fn pricing_selector_must_name_the_default_catalog_at_the_line_start() {
    for (line, found) in [
        (selector_line(), true),
        (format!("    {}", selector_line()), false),
        ("DEFAULT_CATALOG = MODELS_DIR / \"\"".to_string(), false),
        (
            "DEFAULT_CATALOG = MODELS_DIR / 'cat.json'".to_string(),
            false,
        ),
        (
            "OPENAI_CATALOG = MODELS_DIR / \"cat.json\"".to_string(),
            false,
        ),
    ] {
        let root = pricing_fixture("pricing-selector");
        write_selector(&root, &line);
        let (status, stdout, stderr) = pricing(&root);
        if found {
            assert_eq!((status, stdout.as_str()), (0, PRICING_OK));
        } else {
            assert_eq!(status, 1, "{line:?}");
            assert_eq!(
                stderr, "error: Cannot locate the default Anthropic catalog.\n",
                "{line:?}"
            );
        }
    }
    assert_eq!(
        default_catalog_name("x\nDEFAULT_CATALOG = MODELS_DIR / \"a.json\"\n"),
        Some("a.json".to_string())
    );
    assert_eq!(
        default_catalog_name("DEFAULT_CATALOG = MODELS_DIR / \"a\nb\""),
        Some("a\nb".to_string())
    );
    assert_eq!(default_catalog_name(""), None);
    assert_eq!(
        default_catalog_name("DEFAULT_CATALOG = MODELS_DIR / \"unterminated"),
        None
    );
}

#[test]
fn pricing_catalog_name_cannot_leave_the_models_directory() {
    for name in ["../escape.json", "/etc/passwd", "a\\b.json"] {
        let root = pricing_fixture("pricing-escape");
        write_selector(&root, &format!("DEFAULT_CATALOG = MODELS_DIR / \"{name}\""));
        let (status, stdout, stderr) = pricing(&root);
        assert_eq!(status, 1, "{name}");
        assert_eq!(stdout, "");
        assert!(
            stderr.starts_with("error: cannot read skills/agent-advisor/references/models/"),
            "{name}: {stderr}"
        );
    }
    let root = pricing_fixture("pricing-escape-reason");
    write_selector(&root, "DEFAULT_CATALOG = MODELS_DIR / \"../escape.json\"");
    assert!(pricing(&root).2.contains("parent traversal"));
}

#[test]
fn pricing_missing_inputs_abort_with_one_message() {
    let root = pricing_fixture("pricing-missing-input");
    write_catalog(&root, &catalog_json(&["anthropic.unpriced"]));
    fs::remove_file(
        root.join("skills/azure-to-aws/references/vendored/ai/bedrock-pricing-cache.md"),
    )
    .expect("remove cache");
    let (status, stdout, stderr) = pricing(&root);
    // The earlier gcp finding is discarded, like the Python except clause.
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "error: cannot read skills/azure-to-aws/references/vendored/ai/bedrock-pricing-cache.md: file does not exist\n"
    );

    let root = pricing_fixture("pricing-missing-selector");
    fs::remove_file(root.join("skills/agent-advisor/scripts/model_recommendation.py"))
        .expect("remove selector");
    let (status, _, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        "error: cannot read skills/agent-advisor/scripts/model_recommendation.py: file does not exist\n"
    );

    let root = pricing_fixture("pricing-missing-catalog");
    fs::remove_file(root.join(format!(
        "skills/agent-advisor/references/models/{CATALOG_NAME}"
    )))
    .expect("remove catalog");
    let (status, _, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("models/cat.json: file does not exist"),
        "{stderr}"
    );
}

#[test]
fn pricing_malformed_catalogs_are_reported_not_ignored() {
    let cases: [(&str, &str); 7] = [
        ("{", "cat.json"),
        (r#"{"a":1,"a":2}"#, "cat.json"),
        (r#"{"other":{}}"#, "error: 'models'\n"),
        (r#"{"models":[]}"#, "'models' is array, not an object"),
        (r#"{"models":{"m":{}}}"#, "error: 'paths'\n"),
        (
            r#"{"models":{"m":{"paths":{}}}}"#,
            "error: 'runtime_converse'\n",
        ),
        (
            r#"{"models":{"m":{"paths":{"runtime_converse":{}}}}}"#,
            "error: 'model_id'\n",
        ),
    ];
    for (catalog, expected) in cases {
        let root = pricing_fixture("pricing-malformed");
        write_catalog(&root, catalog);
        let (status, stdout, stderr) = pricing(&root);
        assert_eq!(status, 1, "{catalog}");
        assert_eq!(stdout, "", "{catalog}");
        assert!(stderr.starts_with("error: "), "{catalog}: {stderr}");
        assert!(stderr.contains(expected), "{catalog}: {stderr}");
    }
}

#[test]
fn pricing_non_string_model_ids_are_findings_or_errors() {
    let root = pricing_fixture("pricing-ids");
    for (id, shown) in [
        ("5", "5"),
        ("null", "None"),
        ("true", "True"),
        ("1.5", "1.5"),
    ] {
        write_catalog(
            &root,
            &format!(
                r#"{{"models":{{"m":{{"paths":{{"runtime_converse":{{"model_id":{id}}}}}}}}}}}"#
            ),
        );
        let (status, _, stderr) = pricing(&root);
        assert_eq!(status, 1, "{id}");
        assert!(
            stderr.contains(&format!(
                "gcp-to-aws: candidate {shown} has no numeric Active price row."
            )),
            "{id}: {stderr}"
        );
    }
    write_catalog(
        &root,
        r#"{"models":{"m":{"paths":{"runtime_converse":{"model_id":["x"]}}}}}"#,
    );
    let (status, _, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert_eq!(stderr, "error: unhashable model_id in the catalog\n");
}

#[cfg(unix)]
#[test]
fn pricing_refuses_symbolic_links() {
    let root = pricing_fixture("pricing-symlink");
    let cache = root.join("skills/gcp-to-aws/references/vendored/ai/bedrock-pricing-cache.md");
    let outside = root.join("outside.md");
    fs::rename(&cache, &outside).expect("move cache");
    std::os::unix::fs::symlink(&outside, &cache).expect("create link");
    let (status, stdout, stderr) = pricing(&root);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
}

#[test]
fn real_plugin_pricing_coverage_passes() {
    let (status, stdout, stderr) = pricing(&real_plugin());
    assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
    assert_eq!(stdout, PRICING_OK);
}

#[test]
fn real_plugin_active_check_matches_the_python_word_boundary_rule() {
    assert!(starts_with_active("Active"));
    assert!(starts_with_active("active (Global CRIS)"));
    assert!(!starts_with_active("activeX"));
    assert!(!starts_with_active("Inactive"));
}

// ---- program-artifacts ---------------------------------------------------

const ACTIVATE_LINK_MD: &str = "[Apply](https://aws.amazon.com/startups/credits/)";

fn prefs(block: &str, value_json: &str) -> String {
    format!(r#"{{"{block}":{{"startup_program_status":{{"value":{value_json}}}}}}}"#)
}

fn unknown_prefs() -> String {
    prefs("startup_constraints", r#""unknown""#)
}

fn artifacts(dir: &Path) -> (u8, String, String) {
    exec(&[
        "startup",
        "program-artifacts",
        "--migration-dir",
        path_text(dir),
    ])
}

const STARTUP_OK: &str = "STARTUP_OK | startup_program_status aligned with artifacts\n";

fn startup_fail<S: AsRef<str>>(lines: &[S]) -> String {
    let mut text = String::from("STARTUP_FAIL | startup-program-artifacts\n");
    for line in lines {
        text.push_str("  - ");
        text.push_str(line.as_ref());
        text.push('\n');
    }
    let _ = writeln!(
        text,
        "error: {} startup program artifact problem(s)",
        lines.len()
    );
    text
}

#[test]
fn program_requires_the_migration_dir_option() {
    let (status, stdout, stderr) = exec(&["startup", "program-artifacts"]);
    assert_eq!(status, 2);
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("error: the following arguments are required: --migration-dir\n"),
        "{stderr}"
    );
    for args in [
        vec!["startup", "program-artifacts", "--migration-dir"],
        vec!["startup", "program-artifacts", "extra"],
        vec!["startup", "program-artifacts", "--wat"],
        vec!["startup", "unknown-command"],
        vec!["startup"],
    ] {
        assert_eq!(exec(&args).0, 2, "{args:?}");
    }
}

#[test]
fn program_missing_directory_is_a_runtime_failure_with_the_python_line() {
    let root = TempDir::new("program-missing");
    let missing = root.join("absent");
    let (status, stdout, stderr) = artifacts(&missing);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with(&format!(
            "STARTUP_FAIL | reason=not_a_directory | path={}\n",
            missing.display()
        )),
        "{stderr}"
    );
    // A file is not a directory either.
    write(&root, "file.txt", "x");
    let (status, _, stderr) = artifacts(&root.join("file.txt"));
    assert_eq!(status, 1);
    assert!(
        stderr.starts_with("STARTUP_FAIL | reason=not_a_directory"),
        "{stderr}"
    );
}

#[test]
fn program_without_preferences_enforces_nothing() {
    let dir = TempDir::new("program-no-prefs");
    write(
        &dir,
        "STARTUP_PROGRAMS.md",
        "Your status: eligible founders tier",
    );
    let (status, stdout, stderr) = artifacts(&dir);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (0, STARTUP_OK, "")
    );
}

#[test]
fn program_only_the_string_unknown_enforces() {
    let bad = "Your status: eligible. AWS Activate.";
    for (value, enforced) in [
        (r#""unknown""#, true),
        (r#"["unknown"]"#, true),
        (r#"["unknown","founders"]"#, true),
        (r#""founders""#, false),
        (r#""Unknown""#, false),
        (r#""unknown ""#, false),
        (r#"["founders","unknown"]"#, false),
        (r#"[["unknown"]]"#, false),
        ("[]", false),
        ("null", false),
        ("5", false),
        (r#"{"x":"unknown"}"#, false),
    ] {
        let dir = TempDir::new("program-values");
        write(
            &dir,
            "preferences.json",
            &prefs("startup_constraints", value),
        );
        write(&dir, "STARTUP_PROGRAMS.md", bad);
        let (status, _, _) = artifacts(&dir);
        assert_eq!(status == 1, enforced, "{value}");
    }
}

#[test]
fn program_status_block_without_a_value_or_with_other_shapes_is_not_enforced() {
    let bad = "Your status: eligible. AWS Activate.";
    for prefs_json in [
        r#"{"startup_constraints":{"startup_program_status":{}}}"#,
        r#"{"startup_constraints":{"startup_program_status":"unknown"}}"#,
        r#"{"startup_constraints":{"startup_program_status":["unknown"]}}"#,
        r#"{"startup_constraints":{"other":1}}"#,
        r#"{"startup_constraints":null}"#,
        r#"{"startup_constraints":{}}"#,
        r"{}",
    ] {
        let dir = TempDir::new("program-shapes");
        write(&dir, "preferences.json", prefs_json);
        write(&dir, "STARTUP_PROGRAMS.md", bad);
        let (status, stdout, _) = artifacts(&dir);
        assert_eq!((status, stdout.as_str()), (0, STARTUP_OK), "{prefs_json}");
    }
}

#[test]
fn program_searches_every_status_block_and_takes_the_first_object() {
    let bad = "Your status: eligible.";
    // A truthy block without the key must not stop the search.
    let dir = TempDir::new("program-blocks");
    write(
        &dir,
        "preferences.json",
        r#"{"startup_constraints":{"q":1},"ai_constraints":{"x":true},"design_constraints":{"startup_program_status":{"value":"unknown"}}}"#,
    );
    write(&dir, "STARTUP_PROGRAMS.md", bad);
    assert_eq!(artifacts(&dir).0, 1);

    // The first block that carries an object wins, even when later ones differ.
    let dir = TempDir::new("program-blocks-first");
    write(
        &dir,
        "preferences.json",
        r#"{"design_constraints":{"startup_program_status":{"value":"unknown"}},"ai_constraints":{"startup_program_status":{"value":"founders"}}}"#,
    );
    write(&dir, "STARTUP_PROGRAMS.md", bad);
    assert_eq!(
        artifacts(&dir).0,
        0,
        "ai_constraints precedes design_constraints"
    );

    // A non-object status in an earlier block is skipped.
    let dir = TempDir::new("program-blocks-skip");
    write(
        &dir,
        "preferences.json",
        r#"{"startup_constraints":{"startup_program_status":"founders"},"ai_constraints":{"startup_program_status":{"value":["unknown"]}}}"#,
    );
    write(&dir, "STARTUP_PROGRAMS.md", bad);
    assert_eq!(artifacts(&dir).0, 1);
}

#[test]
fn program_unreadable_preferences_are_runtime_errors() {
    for (content, expected) in [
        ("{", "preferences.json:"),
        ("[]", "expected a JSON object, found array"),
        (
            r#"{"startup_constraints":"text"}"#,
            "startup_constraints must be an object, found string",
        ),
        (
            r#"{"ai_constraints":[1]}"#,
            "ai_constraints must be an object, found array",
        ),
    ] {
        let dir = TempDir::new("program-bad-prefs");
        write(&dir, "preferences.json", content);
        let (status, stdout, stderr) = artifacts(&dir);
        assert_eq!(status, 1, "{content}");
        assert_eq!(stdout, "");
        assert!(stderr.starts_with("error: "), "{content}: {stderr}");
        assert!(stderr.contains(expected), "{content}: {stderr}");
    }
    // A directory named preferences.json is "not a file", like Path.is_file().
    let dir = TempDir::new("program-prefs-dir");
    fs::create_dir(dir.join("preferences.json")).expect("create dir");
    assert_eq!(artifacts(&dir).1, STARTUP_OK);
}

#[test]
fn program_every_forbidden_phrase_is_reported_with_its_pattern() {
    let sources = [
        r"your\s+status:\s*eligible",
        r"\beligible\s+founders\s+tier\b",
        r"\beligible\s+for\s+up\s+to\s+\$5k\s+aws\s+activate\b",
        r"\bactivate\s+portfolio\s+credits\s+eligibility\b",
        r"\*\*your\s+status:\s*eligible_founders\*\*",
        r"\*\*your\s+status:\s*eligible_portfolio\*\*",
    ];
    // Phrases chosen so that exactly the pattern at the same index is the
    // first one that matches.
    let phrases = [
        "Your   STATUS:eligible",
        "You are in the Eligible Founders Tier.",
        "ELIGIBLE FOR UP TO $5K AWS ACTIVATE credits",
        "see Activate Portfolio Credits Eligibility here",
        "**Your status: eligible_founders**",
        "**Your status: eligible_portfolio**",
    ];
    for (index, phrase) in phrases.iter().enumerate() {
        let dir = TempDir::new("program-forbidden");
        write(&dir, "preferences.json", &unknown_prefs());
        // Include the apply link so only the phrase check can fail.
        write(
            &dir,
            "STARTUP_PROGRAMS.md",
            &format!("{phrase}\n{ACTIVATE_LINK_MD}\n"),
        );
        let (status, stdout, stderr) = artifacts(&dir);
        assert_eq!(status, 1, "{phrase}");
        assert_eq!(stdout, "");
        let expected_source = if index >= 4 {
            sources[0]
        } else {
            sources[index]
        };
        assert_eq!(
            stderr,
            startup_fail(&[&format!(
                "STARTUP_PROGRAMS.md: startup_program_status is \"unknown\" but content implies a confirmed Activate tier ({expected_source})"
            )]),
            "{phrase}"
        );
    }
}

#[test]
fn program_forbidden_phrases_respect_word_boundaries() {
    for clean in [
        "ineligible founders tier",
        "eligible founders tiers",
        "eligible founders",
        "not eligible for up to $5k aws activated",
        "your status eligible",
        "preactivate portfolio credits eligibility",
        "activate portfolio credits eligibility2",
    ] {
        let dir = TempDir::new("program-boundaries");
        write(&dir, "preferences.json", &unknown_prefs());
        write(&dir, "STARTUP_PROGRAMS.md", clean);
        let (status, _, stderr) = artifacts(&dir);
        assert_eq!(status, 0, "{clean}: {stderr}");
    }
}

#[test]
fn program_reports_one_phrase_finding_per_file_in_pattern_order() {
    let dir = TempDir::new("program-one");
    write(&dir, "preferences.json", &unknown_prefs());
    write(
        &dir,
        "STARTUP_PROGRAMS.md",
        "Activate Portfolio Credits Eligibility\nYour status: eligible\n",
    );
    let (status, _, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        startup_fail(&["STARTUP_PROGRAMS.md: startup_program_status is \"unknown\" but content implies a confirmed Activate tier (your\\s+status:\\s*eligible)"])
    );
}

#[test]
fn program_activate_mentions_need_a_clickable_link() {
    let message = "mentions AWS Activate but has no clickable link to https://aws.amazon.com/startups/credits/ — add <a href=\"...\"> in HTML or [text](url) in Markdown";
    for (text, linked) in [
        ("AWS Activate is a program.", false),
        ("aws   activate", false),
        (
            "AWS Activate https://aws.amazon.com/startups/credits",
            false,
        ),
        (
            "AWS Activate https://aws.amazon.com/startups/credits/",
            true,
        ),
        (
            "AWS Activate [x](https://aws.amazon.com/startups/credits/)",
            true,
        ),
        (
            "AWS Activate <a href=\"https://aws.amazon.com/startups/credits\">x</a>",
            true,
        ),
        (
            "AWS Activate <a href='http://aws.amazon.com/startups/credits/'>x</a>",
            true,
        ),
        (
            "AWS Activate <A HREF=\"HTTPS://AWS.AMAZON.COM/STARTUPS/CREDITS\">x</A>",
            true,
        ),
        // The plain-URL fallback (with trailing slash) also satisfies the rule.
        (
            "AWS Activate <a href=\"https://aws.amazon.com/startups/credits/x\">x</a>",
            true,
        ),
        (
            "AWS Activate <a href=\"https://aws.amazon.com/startups/credits>x</a>",
            false,
        ),
        (
            "AWS Activate <a href=\"https://aws.amazon.com/startups/credit\">x</a>",
            false,
        ),
        (
            "AWS Activate <a href=https://aws.amazon.com/startups/credits>x</a>",
            false,
        ),
        // Mixed quote characters are accepted, like the Python `["']` classes.
        (
            "AWS Activate <a href=\"https://aws.amazon.com/startups/credits'>x</a>",
            true,
        ),
        ("no mention of the program at all", true),
        ("AWS Activated credits", true),
        ("preAWS Activate", true),
    ] {
        let dir = TempDir::new("program-link");
        write(&dir, "preferences.json", &unknown_prefs());
        write(&dir, "migration-report.html", text);
        let (status, _, stderr) = artifacts(&dir);
        if linked {
            assert_eq!(status, 0, "{text}: {stderr}");
        } else {
            assert_eq!(status, 1, "{text}");
            assert_eq!(
                stderr,
                startup_fail(&[&format!("migration-report.html: {message}")]),
                "{text}"
            );
        }
    }
}

#[test]
fn program_checks_all_three_artifacts_with_their_labels() {
    let dir = TempDir::new("program-labels");
    write(&dir, "preferences.json", &unknown_prefs());
    write(&dir, "STARTUP_PROGRAMS.md", "Your status: eligible\n");
    write(
        &dir,
        "migration-report.html",
        "<p>Eligible Founders Tier</p>",
    );
    write(
        &dir,
        "ai-migration/STARTUP_PROGRAMS.md",
        "AWS Activate only\n",
    );
    let (status, _, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "STARTUP_FAIL | startup-program-artifacts");
    assert!(lines[1].starts_with("  - STARTUP_PROGRAMS.md: startup_program_status is"));
    assert!(lines[2].starts_with("  - migration-report.html: startup_program_status is"));
    assert!(lines[3].starts_with("  - ai-migration/STARTUP_PROGRAMS.md: mentions AWS Activate"));
    assert_eq!(lines[4], "error: 3 startup program artifact problem(s)");
    assert_eq!(lines.len(), 5);
}

#[test]
fn program_one_file_can_have_a_phrase_and_a_link_finding() {
    let dir = TempDir::new("program-two");
    write(&dir, "preferences.json", &unknown_prefs());
    write(
        &dir,
        "STARTUP_PROGRAMS.md",
        "Your status: eligible for AWS Activate\n",
    );
    let (status, _, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert_eq!(stderr.lines().count(), 4, "{stderr}");
}

#[test]
fn program_honest_artifacts_pass() {
    let dir = TempDir::new("program-honest");
    write(&dir, "preferences.json", &unknown_prefs());
    write(
        &dir,
        "STARTUP_PROGRAMS.md",
        &format!("Check AWS Activate eligibility yourself: {ACTIVATE_LINK_MD}\n"),
    );
    write(
        &dir,
        "migration-report.html",
        "<a href=\"https://aws.amazon.com/startups/credits/\">AWS Activate</a>",
    );
    let (status, stdout, stderr) = artifacts(&dir);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (0, STARTUP_OK, "")
    );
}

#[test]
fn program_non_utf8_artifacts_are_runtime_errors() {
    let dir = TempDir::new("program-utf8");
    write(&dir, "preferences.json", &unknown_prefs());
    fs::write(dir.join("STARTUP_PROGRAMS.md"), b"\xff\xfe broken").expect("write");
    let (status, stdout, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "error: cannot read STARTUP_PROGRAMS.md: not valid UTF-8\n"
    );
}

#[cfg(unix)]
#[test]
fn program_refuses_symbolic_links_instead_of_passing_silently() {
    let dir = TempDir::new("program-symlink");
    let outside = TempDir::new("program-symlink-target");
    write(&outside, "prefs.json", &unknown_prefs());
    std::os::unix::fs::symlink(outside.join("prefs.json"), dir.join("preferences.json"))
        .expect("link");
    let (status, stdout, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );

    let dir = TempDir::new("program-symlink-artifact");
    write(&dir, "preferences.json", &unknown_prefs());
    write(&outside, "report.html", "Your status: eligible");
    std::os::unix::fs::symlink(
        outside.join("report.html"),
        dir.join("migration-report.html"),
    )
    .expect("link");
    let (status, _, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
}

// ---- model-id-lint -------------------------------------------------------

const REGISTRY: &str = "\
# Registry

| Model | Model ID | Status | Active Replacement |
| --- | --- | --- | --- |
| Alpha | `acme.alpha-v1:0` | **excluded** (EOL soon) | `acme.beta-v1:0` |
| Reel | `acme.reel-v1:0` / `v1:1` | **excluded** | `acme.next-v1:0` |
| Glob | `acme.llama3-2-*-instruct-v1:0` | **excluded** | `acme.llama3-3-70b-instruct-v1:0` |
| Old | `acme.legacy-v1:0` | **legacy** | none |

**Removed (past EOL):**

- Gone (`acme.gone-v1:0`), replacement: `acme.fresh-v1:0`
- Pair (`acme.pair-a-v1:0`, `acme.pair-b-v1:0`)

> Notes: `acme.note-v1:0` is not an ID list.
- not in the Removed list any more: `acme.later-v1:0`
";

const BANNED: [&str; 7] = [
    "acme.alpha-v1:0",
    "acme.reel-v1:0",
    "acme.reel-v1:1",
    "acme.llama3-2-*-instruct-v1:0",
    "acme.gone-v1:0",
    "acme.pair-a-v1:0",
    "acme.pair-b-v1:0",
];

const REGISTRY_REL: &str = "skills/shared/ai/ai-model-lifecycle.md";

fn lint_fixture(label: &str) -> TempDir {
    let root = TempDir::new(label);
    write(&root, REGISTRY_REL, REGISTRY);
    root
}

fn lint(root: &Path) -> (u8, String, String) {
    exec(&["startup", "model-id-lint", "--plugin-root", path_text(root)])
}

fn lint_message(rel_and_line: &str, id: &str) -> String {
    format!(
        "{rel_and_line}: retired Bedrock model `{id}` (excluded/removed per ai-model-lifecycle.md) used outside the model catalog"
    )
}

#[test]
fn lint_clean_registry_copy_passes() {
    let root = lint_fixture("lint-clean");
    let (status, stdout, stderr) = lint(&root);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (
            0,
            "model-id lint: OK (7 retired IDs enforced from the registry)\n",
            ""
        )
    );
}

#[test]
fn lint_every_retired_id_fails_outside_the_catalog() {
    let root = lint_fixture("lint-every");
    for (index, id) in BANNED.iter().enumerate() {
        // A wildcard ID is written with a concrete expansion.
        let concrete = id.replace('*', "90b");
        write(
            &root,
            &format!("skills/example/bad-{index}.md"),
            &format!("recommend `{concrete}`\n"),
        );
    }
    let (status, stdout, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    let mut expected = String::from("model-id lint: 7 problem(s)\n");
    for (index, id) in BANNED.iter().enumerate() {
        let _ = writeln!(
            expected,
            "  - {}",
            lint_message(&format!("skills/example/bad-{index}.md:1"), id)
        );
    }
    expected.push_str("error: model-id lint found 7 problem(s)\n");
    assert_eq!(stderr, expected);
}

#[test]
fn lint_glob_ids_stay_inside_one_token() {
    let matcher = IdMatcher::new("acme.llama3-2-*-instruct-v1:0");
    for (line, hit) in [
        ("use `acme.llama3-2-90b-instruct-v1:0` now", true),
        ("acme.llama3-2--instruct-v1:0", true),
        ("x acme.llama3-2-a-b-c-instruct-v1:0 y", true),
        // The wildcard does not run over whitespace or a backtick into the
        // replacement ID named right after it.
        ("acme.llama3-2-x and acme.llama3-3-70b-instruct-v1:0", false),
        ("`acme.llama3-2-` then `-instruct-v1:0`", false),
        ("acme.llama3-2-\u{a0}x-instruct-v1:0", false),
        ("acme.llama3-3-70b-instruct-v1:0", false),
        ("ACME.LLAMA3-2-90B-INSTRUCT-V1:0", false),
    ] {
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(matcher.is_match(line, &chars), hit, "{line:?}");
    }
    // Several wildcards, a leading and a trailing one.
    let multi = IdMatcher::new("*a*b*");
    for (line, hit) in [("xaxbx", true), ("ab", true), ("a b", false), ("ba", false)] {
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(multi.is_match(line, &chars), hit, "{line:?}");
    }
}

#[test]
fn lint_ids_are_substring_matches_like_the_python_search() {
    let root = lint_fixture("lint-substring");
    write(
        &root,
        "skills/example/a.md",
        "prefix-acme.alpha-v1:0-suffix\n",
    );
    write(
        &root,
        "skills/example/b.md",
        "acme.alpha-v1:1 is another revision\n",
    );
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert!(stderr.contains("skills/example/a.md:1"), "{stderr}");
    assert!(!stderr.contains("skills/example/b.md"), "{stderr}");
}

#[test]
fn lint_dual_id_continuations_are_rebuilt_not_banned_as_bare_text() {
    let root = lint_fixture("lint-dual");
    write(
        &root,
        "skills/example/ok.md",
        "versions v1:1 and v1:0 are fine\n",
    );
    assert_eq!(lint(&root).0, 0);
    write(&root, "skills/example/bad.md", "`acme.reel-v1:1`\n");
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert!(stderr.contains(&lint_message("skills/example/bad.md:1", "acme.reel-v1:1")));
}

#[test]
fn lint_replacements_and_notes_are_not_banned() {
    let root = lint_fixture("lint-replacements");
    write(
        &root,
        "skills/example/ok.md",
        "acme.beta-v1:0 acme.next-v1:0 acme.fresh-v1:0 acme.legacy-v1:0 acme.note-v1:0 acme.later-v1:0 acme.llama3-3-70b-instruct-v1:0\n",
    );
    assert_eq!(lint(&root).0, 0);
}

#[test]
fn lint_legacy_model_outside_the_exclusion_zone_is_allowed() {
    let root = lint_fixture("lint-legacy");
    write(
        &root,
        "skills/example/legacy.md",
        "recommend `acme.legacy-v1:0`\n",
    );
    assert_eq!(lint(&root).0, 0);
}

#[test]
fn lint_catalog_files_may_name_retired_ids_and_vendored_copies_inherit() {
    let root = lint_fixture("lint-catalog");
    let text = "existing users may still see `acme.alpha-v1:0`\n";
    for rel in [
        "skills/shared/ai/bedrock-pricing-cache.md",
        "skills/azure-to-aws/references/shared/pricing-cache.md",
        "skills/gcp-to-aws/references/vendored/ai/bedrock-pricing-cache.md",
        "skills/agent-advisor/references/vendored/ai/ai-model-lifecycle.md",
    ] {
        write(&root, rel, text);
    }
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 0, "{stderr}");

    // The lookalike paths below are not catalog files.
    write(
        &root,
        "skills/gcp-to-aws/references/shared/pricing-cache.md",
        text,
    );
    write(
        &root,
        "skills/gcp-to-aws/references/other/ai/bedrock-pricing-cache.md",
        text,
    );
    write(
        &root,
        "references/vendored/ai/bedrock-pricing-cache.md",
        text,
    );
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    for rel in [
        "skills/gcp-to-aws/references/shared/pricing-cache.md",
        "skills/gcp-to-aws/references/other/ai/bedrock-pricing-cache.md",
        "references/vendored/ai/bedrock-pricing-cache.md",
    ] {
        assert!(stderr.contains(&format!("{rel}:1:")), "{rel}: {stderr}");
    }
}

#[test]
fn lint_checks_every_scanned_extension_and_nothing_else() {
    let root = lint_fixture("lint-exts");
    for ext in ["md", "py", "json", "ts", "tf", "sh", "template", "html"] {
        write(&root, &format!("fixtures/f.{ext}"), "`acme.alpha-v1:0`\n");
    }
    for skipped in [
        "fixtures/f.txt",
        "fixtures/f",
        "fixtures/.md",
        "fixtures/f.md.bak",
        "fixtures/f.",
    ] {
        write(&root, skipped, "`acme.alpha-v1:0`\n");
    }
    write(&root, "node_modules/pkg/index.md", "`acme.alpha-v1:0`\n");
    write(&root, "skills/x/node_modules/y.md", "`acme.alpha-v1:0`\n");
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert_eq!(
        stderr.matches("retired Bedrock model").count(),
        8,
        "{stderr}"
    );
    assert!(stderr.contains("fixtures/f.html:1:"), "{stderr}");
    assert!(!stderr.contains("node_modules"), "{stderr}");
    assert!(
        !stderr.contains("f.txt") && !stderr.contains("f.bak"),
        "{stderr}"
    );
    assert_eq!(suffix("f.tar.md"), ".md");
    assert_eq!(suffix(".md"), "");
    assert_eq!(suffix("f."), "");
    assert_eq!(suffix("f"), "");
    assert_eq!(suffix(".a.md"), ".md");
}

#[test]
fn lint_reports_line_numbers_with_universal_newlines() {
    let root = lint_fixture("lint-lines");
    write(
        &root,
        "skills/example/crlf.md",
        "one\r\ntwo\rthree `acme.alpha-v1:0`\n\nfive\u{2028}six `acme.gone-v1:0`\n",
    );
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert!(
        stderr.contains(&lint_message("skills/example/crlf.md:3", "acme.alpha-v1:0")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&lint_message("skills/example/crlf.md:6", "acme.gone-v1:0")),
        "{stderr}"
    );
}

#[test]
fn lint_orders_files_like_python_paths_not_like_strings() {
    let root = lint_fixture("lint-order");
    write(&root, "a-b.md", "`acme.alpha-v1:0`\n");
    write(&root, "a/z.md", "`acme.alpha-v1:0`\n");
    write(&root, "B.md", "`acme.alpha-v1:0`\n");
    let (_, _, stderr) = lint(&root);
    let order: Vec<&str> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("  - "))
        .map(|line| line.split(':').next().expect("path"))
        .collect();
    assert_eq!(order, ["B.md", "a/z.md", "a-b.md"]);
}

#[test]
fn lint_one_line_can_report_several_ids_in_registry_order() {
    let root = lint_fixture("lint-multi");
    write(&root, "x.md", "`acme.pair-b-v1:0` and `acme.alpha-v1:0`\n");
    let (_, _, stderr) = lint(&root);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[1],
        format!("  - {}", lint_message("x.md:1", "acme.alpha-v1:0"))
    );
    assert_eq!(
        lines[2],
        format!("  - {}", lint_message("x.md:1", "acme.pair-b-v1:0"))
    );
}

#[test]
fn lint_html_fixtures_are_scanned() {
    let root = lint_fixture("lint-html");
    write(&root, "fixtures/report.html", "<p>acme.alpha-v1:0</p>\n");
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert!(stderr.contains("fixtures/report.html:1:"), "{stderr}");
}

#[test]
fn lint_fabricated_dated_ids_fail_except_in_the_repair_helper() {
    let sonnet = format!("claude-{}-{}", "sonnet-4-6", "20250929");
    let opus = format!("claude-{}-{}", "opus-4-8", "20250929");
    let root = lint_fixture("lint-fab");
    write(
        &root,
        "skills/example/fab.md",
        &format!("use {sonnet} or {opus}\nclean\n"),
    );
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        "model-id lint: 1 problem(s)\n  - skills/example/fab.md:1: fabricated dated ID — Sonnet 4.6 / Opus 4.8 Bedrock IDs are undated; this form never existed\nerror: model-id lint found 1 problem(s)\n"
    );

    let allowed = lint_fixture("lint-fab-allowed");
    write(
        &allowed,
        "skills/llm-to-bedrock/references/helpers/resolve-bedrock-model-id/resolve-bedrock-model-id.md",
        &format!("example of a broken id: {sonnet}\n"),
    );
    let (status, _, stderr) = lint(&allowed);
    assert_eq!(status, 0, "{stderr}");

    // The allowlist exempts the fabricated heuristic only, not retired IDs.
    write(
        &allowed,
        "skills/llm-to-bedrock/references/helpers/resolve-bedrock-model-id/resolve-bedrock-model-id.md",
        "`acme.alpha-v1:0`\n",
    );
    assert_eq!(lint(&allowed).0, 1);
}

#[test]
fn lint_fabricated_heuristic_boundaries() {
    for (line, hit) in [
        ("claude-sonnet-4-6-20250929", true),
        ("x.claude-opus-4-8-12345678y", true),
        ("claude-sonnet-4-6-2025092", false),
        ("claude-sonnet-4-6", false),
        ("claude-sonnet-4-6-v1", false),
        ("claude-sonnet-4-7-20250929", false),
        ("claude-haiku-4-5-20251001", false),
        ("Claude-Sonnet-4-6-20250929", false),
        (
            "claude-sonnet-4-6-\u{661}\u{662}\u{663}\u{664}\u{665}\u{666}\u{667}\u{668}",
            false,
        ),
        ("claude-claude-opus-4-8-20250929", true),
        ("claude-sonnet-4-6 claude-opus-4-8-20250929", true),
    ] {
        assert_eq!(has_fabricated_id(line), hit, "{line:?}");
    }
}

#[test]
fn lint_zero_parse_guardrails_refuse_a_no_op_gate() {
    let stub = "# registry\n| Model | Model ID | Status |\n| --- | --- | --- |\n**Removed:**\n- nothing listed\n";
    let root = TempDir::new("lint-zero");
    write(&root, REGISTRY_REL, stub);
    write(&root, "skills/example/ok.md", "no model here\n");
    let (status, stdout, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "model-id lint: 2 setup problem(s)\n  - skills/shared/ai/ai-model-lifecycle.md: parsed ZERO **excluded** table rows — the Legacy/EOL table format may have changed. Refusing to run a no-op gate.\n  - skills/shared/ai/ai-model-lifecycle.md: parsed ZERO IDs from the **Removed** (past-EOL) list — its header/format may have changed. Refusing to run a no-op gate.\nerror: model-id lint stopped on 2 setup problem(s)\n"
    );

    // Each section is checked on its own: renaming only the Removed header
    // must not hide behind the still-parsing table rows.
    let renamed = REGISTRY.replace("**Removed (past EOL):**", "**Archive:**");
    write(&root, REGISTRY_REL, &renamed);
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert!(
        stderr.starts_with("model-id lint: 1 setup problem(s)\n"),
        "{stderr}"
    );
    assert!(stderr.contains("ZERO IDs from the **Removed**"), "{stderr}");
}

#[test]
fn lint_missing_registry_and_plugin_root_are_distinct_failures() {
    let root = TempDir::new("lint-no-registry");
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert_eq!(
        stderr,
        "model-id lint: 1 setup problem(s)\n  - registry not found: skills/shared/ai/ai-model-lifecycle.md\nerror: model-id lint stopped on 1 setup problem(s)\n"
    );

    let missing = root.join("does-not-exist");
    let (status, stdout, stderr) = lint(&missing);
    assert_eq!(status, 2);
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("error: Plugin directory not found: "),
        "{stderr}"
    );
    assert!(stderr.contains("does-not-exist"), "{stderr}");
}

#[test]
fn lint_registry_rows_read_only_the_model_id_column() {
    let registry = "\
| Model | Model ID | Status | Replacement |
|---|---|---|---|
|  | `acme.cell-v1:0` | excluded | `acme.replacement-v1:0` |
| only one cell excluded |
||| excluded |
  | Indented | `acme.indented-v1:0` | Excluded | none |
| Missing status | `acme.fine-v1:0` | legacy | none |

**Removed:**
  - indented (`acme.removed-v1:0`) replacement: `acme.after-v1:0`
-no space (`acme.tight-v1:0`)
**Next section**
- `acme.after-section-v1:0`
";
    let root = TempDir::new("lint-columns");
    write(&root, REGISTRY_REL, registry);
    write(
        &root,
        "x.md",
        "acme.replacement-v1:0 acme.fine-v1:0 acme.after-v1:0 acme.after-section-v1:0\n",
    );
    let (status, stdout, stderr) = lint(&root);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
    assert_eq!(
        stdout,
        "model-id lint: OK (4 retired IDs enforced from the registry)\n"
    );
    for banned in [
        "acme.cell-v1:0",
        "acme.indented-v1:0",
        "acme.removed-v1:0",
        "acme.tight-v1:0",
    ] {
        write(&root, "y.md", &format!("`{banned}`\n"));
        assert_eq!(lint(&root).0, 1, "{banned}");
    }
}

#[test]
fn lint_duplicate_registry_ids_are_counted_once() {
    let registry = format!("{REGISTRY}\n**Removed again:**\n- again (`acme.gone-v1:0`)\n");
    let root = TempDir::new("lint-dupe");
    write(&root, REGISTRY_REL, &registry);
    let (status, stdout, _) = lint(&root);
    assert_eq!(status, 0);
    assert!(stdout.contains("(7 retired IDs enforced"), "{stdout}");
}

#[test]
fn lint_skips_non_utf8_files_like_the_python_lint() {
    let root = lint_fixture("lint-skip");
    fs::write(root.join("binary.md"), b"\xff\xfe`acme.alpha-v1:0`").expect("write");
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 0, "{stderr}");
}

#[cfg(unix)]
#[test]
fn lint_never_follows_symbolic_links() {
    let root = lint_fixture("lint-symlink");
    let outside = TempDir::new("lint-symlink-outside");
    write(&outside, "bad.md", "`acme.alpha-v1:0`\n");
    write(&outside, "dir/bad.md", "`acme.alpha-v1:0`\n");
    std::os::unix::fs::symlink(outside.join("bad.md"), root.join("linked.md")).expect("file link");
    std::os::unix::fs::symlink(outside.join("dir"), root.join("linked-dir")).expect("dir link");
    let (status, stdout, stderr) = lint(&root);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
    assert!(stdout.starts_with("model-id lint: OK"));

    // A symlinked registry is refused rather than read.
    let registry = root.join(REGISTRY_REL);
    fs::remove_file(&registry).expect("remove registry");
    std::os::unix::fs::symlink(outside.join("bad.md"), &registry).expect("registry link");
    let (status, _, stderr) = lint(&root);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
}

#[test]
fn lint_skips_the_legacy_python_script_only_at_its_own_path() {
    let root = lint_fixture("lint-self");
    write(&root, "scripts/model-id-lint.py", "# `acme.alpha-v1:0`\n");
    assert_eq!(lint(&root).0, 0);
    write(&root, "tools/model-id-lint.py", "# `acme.alpha-v1:0`\n");
    assert_eq!(lint(&root).0, 1);
}

#[test]
fn lint_ids_inside_backticks_follow_the_python_regular_expression() {
    for (body, expected) in [
        ("anthropic.claude-sonnet-4-6", true),
        ("anthropic.claude-sonnet-4-6:0", true),
        ("amazon.nova-reel-v1:0", true),
        ("meta.llama3-2-*-instruct-v1:0", true),
        ("a.b", true),
        ("a-b", true),
        ("a*.b", true),
        ("a.b*c.d", true),
        ("v1:1", true),
        ("v10:20", true),
        ("a.b:", false),
        ("a.b:x", false),
        ("a.b:1:2", false),
        ("ab", false),
        ("a", false),
        ("", false),
        (".a.b", false),
        ("-a.b", false),
        ("a.", false),
        ("a.b-", false),
        ("a*b", false),
        ("A.b", false),
        ("a.B", false),
        ("a b.c", false),
        ("a.b_c", false),
        ("v1", false),
        ("v:1", false),
        ("v1:", false),
        ("V1:1", false),
        ("v1:1:1", false),
        ("*a.b", false),
    ] {
        assert_eq!(is_model_id(body), expected, "{body:?}");
    }
}

#[test]
fn lint_findall_skips_non_ids_and_reuses_the_closing_backtick() {
    assert_eq!(
        ids_in_backticks("x `foo` y `a.b` z `v1:1` `c-d:2`"),
        ["a.b", "v1:1", "c-d:2"]
    );
    // `foo` is not an ID, so its closing backtick opens the span " bar ",
    // which fails too; the next backtick then opens `a.b`.
    assert_eq!(ids_in_backticks("`foo` bar `a.b`"), ["a.b"]);
    assert_eq!(ids_in_backticks("`a.b``c.d`"), ["a.b", "c.d"]);
    assert_eq!(ids_in_backticks("``a.b``"), ["a.b"]);
    assert_eq!(ids_in_backticks("`a.b"), Vec::<String>::new());
    assert_eq!(ids_in_backticks("no ticks a.b"), Vec::<String>::new());
    assert_eq!(ids_in_backticks(""), Vec::<String>::new());
}

#[test]
fn lint_dual_ids_rebuild_against_the_preceding_full_id() {
    let strings =
        |values: &[&str]| -> Vec<String> { values.iter().map(|v| (*v).to_string()).collect() };
    assert_eq!(
        rebuild_dual_ids(&strings(&["amazon.nova-reel-v1:0", "v1:1"])),
        strings(&["amazon.nova-reel-v1:0", "amazon.nova-reel-v1:1"])
    );
    // The continuation does not become the new "full" ID.
    assert_eq!(
        rebuild_dual_ids(&strings(&["a.b-v1:0", "v1:1", "v1:2"])),
        strings(&["a.b-v1:0", "a.b-v1:1", "a.b-v1:2"])
    );
    // Without a revision on the full ID, or without any full ID, nothing is rebuilt.
    assert_eq!(
        rebuild_dual_ids(&strings(&["a.b-v1", "v1:1"])),
        strings(&["a.b-v1", "v1:1"])
    );
    assert_eq!(rebuild_dual_ids(&strings(&["v1:1"])), strings(&["v1:1"]));
    // Quirk kept from the Python lint: any token starting with `v` after a full
    // ID that has a revision is read as a continuation, even a full ID.
    assert_eq!(
        rebuild_dual_ids(&strings(&["vx.a-v1:0", "vx.b-v1:0"])),
        strings(&["vx.a-v1:0", "vx.a-vx.b-v1:0"])
    );
    // No "-v" in the stem: the whole ID is the stem.
    assert_eq!(
        rebuild_dual_ids(&strings(&["a.b:0", "v2:3"])),
        strings(&["a.b:0", "a.b:0-v2:3"])
    );
}

#[test]
fn lint_vendored_paths_collapse_onto_the_shared_tree() {
    for (rel, expected) in [
        (
            "skills/gcp-to-aws/references/vendored/ai/x.md",
            "skills/shared/ai/x.md",
        ),
        ("skills/a/references/vendored/x.md", "skills/shared/x.md"),
        ("skills/shared/ai/x.md", "skills/shared/ai/x.md"),
        (
            "skills//references/vendored/x.md",
            "skills//references/vendored/x.md",
        ),
        (
            "skills/a/b/references/vendored/x.md",
            "skills/a/b/references/vendored/x.md",
        ),
        (
            "plugins/skills/a/references/vendored/x.md",
            "plugins/skills/a/references/vendored/x.md",
        ),
        (
            "skills/a/references/vendored",
            "skills/a/references/vendored",
        ),
    ] {
        assert_eq!(canonicalize(rel), expected, "{rel}");
    }
}

#[test]
fn real_plugin_model_ids_pass_the_lint() {
    let (status, stdout, stderr) = lint(&real_plugin());
    assert_eq!((status, stderr.as_str()), (0, ""), "{stderr}");
    assert!(stdout.starts_with("model-id lint: OK ("), "{stdout}");
    assert!(
        stdout.contains("retired IDs enforced from the registry"),
        "{stdout}"
    );
    assert!(!stdout.starts_with("model-id lint: OK (0 "), "{stdout}");
}

#[test]
fn real_plugin_registry_yields_both_kinds_of_retired_id() {
    let (banned, errors) =
        super::model_ids::load_banned_ids(&real_plugin()).expect("registry reads");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!banned.is_empty());
}

#[test]
fn startup_commands_are_registered_and_documented() {
    for path in ["model-pricing", "program-artifacts", "model-id-lint"] {
        assert!(
            crate::COMMANDS
                .iter()
                .any(|spec| spec.path == ["startup", path]),
            "{path}"
        );
        assert!(crate::USAGE.contains(&format!("startup {path}")), "{path}");
    }
    let (status, stdout, _) = exec(&["startup", "model-pricing", "--help"]);
    assert_eq!(status, 0);
    assert!(stdout.contains("startup model-id-lint"));
}

#[test]
fn closed_pipe_after_a_clean_run_is_success() {
    struct BrokenPipe;
    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }
    let root = pricing_fixture("pricing-pipe");
    let args: Vec<OsString> = [
        "startup",
        "model-pricing",
        "--plugin-root",
        path_text(&root),
    ]
    .iter()
    .map(OsString::from)
    .collect();
    let mut err = Vec::new();
    assert_eq!(run(&args, &mut BrokenPipe, &mut err), 0);
    assert!(err.is_empty());
}

// ---- descendant symbolic links (#4584) -------------------------------------

#[cfg(unix)]
#[test]
fn refuse_links_below_checks_every_component_under_the_root() {
    use super::refuse_links_below;
    use std::os::unix::fs::symlink;

    let root = TempDir::new("below-root");
    let outside = TempDir::new("below-outside");
    write(&outside, "STARTUP_PROGRAMS.md", "outside");
    write(&root, "plain/inner/file.md", "inside");
    write(&root, "plain/regular.txt", "x");
    symlink(&*outside, root.join("linked")).expect("directory link");
    symlink(outside.join("absent"), root.join("dangling")).expect("dangling link");
    symlink(&*outside, root.join("plain/inner/deep")).expect("deep link");

    // Ordinary paths, missing paths and paths below a regular file pass.
    for rel in ["plain/inner/file.md", "missing/x.md", "plain/regular.txt/x"] {
        assert_eq!(refuse_links_below(&root, &root.join(rel)), Ok(()), "{rel}");
    }
    // A link at the first, a middle or the last component, and a dangling one.
    for rel in [
        "linked/STARTUP_PROGRAMS.md",
        "linked",
        "dangling",
        "dangling/x",
        "plain/inner/deep/STARTUP_PROGRAMS.md",
    ] {
        let error = refuse_links_below(&root, &root.join(rel)).expect_err(rel);
        assert!(error.contains("symbolic links are not followed"), "{error}");
    }
    // The explicit root itself may be a link: it is the caller's choice.
    let via_link = TempDir::new("below-via-link");
    symlink(&*root, via_link.join("root")).expect("root link");
    let linked_root = via_link.join("root");
    assert_eq!(
        refuse_links_below(&linked_root, &linked_root.join("plain/inner/file.md")),
        Ok(())
    );
}

#[cfg(unix)]
#[test]
fn program_refuses_a_symlinked_descendant_directory() {
    let outside = TempDir::new("program-descendant-target");
    // The review's exact input: outside content that would be a finding.
    write(&outside, "STARTUP_PROGRAMS.md", "Your status: eligible");
    let dir = TempDir::new("program-descendant");
    write(&dir, "preferences.json", &unknown_prefs());
    std::os::unix::fs::symlink(&*outside, dir.join("ai-migration")).expect("directory link");
    let (status, stdout, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("eligible"),
        "outside content was read: {stderr}"
    );

    // The same link to innocuous content is refused, not a silent pass.
    write(&outside, "STARTUP_PROGRAMS.md", "Nothing to see here.");
    let (status, stdout, stderr) = artifacts(&dir);
    assert_eq!(status, 1, "{stdout}");
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn program_refuses_a_dangling_descendant_link_and_reads_a_real_directory() {
    let dir = TempDir::new("program-dangling");
    write(&dir, "preferences.json", &unknown_prefs());
    std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("ai-migration")).expect("link");
    let (status, _, stderr) = artifacts(&dir);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );

    let normal = TempDir::new("program-normal-descendant");
    write(&normal, "preferences.json", &unknown_prefs());
    write(&normal, "ai-migration/STARTUP_PROGRAMS.md", "Nothing here.");
    let (status, stdout, stderr) = artifacts(&normal);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (0, STARTUP_OK, "")
    );
    write(
        &normal,
        "ai-migration/STARTUP_PROGRAMS.md",
        "Your status: eligible",
    );
    let (status, _, stderr) = artifacts(&normal);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("ai-migration/STARTUP_PROGRAMS.md"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn model_id_lint_refuses_a_symlinked_registry_directory() {
    let outside = TempDir::new("lint-descendant-target");
    write(&outside, "shared/ai/ai-model-lifecycle.md", REGISTRY);
    let plugin = TempDir::new("lint-descendant");
    std::os::unix::fs::symlink(&*outside, plugin.join("skills")).expect("directory link");
    let (status, _, stderr) = lint(&plugin);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
}

// ---- Python word class (#4585) ----------------------------------------------

#[test]
fn word_characters_are_python_letters_numbers_and_underscore_only() {
    // Combining marks (including the alphabetic vowel signs), join controls
    // and connector punctuation other than `_` are not word characters.
    for c in [
        '\u{301}', '\u{93e}', '\u{200d}', '\u{200c}', '\u{203f}', '\u{ad}',
    ] {
        assert!(!is_word(c), "{c:?}");
    }
    // Letters, every number category and `_` are.
    for c in ['é', '日', 'ǅ', 'ʰ', '²', '٣', 'Ⅷ', '½', '_'] {
        assert!(is_word(c), "{c:?}");
    }
}
