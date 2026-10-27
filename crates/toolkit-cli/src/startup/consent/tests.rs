//! Tests for `toolkit startup consent-audit`, migrated from
//! `tests/test_consent_disclosure_audit.py`: scratch plugin trees with one
//! `discover-example-api.md`, the unit rules of the matchers, and runs
//! against the real plugin tree.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    audit, methods_in_capture_section, paragraphs, sentences, target_files, unqualified_claims,
};
use crate::run;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-consent-{label}-{}-{}",
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

const TARGET_REL: &str = "skills/example-skill/references/phases/discover/discover-example-api.md";

fn real_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

/// Runs the audit on a plugin tree; returns (status, stdout, stderr).
fn run_audit(plugin: &Path) -> (u8, String, String) {
    exec(&[
        "startup",
        "consent-audit",
        "--plugin-root",
        plugin.to_str().expect("utf-8 path"),
    ])
}

fn write_fixture(dir: &Path, text: &str) -> PathBuf {
    let root = dir.join("plugin");
    let path = root.join(TARGET_REL);
    fs::create_dir_all(path.parent().expect("parent")).expect("create fixture dir");
    fs::write(path, text).expect("write fixture");
    root
}

/// Writes `text` as the only discovery file and runs the audit.
fn audit_text(label: &str, text: &str) -> (u8, String, String) {
    let dir = TempDir::new(label);
    run_audit(&write_fixture(&dir, text))
}

/// Asserts the audit passes.
fn assert_clean(label: &str, text: &str) {
    let (code, out, err) = audit_text(label, text);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("consent-disclosure audit: OK"), "{out}");
}

/// Asserts the audit fails; returns stderr.
fn assert_fails(label: &str, text: &str) -> String {
    let (code, out, err) = audit_text(label, text);
    assert_eq!(code, 1, "stdout: {out}");
    err
}

const GET_ONLY_FILE: &str = "\
# Discover Phase: Example Usage API Discovery

## Security Contract (applies to every step)

1. Exact-endpoint allowlist, GET only. Call ONLY the endpoints below.

## Step 0: Consent Gate

This runs GET requests only, against a fixed list of usage endpoints.

## Step 2: Capture

**2c. Capture Endpoint Table.** Every row is `GET` against
`https://example.com/api/v1`.

| # | Endpoint (GET) | Purpose | Output file |
| - | -------------- | ------- | ------------ |
| 1 | `/usage`       | usage   | usage.json   |

## Scope Boundary

- Any non-GET method is forbidden.
";

const POST_ROW: &str = "| 1 | `/usage`       | usage   | usage.json   |";
const POST_ROW_PLUS: &str = "| 1 | `/usage`       | usage   | usage.json   |\n| 2 | `POST /analytics/query` | per-model split | analytics.json |";

fn unqualified_post_file() -> String {
    GET_ONLY_FILE.replace(POST_ROW, POST_ROW_PLUS)
}

const QUALIFIED_POST_FILE: &str = "\
# Discover Phase: Example Usage API Discovery

## Security Contract (applies to every step)

1. Exact-endpoint allowlist, GET + one scoped POST. All are `GET` EXCEPT
   `POST /analytics/query`, a read-only aggregate query.

## Step 0: Consent Gate

This runs GET requests against a fixed list of usage endpoints, plus one
read-only POST to /analytics/query for an aggregate split.

## Step 2: Capture

**2c. Capture Endpoint Table.** Row 1 is `GET` against
`https://example.com/api/v1`. Row 2 is `POST` to that same base, read-only.

| # | Endpoint                | Purpose          | Output file      |
| - | ------------------------ | ---------------- | ----------------- |
| 1 | `GET /usage`             | usage            | usage.json        |
| 2 | `POST /analytics/query`  | per-model split  | analytics.json    |

## Scope Boundary

- Any HTTP method other than GET EXCEPT the one read-only POST
  /analytics/query aggregate query is forbidden.
";

const GET_CLAIM: &str = "This runs GET requests only, against a fixed list of usage endpoints.";

const HEADER_METHOD_FILE: &str = "\
# Discover Phase: Example Usage API Discovery

## Security Contract (applies to every step)

1. Exact-endpoint allowlist, GET only. Call ONLY the two endpoints below.

## Step 0: Consent Gate

This runs GET requests only, against a fixed list of usage/cost endpoints.

## Step 2: Capture

