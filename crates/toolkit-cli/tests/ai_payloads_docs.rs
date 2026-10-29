//! Slice S4 (#5278): AP-DOC-*, the skill prose directs agents to the Rust commands
//! `toolkit ai-payloads render` and `toolkit ai-payloads reward check`.
//!
//! The tests read the canonical skill pages (and their `plugins/` mirrors) as text
//! and run the real `toolkit` binary against the command lines the pages document.
//! No Python is run and the templates are never executed.
//!
//! Requirement map (design section 5, R15, and the S4 row of section 7):
//! - AP-DOC-01: the four `code_output_guide.md` files, the training and processing
//!   `configuration.md` and every other page that renders a template name
//!   `toolkit ai-payloads render`:
//!   `ap_doc_01_render_pages_name_the_render_command`,
//!   `ap_doc_01_guides_document_the_values_file_and_catalog`
//! - AP-DOC-02: `toolkit sync plugin-skills --check` exits 0 and the mirrors are
//!   byte-identical: `ap_doc_02_sync_plugin_skills_check_is_clean`,
//!   `ap_doc_02_every_reference_page_matches_its_mirror`
//! - AP-DOC-03: each reward-template caller page names `toolkit ai-payloads reward check`:
//!   `ap_doc_03_reward_pages_name_the_checker_and_their_formats`
//! - AP-DOC-04: documented command lines parse against the real CLI usage:
//!   `ap_doc_04_documented_command_lines_parse_against_the_real_cli`,
//!   `ap_doc_04_documented_flags_are_in_the_cli_synopsis`,
//!   `ap_doc_04_documented_command_forms_are_all_present`; inline code-span commands
//!   (#5282) are checked like fenced ones, each against its own route:
//!   `ap_doc_04_inline_commands_are_documented_commands`,
//!   `ap_doc_04_bare_inline_mentions_name_real_routes`,
//!   `ap_doc_04_inline_render_with_a_foreign_route_flag_fails`
//! - AP-DOC-05: no page still instructs hand rendering:
//!   `ap_doc_05_no_page_instructs_hand_rendering`
//! - AP-DOC-06: the EULA step names `--consent ACCEPT_EULA` (OD-12):
//!   `ap_doc_06_eula_consent_step_names_the_consent_flag`
//! - AP-DOC-07: the append step saves, appends, then opens the `open:` path (B-12):
//!   `ap_doc_07_append_step_saves_appends_then_opens`
//! - AP-DOC-08: dataset-transformation Step 9 passes
//!   `--cell 2=<project-dir>/scripts/transform_fn.py`:
//!   `ap_doc_08_dataset_transformation_step_9_passes_the_module_cell`
//! - AP-DOC-09: the manual edit points stay prose (section 4.4.3):
//!   `ap_doc_09_manual_edit_points_stay_prose`
//! - AP-DOC-10: templates are untouched and these tests never launch Python:
//!   `ap_doc_10_templates_match_their_mirrors_and_tests_never_launch_python`
//! - AP-DOC-11 (#5283): one values/assign/manual paragraph per configuration page:
//!   `ap_doc_11_configuration_pages_keep_one_values_paragraph`
//! - AP-DOC-12 (#5284): the HuggingFace guide collects an ARN, never a token:
//!   `ap_doc_12_huggingface_guide_collects_an_arn_never_a_token`
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

const REFS: &str = "skills/core-skills/aws-ai-ml/references";
const MIRROR_REFS: &str = "plugins/aws-core/skills/aws-ai-ml/references";

