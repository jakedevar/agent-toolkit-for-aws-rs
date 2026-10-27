//! Offline, deterministic tests of `startup skill-refs` (#4407): the ported
//! `test_check_skill_refs.py` (a throwaway plugin tree exercises each
//! resolution and skip rule), a `fancy_regex` oracle that runs the original
//! Python token pattern against the hand-written scanner, and the link,
//! baseline, output and hostile-input cases the Python never covered.

#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::parse_json;

use super::*;
use crate::startup::scoring::fixtures::{exec, j, path_text, real_skill, write, TempDir};

/// The Python `_TOKEN_RE`, verbatim.
const ORACLE: &str = r"(?P<prefix>(?:\$\{?[A-Z_]+\}?/|<[A-Z_]+>/|\./|(?:\.\./)+)*)(?P<path>[A-Za-z0-9_.\-]+(?:/[A-Za-z0-9_.\-]+)*\.(?:md|json|py|sh|tf|ya?ml|html|txt|toml))(?![A-Za-z0-9_.])";

fn plugin(tmp: &TempDir) -> PathBuf {
    let plugin = tmp.dir("plugins/p");
    for dir in [
        "skills/alpha/references/phases",
        "skills/alpha/scripts",
        "skills/alpha/references/vendored/dsl",
        "skills/shared/dsl",
        "skills/shared/ai",
        "skills/gcp-to-aws/references/design-refs",
        "skills/agent-advisor",
        "skills/beta/references",
        "agents",
    ] {
        fs::create_dir_all(plugin.join(dir)).expect("fixture dir");
    }
    for (relative, text) in [
        ("skills/alpha/references/phases/design.md", "x"),
        ("skills/alpha/scripts/tool.py", "x"),
        ("skills/shared/dsl/INTERPRETER.md", "x"),
        (
            "skills/alpha/references/vendored/dsl/INTERPRETER.md",
            "see `references/vendored/state/phase-status.schema.json` and `shared/ai-guardrails.md`",
        ),
        ("skills/shared/ai/ai-guardrails.md", "x"),
        (
            "skills/shared/state.md",
            "vendored at `references/vendored/state.md`",
        ),
        ("skills/gcp-to-aws/references/design-refs/ai.md", "x"),
        ("skills/beta/references/offers.md", "x"),
        ("agents/worker.md", "validate with `scripts/tool.py`"),
    ] {
        write(&plugin, relative, text);
    }
    plugin
}

fn scan_rel(plugin: &Path, relative: &str) -> Vec<Reference> {
    Tree::new(plugin)
        .scan_file(&plugin.join(relative), relative)
        .expect("the file scans")
}

/// token → resolved, for a skill file with `text`.
fn refs(plugin: &Path, skill: &str, text: &str, name: &str) -> BTreeMap<String, bool> {
    let relative = format!("skills/{skill}/{name}");
    write(plugin, &relative, text);
    scan_rel(plugin, &relative)
        .into_iter()
        .map(|r| (r.raw, r.resolved.is_some()))
        .collect()
}

fn resolved(plugin: &Path, relative: &str) -> BTreeMap<String, Option<PathBuf>> {
    scan_rel(plugin, relative)
        .into_iter()
        .map(|r| (r.raw, r.resolved))
        .collect()
}

fn expected(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
}

fn tokens(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    find_tokens(&chars)
        .into_iter()
        .map(|t| chars[t.start..t.end].iter().collect())
        .collect()
}

// ---- the ported Python tests -------------------------------------------------

#[test]
fn skill_relative_and_variable_prefixes_resolve() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let got = refs(
        &plugin,
        "alpha",
        "Load `references/phases/design.md`. Run `python3 \"<SKILL_BASE>/scripts/tool.py\"`. \
         Also `$SKILL_BASE/scripts/tool.py`, `./scripts/tool.py`, \
         `$PLUGIN_ROOT/skills/shared/dsl/INTERPRETER.md`, \
         `${CLAUDE_PLUGIN_ROOT}/skills/alpha/scripts/tool.py`, and `../beta/references/offers.md`.",
        "SKILL.md",
    );
    assert_eq!(
        got,
        expected(&[
            ("references/phases/design.md", true),
            ("<SKILL_BASE>/scripts/tool.py", true),
            ("$SKILL_BASE/scripts/tool.py", true),
            ("./scripts/tool.py", true),
            ("$PLUGIN_ROOT/skills/shared/dsl/INTERPRETER.md", true),
            ("${CLAUDE_PLUGIN_ROOT}/skills/alpha/scripts/tool.py", true),
            ("../beta/references/offers.md", true),
        ])
    );
}

#[test]
fn a_missing_reference_is_reported() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let got = refs(
        &plugin,
        "alpha",
        "Load `references/phases/nope.md` then `scripts/gone.py`.",
        "SKILL.md",
    );
    assert_eq!(
        got,
        expected(&[
            ("references/phases/nope.md", false),
            ("scripts/gone.py", false)
        ])
    );
}

#[test]
fn run_artifacts_templates_urls_and_relative_outputs_are_not_references() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let got = refs(
        &plugin,
        "alpha",
        "Write `$MIGRATION_DIR/scripts/02-migrate-data.sh`; read `$BEDROCK_RUN_DIR/.phase-status.json`; \
         read `.migration/<run>/references/x.md`; \
         see https://example.com/references/phases/design.md ; open `references/offers/<slug>.md`; \
         then `./deploy.sh` and `../plan.md` and `scenarios/scenario-NNN.json`.",
        "SKILL.md",
    );
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn library_origin_files_resolve_against_shared() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let got = resolved(
        &plugin,
        "skills/alpha/references/vendored/dsl/INTERPRETER.md",
    );
    // `shared/ai-guardrails.md` → skills/shared/ai/ai-guardrails.md (flattened
    // directory); the state schema is genuinely absent everywhere.
    assert!(got["shared/ai-guardrails.md"].is_some());
    assert!(got["references/vendored/state/phase-status.schema.json"].is_none());
    let shared_doc = resolved(&plugin, "skills/shared/state.md");
    assert!(shared_doc["references/vendored/state.md"].is_some());
}