**2c. Capture Endpoint Table.** Row 1 is filtered to the selected workspaces
in a single call via repeated `workspace_ids[]=<id>` query parameters. Row 2
has no `workspace_ids[]` parameter at all — the endpoint has none — but it
does accept `group_by` as an array, so it runs in one single call with both
dimensions together. Each returned row already carries its own workspace_id
field plus the parsed description dimension. After the call returns, filter
the rows client-side to keep only those whose workspace_id is in the
selection — discard the rest before anything is written to the output file.
Unselected workspaces' rows pass through this one response but are never
stored past that filter, consistent with the Step 0 consent promise made
above. This padding exists so the header cell below lands well past the
400-character mark some bounded-slice implementations use.

| # | Endpoint ({method}, `https://api.example.com`) | Query parameters | Output file |
|---|---|---|---|
| 1 | `/v1/organizations/usage_report/messages` | `starting_at`, `bucket_width=1d` | usage-messages.json |
| 2 | `/v1/organizations/cost_report` | `starting_at`, `bucket_width=1d` | cost-report.json |

## Scope Boundary

- Any non-GET method is forbidden.
";

const GET_POST_PATCH_TABLE: &str = "
## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Endpoint | Purpose | Output file |
| - | -------- | ------- | ------------ |
| 1 | `GET /usage` | usage | usage.json |
| 2 | `POST /analytics/query` | per-model split | analytics.json |
| 3 | `PATCH /settings` | update settings | settings.json |
";

fn methods(text: &str) -> (Vec<&'static str>, bool) {
    let (found, unrecognized) = methods_in_capture_section(text);
    (found.into_iter().collect(), unrecognized)
}

fn with_table(head: &str) -> String {
    format!("# Discover Phase: Example Usage API Discovery\n\n## Step 0: Consent Gate\n\n{head}\n{GET_POST_PATCH_TABLE}")
}

#[test]
fn clean_get_only_file_passes() {
    assert_clean("clean", GET_ONLY_FILE);
}

#[test]
fn unqualified_post_claim_fails_in_every_section() {
    let err = assert_fails("unqualified", &unqualified_post_file());
    assert!(err.contains("GET requests only"), "{err}");
    assert!(err.contains("Every row is"), "{err}");
    assert!(err.contains("non-GET method"), "{err}");
    assert!(err.contains(TARGET_REL), "{err}");
    assert!(
        err.starts_with("consent-disclosure audit: 4 problem(s)\n"),
        "{err}"
    );
    assert!(err.contains(
        "\n  - skills/example-skill/references/phases/discover/discover-example-api.md:5: claims a GET-only restriction, but the Capture Endpoint Table calls POST too — add an inline exception naming the method, or the endpoint, to this line: '1. Exact-endpoint allowlist, GET only. Call ONLY the endpoints below.'\n"
    ), "{err}");
    assert!(
        err.ends_with("error: consent-disclosure audit found 4 problem(s)\n"),
        "{err}"
    );
}

#[test]
fn qualified_post_claim_passes() {
    assert_clean("qualified", QUALIFIED_POST_FILE);
}

#[test]
fn exception_naming_the_wrong_method_still_fails() {
    let text = unqualified_post_file().replace(
        GET_CLAIM,
        "This runs GET requests only, against a fixed list of usage endpoints, except for an unrelated PATCH on a retired endpoint.",
    );
    let err = assert_fails("wrong-method", &text);
    assert!(
        err.contains("the Capture Endpoint Table calls POST too"),
        "{err}"
    );
}

#[test]
fn files_outside_the_discover_api_pattern_are_ignored() {
    let dir = TempDir::new("scope");
    let root = write_fixture(&dir, GET_ONLY_FILE);
    fs::write(
        root.join("skills/example-skill/SKILL.md"),
        "This flow runs GET requests only.\n\n| 1 | `POST /x` | y | z.json |\n",
    )
    .expect("write SKILL.md");
    let discover = root.join("skills/example-skill/references/phases/discover");
    fs::write(discover.join("discover-api.md"), "GET requests only.\n").expect("no star part");
    fs::write(discover.join("notes.md"), "GET requests only.\n").expect("other name");
    let (code, out, err) = run_audit(&root);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "consent-disclosure audit: OK (1 discovery sub-file(s) checked)\n"
    );
}

