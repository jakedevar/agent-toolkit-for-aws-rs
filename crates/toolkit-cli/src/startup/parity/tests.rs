//! Tests for `toolkit startup pricing-parity`: the text helpers, the checks
//! against the real plugin tree and the pre-split commit, and mutated copies
//! of that tree that must fail with the Python messages.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    accuracy_bands_in, git_show, is_decimal, markdown_data_rows, metadata_field, model_rows,
    pricing_accuracy_bands, run_checks, section, strip_embeddings, subsection, AZURE_CACHE,
    AZURE_ESTIMATE, AZURE_ESTIMATE_AI, AZURE_OPENAI, BASE_SHA, CANONICAL, GCP_CACHE, GCP_ESTIMATE,
    GCP_ESTIMATE_AI, GCP_SKILL, NEUTRAL_CONSUMERS, VENDORED,
};
use crate::run;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-parity-{label}-{}-{}",
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

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn real_plugin() -> PathBuf {
    repo().join("plugins/aws-startup-advisor")
}

/// The pre-split commit may be missing from a shallow clone; the checks that
/// need it are skipped there (they run wherever the full history exists).
fn base_available() -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo())
        .args(["cat-file", "-e", &format!("{BASE_SHA}^{{commit}}")])
        .status()
        .is_ok_and(|status| status.success())
}