#[test]
fn gcp_dependents_may_name_the_gcp_tree_and_agents_search_one_skill() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let got = refs(
        &plugin,
        "agent-advisor",
        "remap `design-refs/ai.md` → `$GCP_BASE/references/design-refs/ai.md`",
        "SKILL.md",
    );
    assert_eq!(
        got,
        expected(&[
            ("design-refs/ai.md", true),
            ("$GCP_BASE/references/design-refs/ai.md", true),
        ])
    );
    // agents/worker.md names scripts/tool.py, which exists only under alpha:
    // agents resolve against llm-to-bedrock ONLY, so this is MISSING.
    let agent = resolved(&plugin, "agents/worker.md");
    assert_eq!(agent.len(), 1);
    assert!(agent["scripts/tool.py"].is_none());
}

#[test]
fn agents_resolve_only_against_llm_to_bedrock() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    write(&plugin, "skills/agent-advisor/scripts/pyproject.toml", "x");
    write(
        &plugin,
        "agents/evaluator.md",
        "run with `scripts/pyproject.toml` and `<BDD_DIR>/references/openai-to-bedrock.md`",
    );
    let got = resolved(&plugin, "agents/evaluator.md");
    assert!(got["scripts/pyproject.toml"].is_none());
    assert!(got["<BDD_DIR>/references/openai-to-bedrock.md"].is_none());
    write(&plugin, "skills/llm-to-bedrock/scripts/pyproject.toml", "x");
    write(
        &plugin,
        "skills/llm-to-bedrock/references/helpers/bdd/references/openai-to-bedrock.md",
        "x",
    );
    let got = resolved(&plugin, "agents/evaluator.md");
    assert_eq!(
        got["scripts/pyproject.toml"],
        Some(plugin.join("skills/llm-to-bedrock/scripts/pyproject.toml"))
    );
    let bdd = got["<BDD_DIR>/references/openai-to-bedrock.md"]
        .as_ref()
        .expect("found through the helper tree");
    assert!(bdd.to_string_lossy().contains("llm-to-bedrock"));
}

#[test]
fn the_run_artifact_skip_is_limited_to_the_current_token() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let missing = "references/shared/schema-design-aws-ai.md";
    let text = format!(
        "Alone: `{missing}`.\n\
         Write `aws-design-ai.json` to `$MIGRATION_DIR/` per `{missing}`.\n\
         Emit `$MIGRATION_DIR/scripts/02-migrate-data.sh`\n`{missing}`\n\
         `<run_dir>/{missing}` and `.migration/<run>/{missing}` and `$MIGRATION_DIR/{missing}`"
    );
    assert_eq!(
        refs(&plugin, "alpha", &text, "design-ai.md"),
        expected(&[(missing, false)])
    );
    let lines: Vec<usize> = scan_rel(&plugin, "skills/alpha/design-ai.md")
        .iter()
        .map(|r| r.line)
        .collect();
    assert_eq!(lines, [1, 2, 4]);
}

#[test]
fn explicit_base_prefixes_do_not_fall_back_to_other_skills() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    write(&plugin, "skills/beta/scripts/validate.py", "x");
    write(&plugin, "skills/gcp-to-aws/SKILL.md", "x");
    // A library-origin file (skills/<skill>/references/shared/).
    write(
        &plugin,
        "skills/alpha/references/shared/contract.md",
        "run `<SKILL_BASE>/scripts/validate.py`, `$PLUGIN_ROOT/scripts/validate.py`, \
         `$GCP_BASE/scripts/validate.py`, `./scripts/validate.py`; bare `scripts/validate.py`; \
         see `<SKILL_BASE>/../gcp-to-aws/SKILL.md` and `<SKILL_BASE>/scripts/tool.py`",
    );
    let got = resolved(&plugin, "skills/alpha/references/shared/contract.md");
    assert!(got["<SKILL_BASE>/scripts/validate.py"].is_none());
    assert!(got["$PLUGIN_ROOT/scripts/validate.py"].is_none());
    assert!(got["$GCP_BASE/scripts/validate.py"].is_none());
    assert!(got["./scripts/validate.py"].is_none());
    // An unqualified reference from a library-origin file may mean any vendoring skill.
    assert_eq!(
        got["scripts/validate.py"],
        Some(plugin.join("skills/beta/scripts/validate.py"))
    );
    // Explicit bases that name an existing file keep resolving, including `../`.
    assert_eq!(
        got["<SKILL_BASE>/../gcp-to-aws/SKILL.md"],
        Some(plugin.join("skills/gcp-to-aws/SKILL.md"))
    );
    assert_eq!(
        got["<SKILL_BASE>/scripts/tool.py"],
        Some(plugin.join("skills/alpha/scripts/tool.py"))
    );
    // A gcp dependent naming `$GCP_BASE/…` resolves there and only there.
    write(&plugin, "skills/agent-advisor/references/ai.md", "x");
    let got = refs(
        &plugin,
        "agent-advisor",
        "`$GCP_BASE/references/design-refs/ai.md` `$GCP_BASE/references/ai.md`",
        "SKILL.md",
    );
    assert_eq!(
        got,
        expected(&[
            ("$GCP_BASE/references/design-refs/ai.md", true),
            ("$GCP_BASE/references/ai.md", false),
        ])
    );
}

