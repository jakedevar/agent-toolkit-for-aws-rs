//! Workspace contract for untrusted text in diagnostics (#4639).
//!
//! Three slices (#4602 ai-data, #4625 startup scoring, #4634 cloudwatch) failed
//! review for echoing caller or payload text unescaped. This test runs EVERY
//! entry of [`COMMANDS`] through the real `toolkit` binary with hostile argv
//! values and with invalid file/directory payloads that carry ESC, BEL, CR,
//! U+202E and a fake secret marker, and asserts that none of it reaches stdout
//! or stderr: no raw control character (newline is the line separator and the
//! only one allowed), no bidirectional override and no payload marker.
//!
//! A new slice that registers a command is covered automatically. If this test
//! fails for a command, do not echo the offending value: report the field,
//! location and type, or pass the text through `toolkit_core::quote_untrusted`
//! (see the README contributor note) and keep payload contents out entirely.
//!
//! Every spawn runs with an empty environment (no credentials, no endpoint),
//! stdin closed and its working directory inside a scratch directory, so the
//! test is offline and cannot write outside it. Commands that need the network
//! or credentials must fail closed, which is exactly what is asserted.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use toolkit_cli::{CommandSpec, COMMANDS};
use toolkit_core::is_unsafe_char;

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

/// A fake secret. It never appears in an argument; it only rides in payloads.
const SECRET: &str = "FAKE-SECRET-4639-AKIAZZZZZZZZZZZZZZZZ";
/// Terminal and line control plus a right-to-left override.
const HOSTILE: &str = "\u{1b}[31mFORGED\u{7}\r\u{202e}gpj.exe\nerror: injected line";
/// A longer marker used only in argument values, to detect echo of a value.
const ARG_MARKER: &str = "ARGVECHOMARKER4639";

const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Bidirectional controls and other invisible reordering characters, spelled
/// out here independently of the shared helper so the test is not circular.
fn is_bidi_or_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        // Unique per call: the tests of this file run in parallel in one process.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-diag-contract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The hostile payload text: invalid in every format the commands read, with
/// the controls and the secret inside what a parser would quote back.
fn payload_text() -> String {
    format!("{{\"name\": \"{HOSTILE}{SECRET}\", \"k\": [1, 2,\n---\n\u{feff}key: {SECRET}\u{1b}]\u{7}\r\n")
}

/// Writes the payload file and the payload directory (a plausible plugin,
/// skill, migration and run tree whose every file is hostile and invalid).
fn write_payloads(root: &Path) -> (PathBuf, PathBuf) {
    let file = root.join("payload.json");
    fs::write(&file, payload_text()).expect("write payload file");
    let dir = root.join("payload");
    for relative in [
        "plugin.json",
        ".claude-plugin/plugin.json",
        ".mcp.json",
        "manifest.json",
        "baseline.json",
        "config.json",
        "model.json",
        "loop-state.json",
        "pricing.json",
        "estimation-infra.json",
        "estimation-ai.json",
        "aws-design.json",
        "metrics.json",
        "data.json",
        "data.jsonl",
        "data.csv",
        "data.parquet",
        "spec.md",
        "spec.yaml",
        "SKILL.md",
        "skills/hostile/SKILL.md",
        "skills/shared/README.md",
        "plugins/hostile/skills/hostile/SKILL.md",
        "main.tf",
        "plan.tfplan.json",
        "README.md",
        "migration/report.md",
        "run/state.json",
    ] {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create payload dir");
        fs::write(&path, payload_text()).expect("write payload");
    }
    (file, dir)
}

/// What one spawn produced.
struct Run {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn spawn_toolkit(args: &[String], cwd: &Path) -> Run {
    let mut child: Child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn toolkit");
    // Drain both pipes on threads so a chatty command cannot block on a full pipe.
    let mut out_pipe = child.stdout.take().expect("stdout pipe");
    let mut err_pipe = child.stderr.take().expect("stderr pipe");
    let out_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out_pipe.read_to_end(&mut bytes);
        bytes
    });
    let err_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err_pipe.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(status) => break status.code(),
            None if started.elapsed() > SPAWN_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    Run {
        status,
        stdout: out_thread.join().expect("stdout thread"),
        stderr: err_thread.join().expect("stderr thread"),
    }
}

