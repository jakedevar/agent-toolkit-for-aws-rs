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
        let path =
            std::env::temp_dir().join(format!("toolkit-diag-contract-{}", std::process::id()));
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

fn value_for(fill: Fill, scratch: &Path, file: &Path, dir: &Path) -> String {
    match fill {
        Fill::Text | Fill::Attached => format!("{HOSTILE}{ARG_MARKER}"),
        Fill::HostilePath => format!("{}/{HOSTILE}{ARG_MARKER}", scratch.display()),
        Fill::PayloadFile => file.display().to_string(),
        Fill::PayloadDir => dir.display().to_string(),
    }
}

/// The argument vectors that exercise `spec` for one fill.
fn argv_variants(
    spec: &CommandSpec,
    fill: Fill,
    scratch: &Path,
    file: &Path,
    dir: &Path,
) -> Vec<Vec<String>> {
    let value = value_for(fill, scratch, file, dir);
    let base: Vec<String> = spec.path.iter().map(|word| (*word).to_string()).collect();
    let mut with_values = base.clone();
    for flag in spec
        .value_flags
        .iter()
        .filter(|flag| !flag_is_excluded(flag))
    {
        if matches!(fill, Fill::Attached) {
            with_values.push(format!("{flag}={value}"));
        } else {
            with_values.push((*flag).to_string());
            with_values.push(value.clone());
        }
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
    for flag in spec
        .value_flags
        .iter()
        .filter(|flag| !flag_is_excluded(flag))
    {
        let mut one = base.clone();
        if matches!(fill, Fill::Attached) {
            one.push(format!("{flag}={value}"));
        } else {
            one.push((*flag).to_string());
            one.push(value.clone());
        }
        variants.push(one);
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
    if let Some(flag) = spec.value_flags.first() {
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
        for fill in FILLS {
            variants.extend(argv_variants(spec, fill, &scratch.0, &file, &dir));
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
        for fill in FILLS {
            count += argv_variants(spec, fill, &scratch, &file, &dir).len();
        }
        assert!(count > 0, "{:?}", spec.path);
    }
}