#[test]
fn relative_prefixes_do_not_fall_back_to_the_unprefixed_skill_path() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    write(
        &plugin,
        "skills/alpha/knowledge/estimate/estimate-defaults.json",
        "{}",
    );
    let relative = "skills/alpha/references/phases/estimate/estimate-infra.md";
    write(
        &plugin,
        relative,
        "see [`knowledge/estimate/estimate-defaults.json`](../../../knowledge/estimate/estimate-defaults.json).\n\
         short [`knowledge/estimate/estimate-defaults.json`](../../knowledge/estimate/estimate-defaults.json).\n\
         dot [`knowledge/estimate/estimate-defaults.json`](./knowledge/estimate/estimate-defaults.json).\n",
    );
    let got = resolved(&plugin, relative);
    assert_eq!(
        got["../../../knowledge/estimate/estimate-defaults.json"],
        Some(plugin.join("skills/alpha/knowledge/estimate/estimate-defaults.json"))
    );
    assert!(got["../../knowledge/estimate/estimate-defaults.json"].is_none());
    assert!(got["./knowledge/estimate/estimate-defaults.json"].is_none());
}

fn baseline_fixture(plugin: &Path) -> (Tree, Vec<Reference>) {
    write(
        plugin,
        "skills/alpha/SKILL.md",
        "`references/phases/nope.md` and `scripts/gone.py`",
    );
    let tree = Tree::new(plugin);
    let missing = scan_rel(plugin, "skills/alpha/SKILL.md")
        .into_iter()
        .filter(|r| r.resolved.is_none())
        .collect();
    (tree, missing)
}

#[test]
fn baseline_ignore_rules_entries_and_stale_entries() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let (tree, missing) = baseline_fixture(&plugin);
    // Sources are matched as repository-relative paths (`plugins/p/skills/…`).
    let baseline = j(
        r#"{"ignore":[{"source":"*","ref":"scripts/gone.py","reason":"generated"}],
            "entries":[{"source":"*alpha/SKILL.md","ref":"references/phases/nope.md","reason":"todo"},
                       {"source":"*","ref":"never.md","reason":"stale"}]}"#,
    );
    let (unbaselined, baselined, stale) =
        apply_baseline(&tree, missing, &baseline).expect("a valid baseline");
    assert!(unbaselined.is_empty());
    assert_eq!(
        baselined.iter().map(|r| r.raw.as_str()).collect::<Vec<_>>(),
        ["references/phases/nope.md"]
    );
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].get("ref").and_then(Json::as_str), Some("never.md"));
}

#[test]
fn a_malformed_baseline_is_an_error_not_a_crash() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    for (text, wanted) in [
        ("[]", "the document must be a JSON object"),
        (r#"{"ignore":{}}"#, "'ignore' must be an array"),
        (r#"{"entries":[3]}"#, "entries[0] must be an object"),
        (
            r#"{"entries":[{"source":7}]}"#,
            "entries[0].source must be a string",
        ),
        (
            r#"{"ignore":[{"ref":null}]}"#,
            "ignore[0].ref must be a string",
        ),
    ] {
        let (tree, missing) = baseline_fixture(&plugin);
        let error = apply_baseline(&tree, missing, &j(text)).expect_err(text);
        assert!(error.contains(wanted), "{text}: {error}");
    }
}

#[test]
fn the_command_exits_one_on_a_missing_reference_and_zero_when_clean() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let root = path_text(&plugin);
    write(
        &plugin,
        "skills/alpha/SKILL.md",
        "`references/phases/nope.md`",
    );
    let (status, stdout, stderr) = exec(&[
        "startup",
        "skill-refs",
        "--check",
        "--plugin-root",
        root,
        "--no-baseline",
        "--json",
    ]);
    assert_eq!(status, 1, "{stderr}");
    let report = parse_json(&stdout).expect("a JSON report");
    let missing = report.get("missing").expect("missing list");
    assert!(
        stdout.contains("\"ref\": \"references/phases/nope.md\""),
        "{stdout}"
    );
    assert!(matches!(missing, Json::Array(items) if !items.is_empty()));
    assert!(stderr.ends_with("skill reference problem(s)\n"), "{stderr}");

    write(
        &plugin,
        "skills/alpha/SKILL.md",
        "`references/phases/design.md`",
    );
    // The fixture tree carries two deliberately dead references: the vendored
    // INTERPRETER.md's state schema and agents/worker.md's scripts/tool.py.
    fs::remove_file(plugin.join("skills/alpha/references/vendored/dsl/INTERPRETER.md"))
        .expect("rm");
    fs::remove_file(plugin.join("agents/worker.md")).expect("rm");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "skill-refs",
        "--check",
        "--plugin-root",
        root,
        "--no-baseline",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    assert!(
        stdout
            .trim_end()
            .ends_with("0 missing, 0 baselined, 0 stale baseline entries"),
        "{stdout}"
    );
    assert!(stdout.contains("\nPASS — "), "{stdout}");
}

#[test]
fn the_real_repository_passes_with_no_stale_entries() {
    let plugin = real_skill().join("../..");
    let (status, stdout, stderr) = exec(&[
        "startup",
        "skill-refs",
        "--check",
        "--json",
        "--plugin-root",
        path_text(&plugin),
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""), "{stdout}");
    let report = parse_json(&stdout).expect("a JSON report");
    assert_eq!(
        report.get("missing"),
        Some(&Json::Array(Vec::new())),
        "{stdout}"
    );
    assert_eq!(
        report.get("stale_baseline_entries"),
        Some(&Json::Array(Vec::new())),
        "{stdout}"
    );
    let Some(Json::Int(count)) = report.get("references") else {
        panic!("references count in {stdout}");
    };
    assert!(*count > 500, "{count}");
    let Some(Json::Array(baselined)) = report.get("baselined") else {
        panic!("baselined list in {stdout}");
    };
    assert!(baselined.len() >= 6, "the six burn-down entries: {stdout}");
}