/// One argument-filling strategy.
#[derive(Clone, Copy, Debug)]
enum Fill {
    /// Hostile text in every value-flag value and positional.
    Text,
    /// Each value is `<scratch>/<hostile>`: a path that does not exist.
    HostilePath,
    /// Each value is the invalid payload file.
    PayloadFile,
    /// Each value is the directory of invalid payload files.
    PayloadDir,
    /// `--flag=<hostile>` spelling for value flags; hostile positionals.
    Attached,
}

const FILLS: [Fill; 5] = [
    Fill::Text,
    Fill::HostilePath,
    Fill::PayloadFile,
    Fill::PayloadDir,
    Fill::Attached,
];

/// Flags that spend money, deploy or submit work are never passed: the test is
/// about what is printed, not about exercising those paths.
fn flag_is_excluded(flag: &str) -> bool {
    ["yes", "deploy", "spend"]
        .iter()
        .any(|word| flag.contains(word))
}

fn value_for(mode: Fill, scratch: &Path, file: &Path, dir: &Path) -> String {
    match mode {
        Fill::Text | Fill::Attached => format!("{HOSTILE}{ARG_MARKER}"),
        Fill::HostilePath => format!("{}/{HOSTILE}{ARG_MARKER}", scratch.display()),
        Fill::PayloadFile => file.display().to_string(),
        Fill::PayloadDir => dir.display().to_string(),
    }
}

/// Every value flag of `spec`: the union of `value_flags` and
/// `repeatable_flags` (a flag may be declared only in the latter, #4697), each
/// once, in declaration order.
fn all_value_flags(spec: &CommandSpec) -> Vec<&'static str> {
    let mut flags: Vec<&'static str> = Vec::new();
    for flag in spec.value_flags.iter().chain(spec.repeatable_flags) {
        if !flags.contains(flag) {
            flags.push(flag);
        }
    }
    flags
}

/// The words that pass `value` to `flag`. A plain flag is passed once, in the
/// fill's spelling. A repeatable flag is passed in two occurrences, one per
/// spelling (`--f V` then `--f=V`), so a handler that echoes any occurrence of
/// the flag, not only the first or the last, is caught.
fn flag_words(spec: &CommandSpec, flag: &str, value: &str, mode: Fill) -> Vec<String> {
    let spaced = [flag.to_string(), value.to_string()];
    let attached = format!("{flag}={value}");
    if spec.repeatable_flags.contains(&flag) {
        let mut words = Vec::from(spaced);
        words.push(attached);
        words
    } else if matches!(mode, Fill::Attached) {
        vec![attached]
    } else {
        Vec::from(spaced)
    }
}

/// The argument vectors that exercise `spec` for one fill.
fn argv_variants(
    spec: &CommandSpec,
    mode: Fill,
    scratch: &Path,
    file: &Path,
    dir: &Path,
) -> Vec<Vec<String>> {
    let value = value_for(mode, scratch, file, dir);
    let base: Vec<String> = spec.path.iter().map(|word| (*word).to_string()).collect();
    let mut with_values = base.clone();
    let value_flags: Vec<&'static str> = all_value_flags(spec)
        .into_iter()
        .filter(|flag| !flag_is_excluded(flag))
        .collect();
    for flag in &value_flags {
        with_values.extend(flag_words(spec, flag, &value, mode));
    }
    let positionals = spec.max_positionals.min(3);
    for _ in 0..positionals {
        with_values.push(value.clone());
    }
    let mut with_flags = with_values.clone();
    for flag in spec.flags.iter().filter(|flag| !flag_is_excluded(flag)) {
        with_flags.push((*flag).to_string());
    }
    let mut variants = vec![with_values];
    if with_flags != variants[0] {
        variants.push(with_flags);
    }
    // A single value flag at a time, so a command that validates the first
    // flag it sees and stops is still pushed through every later one.
    for flag in &value_flags {
        let mut one = base.clone();
        one.extend(flag_words(spec, flag, &value, mode));
        variants.push(one);
        if spec.repeatable_flags.contains(flag) {
            // A benign first occurrence, then the hostile one in each spelling:
            // a handler that validates the first value and stops, or echoes
            // only a later one, is still pushed through the later occurrence.
            for hostile_second in [
                vec![(*flag).to_string(), value.clone()],
                vec![format!("{flag}={value}")],
            ] {
                let mut later = base.clone();
                later.extend([(*flag).to_string(), "benign".to_string()]);
                later.extend(hostile_second);
                variants.push(later);
            }
        }
    }
    variants
}

