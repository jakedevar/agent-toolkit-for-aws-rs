//! x402 slice S7 (#5526, programme #4410): the cutover documents.
//!
//! The Rust port of `agents-pay/scripts/test_portability.py` (design R-PORT) and
//! the text assertions for the S7 obligations of
//! `docs/migration/x402-payments-design.md`. The tests read the skill pages as
//! text; no Python is run, no command line is executed and nothing touches the
//! network.
//!
//! R-PORT, the document checks of the Python test, over both `SKILL.md` copies
//! and `references/`:
//! - `s7_port_frontmatter_*`: the frontmatter parses, `description` has at least
//!   20 characters, `name` is the directory, no `stages` or `owner_*` keys;
//! - `s7_port_no_render_time_execution`: no `` !`cmd` `` and no ```` ```! ````;
//! - `s7_port_no_parent_escapes`: no `../` link or path;
//! - `s7_port_links_resolve_in_an_isolated_copy`: every relative link resolves in
//!   a temporary copy that holds only `SKILL.md` and `references/`;
//! - `s7_port_both_skill_copies_are_identical`: the two `SKILL.md` copies are
//!   byte-identical (design section 1.1);
//! - the replacement of "the scripts run from the copy":
//!   `s7_port_every_documented_command_is_a_registered_route`,
//!   `s7_port_no_script_path_remains`, `s7_port_no_python_command_remains`.
//!
//! Text obligations: `s7_text_*` (L-TERMS, the R-DNS residual, B1, B3, B4, Q21,
//! Q2, Q3, D23), `s7_inventory_*` and `s7_migration_doc_*`.
//!
//! The OpenClaw parts belong to OpenClaw B5, which edits them on top of this
//! slice: the `### OpenClaw` subsection of `SKILL.md` and
//! `references/openclaw-setup.md` are exempt from the content rules below
//! (`openclaw_exempt`). B5 deletes that exemption when it rewrites them.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SKILL_DIR: &str = "plugins/aws-agents/skills/agents-pay";
const PACKAGED_SKILL: &str =
    "plugins/aws-agents/skills/agents-pay/packages/openclaw/skills/agents-pay/SKILL.md";
const PAYMENTS_REFERENCE: &str = "plugins/aws-agents/skills/agents-build/references/payments.md";
const OPENCLAW_REFERENCE: &str = "openclaw-setup.md";
const OPENCLAW_HEADING: &str = "### OpenClaw";
const AFTER_OPENCLAW_HEADING: &str = "### Any other host";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// One page of the skill: its repository-relative path and its text.
struct Page {
    rel: String,
    text: String,
}

/// `SKILL.md` (both copies) and every `references/*.md` of `agents-pay`.
fn pay_pages() -> Vec<Page> {
    let mut rels = vec![format!("{SKILL_DIR}/SKILL.md"), PACKAGED_SKILL.to_owned()];
    let mut references: Vec<String> = fs::read_dir(root().join(SKILL_DIR).join("references"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_ascii_lowercase().ends_with(".md"))
        .collect();
    references.sort();
    rels.extend(
        references
            .into_iter()
            .map(|name| format!("{SKILL_DIR}/references/{name}")),
    );
    rels.into_iter()
        .map(|rel| Page {
            text: read(&rel),
            rel,
        })
        .collect()
}

/// The pages whose content rules apply, with the OpenClaw parts removed.
fn openclaw_exempt(rel: &str, text: &str) -> String {
    if rel.ends_with(OPENCLAW_REFERENCE) {
        return String::new();
    }
    // Only the two `SKILL.md` copies carry the exempt subsection; a same-named
    // heading in another page is ordinary content.
    let Some(start) = rel
        .ends_with("/SKILL.md")
        .then(|| text.find(OPENCLAW_HEADING))
        .flatten()
    else {
        return text.to_owned();
    };
    let after = text[start..]
        .find(AFTER_OPENCLAW_HEADING)
        .map_or(text.len(), |at| start + at);
    format!("{}{}", &text[..start], &text[after..])
}

/// Every page the content rules cover: the skill pages minus the OpenClaw
/// parts, plus the `agents-build` payments reference.
fn governed_pages() -> Vec<Page> {
    let mut pages: Vec<Page> = pay_pages()
        .into_iter()
        .map(|page| Page {
            text: openclaw_exempt(&page.rel, &page.text),
            rel: page.rel,
        })
        .collect();
    pages.push(Page {
        rel: PAYMENTS_REFERENCE.to_owned(),
        text: read(PAYMENTS_REFERENCE),
    });
    pages
}

/// Collapse every whitespace run to one space, so wrapped lines compare.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_mentions(rel: &str, text: &str, needles: &[&str]) {
    let flat_text = flat(text);
    let missing: Vec<&&str> = needles
        .iter()
        .filter(|needle| !flat_text.contains(&flat(needle)))
        .collect();
    assert!(missing.is_empty(), "{rel} lacks {missing:?}");
}