// ---- the scanner ----------------------------------------------------------------

#[test]
fn tokens_follow_the_greedy_path_pattern() {
    assert_eq!(
        tokens("Load `references/phases/design.md`."),
        ["references/phases/design.md"]
    );
    assert_eq!(tokens("a.md/b.md"), ["a.md/b.md"]);
    assert_eq!(
        tokens("x.mdx and y.md.bak and .md and ./.md"),
        Vec::<String>::new()
    );
    assert_eq!(tokens("x.md-foo"), ["x.md"]);
    assert_eq!(
        tokens("p.yaml q.yml r.toml s.html t.txt u.tf v.sh w.json x.py"),
        ["p.yaml", "q.yml", "r.toml", "s.html", "t.txt", "u.tf", "v.sh", "w.json", "x.py",]
    );
    assert_eq!(tokens("file.tar.md"), ["file.tar.md"]);
    assert_eq!(tokens("a//b.md"), ["b.md"]);
    assert_eq!(tokens("dir/x.md/"), ["dir/x.md"]);
    assert_eq!(tokens("../../a/b.md"), ["../../a/b.md"]);
    assert_eq!(
        tokens("${CLAUDE_PLUGIN_ROOT}/skills/x.md <SKILL_BASE>/../y.md $HOME/z.md"),
        [
            "${CLAUDE_PLUGIN_ROOT}/skills/x.md",
            "<SKILL_BASE>/../y.md",
            "$HOME/z.md"
        ]
    );
    assert_eq!(tokens(".../x.md"), [".../x.md"]);
}

#[test]
fn the_scanner_agrees_with_the_python_pattern_on_hard_lines() {
    let oracle = fancy_regex::Regex::new(ORACLE).expect("the Python pattern compiles");
    let lines = [
        "`references/a.md` `./b.md` ../../c/d.json $X/e.py ${Y}/f.sh <Z>/g.tf",
        "a.md/b.md/c.md.md .md.md a..md ..md .../..md",
        "x.yaml.yml y.yamlx z.ymll ./././a.md ../.././b.md ./../c.md",
        "$A$B/c.md <A><B>/d.md ${A/e.md $A}/f.md ${}/g.md $/h.md <>/i.md",
        "path/with-dash/and_under/file-name.v2.md,next/one.json;",
        "ref.md/ ref.md// ref.md/. ref.md/.md /ref.md //ref.md",
        "-a.md _a.md 9.md .a.md a.md. a.md.. a.md-",
        "scripts/x.pyc scripts/x.py3 scripts/x.py_ scripts/x.py. scripts/x.py-y",
        "$MIGRATION_DIR/scripts/a.sh <PLAN_DIR>/b.md .migration/c.md",
        "",
        "no tokens here at all",
    ];
    for line in lines {
        let expected: Vec<String> = oracle
            .find_iter(line)
            .map(|m| m.expect("match").as_str().to_string())
            .collect();
        assert_eq!(tokens(line), expected, "{line:?}");
    }
}

#[test]
fn the_scanner_agrees_with_the_python_pattern_on_generated_lines() {
    let oracle = fancy_regex::Regex::new(ORACLE).expect("the Python pattern compiles");
    let pieces = [
        "a", "b", "-", "_", ".", "/", "./", "../", "$", "{", "}", "<", ">", "X", "Y", " ", "md",
        ".md", ".json", ".yaml", "ml", "yml", "`", "9",
    ];
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..30_000 {
        let mut line = String::new();
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let length = 1 + (seed >> 33) % 14;
        for _ in 0..length {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            line.push_str(
                pieces[usize::try_from((seed >> 33) % pieces.len() as u64).expect("index")],
            );
        }
        let expected: Vec<String> = oracle
            .find_iter(&line)
            .map(|m| m.expect("match").as_str().to_string())
            .collect();
        assert_eq!(tokens(&line), expected, "{line:?}");
    }
}

#[test]
fn the_scanner_agrees_with_the_python_pattern_on_the_real_skill_text() {
    let oracle = fancy_regex::Regex::new(ORACLE).expect("the Python pattern compiles");
    let plugin = real_skill().join("../..");
    let mut files = Vec::new();
    for sub in ["skills", "agents"] {
        walk(
            &plugin.join(sub),
            &mut |path| {
                if path.extension().is_some_and(|e| e == "md") {
                    files.push(path.to_path_buf());
                }
            },
            &SKIPPED_DIRS,
        )
        .expect("walk the real tree");
    }
    assert!(files.len() > 500, "{}", files.len());
    let (mut compared, mut skipped) = (0, 0);
    for file in files {
        let text = universal_newlines(&fs::read_to_string(&file).expect("utf-8 skill file"));
        for line in py_splitlines(&text) {
            let stripped: String = strip_urls(&line.chars().collect::<Vec<_>>())
                .into_iter()
                .collect();
            // The Python pattern is quadratic: a very long line can exhaust the
            // oracle's backtrack budget, which says nothing about the scanner.
            let Ok(expected) = oracle
                .find_iter(&stripped)
                .map(|m| m.map(|m| m.as_str().to_string()))
                .collect::<Result<Vec<String>, _>>()
            else {
                skipped += 1;
                continue;
            };
            assert_eq!(tokens(&stripped), expected, "{}: {line:?}", file.display());
            compared += expected.len();
        }
    }
    assert!(compared > 500, "{compared}");
    assert!(skipped < 20, "{skipped} lines exhausted the oracle");
}