/// Arguments that must be usage errors, with hostile unknown flags and extra
/// words (skipped for passthrough commands, which take every word).
fn usage_variants(spec: &CommandSpec) -> Vec<Vec<String>> {
    let base: Vec<String> = spec.path.iter().map(|word| (*word).to_string()).collect();
    let mut unknown_flag = base.clone();
    unknown_flag.push(format!("--{HOSTILE}{ARG_MARKER}"));
    let mut variants = vec![unknown_flag];
    if !spec.passthrough && spec.max_positionals < 8 {
        let mut extra = base.clone();
        for _ in 0..=spec.max_positionals {
            extra.push(format!("{HOSTILE}{ARG_MARKER}"));
        }
        variants.push(extra);
    }
    if let Some(flag) = all_value_flags(spec).first() {
        // A value flag at the end of the arguments has no value.
        let mut dangling = base;
        dangling.push((*flag).to_string());
        variants.push(dangling);
    }
    variants
}

/// Every reason `stream` is not safe terminal text.
fn violations(stream: &str, label: &str, args: &[String], found: &mut BTreeSet<String>) {
    // `skills list` separates name and description with a tab on stdout.
    let tab_allowed =
        label == "stdout" && args.starts_with(&["skills".to_string(), "list".to_string()]);
    let mut push = |what: &str| {
        found.insert(format!("{what} in {label} for {}", display_args(args)));
    };
    for c in stream.chars() {
        if c == '\n' || (c == '\t' && tab_allowed) {
            continue;
        }
        if c.is_control() {
            push(&format!("raw control U+{:04X}", u32::from(c)));
        } else if is_bidi_or_invisible(c) || is_unsafe_char(c) {
            push(&format!("bidi/invisible U+{:04X}", u32::from(c)));
        }
    }
    // Payload contents are never echoed. An argument value may appear in a
    // diagnostic (an error naming the path it could not read) but only escaped,
    // which the control and forged-line checks enforce.
    for marker in [SECRET, "FAKE-SECRET-4639", "AKIAZZZZZZZZ"] {
        if stream.contains(marker) {
            push(&format!("marker {marker:?}"));
        }
    }
    if stream
        .lines()
        .any(|line| line.starts_with("error: injected line"))
    {
        push("forged line from an argument newline");
    }
}

/// The argument list as a safe, single-line description for the failure text.
fn display_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| toolkit_core::quote_untrusted_bounded(arg, 60))
        .collect::<Vec<_>>()
        .join(" ")
}

fn check(args: &[String], scratch: &Path, found: &mut BTreeSet<String>) {
    let run = spawn_toolkit(args, scratch);
    if run.status.is_none() {
        found.insert(format!(
            "timed out or killed by signal: {}",
            display_args(args)
        ));
    }
    for (label, bytes) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
        match std::str::from_utf8(bytes) {
            Ok(text) => violations(text, label, args, found),
            Err(_) => {
                found.insert(format!("{label} is not UTF-8 for {}", display_args(args)));
            }
        }
    }
}

#[test]
fn every_registered_command_keeps_untrusted_text_out_of_its_output() {
    let scratch = Scratch::new();
    let (file, dir) = write_payloads(&scratch.0);
    assert!(!COMMANDS.is_empty());

    let mut found = BTreeSet::new();
    let mut spawns = 0_usize;
    for spec in COMMANDS {
        let mut variants: Vec<Vec<String>> = Vec::new();
        for mode in FILLS {
            variants.extend(argv_variants(spec, mode, &scratch.0, &file, &dir));
        }
        variants.extend(usage_variants(spec));
        variants.sort();
        variants.dedup();
        for args in &variants {
            check(args, &scratch.0, &mut found);
            spawns += 1;
        }
    }
    // The router itself: an unknown command and an unknown top-level word.
    for args in [
        vec![format!("{HOSTILE}{ARG_MARKER}")],
        vec![format!("--{HOSTILE}{ARG_MARKER}")],
        vec!["skills".to_string(), format!("{HOSTILE}{ARG_MARKER}")],
    ] {
        check(&args, &scratch.0, &mut found);
        spawns += 1;
    }
    assert!(
        spawns > COMMANDS.len(),
        "the contract must actually run commands"
    );
    assert!(
        found.is_empty(),
        "untrusted text reached stdout/stderr ({} distinct findings over {spawns} runs); \
         see the toolkit_core::quote_untrusted contributor note:\n{}",
        found.len(),
        found.iter().cloned().collect::<Vec<_>>().join("\n")
    );
}