fn show(path: &str) -> Result<String, String> {
    git_show(&repo(), path)
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

#[test]
fn sections_end_at_the_next_second_level_heading() {
    let text = "intro\n## A one\nx\n### sub\n## B\ny\n";
    assert_eq!(section(text, "A"), Ok("## A one\nx\n### sub\n"));
    assert_eq!(section(text, "B"), Ok("## B\ny\n"));
    assert_eq!(section(text, "C"), Err("missing section: C".to_string()));
    assert_eq!(section("## A", "A"), Ok("## A"));
}

#[test]
fn subsections_end_at_the_next_second_or_third_level_heading() {
    let text = "## S\n### E x\nrow\n#### deeper\n### F\n## T\n";
    assert_eq!(subsection(text, "E"), Ok("### E x\nrow\n#### deeper\n"));
    assert_eq!(subsection("## S\n### E\n## T\n", "E"), Ok("### E\n"));
    assert_eq!(
        subsection(text, "Z"),
        Err("missing subsection: Z".to_string())
    );
}

#[test]
fn metadata_fields_follow_the_regular_expression() {
    assert_eq!(
        metadata_field("**Region:** us-east-1\n", "Region").as_deref(),
        Some("us-east-1")
    );
    assert_eq!(
        metadata_field("x\n**Region:**us-east-1", "Region").as_deref(),
        Some("us-east-1")
    );
    assert_eq!(
        metadata_field("**Region:** a\n**Region:** b\n", "Region").as_deref(),
        Some("a")
    );
    assert_eq!(metadata_field("**Region** x\n", "Region"), None);
    assert_eq!(metadata_field(" **Region:** x\n", "Region"), None);
    // `\s*` crosses a line break, so an empty value takes the next line.
    assert_eq!(
        metadata_field("**Region:**   \n**Currency:** USD\n", "Region").as_deref(),
        Some("**Currency:** USD")
    );
    // At the end of the text it gives whitespace back to `.+`.
    assert_eq!(
        metadata_field("**Region:** \n", "Region").as_deref(),
        Some(" ")
    );
    assert_eq!(metadata_field("**Region:**\n", "Region"), None);
}

#[test]
fn decimals_need_digits_on_both_sides_of_the_point() {
    for good in ["3", "3.00", "0.5", "10"] {
        assert!(is_decimal(good), "{good}");
    }
    for bad in ["", ".5", "3.", "1.2.3", "n/a", "-1", "1e3", " 1"] {
        assert!(!is_decimal(bad), "{bad}");
    }
}

#[test]
fn model_rows_read_price_pairs_and_skip_the_embeddings_subsection() {
    let text = "\
## Bedrock Models (On-Demand)

| Provider | Key | Name | In | Out |
| --- | --- | --- | --- | --- |
| A | anthropic.claude-x | Claude | 3.00 | 15.00 |
| A | nodot | Claude | 3 | 15 |
| A | amazon.y | Y | n/a | 1 |
| A | short.row | 1 | 2 |

### Embeddings — Bedrock (per 1M)

| E | amazon.titan | T | 0.1 | 1536 |

### Other

|| A | meta.z | Z | 0.5 | 1.5 ||

## Next

| A | outside.k | | 1 | 2 |
";
    let rows = model_rows(text).expect("rows");
    let shown: Vec<_> = rows
        .iter()
        .map(|(key, (input, output))| format!("{key}={input}/{output}"))
        .collect();
    assert_eq!(shown, ["anthropic.claude-x=3.00/15.00", "meta.z=0.5/1.5"]);
    assert_eq!(
        model_rows("## Other\n").err().as_deref(),
        Some("missing section: Bedrock Models (On-Demand)")
    );
}

#[test]
fn data_rows_exclude_separators_and_model_headers() {
    let text =
        "| Model | a |\n|---|---|\n| :-: | - |\n| Models x | y |\nprose\n| row | 1 |\n|row2|2|\n";
    assert_eq!(
        markdown_data_rows(text),
        ["| Models x | y |", "| row | 1 |", "|row2|2|"]
    );
}

#[test]
fn accuracy_bands_are_the_two_signed_ranges_of_the_contract_line() {
    assert_eq!(
        accuracy_bands_in("a ±5-10% b ±15-25% c ±3%"),
        ["±5-10%", "±15-25%"]
    );
    assert!(accuracy_bands_in("±5-%, ±-5%, ± 5-10%").is_empty());
    let text = "intro\n- Primary pricing source: infra ±5-10%, AI ±15-25%.\n";
    assert_eq!(
        pricing_accuracy_bands(text),
        Ok(("±5-10%".to_string(), "±15-25%".to_string()))
    );
    assert_eq!(
        pricing_accuracy_bands("nothing"),
        Err("missing primary pricing source contract".to_string())
    );
    assert_eq!(
        pricing_accuracy_bands("- Primary pricing source ±5-10%"),
        Err("expected infrastructure and AI pricing accuracy bands".to_string())
    );
}

#[test]
fn the_embeddings_subsection_is_stripped_up_to_the_stability_heading() {
    let start = "### Embeddings — Bedrock (per 1M input tokens, US East)";
    let text = format!("a\n{start}\nrows\n### Stability AI\nb\n{start}\nrest-without-stop");
    assert_eq!(
        strip_embeddings(&text),
        format!("a\n### Stability AI\nb\n{start}\nrest-without-stop")
    );
    assert_eq!(strip_embeddings("plain"), "plain");
}

/// The pre-split texts, derived from the CURRENT real files so the checks can
/// pass independent of the upstream cache's later growth: the old GCP cache is
/// the remaining cache plus the Bedrock section without the reinstated
/// embeddings subsection, the old Azure cache carries the embeddings table.
fn synthetic_show(path: &str) -> Result<String, String> {
    let read = |rel: &str| fs::read_to_string(real_plugin().join(rel)).map_err(|e| e.to_string());
    let canonical = read(CANONICAL)?;
    match path {
        p if p == super::OLD_CACHE => {
            let mut old = read(GCP_CACHE)?;
            for key in ["Last updated", "Region", "Currency"] {
                let prefix = format!("**{key}:**");
                let (new_start, new_end) =
                    super::find_line(&canonical, &prefix).ok_or("canonical metadata")?;
                let (start, end) = super::find_line(&old, &prefix).ok_or("old metadata")?;
                old = format!(
                    "{}{}{}",
                    &old[..start],
                    &canonical[new_start..new_end],
                    &old[end..]
                );
            }
            if !old.ends_with('\n') {
                old.push('\n');
            }
            let bedrock = section(&canonical, "Bedrock Models (On-Demand)")?;
            Ok(old + &strip_embeddings(bedrock))
        }
        p if p == super::OLD_AZURE_CACHE => {
            let embeddings = subsection(&canonical, "Embeddings — Bedrock")?;
            Ok(read(AZURE_CACHE)? + "\n## Pre-split embeddings\n" + embeddings)
        }
        p if p == super::OLD_SKILL => read(GCP_SKILL),
        other => Err(format!("unexpected pre-split path {other}")),
    }
}

#[test]
fn the_real_tree_has_drifted_from_the_pre_split_commit() {
    // The cache kept growing after the comparison commit (Opus 5.5 rows and
    // notes), so the pinned proof fails on the real tree exactly as the Python
    // script does; the failure is the Bedrock section comparison.
    if !base_available() {
        return;
    }
    assert_eq!(
        run_checks(&real_plugin(), &show).expect_err("the cache has grown since"),
        "Bedrock section changed during extraction"
    );
    let plugin = real_plugin();
    let (code, out, err) = exec(&[
        "startup",
        "pricing-parity",
        "--plugin-root",
        plugin.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert_eq!(err, "error: Bedrock section changed during extraction\n");
}

#[test]
fn a_faithful_tree_passes_and_the_report_is_sorted_json() {
    let report = run_checks(&real_plugin(), &synthetic_show).expect("the split is intact");
    assert_eq!(
        report.get("status").and_then(toolkit_core::Json::as_str),
        Some("PASS")
    );
    let text = super::dumps(&report, Some(2), true);
    assert!(text.starts_with("{\n  \"baseline\": \"883270642209b06e1e76dcc4f944a1174abf0675\",\n  \"bedrock_model_lookup_keys\": "), "{text}");
    assert!(text.contains("\n  \"pricing_accuracy_bands\": {\n    \"ai_models\": \"\\u00b115-25%\",\n    \"infrastructure\": \"\\u00b15-10%\"\n  },"), "{text}");
    assert!(text.contains("\n  \"pricing_behavior\": \"not checked: needs the Python bedrock_pricing.py modules\","), "{text}");
    assert!(
        text.contains(
            "\n  \"representative_monthly_estimates\": {\n    \"amazon.nova-pro-v1:0\": "
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\n  \"status\": \"PASS\",\n  \"vendored_copies\": 3\n}"),
        "{text}"
    );
}

#[test]
fn a_missing_plugin_directory_is_a_usage_error_and_a_bad_tree_fails() {
    let dir = TempDir::new("missing");
    let absent = dir.join("absent");
    let (code, out, err) = exec(&[
        "startup",
        "pricing-parity",
        "--plugin-root",
        absent.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.contains("Plugin directory not found: "), "{err}");

    // An empty plugin directory fails on the first unreadable input.
    let empty = dir.join("plugins/empty");
    fs::create_dir_all(&empty).expect("create empty plugin");
    let (code, out, err) = exec(&[
        "startup",
        "pricing-parity",
        "--plugin-root",
        empty.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(err.starts_with("error: "), "{err}");
}

/// Copies the files the checks read into a scratch plugin.
fn scratch_plugin(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    let mut files = vec![
        CANONICAL,
        GCP_CACHE,
        GCP_SKILL,
        GCP_ESTIMATE,
        GCP_ESTIMATE_AI,
        AZURE_CACHE,
        AZURE_ESTIMATE,
        AZURE_ESTIMATE_AI,
        AZURE_OPENAI,
    ];
    files.extend(VENDORED);
    files.extend(NEUTRAL_CONSUMERS);
    for rel in files {
        let target = dir.join(rel);
        fs::create_dir_all(target.parent().expect("parent")).expect("create dir");
        fs::copy(real_plugin().join(rel), target).expect("copy file");
    }
    dir
}

fn mutate(plugin: &Path, rel: &str, change: impl Fn(String) -> String) {
    let path = plugin.join(rel);
    let text = fs::read_to_string(&path).expect("read copy");
    fs::write(path, change(text)).expect("write mutation");
}

fn failure(plugin: &Path) -> String {
    run_checks(plugin, &synthetic_show).expect_err("the mutated tree must fail")
}

#[test]
fn the_unmodified_scratch_copy_passes() {
    let dir = scratch_plugin("pass");
    run_checks(&dir, &synthetic_show).expect("a faithful copy passes");
}

/// A mutation case: label, file, change and the expected message part.
type Case = (
    &'static str,
    &'static str,
    fn(String) -> String,
    &'static str,
);

#[test]
fn mutated_copies_fail_with_the_python_messages() {
    let cases: [Case; 5] = [
        (
            "vendored-byte",
            VENDORED[1],
            |text| text + " ",
            "vendored drift: ",
        ),
        (
            "bedrock-heading",
            CANONICAL,
            |text| {
                text.replace(
                    "## Bedrock Models (On-Demand)",
                    "## Bedrock Models (On-Demand) X",
                )
            },
            "Bedrock section changed during extraction",
        ),
        (
            "neutral-consumer",
            NEUTRAL_CONSUMERS[2],
            |text| text + "\nskills/gcp-to-aws/references/shared/pricing-cache.md\n",
            "neutral GCP pricing dependency remains: ",
        ),
        (
            "region",
            CANONICAL,
            |text| text.replacen("**Region:** us-east-1", "**Region:** eu-west-1", 1),
            "Bedrock Region metadata changed",
        ),
        (
            "azure-section",
            AZURE_CACHE,
            |text| text + "\n## Bedrock Models (On-Demand)\n",
            "assertion failed: the Azure cache still has the Bedrock section",
        ),
    ];
    for (label, rel, change, expected) in cases {
        let dir = scratch_plugin(label);
        mutate(&dir, rel, change);
        let message = failure(&dir);
        assert!(message.contains(expected), "{label}: {message}");
    }
}
