//! Real-process tests of `toolkit ai-payloads templates` (slice S1 of #4386,
//! Issue #5143): the template grammar of `docs/migration/ai-payloads-design.md`
//! section 4.3, the placeholder lexer and classification, the catalog of the
//! 23 real `code_templates` files, and the command contract.
//!
//! Test IDs (design section 5): AP-TPL-*, AP-SUB-01..07, AP-CAT-01..04 and
//! AP-CLI-01/02 are the S1 list; the rest are named here as extensions of
//! the same requirement (AP-TPL-08..12 are the grammar rules 3, 5, 6 and 7 of
//! section 4.3, AP-CAT-06/07 the `targets` and `manual_edits` members of
//! section 4.4.3, AP-CLI-03 the exit codes and the filesystem refusals).
//!
//! Every template under test is either a synthetic string written to a
//! temporary skill tree or one of the repository's real templates, which are
//! only read. Nothing is executed, and there is no network or AWS access.
//! The tests go through the real binary, so they pin the command's JSON and
//! text contract rather than a private API.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{is_unsafe_char, parse_json, Json};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The skill directory of the repository: the default `--root` of the command
/// when it runs from the repository root, and the canonical template tree.
fn canonical_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/core-skills/aws-ai-ml")
}

/// The `plugins/` mirror of the canonical tree (maintained by `toolkit sync`).
fn mirror_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-core/skills/aws-ai-ml")
}

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-ai-payloads-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch");
        Self(path)
    }

    /// The skill root of the scratch tree: `references/` lives below it.
    fn root(&self) -> PathBuf {
        self.0.join("skill")
    }

    /// Writes `references/<relative>` below the skill root.
    fn template(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root().join("references").join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        fs::write(&path, text).expect("write template");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_in(cwd: &Path, args: &[&str]) -> Run {
    let output = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .stdin(Stdio::null())
        .output()
        .expect("spawn toolkit");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// `toolkit ai-payloads templates --root ROOT` plus extra arguments.
fn templates_at(root: &Path, extra: &[&str]) -> Run {
    let root = root.to_str().expect("utf-8 root");
    let mut args = vec!["ai-payloads", "templates", "--root", root];
    args.extend_from_slice(extra);
    run_in(&std::env::temp_dir(), &args)
}

fn json_of(run: &Run) -> Json {
    assert_eq!(run.stderr, "", "stderr of a JSON run");
    parse_json(&run.stdout).unwrap_or_else(|error| panic!("stdout is not JSON: {error:?}"))
}

// ---------------------------------------------------------------- JSON access

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing member {key:?} in {value:?}"))
}

fn items(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        other => panic!("not an array: {other:?}"),
    }
}

fn string(value: &Json) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value:?}"))
}

fn int(value: &Json) -> i64 {
    match value {
        Json::Int(number) => *number,
        other => panic!("not an integer: {other:?}"),
    }
}

fn flag(value: &Json) -> bool {
    match value {
        Json::Bool(flag) => *flag,
        other => panic!("not a bool: {other:?}"),
    }
}