/// The four code output guides (paths under `references/`).
const GUIDES: [&str; 4] = [
    "finetuning/references/code_output_guide.md",
    "model-deployment/references/code_output_guide.md",
    "model-evaluation/references/code_output_guide.md",
    "dataset-transformation/references/code_output_guide.md",
];
/// Training and processing configuration pages.
const CONFIGS: [&str; 2] = [
    "training-jobs/references/configuration.md",
    "processing-jobs/references/configuration.md",
];
/// Every other page of design section 1.2 that describes rendering a template.
const OTHER_RENDER_PAGES: [&str; 19] = [
    "finetuning/overview.md",
    "training-jobs/overview.md",
    "processing-jobs/overview.md",
    "dataset-transformation/overview.md",
    "finetuning/references/continuous_customization.md",
    "model-deployment/references/deploy-jumpstart-sagemaker.md",
    "model-deployment/references/deploy-nova-bedrock.md",
    "model-deployment/references/deploy-nova-sagemaker.md",
    "model-deployment/references/deploy-oss-bedrock.md",
    "model-deployment/references/deploy-oss-sagemaker.md",
    "model-deployment/references/optimization/benchmark-workflow.md",
    "model-deployment/references/optimization/huggingface-to-s3.md",
    "model-deployment/references/optimization/recommendation-workflow.md",
    "model-evaluation/references/custom-scorer-evaluation.md",
    "model-evaluation/references/llmaaj-evaluation.md",
    "finetuning/references/rlvr_reward_function.md",
    "model-deployment/references/optimization/interpreting-results.md",
    "model-deployment/references/optimization/recommendation-deploy.md",
    "model-deployment/references/optimization/recommendation-options.md",
];
/// Reward-template caller pages (R15, AP-DOC-03).
const REWARD_PAGES: [&str; 2] = [
    "model-evaluation/references/create-reward-function.md",
    "finetuning/references/rlvr_reward_function.md",
];

/// Sentences that told an agent to assemble a notebook or script by hand. The pages
/// now direct the agent to `toolkit ai-payloads render`; none may keep these.
const HAND_RENDERING: [&str; 14] = [
    "Use your file write tool to create the complete notebook JSON",
    "Do NOT use bash commands, shell scripts, or `echo`/`cat` piping",
    "2-space JSON indentation",
    "Each source line is a separate string ending with",
    "Escape quotes:",
    "Replace `[PLACEHOLDER]` values with user-specific configuration",
    "Split on these markers",
    "Each cell's content comes from",
    "split on the `# Cell N:` comments",
    "substitute the collected values into the placeholders, and write the cells",
    "use the template's `# Cell 0 [markdown]:` cell as the section divider",
    "Write the code from the template following the rules in",
    "Use `# %%` cell markers to preserve logical sections",
    "use the next available number in `<project-dir>/scripts/`",
];

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn page(rel: &str) -> String {
    fs::read_to_string(root().join(REFS).join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}
fn mirror_page(rel: &str) -> String {
    fs::read_to_string(root().join(MIRROR_REFS).join(rel))
        .unwrap_or_else(|e| panic!("mirror {rel}: {e}"))
}
fn render_pages() -> Vec<&'static str> {
    GUIDES
        .iter()
        .chain(CONFIGS.iter())
        .chain(OTHER_RENDER_PAGES.iter())
        .copied()
        .collect()
}
/// Every markdown page under `dir`, as paths relative to it.
fn markdown_files(dir: &Path, base: &Path, found: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            markdown_files(&path, base, found);
        } else if path.extension().is_some_and(|e| e == "md") {
            found.push(
                path.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}
fn all_pages() -> Vec<String> {
    let base = root().join(REFS);
    let mut found = Vec::new();
    markdown_files(&base, &base, &mut found);
    found
}
fn text(out: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ---- AP-DOC-01 ------------------------------------------------------------

#[test]
fn ap_doc_01_render_pages_name_the_render_command() {
    for rel in render_pages() {
        for (label, body) in [("canonical", page(rel)), ("mirror", mirror_page(rel))] {
            assert!(
                body.contains("toolkit ai-payloads render"),
                "{label} {rel} does not name `toolkit ai-payloads render`"
            );
        }
    }
}

#[test]
fn ap_doc_01_guides_document_the_values_file_and_catalog() {
    for rel in GUIDES {
        let body = page(rel);
        for needle in [
            "toolkit ai-payloads templates",
            "--values",
            "--mode notebook",
            "--mode script",
            "--project-dir",
            "\"values\"",
            "\"assign\"",
            "output:",
        ] {
            assert!(body.contains(needle), "{rel} lacks `{needle}`");
        }
    }
    for rel in CONFIGS {
        let body = page(rel);
        assert!(
            body.contains("\"values\"") || body.contains("values file"),
            "{rel} does not tell the agent to put placeholders in the values file"
        );
    }
}

// ---- AP-DOC-02 ------------------------------------------------------------

#[test]
fn ap_doc_02_sync_plugin_skills_check_is_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["sync", "plugin-skills", "--check"])
        .current_dir(root())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
}