/// The detector must see what it is meant to see: a self-test so a mistake in
/// the checker cannot turn the contract into a vacuous pass.
#[test]
fn the_detector_flags_each_hostile_class_and_accepts_escaped_text() {
    let args = vec!["x".to_string()];
    let mut found = BTreeSet::new();
    for (label, text) in [
        ("esc", "a\u{1b}[0m"),
        ("bel", "a\u{7}"),
        ("cr", "a\rb"),
        ("bidi", "a\u{202e}b"),
        ("secret", SECRET),
        ("forged", "ok\nerror: injected line forged"),
        ("nul", "a\u{0}b"),
        ("c1", "a\u{9b}b"),
    ] {
        let mut one = BTreeSet::new();
        violations(text, "stdout", &args, &mut one);
        assert!(!one.is_empty(), "{label} must be flagged");
        found.extend(one);
    }
    let mut clean = BTreeSet::new();
    violations(
        "error: line one\nerror: \"a\\u{001B}[0m\\r\\u{202E}\" at line 3\n",
        "stderr",
        &args,
        &mut clean,
    );
    assert!(clean.is_empty(), "{clean:?}");
}

/// Every registered command is reached by at least one fill: a command that
/// takes neither flags nor positionals is still run with hostile usage words.
#[test]
fn every_command_has_hostile_variants() {
    let scratch = PathBuf::from("/nonexistent-scratch");
    let file = scratch.join("payload.json");
    let dir = scratch.join("payload");
    for spec in COMMANDS {
        let mut count = usage_variants(spec).len();
        for mode in FILLS {
            count += argv_variants(spec, mode, &scratch, &file, &dir).len();
        }
        assert!(count > 0, "{:?}", spec.path);
    }
}

/// A handler that echoes only the SECOND occurrence of its repeatable flag,
/// raw. A generator that passes a repeatable flag once, or not at all, never
/// reaches it.
fn echo_second_probe(
    invocation: &toolkit_cli::Invocation,
    output: &mut toolkit_cli::Output<'_>,
) -> Result<(), toolkit_core::ToolkitError> {
    if let Some(second) = invocation.values_of("--probe").nth(1) {
        output.line(second)?;
    }
    Ok(())
}

/// A synthetic command whose only value flag is repeatable (#4697): no
/// production command is changed.
const REPEATABLE_ONLY_SPEC: CommandSpec = CommandSpec {
    path: &["probe"],
    flags: &[],
    value_flags: &[],
    strict_values: false,
    repeatable_flags: &["--probe"],
    max_positionals: 0,
    passthrough: false,
    handler: echo_second_probe,
};

/// Runs `args` through the router and the synthetic handler in-process and
/// returns what the contract detector finds in its output.
fn probe_findings(args: &[String]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let specs = [REPEATABLE_ONLY_SPEC];
    if let Ok(toolkit_cli::Routed::Run(spec, invocation)) = toolkit_cli::route(&specs, args) {
        let mut stdout = Vec::new();
        let mut output = toolkit_cli::Output::new(&mut stdout);
        (spec.handler)(&invocation, &mut output).expect("fixture handler");
        let text = String::from_utf8(stdout).expect("UTF-8 output");
        violations(&text, "stdout", args, &mut found);
    }
    found
}