#[test]
fn no_matching_files_is_a_setup_error_not_a_silent_pass() {
    let dir = TempDir::new("zero");
    fs::create_dir_all(dir.join("plugin/skills")).expect("skills dir");
    let (code, out, err) = run_audit(&dir.join("plugin"));
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(
        err.starts_with("consent-disclosure audit: 1 setup problem(s)\n"),
        "{err}"
    );
    assert!(
        err.contains(
            "found ZERO files matching skills/*/references/phases/discover/discover-*-api.md"
        ),
        "{err}"
    );
    assert!(
        err.ends_with("error: consent-disclosure audit stopped on 1 setup problem(s)\n"),
        "{err}"
    );
}

#[test]
fn a_missing_plugin_directory_is_a_usage_error() {
    let dir = TempDir::new("missing");
    let (code, out, err) = run_audit(&dir.join("absent"));
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.contains("Plugin directory not found: "), "{err}");
}

#[test]
fn methods_read_table_rows() {
    let (found, unrecognized) = methods(&unqualified_post_file());
    assert_eq!(found, ["GET", "POST"]);
    assert!(!unrecognized);
}

#[test]
fn scope_boundary_mentions_are_not_calls() {
    let (found, unrecognized) = methods(GET_ONLY_FILE);
    assert_eq!(found, ["GET"]);
    assert!(!unrecognized);
}

#[test]
fn header_cell_convention_is_read_far_from_the_label() {
    let text = HEADER_METHOD_FILE.replace("{method}", "GET");
    assert_eq!(methods(&text), (vec!["GET"], false));
}

#[test]
fn header_cell_post_is_detected_even_far_from_the_label() {
    let err = assert_fails(
        "header-post",
        &HEADER_METHOD_FILE.replace("{method}", "POST"),
    );
    assert!(err.contains("GET requests only"), "{err}");
    assert!(err.contains("non-GET method"), "{err}");
}

#[test]
fn committed_anthropic_and_azure_openai_files_have_their_method_detected() {
    for rel in [
        "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md",
        "skills/azure-to-aws/references/phases/discover/discover-anthropic-api.md",
        "skills/azure-to-aws/references/phases/discover/discover-openai-api.md",
    ] {
        let text = fs::read_to_string(real_plugin().join(rel)).expect("read real file");
        assert_eq!(methods(&text), (vec!["GET"], false), "{rel}");
    }
}

#[test]
fn all_six_committed_discovery_files_return_a_nonempty_method_set() {
    let files = target_files(&real_plugin());
    assert_eq!(files.len(), 6, "{files:?}");
    for (skill, name) in files {
        let path = real_plugin()
            .join("skills")
            .join(&skill)
            .join("references/phases/discover")
            .join(&name);
        let text = fs::read_to_string(&path).expect("read real file");
        let (found, unrecognized) = methods_in_capture_section(&text);
        assert!(!found.is_empty(), "{path:?}: no HTTP method read");
        assert!(!unrecognized, "{path:?}");
    }
}

#[test]
fn unrecognized_table_shape_fails_closed_instead_of_skipping() {
    let text = "\
## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Something unrecognized | Purpose |
| - | ----------------------- | ------- |
| 1 | mystery-cell            | x       |
";
    let err = assert_fails("unrecognized", text);
    assert!(err.contains("no HTTP method could be read"), "{err}");
    assert_eq!(methods(text), (vec![], true));
}

#[test]
fn a_step_2_without_a_table_is_not_an_error() {
    assert_eq!(
        methods("## Step 2: Capture\n\nNo table here.\n"),
        (vec![], false)
    );
    assert_eq!(methods("# No step two\n"), (vec![], false));
}

fn real_anthropic() -> String {
    let rel = "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md";
    fs::read_to_string(real_plugin().join(rel)).expect("read real file")
}

#[test]
fn mutating_the_real_anthropic_header_to_post_is_caught() {
    let real = real_anthropic();
    assert!(real.contains("Endpoint (GET, `https://api.anthropic.com`)"));
    let mutated = real.replace(
        "Endpoint (GET, `https://api.anthropic.com`)",
        "Endpoint (POST, `https://api.anthropic.com`)",
    );
    let err = assert_fails("mutated", &mutated);
    assert!(
        err.contains("GET requests only") || err.contains("non-GET"),
        "{err}"
    );
}