#[test]
fn ap_doc_02_every_reference_page_matches_its_mirror() {
    let pages = all_pages();
    assert!(pages.len() > 50, "found only {} pages", pages.len());
    for rel in pages {
        assert_eq!(
            page(&rel).as_bytes(),
            mirror_page(&rel).as_bytes(),
            "{rel} differs from its plugins/ mirror; run `toolkit sync plugin-skills`"
        );
    }
}

// ---- AP-DOC-03 ------------------------------------------------------------

#[test]
fn ap_doc_03_reward_pages_name_the_checker_and_their_formats() {
    for rel in REWARD_PAGES {
        let body = page(rel);
        assert!(
            body.contains("toolkit ai-payloads reward check"),
            "{rel} does not name `toolkit ai-payloads reward check`"
        );
        assert!(
            body.contains("toolkit ai-payloads reward sample"),
            "{rel} does not name `toolkit ai-payloads reward sample`"
        );
        for needle in ["--event", "--response", "--format"] {
            assert!(body.contains(needle), "{rel} lacks `{needle}`");
        }
    }
    let evaluation = page("model-evaluation/references/create-reward-function.md");
    for format in ["sagemaker-eval", "nova"] {
        assert!(
            evaluation.contains(&format!("--format {format}")),
            "create-reward-function.md lacks `--format {format}`"
        );
    }
    let rlvr = page("finetuning/references/rlvr_reward_function.md");
    for format in ["verl", "openai", "nova"] {
        assert!(
            rlvr.contains(&format!("--format {format}")),
            "rlvr_reward_function.md lacks `--format {format}`"
        );
    }
    for rel in REWARD_PAGES {
        assert!(
            page(rel).contains("toolkit ai-payloads reward check"),
            "{rel}"
        );
        assert!(
            mirror_page(rel).contains("toolkit ai-payloads reward check"),
            "mirror {rel}"
        );
    }
}

// ---- AP-DOC-04 ------------------------------------------------------------

/// Command lines in fenced code blocks that start with `toolkit ai-payloads`,
/// with `\` continuations joined, as `(page, line)`.
fn documented_commands() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for rel in all_pages() {
        for line in commands_in(&page(&rel)) {
            found.push((rel.clone(), line));
        }
    }
    found
}
/// Single-backtick code spans outside fenced blocks, one per `...` pair on a line.
fn code_spans(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced = false;
    for raw in body.lines() {
        if raw.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut parts = raw.split('`');
        parts.next();
        while let (Some(span), Some(_)) = (parts.next(), parts.next()) {
            found.push(span.trim().to_string());
        }
    }
    found
}

/// The command lines of one page body (see `documented_commands`).
fn commands_in(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    {
        let mut fenced = false;
        let mut pending: Option<String> = None;
        for raw in body.lines() {
            if raw.trim_start().starts_with("```") {
                fenced = !fenced;
                pending = None;
                continue;
            }
            if !fenced {
                continue;
            }
            let line = raw.trim();
            let joined = match pending.take() {
                Some(mut so_far) => {
                    so_far.push(' ');
                    so_far.push_str(line);
                    so_far
                }
                None => line.to_string(),
            };
            if !joined.starts_with("toolkit ai-payloads") {
                continue;
            }
            if let Some(stripped) = joined.strip_suffix('\\') {
                pending = Some(stripped.trim_end().to_string());
            } else {
                found.push(joined);
            }
        }
    }
    found
}