/// The catalog entry of a lone synthetic template, from `templates --json`.
/// `--json` alone never fails on a lint error: the violations are members.
fn catalog_of(text: &str) -> Json {
    let scratch = Scratch::new("one");
    scratch.template("demo/code_templates/t.py", text);
    let run = templates_at(&scratch.root(), &["--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let catalog = json_of(&run);
    let all = items(field(&catalog, "templates"));
    assert_eq!(all.len(), 1, "one template expected: {all:?}");
    all[0].clone()
}

/// `(number, label, markdown, notebook_only_section)` per cell.
fn cells_of(template: &Json) -> Vec<(i64, String, bool, bool)> {
    items(field(template, "cells"))
        .iter()
        .map(|cell| {
            (
                int(field(cell, "number")),
                string(field(cell, "label")).to_string(),
                flag(field(cell, "markdown")),
                flag(field(cell, "notebook_only_section")),
            )
        })
        .collect()
}

/// `(code, line, column)` per violation, in report order.
fn violations_of(template: &Json) -> Vec<(String, i64, i64)> {
    items(field(template, "violations"))
        .iter()
        .map(|violation| {
            (
                string(field(violation, "code")).to_string(),
                int(field(violation, "line")),
                int(field(violation, "column")),
            )
        })
        .collect()
}

fn codes_of(template: &Json) -> Vec<String> {
    violations_of(template)
        .into_iter()
        .map(|(code, _, _)| code)
        .collect()
}

/// The placeholder summary named `name`: `(kind, code, comment_only, sentinel)`.
fn slot_of(template: &Json, name: &str) -> (String, i64, bool, i64) {
    let slot = items(field(template, "placeholders"))
        .iter()
        .find(|slot| string(field(slot, "name")) == name)
        .unwrap_or_else(|| panic!("no placeholder {name:?} in {template:?}"));
    (
        string(field(slot, "kind")).to_string(),
        int(field(slot, "code")),
        flag(field(slot, "comment_only")),
        int(field(slot, "sentinel")),
    )
}

fn slot_names(template: &Json) -> Vec<String> {
    items(field(template, "placeholders"))
        .iter()
        .map(|slot| string(field(slot, "name")).to_string())
        .collect()
}

/// `(context, line, column)` of every occurrence of `name`, in file order.
fn occurrences_of(template: &Json, name: &str) -> Vec<(String, i64, i64)> {
    items(field(template, "occurrences"))
        .iter()
        .filter(|occurrence| string(field(occurrence, "name")) == name)
        .map(|occurrence| {
            (
                string(field(occurrence, "context")).to_string(),
                int(field(occurrence, "line")),
                int(field(occurrence, "column")),
            )
        })
        .collect()
}

fn contexts(rows: &[(String, i64, i64)]) -> Vec<&str> {
    rows.iter()
        .map(|(context, _, _)| context.as_str())
        .collect()
}

/// A synthetic template: Cell 0 and Cell 1 (four lines), then `body`, so the
/// first body line is line 5.
fn demo(body: &str) -> String {
    format!("# Cell 0 [markdown]: Demo\n\n# Cell 1: Config\n\n{body}")
}

/// The 1-based line of the first line of `text` that contains `needle`.
fn line_of(text: &str, needle: &str) -> i64 {
    let index = text
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in the text"));
    i64::try_from(index + 1).expect("line number")
}

/// The 1-based character column of `needle` in the first line that has it.
fn column_of(text: &str, needle: &str) -> i64 {
    let line = text
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in the text"));
    let byte = line.find(needle).expect("found");
    i64::try_from(line[..byte].chars().count() + 1).expect("column")
}

// ================================================================ AP-TPL: grammar

/// AP-TPL-01: markers number the cells consecutively from 0; the title is the
/// Cell 0 label; text before the first marker belongs to no cell.
#[test]
fn ap_tpl_01_consecutive_markers_become_cells_and_cell_zero_is_the_title() {
    let text = "# Preamble that names no cell\n\n\
# Cell 0 [markdown]: Fine-Tuning\n\n\
# Cell 1: Install Dependencies\n\nx = 1\n\n\
# Cell 2: Setup & Credentials\n\ny = 2\n\n\
# Cell 3: Done\n";
    let template = catalog_of(text);
    assert_eq!(
        cells_of(&template),
        vec![
            (0, "Fine-Tuning".to_string(), true, false),
            (1, "Install Dependencies".to_string(), false, false),
            (2, "Setup & Credentials".to_string(), false, false),
            (3, "Done".to_string(), false, false),
        ]
    );
    assert_eq!(string(field(&template, "title")), "Fine-Tuning");
    assert_eq!(string(field(&template, "path")), "demo/code_templates/t.py");
    assert_eq!(violations_of(&template), vec![]);
    let markers: Vec<i64> = items(field(&template, "cells"))
        .iter()
        .map(|cell| int(field(cell, "line")))
        .collect();
    assert_eq!(markers, vec![3, 5, 9, 13]);
}

/// AP-TPL-01 (section content, grammar rule 2): leading and trailing blank
/// lines are trimmed, inner lines are kept; Cell 0's text is its comment lines
/// without the leading `# `.
#[test]
fn ap_tpl_01_section_content_is_trimmed_and_cell_zero_text_drops_the_comment_prefix() {
    let text = "# Cell 0 [markdown]: Demo\n# Description line one\n#\n# line three\n\n\
# Cell 1: Config\n\n\na = 1\n\nb = 2\n\n\n\
# Cell 2: Next\nx = 1\n";
    let template = catalog_of(text);
    let rows: Vec<(i64, i64)> = items(field(&template, "cells"))
        .iter()
        .map(|cell| (int(field(cell, "number")), int(field(cell, "lines"))))
        .collect();
    assert_eq!(rows, vec![(0, 3), (1, 3), (2, 1)]);
    let zero = &items(field(&template, "cells"))[0];
    assert_eq!(
        string(field(zero, "text")),
        "Description line one\n\nline three"
    );
    // Only markdown cells carry their text in the catalog.
    assert!(items(field(&template, "cells"))[1].get("text").is_none());
}

/// AP-TPL-02: a gap, a duplicate, a missing Cell 0 and a template with no
/// marker at all are lint errors.
#[test]
fn ap_tpl_02_a_gap_a_duplicate_or_a_missing_cell_zero_is_an_error() {
    let gap = catalog_of("# Cell 0 [markdown]: A\n# Cell 1: B\n# Cell 3: C\n");
    assert_eq!(violations_of(&gap), vec![("cell_gap".to_string(), 3, 0)]);

    let duplicate = catalog_of("# Cell 0 [markdown]: A\n# Cell 1: B\n# Cell 1: C\n");
    assert_eq!(
        violations_of(&duplicate),
        vec![("cell_duplicate".to_string(), 3, 0)]
    );

    let backwards = catalog_of("# Cell 0 [markdown]: A\n# Cell 1: B\n# Cell 2: C\n# Cell 1: D\n");
    assert_eq!(
        violations_of(&backwards),
        vec![("cell_duplicate".to_string(), 4, 0)]
    );

    let no_zero = catalog_of("# Cell 1: B\n# Cell 2: C\n");
    assert_eq!(
        violations_of(&no_zero),
        vec![("cell_zero_missing".to_string(), 1, 0)]
    );

    let none = catalog_of("import os\n\nx = 1\n");
    assert_eq!(violations_of(&none), vec![("no_cells".to_string(), 0, 0)]);
    assert_eq!(cells_of(&none), vec![]);
    assert_eq!(string(field(&none, "title")), "");
}

/// AP-TPL-03: `[markdown]` is only valid on Cell 0, and Cell 0 must have it.
#[test]
fn ap_tpl_03_markdown_on_a_later_cell_or_missing_on_cell_zero_is_an_error() {
    let late = catalog_of("# Cell 0 [markdown]: A\n# Cell 1 [markdown]: B\n# Cell 2: C\n");
    assert_eq!(
        violations_of(&late),
        vec![("cell_markdown_late".to_string(), 2, 0)]
    );
    assert_eq!(
        cells_of(&late)[1],
        (1, "B".to_string(), true, false),
        "the cell is still reported"
    );

    let plain_zero = catalog_of("# Cell 0: A\n# Cell 1: B\n");
    assert_eq!(
        violations_of(&plain_zero),
        vec![("cell_zero_not_markdown".to_string(), 1, 0)]
    );
}

/// AP-TPL-04: `# Cell 2 is intentionally SDK-free ...` (T11:101) is ordinary
/// text, and so is a mid-line or indented `# Cell N:`.
#[test]
fn ap_tpl_04_a_cell_comment_without_a_colon_after_the_number_is_text() {
    let text = "# Cell 0 [markdown]: Demo\n\n# Cell 1: Config\n\n\
# Cell 2 is intentionally SDK-free (stdlib only) so it runs without the SDK.\n\
# Cell structure for a notebook.\n\
x = 1  # Cell 9: not a marker either\n\
\x20\x20\x20\x20# Cell 7: indented, not a marker\n\
#Cell 8: no space after the hash\n\n\
# Cell 2: Real\n";
    let template = catalog_of(text);
    assert_eq!(violations_of(&template), vec![]);
    let numbers: Vec<i64> = cells_of(&template).iter().map(|row| row.0).collect();
    assert_eq!(numbers, vec![0, 1, 2]);
    let cell_one = &items(field(&template, "cells"))[1];
    assert_eq!(int(field(cell_one, "lines")), 5, "the text stays in Cell 1");
}

/// AP-TPL-05: `# NOTEBOOK_ONLY_SECTION` after the label is a flag, not label
/// text; other text with a hash in a label stays.
#[test]
fn ap_tpl_05_the_section_suffix_is_a_flag_and_not_part_of_the_label() {
    let text = "# Cell 0 [markdown]: Demo\n\
# Cell 1: Plot and Display Metrics  # NOTEBOOK_ONLY_SECTION\n\
# Cell 2: Plot  #   NOTEBOOK_ONLY_SECTION   \n\
# Cell 3: Learn C# basics\n\
# Cell 4: Setup\n";
    let template = catalog_of(text);
    assert_eq!(violations_of(&template), vec![]);
    assert_eq!(
        cells_of(&template)[1..],
        [
            (1, "Plot and Display Metrics".to_string(), false, true),
            (2, "Plot".to_string(), false, true),
            (3, "Learn C# basics".to_string(), false, false),
            (4, "Setup".to_string(), false, false),
        ]
    );
}

/// AP-TPL-06: an empty label, and a colon not followed by a space and a label,
/// are lint errors.
#[test]
fn ap_tpl_06_an_empty_label_is_an_error() {
    for (name, marker) in [
        ("blank after colon", "# Cell 1: \n"),
        ("colon at end of line", "# Cell 1:\n"),
        ("only spaces", "# Cell 1:    \n"),
        ("no space after the colon", "# Cell 1:Setup\n"),
        ("markdown with no label", "# Cell 1 [markdown]: \n"),
    ] {
        let template = catalog_of(&format!("# Cell 0 [markdown]: Demo\n{marker}x = 1\n"));
        let codes = codes_of(&template);
        assert!(
            codes.contains(&"cell_label_empty".to_string()),
            "{name}: {codes:?}"
        );
        let first = violations_of(&template)
            .into_iter()
            .find(|(code, _, _)| code == "cell_label_empty")
            .expect("violation");
        assert_eq!(first.1, 2, "{name}: reported on the marker line");
    }
}

/// AP-TPL-07: CR LF line endings and a missing final newline give the same
/// catalog as LF; a lone CR is not a line ending (AP-TPL-10).
#[test]
fn ap_tpl_07_crlf_and_a_missing_final_newline_do_not_change_the_catalog() {
    let lf = "# Cell 0 [markdown]: Demo\n# About [ROLE_ARN]\n\n# Cell 1: Config\n\n\
ROLE = \"[ROLE_ARN]\"\nN = [CONCURRENCY]\n\n# Cell 2: Run\nprint(ROLE)\n";
    let crlf = lf.replace('\n', "\r\n");
    let no_final = lf.trim_end_matches('\n').to_string();
    let crlf_no_final = crlf.trim_end_matches("\r\n").to_string();
    let reference = catalog_of(lf);
    assert_eq!(violations_of(&reference), vec![]);
    for (name, variant) in [
        ("crlf", &crlf),
        ("no final newline", &no_final),
        ("crlf without a final newline", &crlf_no_final),
    ] {
        let template = catalog_of(variant);
        assert_eq!(
            template.to_compact_string(),
            reference.to_compact_string(),
            "{name}"
        );
    }
}

/// AP-TPL-08 (rule 3): a line ending in `# NOTEBOOK_ONLY` (trailing
/// whitespace aside) is counted per cell; the `_SECTION` suffix and an inner
/// mention are not.
#[test]
fn ap_tpl_08_notebook_only_lines_are_counted_by_their_suffix() {
    let text = demo(
        "# %pip install a  # NOTEBOOK_ONLY\n\
%pip install b # NOTEBOOK_ONLY   \n\
x = 1  # NOTEBOOK_ONLY_SECTION\n\
# NOTEBOOK_ONLY is explained here\n\
y = 2\n",
    );
    let template = catalog_of(&text);
    let cell_one = &items(field(&template, "cells"))[1];
    assert_eq!(int(field(cell_one, "notebook_only_lines")), 2);
    let cell_zero = &items(field(&template, "cells"))[0];
    assert_eq!(int(field(cell_zero, "notebook_only_lines")), 0);
}

/// AP-TPL-09 (rule 5): a name with a `_`-delimited segment TOKEN, SECRET,
/// PASSWORD, CREDENTIAL or CREDENTIALS is a secret; `INPUT_TOKENS_MEAN` is not.
#[test]
fn ap_tpl_09_secret_names_are_matched_by_segment() {
    let secrets = [
        "HF_TOKEN_OR_NONE",
        "TOKEN",
        "API_TOKEN",
        "MY_SECRET_KEY",
        "DB_PASSWORD",
        "CREDENTIALS",
        "AWS_CREDENTIAL",
        "TOKEN_1",
    ];
    let plain = [
        "INPUT_TOKENS_MEAN",
        "TOKENS",
        "NOTOKEN",
        "SECRETS",
        "PASSWORDS",
        "TOKENIZER",
        "CREDENTIALED",
    ];
    // INPUT_TOKENS_MEAN is a typed bare slot elsewhere; a quoted slot is fine
    // for the kind check.
    let mut body = String::new();
    for name in secrets.iter().chain(plain.iter()) {
        writeln!(body, "V_{name} = \"[{name}]\"").expect("write to a String");
    }
    let template = catalog_of(&demo(&body));
    assert_eq!(violations_of(&template), vec![]);
    for name in secrets {
        assert_eq!(slot_of(&template, name).0, "secret", "{name}");
    }
    for name in plain {
        assert_eq!(slot_of(&template, name).0, "string", "{name}");
    }
}

/// AP-TPL-10 (rule 7): the source-text policy. Tab and CR LF are fine; any
/// other control, bidi or format character is a lint error with its line and
/// column and the code point, and the diagnostic never echoes the line.
#[test]
fn ap_tpl_10_unsafe_source_characters_are_reported_by_position_without_the_line() {
    let marker = "ECHOED_LINE_MARKER";
    let cases: [(&str, char, &str); 8] = [
        ("esc", '\u{1b}', "U+001B"),
        ("nul", '\u{0}', "U+0000"),
        ("del", '\u{7f}', "U+007F"),
        ("nel", '\u{85}', "U+0085"),
        ("line separator", '\u{2028}', "U+2028"),
        ("rtl override", '\u{202e}', "U+202E"),
        ("isolate", '\u{2066}', "U+2066"),
        ("zero width space", '\u{200b}', "U+200B"),
    ];
    for (name, character, code_point) in cases {
        let text = demo(&format!("x = 1 {character}{marker}\n"));
        let scratch = Scratch::new("unsafe");
        scratch.template("demo/code_templates/t.py", &text);
        let run = templates_at(&scratch.root(), &["--json"]);
        assert_eq!(run.code, Some(0), "{name}: {}", run.stderr);
        let catalog = json_of(&run);
        let template = &items(field(&catalog, "templates"))[0];
        assert_eq!(
            violations_of(template),
            vec![("source_char".to_string(), 5, 7)],
            "{name}"
        );
        let message = string(field(&items(field(template, "violations"))[0], "message"));
        assert!(message.contains(code_point), "{name}: {message}");
        assert!(!run.stdout.contains(marker), "{name}: the line was echoed");
        assert!(!run.stdout.contains(character), "{name}: raw character");

        // --check reports the same position on stdout and exits 1.
        let check = templates_at(&scratch.root(), &["--check"]);
        assert_eq!(check.code, Some(1), "{name}");
        assert!(check.stdout.contains("source_char"), "{name}");
        assert!(!check.stdout.contains(marker) && !check.stderr.contains(marker));
        assert!(!check.stdout.contains(character) && !check.stderr.contains(character));
    }
    // A BOM at the start of the file, even there.
    let bom = catalog_of(&format!("\u{feff}{}", demo("x = 1\n")));
    assert_eq!(violations_of(&bom), vec![("source_char".to_string(), 1, 1)]);
    // A lone CR is not a line ending.
    let lone = catalog_of(&demo("x = 1\ry = 2\n"));
    assert_eq!(
        violations_of(&lone),
        vec![("source_char".to_string(), 5, 6)]
    );
    // Tabs and CR LF are accepted.
    let ok = catalog_of(&demo("if x:\r\n\ty = 1\r\n"));
    assert_eq!(violations_of(&ok), vec![]);
}

/// AP-TPL-11 (rule 6): a bare slot needs an entry in the type table; the nine
/// names of the 23 real templates have one. A later template edit cannot add
/// an untyped bare slot.
#[test]
fn ap_tpl_11_an_untyped_bare_slot_is_a_lint_error() {
    let typed = [
        "ACCEPT_EULA",
        "TRUE_OR_FALSE",
        "INPUT_TOKENS_MEAN",
        "INPUT_TOKENS_STDDEV",
        "OUTPUT_TOKENS_MEAN",
        "OUTPUT_TOKENS_STDDEV",
        "CONCURRENCY",
        "REQUEST_COUNT",
        "METRICS_LIST",
    ];
    let mut body = String::new();
    for name in typed {
        writeln!(body, "v_{} = [{name}]", name.to_lowercase()).expect("write to a String");
    }
    assert_eq!(violations_of(&catalog_of(&demo(&body))), vec![]);

    let text = demo("x = [BRAND_NEW_SLOT]\ny = [ROLE_ARN]\n");
    let template = catalog_of(&text);
    assert_eq!(
        violations_of(&template),
        vec![
            (
                "untyped_bare_slot".to_string(),
                5,
                column_of(&text, "[BRAND_NEW_SLOT]")
            ),
            (
                "untyped_bare_slot".to_string(),
                6,
                column_of(&text, "[ROLE_ARN]")
            ),
        ]
    );
    // A comment mention is not a bare slot.
    let comment = catalog_of(&demo("# **[EXTRA_PARAMS]  # e.g. {}\n"));
    assert_eq!(violations_of(&comment), vec![]);
}

/// AP-TPL-12: one name cannot be both a quoted slot and a bare slot in a
/// template (it would have two types), and an unterminated string is reported
/// without hiding the placeholders after it.
#[test]
fn ap_tpl_12_mixed_kinds_and_unterminated_strings_are_lint_errors() {
    let mixed = catalog_of(&demo("A = \"[CONCURRENCY]\"\nB = [CONCURRENCY]\n"));
    assert_eq!(
        violations_of(&mixed),
        vec![("mixed_slot_kinds".to_string(), 6, 5)]
    );

    let text = demo("X = \"never closed\nY = \"[ROLE_ARN]\"\n");
    let template = catalog_of(&text);
    assert_eq!(
        violations_of(&template),
        vec![("unterminated_string".to_string(), 5, 5)]
    );
    assert_eq!(
        slot_of(&template, "ROLE_ARN"),
        ("string".to_string(), 1, false, 0)
    );

    let triple = catalog_of(&demo("D = \"\"\"never closed\n"));
    assert_eq!(
        violations_of(&triple),
        vec![("unterminated_string".to_string(), 5, 5)]
    );
}

// ============================================ AP-SUB: lexer and classification

/// AP-SUB-01: a string literal whose whole body is the placeholder is a quoted
/// slot, with either quote character; the position is the opening quote.
#[test]
fn ap_sub_01_a_quoted_slot_takes_either_quote_character() {
    let text = demo("A = \"[ROLE_ARN]\"\nB = '[ROLE_ARN]'\nC = {\"k\": \"[ROLE_ARN]\"}\n");
    let template = catalog_of(&text);
    assert_eq!(violations_of(&template), vec![]);
    assert_eq!(
        slot_of(&template, "ROLE_ARN"),
        ("string".to_string(), 3, false, 0)
    );
    let rows = occurrences_of(&template, "ROLE_ARN");
    assert_eq!(contexts(&rows), vec!["quoted", "quoted", "quoted"]);
    assert_eq!(
        rows.iter().map(|row| (row.1, row.2)).collect::<Vec<_>>(),
        vec![(5, 5), (6, 5), (7, column_of(&text, "\"[ROLE_ARN]\"}"))]
    );
    let quotes: Vec<&str> = items(field(&template, "occurrences"))
        .iter()
        .map(|occurrence| string(field(occurrence, "quote")))
        .collect();
    assert_eq!(quotes, vec!["\"", "'", "\""]);
    // A placeholder name may hold spaces and digits.
    let spaced = catalog_of(&demo(
        "NAME = \"[GENERATE A NAME FOR THE EVALUATOR HERE]\"\nT = \"[P2_X 1]\"\n",
    ));
    assert_eq!(
        slot_names(&spaced),
        vec!["GENERATE A NAME FOR THE EVALUATOR HERE", "P2_X 1"]
    );
}

/// AP-SUB-02: a placeholder as an expression token in code is a bare slot; an
/// index such as `CONFIG[KEY]` is not.
#[test]
fn ap_sub_02_a_bare_slot_is_an_expression_token_and_an_index_is_not() {
    let text = demo(
        "X = [CONCURRENCY]\n\
f(a=[REQUEST_COUNT], b=[INPUT_TOKENS_MEAN])\n\
return_value = (\n    [METRICS_LIST]\n)\n",
    );
    let template = catalog_of(&text);
    assert_eq!(violations_of(&template), vec![]);
    for name in [
        "CONCURRENCY",
        "REQUEST_COUNT",
        "INPUT_TOKENS_MEAN",
        "METRICS_LIST",
    ] {
        assert_eq!(
            slot_of(&template, name),
            ("value".to_string(), 1, false, 0),
            "{name}"
        );
    }
    let rows = occurrences_of(&template, "REQUEST_COUNT");
    assert_eq!(
        rows,
        vec![("bare".to_string(), 6, column_of(&text, "[REQUEST_COUNT]"))]
    );
    assert_eq!(
        occurrences_of(&template, "METRICS_LIST"),
        vec![("bare".to_string(), 8, 5)]
    );

    // Indexing is not a placeholder: a name, a call or an index before the `[`.
    let indexed = catalog_of(&demo(
        "v = CONFIG[KEY]\nw = ROWS[0][IDX]\nz = get()[TOP]\nq = \"s\"[END]\n",
    ));
    assert_eq!(slot_names(&indexed), Vec::<String>::new());
    assert_eq!(violations_of(&indexed), vec![]);
    // `[A, B]` and `[a]` are list displays of other things, not placeholders.
    let lists = catalog_of(&demo("a = [ONE, TWO]\nb = [lower]\nc = [A.B]\nd = []\n"));
    assert_eq!(slot_names(&lists), Vec::<String>::new());
    // A keyword before the bracket does not make it an index.
    let keyword = catalog_of(&demo("def f():\n    return [ACCEPT_EULA]\n"));
    assert_eq!(slot_of(&keyword, "ACCEPT_EULA").0, "value");
}

/// AP-SUB-03: a placeholder inside a `#` comment is a comment occurrence and
/// never a code slot, even when it sits in quotes inside the comment.
#[test]
fn ap_sub_03_placeholders_in_comments_are_comment_occurrences() {
    let text = demo(
        "# set [REGION] before running\n\
X = 1  # also \"[REGION]\" and '[ROLE_ARN]' here\n\
#   workload = Workload([CONCURRENCY])\n",
    );
    let template = catalog_of(&text);
    assert_eq!(violations_of(&template), vec![]);
    assert_eq!(
        slot_of(&template, "REGION"),
        ("string".to_string(), 0, true, 0)
    );
    assert_eq!(
        slot_of(&template, "ROLE_ARN"),
        ("string".to_string(), 0, true, 0)
    );
    assert_eq!(
        slot_of(&template, "CONCURRENCY"),
        ("string".to_string(), 0, true, 0)
    );
    let rows = occurrences_of(&template, "REGION");
    assert_eq!(contexts(&rows), vec!["comment", "comment"]);
    assert_eq!(rows[0].1, 5);
    assert_eq!(rows[0].2, column_of(&text, "[REGION] before"));
    // A comment-only name is not an untyped bare slot (rule 6 looks at code).
    assert_eq!(codes_of(&template), Vec::<String>::new());
    // The marker lines are comments too.
    let marker = catalog_of("# Cell 0 [markdown]: About [ROLE_ARN]\n# Cell 1: Setup\n");
    assert_eq!(
        contexts(&occurrences_of(&marker, "ROLE_ARN")),
        vec!["comment"]
    );
}

/// AP-SUB-04: a quoted occurrence that is an operand of `==` or `!=`, or an
/// element of the bracketed right operand of `in` or `not in`, is a sentinel
/// (T17:39): never substituted, counted apart.
#[test]
fn ap_sub_04_sentinel_comparisons_are_classified_apart() {
    let text = demo(
        "if tok in (\"\", \"None\", \"[HF_TOKEN_OR_NONE]\"):\n    pass\n\
if tok == \"[HF_TOKEN_OR_NONE]\":\n    pass\n\
if \"[HF_TOKEN_OR_NONE]\" != tok:\n    pass\n\
if tok not in [\"[HF_TOKEN_OR_NONE]\"]:\n    pass\n\
HF_HUB_TOKEN = \"[HF_TOKEN_OR_NONE]\"\n",
    );
    let template = catalog_of(&text);
    assert_eq!(violations_of(&template), vec![]);
    assert_eq!(
        slot_of(&template, "HF_TOKEN_OR_NONE"),
        ("secret".to_string(), 1, false, 4)
    );
    let rows = occurrences_of(&template, "HF_TOKEN_OR_NONE");
    assert_eq!(
        contexts(&rows),
        vec!["sentinel", "sentinel", "sentinel", "sentinel", "quoted"]
    );
    assert_eq!(
        rows.iter().map(|row| row.1).collect::<Vec<_>>(),
        vec![5, 7, 9, 11, 13]
    );
    assert_eq!(rows[0].2, column_of(&text, "\"[HF_TOKEN_OR_NONE]\"):"));
}

/// AP-SUB-04 (negative shapes): these are quoted slots, not sentinels.
#[test]
fn ap_sub_04_other_comparison_shapes_stay_quoted_slots() {
    let text = demo(
        "A = \"[X_ONE]\"\n\
if tok in \"[X_TWO]\":\n    pass\n\
if tok in foo(\"[X_THREE]\"):\n    pass\n\
bar(a == 1, \"[X_FOUR]\")\n\
y = (\"[X_FIVE]\", tok)\n\
if tok == foo(\"[X_SIX]\"):\n    pass\n\
if \"[X_SEVEN]\" in tok:\n    pass\n\
if tok <= \"[X_EIGHT]\":\n    pass\n",
    );
    let template = catalog_of(&text);
    assert_eq!(violations_of(&template), vec![]);
    for name in [
        "X_ONE", "X_TWO", "X_THREE", "X_FOUR", "X_FIVE", "X_SIX", "X_SEVEN", "X_EIGHT",
    ] {
        assert_eq!(
            slot_of(&template, name),
            ("string".to_string(), 1, false, 0),
            "{name}"
        );
    }
}

#[test]
fn ap_sub_04_grouped_operands_and_expression_components() {
    let template = catalog_of(&demo(concat!(
        "if tok == ((\"[GROUPED_RIGHT]\")):\n    pass\n",
        "if ((\"[GROUPED_LEFT]\")) != tok:\n    pass\n",
        "if tok in (\"[ADDED_MEMBER]\" + suffix,):\n    pass\n",
        "if tok == \"[ADDED_EQUALITY]\" + suffix:\n    pass\n",
        "if tok in ((\"[GROUPED_MEMBER]\"),):\n    pass\n",
    )));
    for name in ["GROUPED_RIGHT", "GROUPED_LEFT", "GROUPED_MEMBER"] {
        assert_eq!(
            contexts(&occurrences_of(&template, name)),
            vec!["sentinel"],
            "{name}"
        );
    }
    for name in ["ADDED_MEMBER", "ADDED_EQUALITY"] {
        assert_eq!(
            contexts(&occurrences_of(&template, name)),
            vec!["quoted"],
            "{name}"
        );
    }
}

#[test]
fn ap_sub_04_sentinel_expression_boundaries() {
    let template = catalog_of(&demo(concat!(
        "ok = \"[ASSIGN_LEFT]\" == tok\nright = tok == \"[ASSIGN_RIGHT]\"\n",
        "def f():\n    return \"[RETURN_LEFT]\" == tok\n    return tok == \"[RETURN_RIGHT]\"\n",
        "assert \"[ASSERT_LEFT]\" == tok\nassert tok == \"[ASSERT_RIGHT]\"\n",
        "x = 1 if \"[IF_LEFT]\" == tok else 2\ny = 1 if tok == \"[IF_RIGHT]\" else 2\n",
        "x = 1 if c else \"[ELSE_LEFT]\" == tok\ny = 1 if c else tok == \"[ELSE_RIGHT]\"\n",
        "x = [1 for t in xs if \"[COMP_LEFT]\" == t for q in z]\n",
        "y = [1 for t in xs if t == \"[COMP_RIGHT]\" for q in z]\n",
        "yield \"[YIELD_LEFT]\" == tok\nyield tok == \"[YIELD_RIGHT]\"\n",
        "await \"[AWAIT_LEFT]\" == tok\nawait tok == \"[AWAIT_RIGHT]\"\n",
        "del \"[DEL_LEFT]\" == tok\ndel tok == \"[DEL_RIGHT]\"\n",
        "raise \"[RAISE_LEFT]\" == tok\nraise tok == \"[RAISE_RIGHT]\"\n",
        "x = \"[IS_LEFT]\" == tok is other\nx = tok == \"[IS_RIGHT]\" is other\n",
        "x += \"[AUG_LEFT]\" == tok\nx += tok == \"[AUG_RIGHT]\"\n",
        "if tok == ((\"[GROUP_RIGHT]\")): pass\nif ((\"[GROUP_LEFT]\")) != tok: pass\n",
        "if tok in ((\"[GROUP_MEMBER]\"),): pass\n",
        "if tok == \"[ADD_RIGHT]\" + suffix: pass\nif prefix + \"[ADD_LEFT]\" == tok: pass\n",
        "if tok in (\"[ADD_MEMBER]\" + suffix,): pass\n",
        "if tok == (\"[INDEX]\")[0]: pass\nif (\"[ATTR]\").lower() == tok: pass\n",
        "if tok == \"[CONCAT_RIGHT]\" 'x': pass\nif 'x' \"[CONCAT_LEFT]\" == tok: pass\n",
    )));
    for name in [
        "ASSIGN_LEFT",
        "ASSIGN_RIGHT",
        "RETURN_LEFT",
        "RETURN_RIGHT",
        "ASSERT_LEFT",
        "ASSERT_RIGHT",
        "IF_LEFT",
        "IF_RIGHT",
        "ELSE_LEFT",
        "ELSE_RIGHT",
        "COMP_LEFT",
        "COMP_RIGHT",
        "YIELD_LEFT",
        "YIELD_RIGHT",
        "AWAIT_LEFT",
        "AWAIT_RIGHT",
        "DEL_LEFT",
        "DEL_RIGHT",
        "RAISE_LEFT",
        "RAISE_RIGHT",
        "IS_LEFT",
        "IS_RIGHT",
        "AUG_LEFT",
        "AUG_RIGHT",
        "GROUP_RIGHT",
        "GROUP_LEFT",
        "GROUP_MEMBER",
    ] {
        assert_eq!(
            contexts(&occurrences_of(&template, name)),
            vec!["sentinel"],
            "{name}"
        );
    }
    for name in [
        "ADD_RIGHT",
        "ADD_LEFT",
        "ADD_MEMBER",
        "INDEX",
        "ATTR",
        "CONCAT_RIGHT",
        "CONCAT_LEFT",
    ] {
        assert_eq!(
            contexts(&occurrences_of(&template, name)),
            vec!["quoted"],
            "{name}"
        );
    }
}

#[test]
fn ap_cat_06_inline_case_assignments_and_soft_keyword_names() {
    let template = catalog_of(&demo(concat!(
        "COUNT = 1\nAUG = 1\nLEFT = 1\nRIGHT = 1\nNESTED = 1\n",
        "case = 1\nmatch = 2\n",
        "match value:\n    case 0: COUNT = 2\n    case 1: AUG += 1\n",
        "    case 2: LEFT, RIGHT = 3, 4\n    case Point(x=0): NESTED = 2\n",
    )));
    let names: Vec<String> = items(field(&template, "targets"))
        .iter()
        .map(|target| string(field(target, "name")).to_string())
        .collect();
    assert_eq!(names, vec!["case", "match"]);
}

/// AP-SUB-05: a regex such as `"[^A-Za-z0-9-]"` (T25-T27), `[markdown]` and
/// other bracketed text that is not `[` + A-Z + `[A-Z0-9_ ]*` + `]` are not
/// placeholders.
#[test]
fn ap_sub_05_regexes_and_lowercase_brackets_are_not_placeholders() {
    let text = demo(
        "PATTERN = re.compile(\"[^A-Za-z0-9-]\")\n\
KIND = \"[markdown]\"\n\
OTHER = [\"[A-Z]\", \"[ab]\", \"[1X]\", \"[ X]\", \"[]\", \"[^X]\", \"[X-Y]\", \"[X.Y]\", \"[ÄB]\"]\n\
y = [lower]\n\
# Cell mention [markdown] and [lower] and [1ST]\n",
    );
    let template = catalog_of(&text);
    assert_eq!(slot_names(&template), Vec::<String>::new());
    assert_eq!(items(field(&template, "occurrences")).len(), 0);
    assert_eq!(violations_of(&template), vec![]);
}

/// AP-SUB-06: a placeholder inside any other string is a lint error, so a
/// later template edit cannot introduce a slot that cannot be escaped: triple
/// quoted strings, f-strings, prefixed strings and longer literals.
#[test]
fn ap_sub_06_a_placeholder_inside_another_string_is_a_lint_error() {
    let cases: [(&str, &str, &str); 9] = [
        (
            "triple double",
            "X = \"\"\"[ROLE_ARN]\"\"\"\n",
            "[ROLE_ARN]",
        ),
        (
            "triple single docstring",
            "'''text [ROLE_ARN] text'''\n",
            "[ROLE_ARN]",
        ),
        ("f-string whole", "X = f\"[ROLE_ARN]\"\n", "[ROLE_ARN]"),
        ("f-string longer", "X = f\"{n}-[ROLE_ARN]\"\n", "[ROLE_ARN]"),
        ("longer literal", "X = \"s3://[BUCKET]/key\"\n", "[BUCKET]"),
        ("raw prefix", "X = r\"[ROLE_ARN]\"\n", "[ROLE_ARN]"),
        ("bytes prefix", "X = b\"[ROLE_ARN]\"\n", "[ROLE_ARN]"),
        ("trailing space", "X = \"[ROLE_ARN] \"\n", "[ROLE_ARN]"),
        ("single quotes longer", "X = 'a [ROLE_ARN]'\n", "[ROLE_ARN]"),
    ];
    for (name, body, needle) in cases {
        let text = demo(body);
        let template = catalog_of(&text);
        assert_eq!(
            violations_of(&template),
            vec![(
                "placeholder_in_string".to_string(),
                5,
                column_of(&text, needle)
            )],
            "{name}"
        );
        assert_eq!(slot_names(&template), Vec::<String>::new(), "{name}");
    }
    // Several placeholders in one literal: one violation each.
    let text = demo("X = \"[ROLE_ARN] and [KEY]\"\n");
    let template = catalog_of(&text);
    assert_eq!(
        violations_of(&template),
        vec![
            (
                "placeholder_in_string".to_string(),
                5,
                column_of(&text, "[ROLE_ARN]")
            ),
            (
                "placeholder_in_string".to_string(),
                5,
                column_of(&text, "[KEY]")
            ),
        ]
    );
    // The line and column track a placeholder inside a multi-line string.
    let multi = demo("X = \"\"\"first\nsecond [ROLE_ARN]\n\"\"\"\n");
    assert_eq!(
        violations_of(&catalog_of(&multi)),
        vec![("placeholder_in_string".to_string(), 6, 8)]
    );
    // Strings without placeholders are fine, docstrings and f-strings included.
    let fine = catalog_of(&demo(
        "\"\"\"A docstring with [brackets] and [1].\"\"\"\nX = f\"{a!r} [x]\"\nY = \"it's [OK-1]\"\n",
    ));
    assert_eq!(violations_of(&fine), vec![]);
    // A doubled quote or an escaped quote does not end the string early.
    let escaped = catalog_of(&demo(
        "A = \"say \\\"[ROLE_ARN]\\\"\"\nB = '\\'[ROLE_ARN]'\n",
    ));
    assert_eq!(
        codes_of(&escaped),
        vec![
            "placeholder_in_string".to_string(),
            "placeholder_in_string".to_string()
        ]
    );
}

/// AP-SUB-07: a `#` inside a string does not start a comment, and a quote
/// inside a comment does not start a string.
#[test]
fn ap_sub_07_a_hash_inside_a_string_is_not_a_comment() {
    let text = demo(
        "A = '#' + \"[ROLE_ARN]\"\n\
B = \"a\\\"#\"  # [COMMENT_ONLY]\n\
C = \"\\\\\"  # [AFTER_BACKSLASH]\n\
# an unmatched \" quote in a comment, then:\n\
D = \"[ROLE_ARN]\"\n",
    );
    let template = catalog_of(&text);
    assert_eq!(violations_of(&template), vec![]);
    assert_eq!(
        contexts(&occurrences_of(&template, "ROLE_ARN")),
        vec!["quoted", "quoted"]
    );
    assert_eq!(
        contexts(&occurrences_of(&template, "COMMENT_ONLY")),
        vec!["comment"]
    );
    assert_eq!(
        contexts(&occurrences_of(&template, "AFTER_BACKSLASH")),
        vec!["comment"]
    );
    // A string holding `# [X]` is a longer literal, not a comment.
    let hash = catalog_of(&demo("E = \"# [ROLE_ARN]\"\n"));
    assert_eq!(codes_of(&hash), vec!["placeholder_in_string".to_string()]);
}

// ========================================= AP-CAT: targets, manual edit points

/// AP-CAT-06 (section 4.4.3 rules): which top-level literal assignments are
/// listed as assignable targets.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture text and the expected table, kept side by side"
)]
fn ap_cat_06_assignable_targets_follow_the_section_443_rules() {
    let text = "# Cell 0 [markdown]: Demo\nMD_NAME = 1\n\n\
# Cell 1: Config\n\n\
BASE_MODEL = \"\"  # a trailing comment\n\
COUNT = 3\n\
RATIO = 0.5\n\
SMALL = 1e-3\n\
FLAG = False\n\
NEG = -1\n\
SINGLE = 'x y'\n\
ACCEPT_EULA = (\n    False  # Set to True to accept the license\n)\n\
YKW = 1\n\
call(YKW=2)\n\
TWICE = \"\"\n\
TWICE = \"again\"\n\
AUG = 1\n\
AUG += 1\n\
ALSO, TUPLED = 1, 2\n\
FSTR = f\"x\"\n\
EXPR = f\"s3://{bucket}/\"\n\
NOT_LITERAL = get_role()\n\
NONE_VALUE = None\n\
HAS_SLOT = \"[SOME_SLOT]\"\n\
TRIPLE = \"\"\"x\"\"\"\n\
RAW = r\"x\"\n\
CONCAT = \"a\" \"b\"\n\
INDENTED = 1\n\
if INDENTED:\n    INDENTED = 2\n\
call(\n    KWONLY=2,\n)\n\
CONFIG = {\n    \"a\": 1,\n}\n\
\n\
# Cell 2: More\n\
LATE = \"value\"\n";
    let template = catalog_of(text);
    assert_eq!(violations_of(&template), vec![], "{template:?}");
    let rows: Vec<(String, i64, i64, String, String)> = items(field(&template, "targets"))
        .iter()
        .map(|target| {
            (
                string(field(target, "name")).to_string(),
                int(field(target, "cell")),
                int(field(target, "line")),
                string(field(target, "kind")).to_string(),
                string(field(target, "literal")).to_string(),
            )
        })
        .collect();
    let at = |needle: &str| line_of(text, needle);
    assert_eq!(
        rows,
        vec![
            (
                "BASE_MODEL".to_string(),
                1,
                at("BASE_MODEL ="),
                "string".to_string(),
                "\"\"".to_string()
            ),
            (
                "COUNT".to_string(),
                1,
                at("COUNT = 3"),
                "int".to_string(),
                "3".to_string()
            ),
            (
                "RATIO".to_string(),
                1,
                at("RATIO ="),
                "float".to_string(),
                "0.5".to_string()
            ),
            (
                "SMALL".to_string(),
                1,
                at("SMALL ="),
                "float".to_string(),
                "1e-3".to_string()
            ),
            (
                "FLAG".to_string(),
                1,
                at("FLAG ="),
                "bool".to_string(),
                "False".to_string()
            ),
            (
                "NEG".to_string(),
                1,
                at("NEG ="),
                "int".to_string(),
                "-1".to_string()
            ),
            (
                "SINGLE".to_string(),
                1,
                at("SINGLE ="),
                "string".to_string(),
                "'x y'".to_string()
            ),
            // The parenthesized form is reported on the `N = (` line.
            (
                "ACCEPT_EULA".to_string(),
                1,
                at("ACCEPT_EULA = ("),
                "bool".to_string(),
                "False".to_string()
            ),
            (
                "YKW".to_string(),
                1,
                at("YKW = 1"),
                "int".to_string(),
                "1".to_string()
            ),
            (
                "LATE".to_string(),
                2,
                at("LATE ="),
                "string".to_string(),
                "\"value\"".to_string()
            ),
        ],
        "excluded: markdown cell, a second assignment (also `+=`, a tuple target, an indented \
         one), f-strings, calls, None, a literal holding a placeholder, triple-quoted, \
         prefixed and concatenated strings, keyword arguments, dict displays"
    );
}