#[test]
fn soft_wrapped_claim_across_two_lines_is_caught() {
    let text = unqualified_post_file().replace(
        GET_CLAIM,
        "This runs GET requests\nonly, against a fixed list of usage endpoints.",
    );
    let err = assert_fails("wrapped", &text);
    assert!(err.contains("GET requests"), "{err}");
    assert!(
        err.contains("discover-example-api.md:9: claims a GET-only restriction"),
        "{err}"
    );
}

#[test]
fn soft_wrapped_exception_still_qualifies_the_claim() {
    let text = QUALIFIED_POST_FILE.replace(
        "This runs GET requests against a fixed list of usage endpoints, plus one\nread-only POST to /analytics/query for an aggregate split.",
        "This runs GET requests against a fixed list of usage\nendpoints, plus one read-only POST to /analytics/query\nfor an aggregate split.",
    );
    assert_clean("wrapped-exception", &text);
}

#[test]
fn adjacent_bullet_exception_does_not_qualify_a_different_bullet() {
    let text = unqualified_post_file().replace(
        GET_CLAIM,
        "- This runs GET requests only.\n- Forbidden: other calls, except POST /analytics/query.",
    );
    let err = assert_fails("bullets", &text);
    assert!(err.contains("This runs GET requests only."), "{err}");
}

#[test]
fn sibling_bullet_with_its_own_exception_still_passes() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Step 0: Consent Gate

- This runs GET requests only, except POST /analytics/query.
- A second, unrelated bullet with no claim at all.

## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Endpoint | Purpose | Output file |
| - | -------- | ------- | ------------ |
| 1 | `GET /usage` | usage | usage.json |
| 2 | `POST /analytics/query` | per-model split | analytics.json |
";
    assert_clean("sibling", text);
}

#[test]
fn exception_naming_only_one_of_two_undisclosed_methods_still_fails() {
    let text = with_table("This runs GET requests only, except POST /analytics/query, against a fixed list of usage endpoints.");
    let err = assert_fails("one-of-two", &text);
    assert!(err.contains("PATCH"), "{err}");
}

#[test]
fn exception_naming_every_undisclosed_method_passes() {
    let text = with_table("This runs GET requests only, except POST /analytics/query and PATCH /settings, against a fixed list of usage endpoints.");
    assert_clean("every-method", &text);
}

#[test]
fn unreadable_file_fails_closed_instead_of_being_skipped() {
    let dir = TempDir::new("unreadable");
    let root = write_fixture(&dir, &unqualified_post_file());
    let mut bytes = unqualified_post_file().into_bytes();
    bytes.push(0xff);
    fs::write(root.join(TARGET_REL), bytes).expect("write invalid UTF-8");
    let (code, out, err) = run_audit(&root);
    assert_eq!(code, 1, "{out}");
    assert!(
        err.contains("could not read file (not valid UTF-8) — treating as a failure, not a skip"),
        "{err}"
    );
    assert!(!out.contains("OK (1 discovery sub-file(s) checked)"));
    let outcome = audit(&root);
    assert_eq!(outcome.checked, 0);
}

#[test]
fn a_directory_named_like_a_target_is_a_read_failure() {
    let dir = TempDir::new("dir-target");
    let root = write_fixture(&dir, GET_ONLY_FILE);
    fs::create_dir(
        root.join("skills/example-skill/references/phases/discover/discover-dir-api.md"),
    )
    .expect("create directory");
    let (code, _, err) = run_audit(&root);
    assert_eq!(code, 1);
    assert!(
        err.contains("discover-dir-api.md: could not read file ("),
        "{err}"
    );
}

#[test]
fn prohibited_example_in_prose_is_not_read_as_a_real_call() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Step 0: Consent Gate

This runs GET requests only, against a fixed list of usage endpoints.

## Step 2: Capture

- Never call `POST /v1/messages`; only the usage-report endpoints below are
  allowed.

**2c. Capture Endpoint Table.**

| # | Endpoint (GET) | Purpose | Output file |
| - | --------------- | ------- | ------------ |
| 1 | `/usage`        | usage   | usage.json   |
";
    assert_clean("prose-example", text);
}

#[test]
fn prohibited_example_outside_step_2_is_also_ignored() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Security Contract (applies to every step)

- Never call `POST /v1/messages`; only the usage-report endpoints below are
  allowed.

## Step 0: Consent Gate

This runs GET requests only, against a fixed list of usage endpoints.

## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Endpoint (GET) | Purpose | Output file |
| - | --------------- | ------- | ------------ |
| 1 | `/usage`        | usage   | usage.json   |
";
    assert_clean("outside-step-2", text);
}