/// Run one documented command line against the real `toolkit` binary in a scratch
/// directory; returns the exit code and the combined output.
fn cli_exit(line: &str) -> (Option<i32>, String) {
    let scratch = std::env::temp_dir().join(format!(
        "ap-doc-04-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let project = scratch.join("docs-project");
    fs::create_dir_all(&project).unwrap();
    let args = concrete_args(line, &project, &scratch);
    assert_eq!(&args[..2], ["toolkit", "ai-payloads"], "{line}");
    let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(&args[1..])
        .current_dir(root())
        .output()
        .unwrap();
    fs::remove_dir_all(&scratch).unwrap();
    (out.status.code(), text(&out))
}

/// The routes of `toolkit ai-payloads`, as the words after the command name.
const ROUTES: [&str; 4] = ["templates", "render", "reward sample", "reward check"];

/// The words of a command line after `toolkit ai-payloads`: its route, if any, and
/// the arguments after it.
fn split_route(line: &str) -> Option<(&'static str, Vec<&str>)> {
    let words: Vec<&str> = line.split_whitespace().skip(2).collect();
    ROUTES.iter().find_map(|route| {
        let n = route.split(' ').count();
        (words.len() >= n && words[..n].join(" ") == *route).then(|| (*route, words[n..].to_vec()))
    })
}

/// The synopsis lines of one route in the real CLI usage text: the route line and
/// its indented continuation lines.
fn route_synopsis(route: &str) -> String {
    let head = format!("  toolkit ai-payloads {route}");
    let mut found = String::new();
    let mut inside = false;
    for line in toolkit_cli::USAGE.lines() {
        if line.starts_with(&head) {
            inside = true;
        } else if inside && !line.starts_with("    ") {
            break;
        }
        if inside {
            found.push_str(line);
            found.push('\n');
        }
    }
    assert!(!found.is_empty(), "no usage line for `{route}`");
    found
}

/// Flags of a command line that its own route does not take (a flag valid on
/// another route does not count).
fn route_flag_errors(line: &str) -> Vec<String> {
    let Some((route, args)) = split_route(line) else {
        return vec![format!("no known route in `{line}`")];
    };
    let synopsis = route_synopsis(route);
    args.iter()
        .filter(|a| a.starts_with("--"))
        .filter(|a| !synopsis.contains(a.split('=').next().unwrap()))
        .map(|a| format!("`{a}` is not a flag of `toolkit ai-payloads {route}`"))
        .collect()
}

/// Replace the documentation placeholders with real, safe paths.
fn concrete_args(line: &str, project: &Path, scratch: &Path) -> Vec<String> {
    let skill = root().join("skills/core-skills/aws-ai-ml");
    let mut line = line
        .replace("<project-dir>", &project.to_string_lossy())
        .replace("<skill-dir>", &skill.to_string_lossy());
    while let Some(open) = line.find('<') {
        let close = open + line[open..].find('>').expect("unclosed <placeholder>");
        let name = line[open + 1..close].to_string();
        line.replace_range(open..=close, &format!("{}/{name}", scratch.display()));
    }
    line.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn ap_doc_04_documented_command_lines_parse_against_the_real_cli() {
    let commands = documented_commands();
    assert!(
        commands.len() >= 8,
        "only {} documented commands",
        commands.len()
    );
    for (rel, line) in commands {
        assert!(
            !line.contains('|')
                && !line.contains('[')
                && !line.contains(']')
                && !line.contains("..."),
            "{rel}: `{line}` is a synopsis, not a concrete command line"
        );
        assert!(
            !line.contains('\'') && !line.contains('"'),
            "{rel}: `{line}`"
        );
        let (code, output) = cli_exit(&line);
        // Exit 2 is the shared router's usage error: an unknown flag, a missing
        // required argument or a bad value shape. Any other exit proves the line
        // parses; missing files (values, event, response) are data errors, exit 1.
        assert!(
            code == Some(0) || code == Some(1),
            "{rel}: `{line}` is not valid against the real CLI usage (exit {code:?})\n{output}"
        );
        let wrong = route_flag_errors(&line);
        assert!(wrong.is_empty(), "{rel}: `{line}`: {wrong:?}");
    }
}

/// The slice-5 follow-up #5282: full inline commands (code spans in prose, outside
/// fences) are documented commands too and go through the same checks.
#[test]
fn ap_doc_04_inline_commands_are_documented_commands() {
    let commands = documented_commands();
    for (page_suffix, needle) in [
        ("continuous_customization.md", "dpo.py"),
        ("benchmark-workflow.md", "optimize-benchmark.py"),
        ("huggingface-to-s3.md", "optimize-huggingface-to-s3.py"),
        ("recommendation-workflow.md", "optimize-recommendation.py"),
        ("training-jobs/overview.md", "recipe_training.py"),
        ("processing-jobs/overview.md", "script_processing.py"),
    ] {
        assert!(
            commands.iter().any(|(rel, line)| rel.ends_with(page_suffix)
                && line.contains("toolkit ai-payloads render")
                && line.contains(needle)),
            "no inline render command with `{needle}` is extracted from {page_suffix}"
        );
    }
    assert!(
        commands
            .iter()
            .any(|(_, l)| l.starts_with("toolkit ai-payloads templates --root ")),
        "the inline `templates --root <skill-dir>` command is not extracted"
    );
}

/// Bare mentions such as `toolkit ai-payloads render` are not commands to run, but
/// each must still name a route the CLI really has.
#[test]
fn ap_doc_04_bare_inline_mentions_name_real_routes() {
    let mut seen = 0;
    for rel in all_pages() {
        for span in code_spans(&page(&rel)) {
            if !span.starts_with("toolkit ai-payloads") {
                continue;
            }
            seen += 1;
            let (route, args) = split_route(&span)
                .unwrap_or_else(|| panic!("{rel}: `{span}` names no route of the CLI"));
            assert!(
                !route_synopsis(route).is_empty(),
                "{rel}: `{span}` ({args:?})"
            );
        }
    }
    assert!(seen >= 40, "only {seen} inline mentions");
}

/// A flag that is valid on another route must not pass: changing an inline render
/// example to use `--batch` (a `reward check` flag) fails AP-DOC-04 twice, in the
/// route-specific synopsis check and against the real CLI.
#[test]
fn ap_doc_04_inline_render_with_a_foreign_route_flag_fails() {
    let rel = "finetuning/references/continuous_customization.md";
    let body = page(rel);
    let inline = commands_in(&body)
        .into_iter()
        .find(|l| l.starts_with("toolkit ai-payloads render") && l.contains("--append"))
        .expect("the inline render example of continuous_customization.md is extracted");
    assert!(route_flag_errors(&inline).is_empty(), "{inline}");
    assert!(
        toolkit_cli::USAGE.contains("--batch"),
        "--batch must exist on another route for this regression to mean anything"
    );

    let mutated_body = body.replace(&inline, &format!("{inline} --batch"));
    assert_ne!(mutated_body, body, "the mutation changed nothing");
    let mutated = commands_in(&mutated_body)
        .into_iter()
        .find(|l| l.contains("--batch"))
        .expect("the mutated inline render example is extracted");
    let wrong = route_flag_errors(&mutated);
    assert!(
        wrong.iter().any(|e| e.contains("--batch")),
        "route check did not flag --batch: {wrong:?}"
    );
    let (code, output) = cli_exit(&mutated);
    assert_eq!(
        code,
        Some(2),
        "the real CLI accepted --batch on render\n{output}"
    );
}

#[test]
fn ap_doc_04_documented_flags_are_in_the_cli_synopsis() {
    let usage = toolkit_cli::USAGE;
    for rel in all_pages() {
        let body = page(&rel);
        for line in body.lines().filter(|l| l.contains("toolkit ai-payloads")) {
            for token in line.split_whitespace() {
                let flag: String = token
                    .trim_matches(|c: char| !(c == '-' || c.is_ascii_alphanumeric()))
                    .to_string();
                if flag.starts_with("--") {
                    assert!(
                        usage.contains(&flag),
                        "{rel}: `{flag}` is not in the CLI usage text: {line}"
                    );
                }
            }
        }
    }
}

#[test]
fn ap_doc_04_documented_command_forms_are_all_present() {
    let commands: Vec<String> = documented_commands().into_iter().map(|(_, l)| l).collect();
    for form in [
        "toolkit ai-payloads templates",
        "toolkit ai-payloads render",
        "--mode notebook",
        "--mode script",
        "--append",
        "--consent ACCEPT_EULA",
        "--cell 2=<project-dir>/scripts/transform_fn.py",
        "toolkit ai-payloads reward sample --format",
        "toolkit ai-payloads reward check --format",
    ] {
        assert!(
            commands.iter().any(|c| c.contains(form)),
            "no documented command line contains `{form}`"
        );
    }
}

// ---- AP-DOC-05 ------------------------------------------------------------

#[test]
fn ap_doc_05_no_page_instructs_hand_rendering() {
    for rel in all_pages() {
        for (label, body) in [("canonical", page(&rel)), ("mirror", mirror_page(&rel))] {
            for phrase in HAND_RENDERING {
                assert!(
                    !body.contains(phrase),
                    "{label} {rel} still instructs hand rendering: `{phrase}`"
                );
            }
        }
    }
    // Pages that name a template without the render command are routing pages; a
    // page that tells the agent to read a template and write cells is not.
    for rel in render_pages() {
        let body = page(rel);
        for line in body.lines() {
            let lower = line.to_lowercase();
            let negated = lower.contains("do not") || lower.contains("never");
            assert!(
                negated
                    || !(lower.contains("write the cells")
                        || lower.contains("write the notebook cells")),
                "{rel}: `{line}`"
            );
        }
    }
}

// ---- AP-DOC-06 ------------------------------------------------------------

#[test]
fn ap_doc_06_eula_consent_step_names_the_consent_flag() {
    for rel in GUIDES.iter().chain(
        [
            "model-deployment/references/deploy-oss-sagemaker.md",
            "finetuning/overview.md",
        ]
        .iter(),
    ) {
        let body = page(rel);
        assert!(
            body.contains("--consent ACCEPT_EULA"),
            "{rel} lacks `--consent ACCEPT_EULA`"
        );
    }
    for rel in GUIDES {
        let body = page(rel);
        let consent = body.find("--consent ACCEPT_EULA").unwrap();
        let window = &body[consent.saturating_sub(700)..(consent + 700).min(body.len())];
        assert!(
            window.contains("explicitly accepted"),
            "{rel}: the consent step does not require the user's explicit acceptance"
        );
        assert!(
            window.contains("Never") || window.contains("never"),
            "{rel}: the consent step does not forbid auto-accepting"
        );
    }
}

// ---- AP-DOC-07 ------------------------------------------------------------

#[test]
fn ap_doc_07_append_step_saves_appends_then_opens() {
    for rel in GUIDES {
        let body = page(rel);
        let save = body
            .find("save the notebook")
            .unwrap_or_else(|| panic!("{rel} does not ask the user to save the notebook"));
        let append = save
            + body[save..]
                .find("--append")
                .unwrap_or_else(|| panic!("{rel} does not run `--append` after the save step"));
        assert!(
            body[append..].contains("`open:`"),
            "{rel} does not tell the user to open the `open:` path"
        );
        assert!(
            body.contains("versioned") || body.contains("-01.ipynb"),
            "{rel} does not explain that the append publishes a new versioned notebook"
        );
        assert!(
            !body.contains("append cells to the existing notebook, or create a new one?")
                || body.contains("--new"),
            "{rel}: the old append question must name `--new` for a fresh notebook"
        );
    }
}

// ---- AP-DOC-08 ------------------------------------------------------------

#[test]
fn ap_doc_08_dataset_transformation_step_9_passes_the_module_cell() {
    let body = page("dataset-transformation/overview.md");
    let step9 = body.find("### Step 9").expect("Step 9 heading");
    let step10 = body.find("### Step 10").expect("Step 10 heading");
    let section = &body[step9..step10];
    assert!(
        section.contains("--cell 2=<project-dir>/scripts/transform_fn.py"),
        "Step 9 does not pass --cell 2=<project-dir>/scripts/transform_fn.py"
    );
    assert!(
        section.contains("toolkit ai-payloads render"),
        "Step 9 does not name the render command"
    );
    assert!(
        !section.contains("add a `%%writefile <project-dir>/scripts/<script_name>.py` code cell"),
        "Step 9 still asks the agent to hand-write the writefile cell"
    );
    // The module is a caller-owned prerequisite written in Step 7.
    let step7 = &body[body.find("### Step 7").unwrap()..body.find("### Step 8").unwrap()];
    assert!(
        step7.contains("transform_fn.py"),
        "Step 7 no longer writes transform_fn.py"
    );
}

// ---- AP-DOC-09 ------------------------------------------------------------

#[test]
fn ap_doc_09_manual_edit_points_stay_prose() {
    for rel in GUIDES {
        let body = page(rel);
        assert!(
            body.contains("manual:"),
            "{rel} does not tell the agent about the `manual:` edit points"
        );
        assert!(
            body.contains("rendered file") || body.contains("rendered notebook"),
            "{rel} does not tell the agent to make manual edits on the rendered file"
        );
    }
    let finetuning = page("finetuning/overview.md");
    for needle in [
        "accept_eula=ACCEPT_EULA,",
        "max_epochs",
        "lr_warmup_steps_ratio",
    ] {
        assert!(
            finetuning.contains(needle),
            "finetuning/overview.md lost the manual edit instruction mentioning `{needle}`"
        );
    }
    let jumpstart = page("model-deployment/references/deploy-jumpstart-sagemaker.md");
    assert!(
        jumpstart.contains("accept_eula"),
        "deploy-jumpstart-sagemaker.md lost its accept_eula dict-member instruction"
    );
}

// ---- AP-DOC-10 ------------------------------------------------------------

#[test]
fn ap_doc_10_templates_match_their_mirrors_and_tests_never_launch_python() {
    let base = root().join(REFS);
    let mirror = root().join(MIRROR_REFS);
    let mut checked = 0;
    for entry in fs::read_dir(&base).unwrap() {
        let area = entry.unwrap().path();
        let templates = area.join("code_templates");
        if !templates.is_dir() {
            continue;
        }
        for template in fs::read_dir(&templates).unwrap() {
            let template = template.unwrap().path();
            let rel = template.strip_prefix(&base).unwrap();
            assert_eq!(
                fs::read(&template).unwrap(),
                fs::read(mirror.join(rel)).unwrap(),
                "{} differs from its mirror",
                rel.display()
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 23, "the 23 TPL templates");
    let source =
        fs::read_to_string(root().join("crates/toolkit-cli/tests/ai_payloads_docs.rs")).unwrap();
    // The needles are escaped in this source, so the test does not match itself.
    for needle in ["Command::new(\"py", "Command::new(\"python"] {
        assert_eq!(source.matches(needle).count(), 0, "{needle}");
    }
}

// ---- S5 follow-ups (#5296): #5283 and #5284 --------------------------------

/// #5283: each training and processing configuration page keeps one complete
/// values/assign/manual/NOTEBOOK_ONLY/missing-value paragraph, in the canonical
/// page and in its mirror.
#[test]
fn ap_doc_11_configuration_pages_keep_one_values_paragraph() {
    for rel in CONFIGS {
        for (label, body) in [("canonical", page(rel)), ("mirror", mirror_page(rel))] {
            let paragraphs: Vec<&str> = body
                .split("\n\n")
                .filter(|p| p.starts_with("Every bracketed placeholder below"))
                .collect();
            assert_eq!(
                paragraphs.len(),
                1,
                "{label} {rel} repeats the values-file paragraph"
            );
            let paragraph = paragraphs[0];
            for needle in [
                "a key under `values` in the values file",
                "written without the brackets",
                "`{\"values\": {\"INSTANCE_TYPE\": \"ml.m5.xlarge\"}}`",
                "`[PROJECT_DIR]` defaults to the project directory",
                "`toolkit ai-payloads templates --root <skill-dir>`",
                "`target` lines",
                "`assign`",
                "`manual:`",
                "`# NOTEBOOK_ONLY`",
                "A missing value stops the render before any file is written.",
            ] {
                assert!(
                    paragraph.contains(needle),
                    "{label} {rel}: the kept paragraph lacks `{needle}`"
                );
            }
            for sentence in ["`# NOTEBOOK_ONLY`", "A missing value stops the render"] {
                assert_eq!(
                    body.matches(sentence).count(),
                    1,
                    "{label} {rel}: `{sentence}` is stated more than once"
                );
            }
        }
    }
}

/// #5284: the HuggingFace download guide collects the Secrets Manager secret ARN
/// (or `null` for a public model) and never asks the agent for a token.
#[test]
fn ap_doc_12_huggingface_guide_collects_an_arn_never_a_token() {
    let rel = "model-deployment/references/optimization/huggingface-to-s3.md";
    for (label, body) in [("canonical", page(rel)), ("mirror", mirror_page(rel))] {
        let step1 = body
            .split("## Step 1")
            .nth(1)
            .and_then(|s| s.split("## Step 2").next())
            .unwrap_or_else(|| panic!("{label}: Step 1 is missing"));
        assert!(
            !step1.contains("ask the user for a HuggingFace access token"),
            "{label}: Step 1 still asks the user for the token itself"
        );
        for needle in ["Secrets Manager", "ARN", "`null`"] {
            assert!(step1.contains(needle), "{label}: Step 1 lacks `{needle}`");
        }
        assert!(
            step1.contains("never ask for") && step1.contains("token itself"),
            "{label}: Step 1 must forbid collecting the token itself"
        );
        // The Step 2 slot text keeps its own rule.
        assert!(
            body.contains("`render` refuses a literal token"),
            "{label}: Step 2 lost the literal-token refusal"
        );
    }
}