/// AP-CAT-07: `NOTE FOR AGENT` and `Uncomment` comments are catalogued as
/// manual edit points, with the first 60 characters of the comment text.
#[test]
fn ap_cat_07_manual_edit_points_list_the_comments_that_instruct_the_agent() {
    let long = format!("NOTE FOR AGENT: {}", "é".repeat(70));
    let text = demo(&format!(
        "# NOTE FOR AGENT: remove this line for non-Meta models\n\
x = 1  # Uncomment the next line to change it\n\
s = \"# NOTE FOR AGENT: inside a string\"\n\
# note for agent: wrong case\n\
# {long}\n\
#   Uncomment   \n\
\n# Cell 2: Next\n# Uncomment the following line\n"
    ));
    let template = catalog_of(&text);
    let rows: Vec<(Option<i64>, i64, String)> = items(field(&template, "manual_edits"))
        .iter()
        .map(|edit| {
            (
                match field(edit, "cell") {
                    Json::Null => None,
                    other => Some(int(other)),
                },
                int(field(edit, "line")),
                string(field(edit, "text")).to_string(),
            )
        })
        .collect();
    let truncated: String = long.chars().take(60).collect();
    assert_eq!(
        rows,
        vec![
            (
                Some(1),
                5,
                "NOTE FOR AGENT: remove this line for non-Meta models".to_string()
            ),
            (
                Some(1),
                6,
                "Uncomment the next line to change it".to_string()
            ),
            (Some(1), 9, truncated),
            (Some(1), 10, "  Uncomment".to_string()),
            (
                Some(2),
                line_of(&text, "# Uncomment the following"),
                "Uncomment the following line".to_string()
            ),
        ]
    );
}