fn skill_text() -> String {
    read(&format!("{SKILL_DIR}/SKILL.md"))
}

// ---------------------------------------------------------------- R-PORT

/// The top-level keys of the frontmatter block.
fn frontmatter_keys(text: &str) -> Vec<String> {
    let mut lines = text.lines();
    assert_eq!(lines.next().map(str::trim), Some("---"), "no frontmatter");
    let mut keys = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if !line.starts_with([' ', '\t']) {
            if let Some((key, _)) = line.split_once(':') {
                keys.push(key.trim().to_owned());
            }
        }
    }
    assert!(closed, "unterminated frontmatter");
    keys
}

fn check_frontmatter(rel: &str, text: &str) {
    let parsed = toolkit_core::frontmatter::parse_frontmatter(text);
    let description = parsed.description.unwrap_or_default();
    assert!(
        description.chars().count() >= 20,
        "{rel}: description is {description:?}"
    );
    assert_eq!(parsed.name.as_deref(), Some("agents-pay"), "{rel}: name");
    let forbidden: Vec<String> = frontmatter_keys(text)
        .into_iter()
        .filter(|key| key == "stages" || key.starts_with("owner_"))
        .collect();
    assert!(forbidden.is_empty(), "{rel}: forbidden keys {forbidden:?}");
}

#[test]
fn s7_port_frontmatter_of_the_plugin_copy() {
    check_frontmatter("SKILL.md", &skill_text());
}

#[test]
fn s7_port_frontmatter_of_the_packaged_copy() {
    check_frontmatter(PACKAGED_SKILL, &read(PACKAGED_SKILL));
}

#[test]
fn s7_port_frontmatter_check_rejects_a_forbidden_key() {
    let bad = "---\nname: agents-pay\ndescription: >\n  long enough description text\nowner_team: x\n---\nbody\n";
    let result = std::panic::catch_unwind(|| check_frontmatter("bad", bad));
    assert!(result.is_err(), "owner_* must be rejected");
}

#[test]
fn s7_port_both_skill_copies_are_identical() {
    assert_eq!(
        skill_text(),
        read(PACKAGED_SKILL),
        "the two SKILL.md copies must stay byte-identical"
    );
}

/// `` !`cmd` `` spans and ```` ```! ```` fences, which some harnesses run at
/// render time.
fn render_time_exec(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```!") {
            hits.push(line.to_owned());
        }
        let bytes = line.as_bytes();
        for (at, byte) in bytes.iter().enumerate() {
            let prev_ok = at == 0
                || !(bytes[at - 1] == b'`'
                    || bytes[at - 1].is_ascii_alphanumeric()
                    || bytes[at - 1] == b'_');
            if *byte == b'!' && prev_ok && bytes.get(at + 1) == Some(&b'`') {
                if let Some(close) = line[at + 2..].find('`') {
                    if close > 0 {
                        hits.push(line[at..at + 3 + close].to_owned());
                    }
                }
            }
        }
    }
    hits
}

#[test]
fn s7_port_no_render_time_execution() {
    let mut pages = pay_pages();
    pages.push(Page {
        rel: PAYMENTS_REFERENCE.to_owned(),
        text: read(PAYMENTS_REFERENCE),
    });
    for page in &pages {
        let hits = render_time_exec(&page.text);
        assert!(hits.is_empty(), "{}: {hits:?}", page.rel);
    }
}