/// `_SKIP_PREFIX_RE`, verbatim.
const SKIP_ORACLE: &str = r"(\$\{?(MIGRATION_DIR|RUN_DIR|BEDROCK_RUN_DIR|REPO|PHASE_DIR|PLAN_DIR|OUT|TARGET|APP|WORKSPACE|TMP)\}?/|<(PLAN_DIR|MIGRATION_DIR|RUN_DIR|REPO|run_dir|migration_dir|path|repo)>/|\.migration/|\.saws-migrate/|terraform/|ai-migration/|poc/|scaffold/|\.github/)";
const ROOTS_ORACLE: &str =
    "(?:references|knowledge|scripts|skills|agents|data|phases|shared|design-refs|fixtures|tools)";

/// The Python `scan_file` loop run on one line, written the way the Python
/// wrote it (the context is re-derived and rescanned for every token, which is
/// the quadratic cost the Rust scanner removes). Returns the raw tokens kept.
struct PythonScan {
    token: fancy_regex::Regex,
    url: fancy_regex::Regex,
    before: fancy_regex::Regex,
    skip: fancy_regex::Regex,
    leading: fancy_regex::Regex,
    roots_start: fancy_regex::Regex,
    roots_inside: fancy_regex::Regex,
    relative: fancy_regex::Regex,
}

impl PythonScan {
    fn new() -> Self {
        let regex = |pattern: &str| fancy_regex::Regex::new(pattern).expect("a valid pattern");
        Self {
            token: regex(ORACLE),
            url: regex(r"https?://\S*"),
            before: regex(r"[^\s`]*$"),
            skip: regex(SKIP_ORACLE),
            leading: regex(r"^(?:<[A-Z_]+>/|\$\{?[A-Z_]+\}?/)+"),
            roots_start: regex(&format!("^{ROOTS_ORACLE}/")),
            roots_inside: regex(&format!("(^|/){ROOTS_ORACLE}/")),
            relative: regex(r"\A(?:\./|\.\./)+\z"),
        }
    }

    /// `None` when the oracle's own backtracking budget runs out.
    fn raws(&self, line: &str) -> Option<Vec<String>> {
        let text = self.url.replace_all(line, "").into_owned();
        let mut kept = Vec::new();
        for captures in self.token.captures_iter(text.as_str()) {
            let captures = captures.ok()?;
            let whole = captures.get(0).expect("the whole match");
            let prefix = captures.name("prefix").map_or("", |m| m.as_str());
            let path = captures.name("path").expect("the path group").as_str();
            let full = whole.as_str();
            let start = whole.start();
            let before = self.before.find(&text[..start]).ok()??.as_str().to_string();
            if self
                .skip
                .is_match(&format!("{before}{}", &full[..prefix.len()]))
                .ok()?
            {
                continue;
            }
            let body = self.leading.replace(full, "");
            if body.chars().any(|c| "<>{}*[]".contains(c))
                || body.contains("NNN")
                || body.contains("/…")
            {
                continue;
            }
            if prefix.is_empty() && !self.roots_start.is_match(path).ok()? {
                continue;
            }
            if !prefix.is_empty()
                && self.relative.is_match(prefix).ok()?
                && !self.roots_inside.is_match(path).ok()?
            {
                continue;
            }
            if text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || "_-/".contains(c))
            {
                continue;
            }
            kept.push(full.to_string());
        }
        Some(kept)
    }
}

/// `(line number, raw token)` of every reference `scan_file` reports.
fn scanned(plugin: &Path, relative: &str) -> Vec<(usize, String)> {
    scan_rel(plugin, relative)
        .into_iter()
        .map(|r| (r.line, r.raw))
        .collect()
}

#[test]
fn scan_file_agrees_with_the_python_loop_on_hard_lines() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let lines = [
        "write `$MIGRATION_DIR/` per `references/shared/schema.md`",
        "terraform/:references/phases/design.md:scripts/tool.py",
        "terraform/ references/phases/design.md",
        "`.migration/x`references/phases/design.md references/phases/design.md",
        "<path>/references/a.md <repo>/scripts/b.sh <other>/scripts/c.sh",
        "$REPO}/scripts/tool.py ${TMP}/scripts/tool.py $TMPX/scripts/tool.py",
        "$RUN_DIR/ ./references/a.md ../scripts/tool.py ./notes.md ../../plan.md",
        "x:references/a.md y.references/a.md z-scripts/tool.py w/skills/a/SKILL.md",
        "see https://e.com/references/a.md and references/phases/design.md",
        "<SKILL_BASE>/scripts/tool.py  $X/y.md  scripts/a*.md  scripts/NNN.md  scripts/…/x.md",
        "scripts/tool.py:scripts/tool.py:scripts/tool.py,agents/worker.md",
        "$SKILL_BASE/.github/references/a.md ai-migration/scripts/tool.py poc/ scripts/tool.py",
    ];
    let oracle = PythonScan::new();
    write(&plugin, "skills/alpha/HARD.md", &lines.join("\n"));
    let expected: Vec<(usize, String)> = lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| {
            oracle
                .raws(line)
                .expect("the oracle finishes")
                .into_iter()
                .map(move |raw| (index + 1, raw))
        })
        .collect();
    assert!(expected.len() > 8, "{expected:?}");
    assert_eq!(scanned(&plugin, "skills/alpha/HARD.md"), expected);
}