#[test]
fn a_real_post_row_is_still_caught_despite_prose_scoping() {
    let err = assert_fails("real-post", &unqualified_post_file());
    assert!(err.contains("POST"), "{err}");
}

#[test]
fn inline_code_wrapped_method_in_an_existing_claim_is_still_caught() {
    let text = unqualified_post_file().replace(
        GET_CLAIM,
        "This runs `GET` requests only, against a fixed list of usage endpoints.",
    );
    let err = assert_fails("code-wrapped", &text);
    assert!(
        err.contains("GET` requests only") || err.contains("GET requests only"),
        "{err}"
    );
}

#[test]
fn inline_code_wrapped_claim_variants_are_all_caught() {
    let post: BTreeSet<&'static str> = ["POST"].into_iter().collect();
    for claim in [
        "This flow calls `GET` only.",
        "Every row is `GET` against the base URL.",
        "It only calls GET today.",
        "I only call `GET` here.",
        "Calls are get-only.",
        "No other call is allowed: any other HTTP method is refused.",
    ] {
        assert_eq!(unqualified_claims(claim, &post).len(), 1, "{claim}");
    }
}

#[test]
fn qualified_unwrapped_claims_still_pass() {
    assert_clean("unwrapped", QUALIFIED_POST_FILE);
}

#[test]
fn a_negated_exception_marker_does_not_falsely_qualify_the_claim() {
    let text = with_table(
        "This runs GET requests only, without exception; POST and PATCH are\nforbidden.",
    );
    assert_fails("negated", &text);
    let upper = with_table("This runs GET requests only, WITHOUT EXCEPTION; POST is forbidden.");
    assert_fails("negated-upper", &upper);
}

#[test]
fn a_method_hiding_inside_an_unrelated_word_does_not_qualify() {
    let text = with_table("This runs GET requests only, except POST /analytics/dispatch.");
    let err = assert_fails("dispatch", &text);
    assert!(err.contains("PATCH"), "{err}");
}

#[test]
fn put_inside_output_does_not_qualify_put() {
    let table = GET_POST_PATCH_TABLE.replace(
        "| 3 | `PATCH /settings` | update settings | settings.json |\n",
        "| 3 | `PUT /settings` | update settings | settings.json |\n",
    );
    let text = format!(
        "# Discover Phase: Example Usage API Discovery\n\n## Step 0: Consent Gate\n\nThis runs GET requests only, except POST /analytics/query; save the output locally.\n{table}"
    );
    let err = assert_fails("output", &text);
    assert!(err.contains("PUT"), "{err}");
}

#[test]
fn a_method_named_only_in_a_separate_denial_sentence_does_not_qualify() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Step 0: Consent Gate

This runs GET requests only, except POST /query. PATCH requests are
forbidden.

## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Endpoint | Purpose | Output file |
| - | -------- | ------- | ------------ |
| 1 | `GET /usage` | usage | usage.json |
| 2 | `POST /query` | q | q.json |
| 3 | `PATCH /settings` | update settings | settings.json |
";
    let err = assert_fails("denial", text);
    assert!(err.contains("PATCH"), "{err}");
}

#[test]
fn both_methods_named_in_the_same_exception_sentence_pass() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Step 0: Consent Gate

This runs GET requests only, except POST /query and PATCH /settings.

## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Endpoint | Purpose | Output file |
| - | -------- | ------- | ------------ |
| 1 | `GET /usage` | usage | usage.json |
| 2 | `POST /query` | q | q.json |
| 3 | `PATCH /settings` | update settings | settings.json |
";
    assert_clean("same-sentence", text);
}

#[test]
fn a_prohibited_example_in_a_get_rows_own_cell_is_not_a_call() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Step 0: Consent Gate

This runs GET requests only, against a fixed list of usage endpoints.

## Step 2: Capture

**2c. Capture Endpoint Table.**

| # | Endpoint (GET) | Purpose | Output file |
| - | --------------- | ------- | ------------ |
| 1 | `/usage` | Never call `POST /v1/messages` here | usage.json |
";
    assert_clean("purpose-cell", text);
}