/// The generator must pass a flag declared only in `repeatable_flags`, in two
/// occurrences and both spellings, and the detector must flag a handler that
/// echoes the second one raw. Without the repeatable-flag support in the
/// generator this fixture would be clean.
#[test]
fn a_repeatable_only_flag_gets_hostile_coverage_in_every_occurrence() {
    let spec = &REPEATABLE_ONLY_SPEC;
    assert!(spec.value_flags.is_empty());
    assert_eq!(all_value_flags(spec), ["--probe"]);

    let scratch = PathBuf::from("/nonexistent-scratch");
    let file = scratch.join("payload.json");
    let dir = scratch.join("payload");
    let mut all_variants = Vec::new();
    for mode in FILLS {
        let variants = argv_variants(spec, mode, &scratch, &file, &dir);
        let flagged = variants
            .iter()
            .filter(|args| !probe_findings(args).is_empty())
            .count();
        // The Text-like fills carry control characters; the payload-file and
        // payload-dir fills carry plain paths, which a raw echo does not hurt.
        if matches!(mode, Fill::Text | Fill::Attached | Fill::HostilePath) {
            assert!(flagged > 0, "{mode:?}: no variant flagged the raw echo");
        }
        all_variants.extend(variants);
    }
    let hostile = format!("{HOSTILE}{ARG_MARKER}");
    let spaced = ["--probe".to_string(), hostile.clone()];
    let attached = format!("--probe={hostile}");
    assert!(
        all_variants
            .iter()
            .any(|args| args.windows(2).any(|pair| pair == spaced)),
        "the spaced spelling must be passed"
    );
    assert!(
        all_variants.iter().any(|args| args.contains(&attached)),
        "the attached spelling must be passed"
    );
    // One variant passes both spellings of the flag: two occurrences.
    assert!(all_variants
        .iter()
        .any(|args| { args.windows(2).any(|pair| pair == spaced) && args.contains(&attached) }));

    // The finding is about the second occurrence only: the first alone is not
    // echoed, so the detector is not firing on something else.
    let first_only = vec!["probe".to_string(), "--probe".to_string(), hostile];
    assert!(probe_findings(&first_only).is_empty());
    let mut variants_for_dangling = usage_variants(spec);
    variants_for_dangling.retain(|args| args.last().is_some_and(|last| last == "--probe"));
    assert_eq!(variants_for_dangling.len(), 1, "dangling repeatable flag");
}

/// The hostile directory name used by the stdout-report fixtures below: every
/// control and bidi class in the name, and a newline that would forge a line.
const HOSTILE_DIR_NAME: &str = "d\u{1b}[31m\u{7}\r\u{202e}gpj\nerror: injected line";

fn run_ok(args: &[String], cwd: &Path) -> (Run, BTreeSet<String>) {
    let run = spawn_toolkit(args, cwd);
    let mut found = BTreeSet::new();
    for (label, bytes) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
        violations(
            std::str::from_utf8(bytes).expect("UTF-8 output"),
            label,
            args,
            &mut found,
        );
    }
    (run, found)
}

fn emit_plan_args(dir: &Path) -> Vec<String> {
    vec![
        "startup".to_string(),
        "emit-plan".to_string(),
        "--migration-dir".to_string(),
        dir.display().to_string(),
    ]
}