// ====================================================== AP-CAT: real templates

fn real_catalog_text() -> Run {
    templates_at(&canonical_root(), &[])
}

/// AP-CAT-01: `templates --check` over the repository's 23 TPL files exits 0.
#[test]
fn ap_cat_01_the_real_templates_satisfy_the_grammar() {
    let run = templates_at(&canonical_root(), &["--check"]);
    assert_eq!(
        run.code,
        Some(0),
        "stdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
    assert_eq!(run.stderr, "");
    assert!(
        run.stdout
            .lines()
            .last()
            .unwrap_or("")
            .contains("23 templates"),
        "{}",
        run.stdout
    );
    let json = templates_at(&canonical_root(), &["--check", "--json"]);
    assert_eq!(json.code, Some(0));
    let catalog = json_of(&json);
    assert!(flag(field(&catalog, "ok")));
    assert_eq!(items(field(&catalog, "templates")).len(), 23);
}

/// AP-CAT-02: the frozen golden catalog (cells, placeholder sets with kinds
/// and counts, assignable targets with their literals, manual edit points)
/// detects any template drift. The golden was derived from the templates by an
/// independent line-oriented script, not by this implementation.
#[test]
fn ap_cat_02_the_golden_catalog_matches_the_real_templates() {
    let run = real_catalog_text();
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let golden = fs::read_to_string(repo_file(
        "crates/toolkit-cli/tests/data/ai_payloads/catalog.txt",
    ))
    .expect("golden catalog");
    assert_eq!(run.stdout, golden, "the template catalog drifted");
}

/// AP-CAT-02 (consent invariant, #5142 context): every literal of the consent
/// target `ACCEPT_EULA` in the 23 templates is `False`, in the six that assign
/// it; T15 holds the bare slot. A template edit that changes it fails here.
#[test]
fn ap_cat_02_every_consent_literal_in_the_real_templates_is_false() {
    let catalog = json_of(&templates_at(&canonical_root(), &["--json"]));
    let mut with_target = Vec::new();
    let mut with_slot = Vec::new();
    for template in items(field(&catalog, "templates")) {
        let path = string(field(template, "path")).to_string();
        for target in items(field(template, "targets")) {
            if string(field(target, "name")) == "ACCEPT_EULA" {
                assert_eq!(string(field(target, "literal")), "False", "{path}");
                assert_eq!(string(field(target, "kind")), "bool", "{path}");
                with_target.push(path.clone());
            }
        }
        if items(field(template, "placeholders"))
            .iter()
            .any(|slot| string(field(slot, "name")) == "ACCEPT_EULA")
        {
            with_slot.push(path);
        }
    }
    assert_eq!(
        with_target,
        vec![
            "finetuning/code_templates/dpo.py",
            "finetuning/code_templates/rlaif_builtin.py",
            "finetuning/code_templates/rlaif_custom_prompt.py",
            "finetuning/code_templates/rlvr.py",
            "finetuning/code_templates/sft.py",
            "training-jobs/code_templates/jumpstart_training.py",
        ]
    );
    assert_eq!(
        with_slot,
        vec!["model-deployment/code_templates/deploy-oss-sagemaker.py"]
    );
}

/// AP-CAT-02 (manual edit points, section 1.4): 27 lines in 7 templates.
#[test]
fn ap_cat_02_the_manual_edit_points_are_27_lines_in_7_templates() {
    let catalog = json_of(&templates_at(&canonical_root(), &["--json"]));
    let counts: Vec<usize> = items(field(&catalog, "templates"))
        .iter()
        .map(|template| items(field(template, "manual_edits")).len())
        .filter(|count| *count > 0)
        .collect();
    assert_eq!(counts.len(), 7, "{counts:?}");
    assert_eq!(counts.iter().sum::<usize>(), 27);
}

/// AP-CAT-03: every placeholder of the real templates in a code context is a
/// quoted or bare slot, so none is unescapable; the bare slots are exactly the
/// nine names of section 1.4; T17 keeps its sentinel and its secret slot.
#[test]
fn ap_cat_03_every_real_code_placeholder_is_a_quoted_or_a_typed_bare_slot() {
    let catalog = json_of(&templates_at(&canonical_root(), &["--json"]));
    let mut bare = std::collections::BTreeSet::new();
    let mut sentinels = 0;
    for template in items(field(&catalog, "templates")) {
        let path = string(field(template, "path"));
        assert_eq!(violations_of(template), vec![], "{path}");
        for occurrence in items(field(template, "occurrences")) {
            let context = string(field(occurrence, "context"));
            assert!(
                ["quoted", "bare", "comment", "sentinel"].contains(&context),
                "{path}: {context}"
            );
            if context == "bare" {
                bare.insert(string(field(occurrence, "name")).to_string());
            }
            if context == "sentinel" {
                sentinels += 1;
            }
        }
    }
    assert_eq!(
        bare.into_iter().collect::<Vec<_>>(),
        vec![
            "ACCEPT_EULA",
            "CONCURRENCY",
            "INPUT_TOKENS_MEAN",
            "INPUT_TOKENS_STDDEV",
            "METRICS_LIST",
            "OUTPUT_TOKENS_MEAN",
            "OUTPUT_TOKENS_STDDEV",
            "REQUEST_COUNT",
            "TRUE_OR_FALSE",
        ]
    );
    assert_eq!(sentinels, 1, "T17:39 is the only sentinel");
    let hf = items(field(&catalog, "templates"))
        .iter()
        .find(|template| string(field(template, "path")).ends_with("optimize-huggingface-to-s3.py"))
        .expect("T17");
    assert_eq!(
        slot_of(hf, "HF_TOKEN_OR_NONE"),
        ("secret".to_string(), 1, false, 1)
    );
    assert_eq!(
        occurrences_of(hf, "HF_TOKEN_OR_NONE")
            .iter()
            .map(|row| (row.0.clone(), row.1))
            .collect::<Vec<_>>(),
        vec![("quoted".to_string(), 23), ("sentinel".to_string(), 39)]
    );
}

/// AP-CAT-04: the `plugins/` mirror produces the identical catalog.
#[test]
fn ap_cat_04_the_canonical_and_mirror_trees_produce_identical_catalogs() {
    assert!(mirror_root().join("references").is_dir(), "mirror tree");
    let canonical = templates_at(&canonical_root(), &[]);
    let mirror = templates_at(&mirror_root(), &[]);
    assert_eq!(canonical.code, Some(0));
    assert_eq!(mirror.code, Some(0));
    assert_eq!(canonical.stdout, mirror.stdout);
    let canonical = templates_at(&canonical_root(), &["--json"]);
    let mirror = templates_at(&mirror_root(), &["--json"]);
    assert_eq!(canonical.stdout, mirror.stdout);
    assert_eq!(
        templates_at(&mirror_root(), &["--check"]).code,
        Some(0),
        "the mirror satisfies the grammar"
    );
}

// =============================================================== AP-CLI

/// AP-CLI-01: the route is registered, and hostile text (control characters,
/// bidi marks, a secret-looking marker) in the root, in a file name, in a
/// label and in the template body never reaches stdout or stderr raw. The
/// shared `tests/diag_contract.rs` runs the same route automatically.
#[test]
fn ap_cli_01_the_route_is_registered_and_hostile_text_is_escaped() {
    assert!(toolkit_cli::COMMANDS
        .iter()
        .any(|spec| spec.path == ["ai-payloads", "templates"]));

    let secret = "SECRET-MARKER-5143";
    let hostile = format!("bad\u{1b}[31m\u{202e}{secret}");
    let scratch = Scratch::new("hostile");
    let file_name = format!("{hostile}.py");
    scratch.template(
        &format!("demo/code_templates/{file_name}"),
        &format!(
            "# Cell 0 [markdown]: Title {hostile}\n# Cell 1: Label {hostile}\n\
X = \"\u{1b}[{secret}]\"\nY = \"[{secret}]\"\n# Cell 3: gap {hostile}\n\
Z = \"[AA{hostile}]\"\n"
        ),
    );
    let mut runs = Vec::new();
    for extra in [
        vec![],
        vec!["--json"],
        vec!["--check"],
        vec!["--check", "--json"],
    ] {
        runs.push(templates_at(&scratch.root(), &extra));
    }
    // A hostile root that does not exist, in both spellings.
    let missing = scratch.0.join(&hostile);
    let root = missing.to_str().expect("utf-8");
    runs.push(run_in(
        &scratch.0,
        &["ai-payloads", "templates", "--root", root],
    ));
    runs.push(run_in(
        &scratch.0,
        &["ai-payloads", "templates", &format!("--root={root}")],
    ));
    for run in &runs {
        for text in [&run.stdout, &run.stderr] {
            let raw: Vec<char> = text
                .chars()
                .filter(|c| *c != '\n' && is_unsafe_char(*c))
                .collect();
            assert!(raw.is_empty(), "raw characters {raw:?} in {text:?}");
        }
    }
    // The placeholder-shaped hostile name is a slot, but its text is escaped;
    // no run echoes the payload marker inside a control sequence.
    assert!(
        runs[4].code == Some(1) && runs[5].code == Some(1),
        "missing root is an error"
    );
    for run in &runs[4..] {
        assert!(!run.stderr.contains('\u{1b}'), "{:?}", run.stderr);
    }
}

/// AP-CLI-02: help lists the route, the slice document names it, and the
/// generated usage text carries the synopsis.
#[test]
fn ap_cli_02_help_and_the_slice_document_name_the_route() {
    let usage = toolkit_cli::USAGE;
    assert!(
        usage.contains("toolkit ai-payloads templates [--root DIR] [--json] [--check]"),
        "{usage}"
    );
    assert!(usage.contains("ai-payloads"));
    let help = run_in(&std::env::temp_dir(), &["help"]);
    assert_eq!(help.code, Some(0));
    assert!(help.stdout.contains("toolkit ai-payloads templates"));
    let per_command = run_in(
        &std::env::temp_dir(),
        &["ai-payloads", "templates", "--help"],
    );
    assert_eq!(per_command.code, Some(0));
    assert_eq!(per_command.stdout, format!("{usage}\n"));

    let document = fs::read_to_string(repo_file("docs/migration/ai-payloads.md"))
        .expect("docs/migration/ai-payloads.md");
    assert!(
        document.contains("ai-payloads templates"),
        "the slice document must name the command"
    );
    assert!(document.contains("--check") && document.contains("--json"));
}

/// AP-CLI-03 (templates): exit codes. 0 for a listing, 1 for a lint error under
/// `--check`, a refusal or a runtime error, 2 for a usage error.
#[test]
fn ap_cli_03_exit_codes_and_usage_errors() {
    let scratch = Scratch::new("exit");
    scratch.template("demo/code_templates/ok.py", &demo("A = \"[ROLE_ARN]\"\n"));
    let root = scratch.root();

    let listing = templates_at(&root, &[]);
    assert_eq!(listing.code, Some(0));
    assert_eq!(listing.stderr, "");
    assert!(listing.stdout.contains("demo/code_templates/ok.py"));
    assert!(listing
        .stdout
        .lines()
        .last()
        .unwrap_or("")
        .ends_with("templates: 1"));
    assert_eq!(templates_at(&root, &["--check"]).code, Some(0));

    // A lint error: --check exits 1, the listing and --json alone exit 0.
    scratch.template("demo/code_templates/bad.py", "x = 1\n");
    let bad = templates_at(&root, &["--check"]);
    assert_eq!(bad.code, Some(1));
    assert!(
        bad.stdout
            .contains("demo/code_templates/bad.py:0:0: no_cells"),
        "{}",
        bad.stdout
    );
    assert!(!bad.stdout.contains("demo/code_templates/ok.py:"));
    assert_eq!(templates_at(&root, &[]).code, Some(0));
    let json = templates_at(&root, &["--check", "--json"]);
    assert_eq!(json.code, Some(1));
    let catalog = json_of(&json);
    assert!(!flag(field(&catalog, "ok")));
    let paths: Vec<&str> = items(field(&catalog, "templates"))
        .iter()
        .map(|template| string(field(template, "path")))
        .collect();
    assert_eq!(
        paths,
        vec!["demo/code_templates/bad.py", "demo/code_templates/ok.py"],
        "sorted by path"
    );

    // Usage errors are exit 2 with the usage text on stderr.
    let cwd = std::env::temp_dir();
    for args in [
        vec!["ai-payloads", "templates", "extra"],
        vec!["ai-payloads", "templates", "--bogus"],
        vec!["ai-payloads", "templates", "--root"],
        vec!["ai-payloads", "templates", "--root", "-"],
        vec!["ai-payloads", "templates", "--root=-"],
        vec!["ai-payloads"],
        vec!["ai-payloads", "unknown"],
    ] {
        let run = run_in(&cwd, &args);
        assert_eq!(run.code, Some(2), "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{args:?}");
    }
}

/// AP-CLI-03 (templates): filesystem refusals are exit 1 with a message on
/// stderr: no default root, a root without `references/`, a file as the root,
/// an over-cap, non-UTF-8 or symlinked template.
#[test]
fn ap_cli_03_filesystem_problems_are_runtime_errors() {
    let scratch = Scratch::new("fs");
    // The default root is skills/core-skills/aws-ai-ml of the working directory.
    let missing = run_in(&scratch.0, &["ai-payloads", "templates"]);
    assert_eq!(missing.code, Some(1));
    assert_eq!(missing.stdout, "");
    assert!(missing.stderr.contains("--root"), "{}", missing.stderr);
    // From the repository root the default finds the canonical templates.
    let repository = run_in(&repo_file(""), &["ai-payloads", "templates", "--check"]);
    assert_eq!(repository.code, Some(0), "{}", repository.stderr);

    let no_references = scratch.0.join("empty");
    fs::create_dir_all(&no_references).expect("dir");
    assert_eq!(templates_at(&no_references, &[]).code, Some(1));
    let file_root = scratch.0.join("file-root");
    fs::write(&file_root, "x").expect("file");
    assert_eq!(templates_at(&file_root, &[]).code, Some(1));

    // No template at all: a listing is empty, a check refuses.
    let none = Scratch::new("none");
    fs::create_dir_all(none.root().join("references/demo/code_templates")).expect("dirs");
    let empty = templates_at(&none.root(), &[]);
    assert_eq!(empty.code, Some(0));
    assert!(empty.stdout.ends_with("templates: 0\n"), "{}", empty.stdout);
    let check = templates_at(&none.root(), &["--check"]);
    assert_eq!(check.code, Some(1));

    // Other file kinds under code_templates are ignored; .py files are read.
    let mixed = Scratch::new("mixed");
    mixed.template("demo/code_templates/notes.md", "# Cell 9: not python\n");
    mixed.template("demo/other/ignored.py", "x = 1\n");
    mixed.template("top-level.py", "x = 1\n");
    mixed.template("demo/code_templates/ok.py", &demo("x = 1\n"));
    let listing = templates_at(&mixed.root(), &["--check"]);
    assert_eq!(listing.code, Some(0), "{}", listing.stdout);
    assert!(listing.stdout.contains("1 templates"), "{}", listing.stdout);

    // An over-cap template (1 MiB + 1), a non-UTF-8 one.
    let big = Scratch::new("big");
    big.template("demo/code_templates/big.py", &"#".repeat(1024 * 1024 + 1));
    let run = templates_at(&big.root(), &[]);
    assert_eq!(run.code, Some(1));
    assert!(run.stderr.contains("big.py"), "{}", run.stderr);
    let bytes = Scratch::new("bytes");
    let path = bytes.template("demo/code_templates/bytes.py", "");
    fs::write(&path, [0x23, 0xff, 0xfe, 0x0a]).expect("write");
    let run = templates_at(&bytes.root(), &[]);
    assert_eq!(run.code, Some(1));
    assert_eq!(run.stdout, "");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let linked = Scratch::new("link");
        let real = linked.template("demo/code_templates/real.py", &demo("x = 1\n"));
        symlink(
            &real,
            linked.root().join("references/demo/code_templates/link.py"),
        )
        .expect("symlink");
        let run = templates_at(&linked.root(), &[]);
        assert_eq!(run.code, Some(1), "a symlinked template is refused");
        assert!(run.stderr.contains("link.py"), "{}", run.stderr);

        let dir_link = Scratch::new("dirlink");
        let target = dir_link.0.join("elsewhere");
        fs::create_dir_all(target.join("code_templates")).expect("dirs");
        fs::write(target.join("code_templates/a.py"), demo("x = 1\n")).expect("write");
        fs::create_dir_all(dir_link.root().join("references")).expect("dirs");
        symlink(&target, dir_link.root().join("references/demo")).expect("symlink");
        let run = templates_at(&dir_link.root(), &[]);
        assert_eq!(run.code, Some(1), "a symlinked directory is refused");
    }
}