#[test]
fn header_looking_prose_outside_the_real_header_row_is_ignored() {
    let text = "\
# Discover Phase: Example Usage API Discovery

## Step 0: Consent Gate

This runs GET requests only, against a fixed list of usage endpoints.

## Step 2: Capture

**2c. Capture Endpoint Table.** Note: Endpoint (POST) is never used here,
only GET.

| # | Endpoint (GET) | Purpose | Output file |
| - | --------------- | ------- | ------------ |
| 1 | `/usage` | usage | usage.json |
";
    assert_clean("header-prose", text);
}

#[test]
fn a_real_post_header_row_is_still_caught_after_column_scoping() {
    let mutated = real_anthropic().replace(
        "Endpoint (GET, `https://api.anthropic.com`)",
        "Endpoint (POST, `https://api.anthropic.com`)",
    );
    assert_fails("header-row", &mutated);
}

#[test]
fn head_and_options_rows_are_recognised_as_non_get() {
    for method in ["HEAD", "OPTIONS"] {
        let text = format!(
            "# Discover Phase: Example Usage API Discovery\n\n## Step 0: Consent Gate\n\nThis runs GET requests only.\n\n## Step 2: Capture\n\n**2c. Capture Endpoint Table.**\n\n| # | Endpoint | Purpose | Output file |\n| - | -------- | ------- | ------------ |\n| 1 | `GET /usage` | usage | usage.json |\n| 2 | `{method} /usage` | check | none |\n"
        );
        let err = assert_fails("head-options", &text);
        assert!(err.contains(method), "{method}: {err}");
    }
}

#[test]
fn any_case_of_except_still_qualifies() {
    for marker in ["EXCEPT", "Except", "except"] {
        let text = format!(
            "# Discover Phase: Example Usage API Discovery\n\n## Step 0: Consent Gate\n\nThis runs GET requests only, {marker} POST /query.\n\n## Step 2: Capture\n\n**2c. Capture Endpoint Table.**\n\n| # | Endpoint | Purpose | Output file |\n| - | -------- | ------- | ------------ |\n| 1 | `GET /usage` | usage | usage.json |\n| 2 | `POST /query` | q | q.json |\n"
        );
        assert_clean(&format!("marker-{marker}"), &text);
    }
}

#[test]
fn real_plugin_discovery_files_pass() {
    let (code, out, err) = run_audit(&real_plugin());
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "consent-disclosure audit: OK (6 discovery sub-file(s) checked)\n"
    );
}

#[test]
fn call_site_rules_follow_the_method_call_pattern() {
    let calls = super::method_calls;
    assert_eq!(calls("`GET /usage`"), ["GET"]);
    assert_eq!(calls("All are `GET` against `https://x`"), ["GET"]);
    assert_eq!(calls("POST to that same base `/x`"), ["POST"]);
    assert_eq!(calls("POST against /x and DELETE `/y`"), ["POST", "DELETE"]);
    assert_eq!(calls("GETS /x"), Vec::<&str>::new());
    assert_eq!(calls("xGET /x"), Vec::<&str>::new());
    assert_eq!(
        calls("get /x"),
        Vec::<&str>::new(),
        "methods are upper case"
    );
    assert_eq!(calls("GET usage"), Vec::<&str>::new());
    assert_eq!(calls("PUT/x"), ["PUT"]);
}

#[test]
fn paragraph_blocks_split_on_blank_lines_and_sibling_items() {
    let blocks = paragraphs("a\nb\n\n- x\n- y\n  more\n  - nested\n1. one\n2. two\n\n\nlast");
    assert_eq!(
        blocks,
        vec![
            (1, vec!["a", "b"]),
            (4, vec!["- x"]),
            (5, vec!["- y", "  more", "  - nested"]),
            (8, vec!["1. one"]),
            (9, vec!["2. two"]),
            (12, vec!["last"]),
        ]
    );
}

#[test]
fn sentences_split_after_sentence_punctuation_only() {
    assert_eq!(
        sentences("a. b? c!  d e.f g"),
        ["a.", "b?", "c!", "d e.f g"]
    );
    assert_eq!(sentences("no punctuation here"), ["no punctuation here"]);
}

#[test]
fn unqualified_claims_report_line_numbers_and_stripped_lines() {
    let non_get: BTreeSet<&'static str> = ["POST"].into_iter().collect();
    let text = "intro\n\n   GET requests only.\nsecond line\n";
    assert_eq!(
        unqualified_claims(text, &non_get),
        vec![(3, "GET requests only.".to_string())]
    );
    assert!(unqualified_claims(text, &BTreeSet::new()).is_empty());
}