#[test]
fn s7_port_render_time_scan_finds_seeded_spans() {
    assert_eq!(render_time_exec("run !`ls` now").len(), 1);
    assert_eq!(render_time_exec("```!\nls\n```").len(), 1);
    assert!(render_time_exec("a `code` and wow! done").is_empty());
}

/// The targets of every Markdown link `](target)`.
fn link_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        let after = &rest[at + 2..];
        if let Some(close) = after.find(')') {
            targets.push(after[..close].to_owned());
        }
        rest = after;
    }
    targets
}

#[test]
fn s7_port_no_parent_escapes() {
    for page in pay_pages() {
        let escapes: Vec<String> = link_targets(&page.text)
            .into_iter()
            .filter(|target| target.starts_with("../"))
            .collect();
        assert!(escapes.is_empty(), "{}: {escapes:?}", page.rel);
        for line in page.text.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with("../") && !trimmed.starts_with("python3 ../"),
                "{}: {line}",
                page.rel
            );
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn s7_port_links_resolve_in_an_isolated_copy() {
    // The portable surface after the cutover is the skill text and its
    // references. The Python scripts are not part of it.
    let scratch = std::env::temp_dir().join(format!("s7-port-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    let copy = scratch.join("skills").join("agents-pay");
    fs::create_dir_all(&copy).unwrap();
    let source = root().join(SKILL_DIR);
    fs::copy(source.join("SKILL.md"), copy.join("SKILL.md")).unwrap();
    copy_tree(&source.join("references"), &copy.join("references"));

    let mut files = vec![copy.join("SKILL.md")];
    let mut references: Vec<PathBuf> = fs::read_dir(copy.join("references"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    references.sort();
    files.extend(references);

    let mut missing = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        for target in link_targets(&text) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
                || target.starts_with('#')
            {
                continue;
            }
            let path = target.split('#').next().unwrap_or("");
            if !file.parent().unwrap().join(path).exists() {
                missing.push(format!(
                    "{} -> {target}",
                    file.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    fs::remove_dir_all(&scratch).unwrap();
    assert!(missing.is_empty(), "unresolved links: {missing:?}");
}

// ----------------------------------------------------- documented commands

/// A command line a page tells the reader to run, with the page it is on.
struct Documented {
    rel: String,
    words: Vec<String>,
    /// A code span mentioning a route, not a full command line.
    inline: bool,
}

/// The words of a shell line up to a pipe, redirect, comment or `&&`.
fn command_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    for word in line.split_whitespace() {
        if word.starts_with('#') || matches!(word, "|" | ">" | ">>" | "&&" | ";" | "<") {
            break;
        }
        words.push(word.to_owned());
    }
    words
}

fn is_shell_fence(info: &str) -> bool {
    matches!(info.trim(), "" | "bash" | "sh" | "shell" | "console")
}

/// Every `toolkit …` line in a shell fence, joined across `\` continuations,
/// and every inline code span that starts with `toolkit `.
fn documented_commands(page: &Page) -> Vec<Documented> {
    let mut found = Vec::new();
    let mut fence: Option<String> = None;
    let mut pending = String::new();
    for line in page.text.lines() {
        let trimmed = line.trim();
        if let Some(info) = trimmed.strip_prefix("```") {
            fence = match fence {
                Some(_) => None,
                None => Some(info.to_owned()),
            };
            pending.clear();
            continue;
        }
        match &fence {
            Some(info) if is_shell_fence(info) => {
                let line = trimmed.strip_prefix("$ ").unwrap_or(trimmed);
                let (piece, continued) = line
                    .strip_suffix('\\')
                    .map_or((line, false), |head| (head, true));
                pending.push_str(piece);
                pending.push(' ');
                if continued {
                    continue;
                }
                let words = command_words(&pending);
                pending.clear();
                if words.first().map(String::as_str) == Some("toolkit") {
                    found.push(Documented {
                        rel: page.rel.clone(),
                        words,
                        inline: false,
                    });
                }
            }
            Some(_) => {}
            None => {
                let mut parts = line.split('`');
                parts.next();
                for (at, span) in parts.enumerate() {
                    // Odd positions after the first split are code spans.
                    if at % 2 == 0 && span.starts_with("toolkit ") {
                        found.push(Documented {
                            rel: page.rel.clone(),
                            words: command_words(span),
                            inline: true,
                        });
                    }
                }
            }
        }
    }
    found
}

/// Why a documented command is not a registered route, if it is not.
fn command_error(doc: &Documented) -> Option<String> {
    let words: Vec<&str> = doc.words.iter().skip(1).map(String::as_str).collect();
    if words
        .first()
        .is_some_and(|w| matches!(*w, "help" | "--help"))
    {
        return None;
    }
    let spec = toolkit_cli::COMMANDS
        .iter()
        .filter(|spec| words.len() >= spec.path.len() && words[..spec.path.len()] == *spec.path)
        .max_by_key(|spec| spec.path.len());
    let Some(spec) = spec else {
        // A span may name only the start of a route (`toolkit agents-pay admin`).
        let prefix = doc.inline
            && toolkit_cli::COMMANDS.iter().any(|spec| {
                spec.path.len() >= words.len() && spec.path[..words.len()] == words[..]
            });
        return (!prefix).then(|| format!("{}: `{}` is not a route", doc.rel, words.join(" ")));
    };
    for word in &words[spec.path.len()..] {
        if let Some(flag) = word.strip_prefix("--") {
            let name = format!("--{}", flag.split('=').next().unwrap_or(""));
            let known = spec.flags.contains(&name.as_str())
                || spec.value_flags.contains(&name.as_str())
                || spec.repeatable_flags.contains(&name.as_str());
            if !known {
                return Some(format!(
                    "{}: `{name}` is not a flag of `{}`",
                    doc.rel,
                    spec.path.join(" ")
                ));
            }
        }
    }
    None
}

#[test]
fn s7_port_every_documented_command_is_a_registered_route() {
    let mut problems = Vec::new();
    let mut routes: BTreeSet<String> = BTreeSet::new();
    for page in governed_pages() {
        for doc in documented_commands(&page) {
            if let Some(problem) = command_error(&doc) {
                problems.push(problem);
            } else if !doc.inline {
                routes.insert(doc.words[1..].join(" "));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
    // The skill must really tell the agent and the operator how to run it.
    for needed in [
        "agents-pay fetch",
        "agents-pay admin init-config",
        "agents-pay admin create-instrument",
        "agents-pay admin new-session",
        "agents-pay admin preflight",
        "agents-pay admin show-config",
        "agents-build setup-payment-user",
    ] {
        assert!(
            routes.iter().any(|line| line.starts_with(needed)),
            "no page documents a `toolkit {needed}` command line: {routes:?}"
        );
    }
}

#[test]
fn s7_port_command_check_rejects_seeded_errors() {
    let page = |text: &str| Page {
        rel: "seed.md".to_owned(),
        text: text.to_owned(),
    };
    let bad_flag = page("```bash\ntoolkit agents-pay fetch --yes https://x\n```\n");
    let bad_route = page("```bash\ntoolkit agents-pay admin bogus\n```\n");
    let good = page("```bash\ntoolkit agents-pay admin init-config \\\n  --recipient 0xabc\n```\n");
    let inline = page("Run `toolkit agents-pay admin` first.\n");
    for (seed, bad) in [
        (&bad_flag, true),
        (&bad_route, true),
        (&good, false),
        (&inline, false),
    ] {
        let docs = documented_commands(seed);
        assert_eq!(docs.len(), 1, "{}", seed.text);
        assert_eq!(command_error(&docs[0]).is_some(), bad, "{}", seed.text);
    }
}

/// Names of the retired Python artifacts. None may appear in a governed page.
const RETIRED_NAMES: [&str; 9] = [
    "agents_pay_admin",
    "x402_policy",
    "x402_fetch.py",
    "x402_fetch_cli",
    "setup_payment_user",
    "process_payment_tool",
    "test_x402_policy",
    "test_portability",
    "test_resource_resolution",
];

#[test]
fn s7_port_no_script_path_remains() {
    for page in governed_pages() {
        for line in page.text.lines() {
            assert!(
                !(line.contains("scripts/") && line.contains(".py")),
                "{}: a scripts/*.py path remains: {line}",
                page.rel
            );
            for name in RETIRED_NAMES {
                assert!(
                    !line.contains(name),
                    "{}: names the retired `{name}`: {line}",
                    page.rel
                );
            }
            // The Python import API is named once, in the B1 boundary.
            if line.contains("x402_fetch") {
                assert!(
                    line.contains("from x402_fetch import"),
                    "{}: names the Python x402_fetch function: {line}",
                    page.rel
                );
            }
            assert!(
                !line.contains("requirements.txt"),
                "{}: names requirements.txt: {line}",
                page.rel
            );
        }
    }
}

#[test]
fn s7_port_no_python_command_remains() {
    for page in governed_pages() {
        let mut fence: Option<String> = None;
        for line in page.text.lines() {
            let trimmed = line.trim();
            if let Some(info) = trimmed.strip_prefix("```") {
                fence = match fence {
                    Some(_) => None,
                    None => Some(info.to_owned()),
                };
                continue;
            }
            if fence.as_deref().is_some_and(is_shell_fence) {
                let words: Vec<&str> = trimmed.split_whitespace().collect();
                let python = words
                    .iter()
                    .any(|w| w.starts_with("python") || matches!(*w, "pip" | "pip3" | "venv"));
                assert!(!python, "{}: a shell line runs Python: {line}", page.rel);
            }
        }
    }
}

// ------------------------------------------------------- S7 text obligations

#[test]
fn s7_text_l_terms_is_in_the_skill_security_notes() {
    let skill = skill_text();
    assert_mentions(
        "SKILL.md",
        &skill,
        &[
            "L-TERMS",
            "Retrying a purchase is idempotent only while the merchant's terms stay the same",
            "a different amount under the cap",
            "another allowed recipient",
            "different token",
            "second payment",
            "per-payment cap, the recipient allowlist and the session budget",
            "Exactly-once purchasing across changed challenges is not provided",
        ],
    );
}

#[test]
fn s7_text_dns_residual_is_documented() {
    let skill = skill_text();
    assert_mentions(
        "SKILL.md",
        &skill,
        &[
            "a stalled DNS lookup can exceed X402_TIMEOUT_SECONDS",
            "no connection, authorization or proof send starts after the deadline",
        ],
    );
    let troubleshooting = read(&format!("{SKILL_DIR}/references/troubleshooting.md"));
    assert_mentions(
        "troubleshooting.md",
        &troubleshooting,
        &[
            "a stalled DNS lookup can exceed X402_TIMEOUT_SECONDS",
            "no connection, authorization or proof send starts after the deadline",
        ],
    );
}

#[test]
fn s7_text_payment_roots_boundary_b4() {
    let skill = skill_text();
    assert_mentions(
        "SKILL.md",
        &skill,
        &[
            "platform certificate store",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "AWS_CA_BUNDLE",
            "TLS-inspecting proxies break",
            "On macOS the payment runtime refuses while SSL_CERT_FILE or SSL_CERT_DIR is set",
        ],
    );
}

#[test]
fn s7_text_import_api_boundary_b1_and_q3() {
    let skill = skill_text();
    assert_mentions(
        "SKILL.md",
        &skill,
        &[
            "from x402_fetch import x402_fetch, payment_session_status, prepare_browser_payment, attach_browser_payment",
            "has no Rust equivalent",
            "a handle cannot be redeemed after the process exits",
            "#5103",
        ],
    );
}

#[test]
fn s7_text_iam_is_the_outer_boundary_b5() {
    assert_mentions(
        "SKILL.md",
        &skill_text(),
        &["A caller that holds the ProcessPaymentRole credentials can call ProcessPayment directly"],
    );
}

#[test]
fn s7_text_toolkit_on_path_q21() {
    let skill = skill_text();
    assert_mentions(
        "SKILL.md",
        &skill,
        &["`toolkit` must be on `PATH`", "toolkit help"],
    );
}

#[test]
fn s7_text_missing_resource_command_name_d23() {
    let mut mentions = 0;
    for page in governed_pages() {
        assert!(
            !page.text.contains("agents_pay_admin.py init-config"),
            "{}: still names the Python init-config",
            page.rel
        );
        if page.text.contains("Missing payment resource") {
            assert_mentions(
                &page.rel,
                &page.text,
                &["toolkit agents-pay admin init-config"],
            );
            mentions += 1;
        }
    }
    assert!(mentions >= 1, "no page quotes the missing-resource refusal");
}

#[test]
fn s7_text_payments_reference_replaces_the_python_template_q2() {
    let text = read(PAYMENTS_REFERENCE);
    assert!(
        !text.contains("process_payment_tool"),
        "the template link remains"
    );
    assert_mentions(
        "payments.md",
        &text,
        &[
            "toolkit agents-pay fetch",
            "subprocess",
            "the policy gate stays in the path",
            "Strands",
            "LangGraph",
            "toolkit agents-build setup-payment-user",
        ],
    );
}

#[test]
fn s7_text_agents_build_step_runs_the_toolkit_command() {
    let text = read(PAYMENTS_REFERENCE);
    assert!(
        !text.contains("scripts/setup_payment_user.py"),
        "the Python script call remains"
    );
    assert_mentions(
        "payments.md",
        &text,
        &["toolkit agents-build setup-payment-user --user-id alice --email alice@example.com --budget 5"],
    );
}

// ------------------------------------------------------- OpenClaw parts

#[test]
fn s7_openclaw_subsection_and_setup_page_are_left_for_b5() {
    let skill = skill_text();
    let start = skill
        .find(OPENCLAW_HEADING)
        .expect("the OpenClaw subsection");
    let end = skill
        .find(AFTER_OPENCLAW_HEADING)
        .expect("the next subsection");
    let section = &skill[start..end];
    assert_mentions(
        "SKILL.md OpenClaw subsection",
        section,
        &[
            "openclaw plugins install clawhub:@aws/aws-agents-pay",
            "**Choose one runtime path.**",
            "openclaw plugins inspect aws-agents-pay",
        ],
    );
    assert!(root()
        .join(SKILL_DIR)
        .join("references")
        .join(OPENCLAW_REFERENCE)
        .is_file());
}

// ------------------------------------------------------------ inventory

fn inventory() -> Value {
    serde_json::from_str(&read("docs/migration/python-inventory.json")).unwrap()
}

#[test]
fn s7_inventory_payments_policy_rows_are_ported() {
    let inv = inventory();
    let rows: Vec<&Value> = inv["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["issue"].as_u64() == Some(4410))
        .collect();
    assert_eq!(rows.len(), 9, "the nine payments-policy rows");
    for row in rows {
        let path = row["path"].as_str().unwrap();
        assert_eq!(row["status"].as_str(), Some("ported"), "{path}");
        let note = row["disposition_note"].as_str().unwrap_or("");
        assert!(
            note.len() >= 40 && note.contains("S7"),
            "{path}: note {note:?}"
        );
        assert!(
            note.contains("retirement"),
            "{path}: the note must leave removal to the programme retirement step: {note}"
        );
    }
}

#[test]
fn s7_inventory_bridge_rows_stay_with_their_owner() {
    let inv = inventory();
    for row in inv["files"].as_array().unwrap() {
        if row["issue"].as_u64() == Some(4411) {
            assert_eq!(
                row["status"].as_str(),
                Some("pending"),
                "{}: #4411 owns this row",
                row["path"]
            );
        }
    }
}

// --------------------------------------------------------- migration doc

#[test]
fn s7_migration_doc_records_the_cutover() {
    let doc = read("docs/migration/agents-pay.md");
    assert_mentions(
        "agents-pay.md",
        &doc,
        &[
            "## Cutover (S7, #5526)",
            "toolkit agents-pay admin init-config",
            "L-TERMS",
            "B1",
            "B3",
            "B4",
            "reference snippet",
            "R-PORT",
            "agents_pay_cutover_docs",
        ],
    );
}