#[test]
fn scan_file_agrees_with_the_python_loop_on_generated_lines() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let oracle = PythonScan::new();
    let pieces = [
        "references/",
        "scripts/",
        "phases/",
        "a",
        "tool",
        ".md",
        ".py",
        "/",
        "./",
        "../",
        "$RUN_DIR/",
        "${OUT}/",
        "$REPO",
        "$X/",
        "}",
        "{",
        "<path>/",
        "<PLAN_DIR>/",
        "<X>/",
        ".migration/",
        "terraform/",
        "poc/",
        " ",
        "`",
        ":",
        "NNN",
        "*",
        "-",
        "_",
        "http://h/",
        "design",
        "references/phases/design.md",
        "scripts/tool.py",
        "agents/worker.md",
        " ",
        " ",
        ", ",
    ];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let (mut expected_total, mut kept_total) = (0, 0);
    for file in 0..8 {
        let mut lines = Vec::new();
        for _ in 0..1500 {
            let mut line = String::new();
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let length = 1 + (seed >> 33) % 16;
            for _ in 0..length {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                line.push_str(
                    pieces[usize::try_from((seed >> 33) % pieces.len() as u64).expect("index")],
                );
            }
            lines.push(line);
        }
        let relative = format!("skills/alpha/GEN{file}.md");
        write(&plugin, &relative, &lines.join("\n"));
        let expected: Vec<(usize, String)> = lines
            .iter()
            .enumerate()
            .flat_map(|(index, line)| {
                oracle
                    .raws(line)
                    .expect("the oracle finishes")
                    .into_iter()
                    .map(move |raw| (index + 1, raw))
            })
            .collect();
        expected_total += lines.iter().map(|line| tokens(line).len()).sum::<usize>();
        kept_total += expected.len();
        assert_eq!(scanned(&plugin, &relative), expected, "{relative}");
    }
    // Both outcomes are common, so neither filter is vacuous.
    assert!(kept_total > 800, "{kept_total}");
    assert!(
        expected_total > kept_total + 1000,
        "{expected_total} {kept_total}"
    );
}

#[test]
fn scan_file_agrees_with_the_python_loop_on_the_real_skill_text() {
    let oracle = PythonScan::new();
    let plugin = real_skill().join("../..");
    let mut files = Vec::new();
    for sub in ["skills", "agents"] {
        walk(
            &plugin.join(sub),
            &mut |path| {
                if path.extension().is_some_and(|e| e == "md") {
                    files.push(path.to_path_buf());
                }
            },
            &SKIPPED_DIRS,
        )
        .expect("walk the real tree");
    }
    files.sort();
    assert!(files.len() > 500, "{}", files.len());
    let mut tree = Tree::new(&plugin);
    let (mut compared, mut skipped) = (0, 0);
    for file in files {
        let relative = file
            .strip_prefix(&plugin)
            .expect("inside the plugin")
            .to_string_lossy()
            .into_owned();
        let found = tree.scan_file(&file, &relative).expect("the file scans");
        let text = universal_newlines(&fs::read_to_string(&file).expect("utf-8 skill file"));
        let mut expected = Vec::new();
        let mut skipped_lines = std::collections::BTreeSet::new();
        for (index, line) in py_splitlines(&text).into_iter().enumerate() {
            // Compare only the lines the oracle can finish: it is quadratic.
            if let Some(raws) = oracle.raws(line) {
                expected.extend(raws.into_iter().map(|raw| (index + 1, raw)));
            } else {
                skipped += 1;
                skipped_lines.insert(index + 1);
            }
        }
        let got: Vec<(usize, String)> = found
            .into_iter()
            .filter(|r| !skipped_lines.contains(&r.line))
            .map(|r| (r.line, r.raw))
            .collect();
        compared += expected.len();
        assert_eq!(got, expected, "{relative}");
    }
    assert!(compared > 500, "{compared}");
    assert!(skipped < 20, "{skipped} lines exhausted the oracle");
}

#[test]
fn a_long_line_of_repeated_tokens_costs_work_linear_in_its_length() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let tokens = 100_000;
    let kept = ":references/phases/design.md";
    // (the line, how many references it holds)
    let cases = [
        (":x.md".repeat(tokens), 0),
        (kept.repeat(tokens), tokens),
        // One skip prefix earlier in the same run hides every later token.
        (format!("terraform/{}", kept.repeat(tokens)), 0),
        (format!("{}$RUN_DIR/", kept.repeat(tokens)), tokens),
        (format!("<path>/{}", ":x.md".repeat(tokens)), 0),
        // A space ends the run: only the tokens after it count.
        (format!("terraform/ {}", kept.repeat(tokens)), tokens),
    ];
    for (index, (line, count)) in cases.iter().enumerate() {
        let relative = format!("skills/alpha/LONG{index}.md");
        write(&plugin, &relative, line);
        let mut tree = Tree::new(&plugin);
        let found = tree
            .scan_file(&plugin.join(&relative), &relative)
            .expect("the file scans");
        // Whatever the content, each character is stepped over a fixed number
        // of times: a pass to find tokens, two index tables, and the length of
        // each token it filters.
        let length = line.chars().count() as u64;
        assert!(tree.work > 0, "case {index}");
        assert!(
            tree.work <= 5 * length,
            "case {index}: {} steps for {length} characters",
            tree.work
        );
        assert_eq!(found.len(), *count, "case {index}");
        assert!(found.iter().all(|r| r.line == 1 && r.resolved.is_some()));
    }
}

#[test]
fn work_per_character_does_not_grow_with_the_line() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let token = ":references/phases/design.md";
    let mut per_character = Vec::new();
    for count in [1_000_usize, 4_000, 16_000] {
        let line = token.repeat(count);
        let relative = format!("skills/alpha/SCALE{count}.md");
        write(&plugin, &relative, &line);
        let mut tree = Tree::new(&plugin);
        tree.scan_file(&plugin.join(&relative), &relative)
            .expect("the file scans");
        per_character.push(tree.work * 1000 / line.chars().count() as u64);
    }
    // Steps per 1000 characters; a quadratic scan would multiply it by 4 at each step.
    assert!(
        per_character[2] * 100 <= per_character[0] * 105,
        "{per_character:?}"
    );
}