/// The generic spawn loop writes only invalid payloads, so a command that
/// reaches its success or status report only for VALID state never printed
/// anything hostile there (#4665, #4666). These fixtures are minimally valid
/// per command and carry hostile text where the command reads or echoes it.
/// Each also asserts the command reached the branch under test, so a fixture
/// that stops matching cannot turn the test into a vacuous pass.
#[test]
fn emit_plan_status_lines_escape_paths_and_omit_payload_values() {
    let scratch = Scratch::new();

    // PLAN_SKIP branch: the status file's `owning_skill` is payload text. The
    // JSON escapes decode to a fake secret followed by U+202E (or controls).
    let skip_dir = scratch.0.join("skip");
    fs::create_dir_all(&skip_dir).expect("create skip dir");
    for owning in [
        format!("{SECRET}\\u202egpj.exe"),
        format!("{SECRET}\\u001b\\u0007\\r\\nerror: injected line"),
    ] {
        let status = format!("{{\"owning_skill\": \"{owning}\", \"run_id\": \"test\"}}");
        fs::write(skip_dir.join(".phase-status.json"), status).expect("write status");
        let (run, found) = run_ok(&emit_plan_args(&skip_dir), &scratch.0);
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert_eq!(run.status, Some(0), "{stdout}");
        assert!(
            stdout.starts_with("PLAN_SKIP | reason=owning_skill"),
            "fixture must reach the PLAN_SKIP branch: {stdout:?}"
        );
        assert!(found.is_empty(), "{found:?}");
    }
    // An ordinary known skill id is still named, exactly as before.
    fs::write(
        skip_dir.join(".phase-status.json"),
        r#"{"owning_skill": "AZURE_TO_AWS", "run_id": "test"}"#,
    )
    .expect("write status");
    let (run, found) = run_ok(&emit_plan_args(&skip_dir), &scratch.0);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "PLAN_SKIP | reason=owning_skill 'AZURE_TO_AWS' has no web handoff yet\n"
    );
    assert!(found.is_empty(), "{found:?}");

    // PLAN_OK branch: the migration directory's own name is caller text.
    let ok_dir = scratch.0.join(HOSTILE_DIR_NAME);
    fs::create_dir_all(&ok_dir).expect("create hostile dir");
    fs::write(
        ok_dir.join(".phase-status.json"),
        r#"{"owning_skill": "GCP_TO_AWS", "run_id": "test"}"#,
    )
    .expect("write status");
    fs::write(
        ok_dir.join("estimation-infra.json"),
        r#"{"projected_costs": {"aws_monthly_balanced": 100}}"#,
    )
    .expect("write estimate");
    let (run, found) = run_ok(&emit_plan_args(&ok_dir), &scratch.0);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(run.status, Some(0), "{stdout}");
    assert!(
        stdout.starts_with("PLAN_OK | path="),
        "fixture must reach the PLAN_OK branch: {stdout:?}"
    );
    assert_eq!(stdout.lines().count(), 1, "one status line: {stdout:?}");
    assert!(stdout.contains("\\u202E"), "{stdout:?}");
    assert!(stdout.contains("\\nerror: injected line"), "{stdout:?}");
    assert!(found.is_empty(), "{found:?}");
    assert!(
        ok_dir.join("plan.json").is_file(),
        "the plan is still written"
    );
}

/// `skills list` prints valid frontmatter (name and description) as listing
/// data; bidi and invisible characters in it must be escaped, and a skill
/// discovered only by its directory name (no usable frontmatter) is covered too.
#[test]
fn skills_list_escapes_bidi_in_valid_frontmatter_and_directory_names() {
    let scratch = Scratch::new();
    let root = scratch.0.join("skills-root");
    let good = root.join("demo");
    fs::create_dir_all(&good).expect("create skill dir");
    fs::write(
        good.join("SKILL.md"),
        "---\nname: safe\u{202e}gpj.exe\ndescription: ordinary\u{200b}\u{feff}text\n---\nbody\n",
    )
    .expect("write skill");
    let fallback = root.join("fall\u{202e}back\u{200b}dir");
    fs::create_dir_all(&fallback).expect("create fallback dir");
    fs::write(fallback.join("SKILL.md"), "no frontmatter at all\n").expect("write skill");

    let args = vec![
        "skills".to_string(),
        "list".to_string(),
        root.display().to_string(),
    ];
    let (run, found) = run_ok(&args, &scratch.0);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(run.status, Some(0), "{stdout}");
    assert!(
        stdout.contains("safe\\u202Egpj.exe\tordinary\\u200B\\uFEFFtext\n"),
        "valid frontmatter must be listed, escaped: {stdout:?}"
    );
    assert!(stdout.contains("fall\\u202Eback\\u200Bdir"), "{stdout:?}");
    assert!(found.is_empty(), "{found:?}");
}

/// The new fixtures must be able to fail: the unescaped forms are flagged.
#[test]
fn the_stdout_fixtures_would_flag_the_unfixed_output() {
    let args = vec!["startup".to_string(), "emit-plan".to_string()];
    for text in [
        "PLAN_SKIP | reason=owning_skill 'FAKE-SECRET-4639-AKIAZZZZZZZZZZZZZZZZ' has no web handoff yet\n",
        "PLAN_OK | path=d\u{1b}[31m/plan.json | platform=GCP | scope=INFRA\n",
        "PLAN_OK | path=d\nerror: injected line/plan.json\n",
    ] {
        let mut found = BTreeSet::new();
        violations(text, "stdout", &args, &mut found);
        assert!(!found.is_empty(), "{text:?}");
    }
    let skills = vec!["skills".to_string(), "list".to_string()];
    let mut found = BTreeSet::new();
    violations(
        "safe\u{202e}gpj.exe\tordinary\n",
        "stdout",
        &skills,
        &mut found,
    );
    assert!(!found.is_empty());
}