#[test]
fn urls_are_removed_up_to_the_next_space() {
    let chars: Vec<char> = "see https://e.com/a.md and http://x/b.md\tok xhttp://y z"
        .chars()
        .collect();
    let stripped: String = strip_urls(&chars).into_iter().collect();
    assert_eq!(stripped, "see  and \tok x z");
}

#[test]
fn very_long_lines_are_scanned_in_linear_time() {
    // The Python pattern is quadratic on these; a quadratic scan would not finish.
    let lines = [
        "a".repeat(400_000),
        "./".repeat(200_000),
        "../".repeat(150_000),
        format!("{}x.md", "a/".repeat(150_000)),
        "$A/".repeat(150_000),
        "<A>/".repeat(100_000),
        "a.md/".repeat(80_000),
        ".".repeat(400_000),
    ];
    for line in lines {
        let _ = tokens(&line);
    }
    assert_eq!(tokens(&format!("{}x.md", "a/".repeat(1000))).len(), 1);
}

// ---- output, links and hostile input ----------------------------------------------

fn command(plugin: &Path, extra: &[&str]) -> (u8, String, String) {
    let mut args = vec!["startup", "skill-refs", "--plugin-root", path_text(plugin)];
    args.extend_from_slice(extra);
    exec(&args)
}

#[test]
fn the_command_needs_a_mode_and_an_existing_plugin() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let (status, stdout, stderr) = command(&plugin, &[]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(
        stderr.starts_with("error: pass --check or --list\n"),
        "{stderr}"
    );
    let absent = plugin.join("absent");
    let (status, _, stderr) = command(&absent, &["--check"]);
    assert_eq!(status, 2);
    assert!(
        stderr.starts_with("error: Plugin directory not found: "),
        "{stderr}"
    );
    for args in [
        vec!["startup", "skill-refs", "--check", "--wat"],
        vec!["startup", "skill-refs", "--check", "extra"],
        vec!["startup", "skill-refs", "--baseline"],
        vec!["startup", "skill-refs", "--check", "--baseline", "--json"],
        vec!["startup", "skill-refs", "--check=1"],
    ] {
        assert_eq!(exec(&args).0, 2, "{args:?}");
    }
}

#[test]
fn list_prints_every_reference_and_where_it_resolved() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    write(
        &plugin,
        "skills/alpha/SKILL.md",
        "Load `references/phases/design.md`, then `scripts/gone.py`.\n",
    );
    let (status, stdout, stderr) = command(&plugin, &["--list"]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    let relative = stdout
        .lines()
        .find(|line| line.contains("skills/alpha/SKILL.md:1"))
        .expect("the alpha reference lines");
    assert!(
        relative.ends_with(
            "references/phases/design.md  ->  plugins/p/skills/alpha/references/phases/design.md"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("scripts/gone.py  ->  MISSING"), "{stdout}");
}

#[test]
fn text_output_lists_missing_stale_and_baselined_with_a_summary() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    fs::remove_file(plugin.join("agents/worker.md")).expect("rm");
    fs::remove_file(plugin.join("skills/alpha/references/vendored/dsl/INTERPRETER.md"))
        .expect("rm");
    write(
        &plugin,
        "skills/alpha/SKILL.md",
        "`references/phases/nope.md` `scripts/gone.py` `scripts/known.py`",
    );
    write(
        &plugin,
        "tools/skill-refs-baseline.json",
        r#"{"ignore":[{"source":"*","ref":"scripts/gone.py"}],
            "entries":[{"source":"*alpha/SKILL.md","ref":"scripts/known.py","reason":"burn-down"},
                       {"source":"*","ref":"never.md","reason":"fixed"}]}"#,
    );
    let (status, stdout, stderr) = command(&plugin, &["--check"]);
    assert_eq!(status, 1);
    assert_eq!(stderr, "error: 2 skill reference problem(s)\n");
    assert_eq!(
        stdout,
        "ERROR  MISSING_REF  plugins/p/skills/alpha/SKILL.md:1  `references/phases/nope.md`\n\
         ERROR  STALE_BASELINE  entry matched nothing — remove it: \
         {\"source\": \"*\", \"ref\": \"never.md\", \"reason\": \"fixed\"}\n\
         baselined (known dead references, explained in plugins/p/tools/skill-refs-baseline.json): 1\n\
         \nFAIL — 4 reference(s) checked, 1 missing, 1 baselined, 1 stale baseline entry\n"
    );
    // --no-baseline ignores the file: both unlisted tokens are missing.
    let (_, stdout, _) = command(&plugin, &["--check", "--no-baseline"]);
    assert!(stdout.contains("`scripts/gone.py`") && stdout.contains("`scripts/known.py`"));
    assert!(
        stdout.ends_with("0 baselined, 0 stale baseline entries\n"),
        "{stdout}"
    );
    // An explicit baseline that does not exist is ignored, like the Python tool.
    let (_, stdout, _) = command(
        &plugin,
        &[
            "--check",
            "--baseline",
            path_text(&plugin.join("none.json")),
        ],
    );
    assert!(stdout.contains("`scripts/known.py`"), "{stdout}");
    // The JSON report has the Python keys and order.
    let (_, stdout, _) = command(&plugin, &["--check", "--json"]);
    let order: Vec<usize> = [
        "\"references\"",
        "\"missing\"",
        "\"baselined\"",
        "\"stale_baseline_entries\"",
    ]
    .iter()
    .map(|key| stdout.find(key).expect("key"))
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{stdout}");
    assert!(stdout.contains("\"skill\": \"alpha\""), "{stdout}");
}

#[test]
fn a_malformed_baseline_file_fails_with_a_one_line_error() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    write(&plugin, "tools/skill-refs-baseline.json", "{\"ignore\": [");
    let (status, stdout, stderr) = command(&plugin, &["--check"]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(
        stderr.contains("skill-refs-baseline.json: invalid JSON ("),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    write(&plugin, "tools/skill-refs-baseline.json", "[]");
    let (status, _, stderr) = command(&plugin, &["--check"]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("the document must be a JSON object"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn links_are_refused_while_scanning_resolving_and_reading_the_baseline() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new("refs");
    let outside = tmp.dir("outside");
    write(&outside, "real.md", "`references/phases/design.md`");
    let plugin = plugin(&tmp);

    // A linked Markdown file in the scanned tree.
    symlink(
        outside.join("real.md"),
        plugin.join("skills/alpha/linked.md"),
    )
    .expect("link");
    let (status, stdout, stderr) = command(&plugin, &["--check", "--no-baseline"]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
    fs::remove_file(plugin.join("skills/alpha/linked.md")).expect("rm");

    // A linked directory.
    symlink(&outside, plugin.join("skills/alpha/dir")).expect("link");
    let (status, _, stderr) = command(&plugin, &["--check", "--no-baseline"]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
    fs::remove_file(plugin.join("skills/alpha/dir")).expect("rm");

    // A reference whose target is behind a link is not followed.
    write(&plugin, "skills/alpha/SKILL.md", "`references/linked/x.md`");
    write(&outside, "x.md", "x");
    symlink(&outside, plugin.join("skills/alpha/references/linked")).expect("link");
    let (status, _, stderr) = command(&plugin, &["--check", "--no-baseline"]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
    fs::remove_file(plugin.join("skills/alpha/references/linked")).expect("rm");

    // A linked default baseline.
    write(&outside, "baseline.json", "{}");
    fs::create_dir_all(plugin.join("tools")).expect("dir");
    symlink(
        outside.join("baseline.json"),
        plugin.join("tools/skill-refs-baseline.json"),
    )
    .expect("link");
    let (status, _, stderr) = command(&plugin, &["--check"]);
    assert_eq!(status, 1);
    assert!(
        stderr.contains("symbolic links are not followed"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn caller_derived_text_is_escaped_in_text_and_json_output() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    let hostile = "evil\u{1b}[31m\u{7}\nerror: forged\u{202e}\u{7f}";
    write(
        &plugin,
        &format!("skills/alpha/{hostile}.md"),
        "`references/phases/gone.md`",
    );
    for extra in [
        &["--check", "--no-baseline"][..],
        &["--check", "--no-baseline", "--json"],
        &["--list"],
    ] {
        let (_, stdout, stderr) = command(&plugin, extra);
        for stream in [&stdout, &stderr] {
            for raw in ['\u{1b}', '\u{7}', '\r', '\u{202e}', '\u{7f}'] {
                assert!(!stream.contains(raw), "{extra:?}: {raw:?} in {stream:?}");
            }
            assert!(
                !stream.lines().any(|line| line.starts_with("error: forged")),
                "{extra:?}: {stream:?}"
            );
        }
        assert!(
            stdout.contains("u001b") || stdout.contains("u001B"),
            "{extra:?}: {stdout}"
        );
    }
    // A stale baseline entry echoes its JSON with every control escaped.
    write(
        &plugin,
        "tools/skill-refs-baseline.json",
        "{\"entries\":[{\"source\":\"x\\u001b\\u007f\\u202e\",\"ref\":\"never.md\"}]}",
    );
    let (_, stdout, _) = command(&plugin, &["--check"]);
    for raw in ['\u{1b}', '\u{202e}', '\u{7f}'] {
        assert!(!stdout.contains(raw), "{raw:?} in {stdout:?}");
    }
    assert!(stdout.contains("STALE_BASELINE"), "{stdout}");
}

#[test]
fn skipped_directories_and_unreadable_files_are_not_scanned() {
    let tmp = TempDir::new("refs");
    let plugin = plugin(&tmp);
    write(
        &plugin,
        "skills/alpha/node_modules/pkg/README.md",
        "`references/gone.md`",
    );
    write(
        &plugin,
        "skills/alpha/fixtures/case.md",
        "`references/gone.md`",
    );
    write(
        &plugin,
        "skills/alpha/.git/notes.md",
        "`references/gone.md`",
    );
    fs::write(plugin.join("skills/alpha/binary.md"), [0xff, 0xfe, b'`']).expect("write");
    write(&plugin, "skills/alpha/notes.txt", "`references/gone.md`");
    let found = Tree::new(&plugin).scan().expect("scan");
    assert!(
        found.iter().all(|r| !r.raw.contains("gone.md")),
        "{found:?}"
    );
    // The same text in a scanned Markdown file is found.
    write(&plugin, "skills/alpha/real.md", "`references/gone.md`");
    let found = Tree::new(&plugin).scan().expect("scan");
    assert_eq!(
        found.iter().filter(|r| r.raw.contains("gone.md")).count(),
        1
    );
}

#[test]
fn references_report_their_skill_from_the_path_inside_the_plugin() {
    let make = |source: &str| Reference {
        source: source.to_string(),
        line: 1,
        raw: String::new(),
        resolved: None,
    };
    assert_eq!(make("skills/alpha/a/b.md").skill(), "alpha");
    assert_eq!(make("agents/worker.md").skill(), "agents");
    assert_eq!(make("other/file.md").skill(), "?");
}
