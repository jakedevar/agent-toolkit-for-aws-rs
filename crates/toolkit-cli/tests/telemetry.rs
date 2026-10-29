//! `toolkit telemetry` through the real binary (#4398): consent, persistence,
//! fail-open hooks and the absence of any network emission. Every spawn runs
//! with an empty environment plus an isolated HOME; nothing reaches a network
//! and no Python runs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");
const UUID: &str = "11111111-2222-3333-4444-555555555555";

struct Home(PathBuf);

impl Home {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-telemetry-it-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("home");
        Self(path)
    }

    fn record(&self) -> PathBuf {
        self.0.join(".aws-startup-advisor/plugin-telemetry.json")
    }

    fn write_record(&self, text: &str) {
        fs::create_dir_all(self.record().parent().expect("parent")).expect("dir");
        fs::write(self.record(), text).expect("record");
    }

    fn accept(&self) {
        self.write_record(&format!(
            "{{\"installId\": \"{UUID}\", \"consentStatus\": \"ACCEPTED\"}}"
        ));
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("CLAUDECODE", "1")
            .current_dir(&self.0);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .expect("spawn toolkit")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

fn wait_within(mut child: Child, limit: Duration) -> (Option<i32>, Duration) {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return (status.code(), start.elapsed());
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return (None, start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn repo_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

#[test]
fn the_consent_flow_persists_and_reads_back_through_the_binary() {
    let home = Home::new("flow");
    let shown = home.run(&["telemetry", "consent", "show"]);
    assert!(shown.status.success());
    assert!(text(&shown.stdout)
        .starts_with("AWS Startup Advisor plugin \u{2014} usage data notice\n\n"));
    assert!(
        !home.0.join(".aws-startup-advisor").exists(),
        "showing the notice records nothing"
    );

    let before = home.run(&["telemetry", "consent", "status"]);
    assert!(text(&before.stdout).contains("Nothing is recorded yet"));
    let accepted = home.run(&["telemetry", "consent", "accept"]);
    assert!(accepted.status.success(), "{}", text(&accepted.stderr));
    assert!(text(&accepted.stdout).contains("usage data collection is ON"));
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.record()).expect("record")).expect("json");
    assert_eq!(record["consentStatus"], "ACCEPTED");
    let install_id = record["installId"].as_str().expect("id").to_string();
    assert_eq!(install_id.len(), 36);

    let status = home.run(&["telemetry", "status"]);
    let report: serde_json::Value = serde_json::from_str(&text(&status.stdout)).expect("json");
    assert_eq!(report["consentStatus"], "ACCEPTED");
    assert_eq!(report["source"], "CLAUDE_CODE");
    assert!(
        report["pluginVersion"].is_null(),
        "no bundle directory given: no version, so nothing can be sent"
    );

    let out = home.run(&["telemetry", "consent", "opt-out"]);
    assert!(out.status.success());
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.record()).expect("record")).expect("json");
    assert_eq!(
        (
            record["consentStatus"].as_str(),
            record["installId"].as_str()
        ),
        (Some("OPT_OUT"), Some(install_id.as_str()))
    );
    assert_eq!(
        text(&home.run(&["telemetry", "consent", "session-start"]).stdout),
        "",
        "an answered notice is silent"
    );
}

#[test]
fn the_session_start_hook_speaks_without_a_home_and_without_reading_stdin() {
    let home = Home::new("session");
    // No HOME at all: no record can exist, so the notice is raised, and the
    // hook exits at once even though its stdin pipe is never closed.
    let mut command = Command::new(BIN);
    command
        .args(["telemetry", "consent", "session-start"])
        .env_clear()
        .current_dir(&home.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn");
    let stdin = child.stdin.take().expect("stdin kept open");
    let mut stdout = child.stdout.take().expect("stdout");
    let (code, elapsed) = wait_within(child, Duration::from_secs(10));
    drop(stdin);
    assert_eq!(code, Some(0));
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    let mut spoken = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut spoken).expect("read");
    assert!(spoken.contains("toolkit telemetry consent show"));
}

#[test]
fn accept_without_a_home_fails_loudly_and_records_nothing() {
    let home = Home::new("nohome");
    let output = Command::new(BIN)
        .args(["telemetry", "consent", "accept"])
        .env_clear()
        .current_dir(&home.0)
        .stdin(Stdio::null())
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("Nothing was saved"));
    assert_eq!(text(&output.stdout), "");
}

#[test]
fn hooks_are_silent_and_exit_zero_without_consent_whatever_arrives_on_stdin() {
    let home = Home::new("silent");
    for (args, stdin) in [
        (
            vec!["telemetry", "skill-invoked"],
            "{\"tool_input\": {\"skill\": \"gcp-to-aws\"}}",
        ),
        (
            vec!["telemetry", "skill-invoked", "--skill", "gcp-to-aws"],
            "",
        ),
        (vec!["telemetry", "migration"], "not json"),
        (vec!["telemetry", "migration", "--reconcile"], "[]"),
        (
            vec!["telemetry", "migration", "--session-end"],
            "{\"cwd\": 5}",
        ),
        (
            vec!["telemetry", "migration", "--via", "cli", "--bogus"],
            "",
        ),
        (vec!["telemetry", "reconcile"], ""),
    ] {
        let mut child = home
            .command(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write");
        let output = child.wait_with_output().expect("wait");
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(
            (text(&output.stdout), text(&output.stderr)),
            (String::new(), String::new()),
            "{args:?}"
        );
    }
}

#[test]
fn a_hook_does_not_block_on_a_stdin_pipe_that_never_closes() {
    let home = Home::new("openpipe");
    home.accept();
    let mut child = home
        .command(&["telemetry", "skill-invoked"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let _held_open = child.stdin.take().expect("stdin kept open");
    let (code, elapsed) = wait_within(child, Duration::from_secs(10));
    assert_eq!(code, Some(0), "the hook gives up on stdin at its deadline");
    assert!(elapsed < Duration::from_secs(6), "{elapsed:?}");
}

#[test]
fn a_run_reported_by_the_shipped_binary_leaves_no_snapshot_and_sends_nothing() {
    // This build has no HTTP client: with consent accepted, a complete bundle
    // and a run to report, the events are held (never recorded as sent) so a
    // build that can send reports them later.
    let home = Home::new("shipped");
    home.accept();
    let project = home.0.join("project");
    let run = project.join(".migration/0226-1430");
    fs::create_dir_all(&run).expect("run");
    fs::write(
        run.join(".phase-status.json"),
        "{\"migration_id\": \"0226-1430\", \"owning_skill\": \"GCP_TO_AWS\", \"current_phase\": \"clarify\", \"phases\": {\"discover\": \"completed\"}}",
    )
    .expect("status");
    let bundle = home.0.join("bundle");
    fs::create_dir_all(&bundle).expect("bundle");
    fs::write(bundle.join("version.json"), "{\"version\": \"2.0.3\"}").expect("version");
    let output = home
        .command(&[
            "telemetry",
            "reconcile",
            "--telemetry-dir",
            &bundle.display().to_string(),
        ])
        .current_dir(&project)
        .stdin(Stdio::null())
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        (text(&output.stdout), text(&output.stderr)),
        (String::new(), String::new())
    );
    assert!(
        !run.join(".telemetry-snapshot.json").exists(),
        "unreachable holds every event"
    );
    assert!(!run.join(".telemetry-lock").exists());
}

/// `toolkit telemetry reconcile` in `project` under umask 000, set in a child
/// shell so this test process's umask is never touched.
#[cfg(unix)]
fn reconcile_under_umask_000(home: &Home, project: &Path, bundle: &Path) -> Output {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "umask 000 && exec \"$0\" \"$@\"", BIN])
        .args([
            "telemetry",
            "reconcile",
            "--telemetry-dir",
            &bundle.display().to_string(),
        ])
        .env_clear()
        .env("HOME", &home.0)
        .env("CLAUDECODE", "1")
        .current_dir(project)
        .stdin(Stdio::null());
    command.output().expect("spawn sh")
}

#[cfg(unix)]
#[test]
fn snapshots_and_locks_stay_private_under_a_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| {
        fs::symlink_metadata(path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777
    };
    let chmod = |path: &Path, bits: u32| {
        fs::set_permissions(path, fs::Permissions::from_mode(bits)).expect("chmod");
    };
    let home = Home::new("umask");
    home.accept();
    let project = home.0.join("project");
    let run = project.join(".migration/0226-1430");
    fs::create_dir_all(&run).expect("run");
    fs::write(
        run.join(".phase-status.json"),
        "{\"migration_id\": \"0226-1430\", \"owning_skill\": \"GCP_TO_AWS\", \"current_phase\": \"clarify\", \"phases\": {\"discover\": \"completed\"}}",
    )
    .expect("status");
    let bundle = home.0.join("bundle");
    fs::create_dir_all(&bundle).expect("bundle");
    fs::write(bundle.join("version.json"), "{\"version\": \"2.0.3\"}").expect("version");
    let snapshot = run.join(".telemetry-snapshot.json");
    let lock = run.join(".telemetry-lock");
    let quiet = |output: &Output| {
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            (text(&output.stdout), text(&output.stderr)),
            (String::new(), String::new())
        );
    };

    // A corrupt snapshot takes the rebuild branch, the one write the shipped
    // sink reaches; its replacement is 0600 although the umask allows 0666.
    fs::write(&snapshot, "[1, 2]").expect("corrupt");
    chmod(&snapshot, 0o600);
    quiet(&reconcile_under_umask_000(&home, &project, &bundle));
    assert_eq!(mode(&snapshot), 0o600);
    assert!(text(&fs::read(&snapshot).expect("snapshot")).contains("\"runId\""));
    assert!(!lock.exists());

    // A world-writable lock directory, fresh by its time, is not honoured: it
    // is replaced by a private one, so the run is processed (its 0666
    // snapshot rebuilt privately, not trusted) and the lock released.
    fs::create_dir(&lock).expect("lock");
    chmod(&lock, 0o777);
    fs::write(
        &snapshot,
        "{\"runId\": \"0badf00d-1111-4222-8333-444444444444\", \"phases\": {}}",
    )
    .expect("forged");
    chmod(&snapshot, 0o666);
    quiet(&reconcile_under_umask_000(&home, &project, &bundle));
    assert!(!lock.exists(), "the open lock was replaced and released");
    assert_eq!(mode(&snapshot), 0o600);
    assert!(
        !text(&fs::read(&snapshot).expect("snapshot")).contains("0badf00d"),
        "the forged identity is not carried over"
    );
}

#[test]
fn the_committed_bundles_are_in_sync_through_the_binary() {
    let home = Home::new("bundles");
    let output = home.run(&[
        "telemetry",
        "bundles",
        "--check",
        "--plugin-root",
        &repo_plugin().display().to_string(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        text(&output.stdout),
        "Telemetry bundles and DSL copies: OK\n"
    );
}

#[test]
fn the_telemetry_sources_open_no_socket_and_start_no_process() {
    // AC4 tripwire: the module has no way to reach a network or to run another
    // program, so no test (or hook) can emit by accident. A future HTTP sink
    // is a separate, reviewed change that adds its own injected-transport tests.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![root.join("telemetry.rs")];
    let mut pending = vec![root.join("telemetry")];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "tests") {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                files.push(path);
            }
        }
    }
    assert!(files.len() >= 7, "{files:?}");
    for file in files {
        let source = fs::read_to_string(&file).expect("source");
        for forbidden in [
            "std::net",
            "TcpStream",
            "UdpSocket",
            "process::Command",
            "Command::new",
            "reqwest",
            "hyper",
            "ureq",
            "curl",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} uses {forbidden}",
                file.display()
            );
        }
    }
}

// ------------------------------------------------- bare `-` path flags (#4961)

/// Every file and directory under `root`, as `(relative path, bytes)` pairs in
/// path order (directories carry no bytes): what a canary must keep exactly.
fn tree(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(dir: &Path, root: &Path, found: &mut Vec<(String, Option<Vec<u8>>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .display()
                .to_string();
            if path.is_dir() {
                found.push((rel, None));
                walk(&path, root, found);
            } else {
                found.push((rel, Some(fs::read(&path).expect("read file"))));
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found
}

/// A `./-` canary directory in the home (also the cwd) shaped so that reading it
/// as a telemetry directory (`version.json`) or as a plugin root (`plugin.json`,
/// `skills/`) would succeed, and writing a bundle into it would change it.
fn canary(home: &Home) -> PathBuf {
    let dash = home.0.join("-");
    fs::create_dir_all(dash.join("skills")).expect("canary dirs");
    fs::write(dash.join("version.json"), "{\"version\":\"9.9.9\"}").expect("canary version");
    fs::write(dash.join("plugin.json"), "{\"version\":\"9.9.9\"}").expect("canary manifest");
    fs::write(dash.join("KEEP"), "KEEP-ORIGINAL\n").expect("canary file");
    dash
}

/// Both spellings of a path flag given the value `-`.
fn dash_spellings(flag: &str) -> [Vec<String>; 2] {
    [
        vec![flag.to_string(), "-".to_string()],
        vec![format!("{flag}=-")],
    ]
}

fn spawn_words(home: &Home, words: &[String]) -> Output {
    let args: Vec<&str> = words.iter().map(String::as_str).collect();
    home.run(&args)
}

/// What a refused `-` must look like: the router's usage error naming the flag.
fn assert_dash_refusal(output: &Output, flag: &str, label: &str) {
    let stderr = text(&output.stderr);
    let first = stderr.lines().next().unwrap_or_default();
    assert_eq!(output.status.code(), Some(2), "{label}: {stderr}");
    assert!(
        first.starts_with("error: invalid arguments: ")
            && first.contains(flag)
            && first.contains("./-"),
        "{label}: {first:?}"
    );
    assert_eq!(text(&output.stdout), "", "{label}: stdout");
}

const PATH_FLAGS: [&str; 2] = ["--telemetry-dir", "--plugin-root"];

/// Interactive subcommands: each path flag, both spellings, is a usage error that
/// reads and writes nothing (a pre-existing `./-` keeps its bytes and no consent
/// record appears).
#[test]
fn interactive_subcommands_refuse_a_bare_dash_path_flag_in_both_spellings() {
    let interactive: [&[&str]; 8] = [
        &["status"],
        &["reconcile"],
        &["consent", "show"],
        &["consent", "status"],
        &["consent", "accept"],
        &["consent", "opt-out"],
        &["bundles", "--check"],
        &["bundles", "--write"],
    ];
    for flag in PATH_FLAGS {
        for words in interactive {
            for spelling in dash_spellings(flag) {
                let home = Home::new("dash-interactive");
                let dash = canary(&home);
                let before = tree(&dash);
                let mut args = vec!["telemetry".to_string()];
                args.extend(words.iter().map(|word| (*word).to_string()));
                args.extend(spelling.clone());
                let label = args.join(" ");
                let output = spawn_words(&home, &args);
                assert_dash_refusal(&output, flag, &label);
                assert_eq!(tree(&dash), before, "{label}: ./- changed");
                assert!(!home.record().exists(), "{label}: wrote a consent record");
                assert_eq!(
                    tree(&home.0)
                        .iter()
                        .filter(|(rel, _)| !rel.starts_with('-'))
                        .count(),
                    0,
                    "{label}: created files outside the canary"
                );
            }
        }
    }
}

/// The flag may precede the subcommand: still refused, still nothing touched.
#[test]
fn a_leading_bare_dash_path_flag_is_refused_before_any_subcommand_runs() {
    for flag in PATH_FLAGS {
        for spelling in dash_spellings(flag) {
            let home = Home::new("dash-leading");
            let dash = canary(&home);
            let before = tree(&dash);
            let mut args = vec!["telemetry".to_string()];
            args.extend(spelling);
            args.push("status".to_string());
            let label = args.join(" ");
            let output = spawn_words(&home, &args);
            assert_dash_refusal(&output, flag, &label);
            assert_eq!(tree(&dash), before, "{label}: ./- changed");
        }
    }
}

/// A bare `-` word to an interactive subcommand is refused like the router's
/// positional message.
#[test]
fn interactive_subcommands_refuse_a_bare_dash_positional() {
    for words in [
        vec!["status", "-"],
        vec!["reconcile", "-"],
        vec!["consent", "show", "-"],
        vec!["bundles", "-"],
        vec!["bundles", "--check", "-"],
    ] {
        let home = Home::new("dash-positional");
        let dash = canary(&home);
        let before = tree(&dash);
        let mut args = vec!["telemetry"];
        args.extend(words.iter().copied());
        let output = home.run(&args);
        let stderr = text(&output.stderr);
        let first = stderr.lines().next().unwrap_or_default();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            first.starts_with("error: invalid arguments: a bare - argument")
                && first.contains("./-"),
            "{args:?}: {first:?}"
        );
        assert_eq!(tree(&dash), before, "{args:?}: ./- changed");
    }
}

/// `./-` stays an explicit literal path for the interactive commands.
#[test]
fn dot_slash_dash_remains_an_explicit_literal_path() {
    let home = Home::new("dash-literal");
    canary(&home);
    let output = home.run(&["telemetry", "status", "--telemetry-dir", "./-"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).contains("\"pluginVersion\": \"9.9.9\""),
        "{}",
        text(&output.stdout)
    );
}

/// Hook entry points never fail: a bare `-` path flag is treated as absent, so
/// the output is exactly that of the same hook without the flag (nothing
/// emitted), the exit is 0 and `./-` is untouched, with and without consent.
#[test]
fn hook_entry_points_treat_a_bare_dash_path_flag_as_absent() {
    let hooks: [&[&str]; 5] = [
        &["skill-invoked"],
        &["skill-invoked", "--skill", "migrate"],
        &["migration"],
        &["migration", "--session-end", "--via", "cli"],
        &["consent", "session-start"],
    ];
    for accepted in [false, true] {
        for flag in PATH_FLAGS {
            for words in hooks {
                for spelling in dash_spellings(flag) {
                    let home = Home::new("dash-hook");
                    if accepted {
                        home.accept();
                    }
                    let dash = canary(&home);
                    let before = tree(&dash);
                    let mut base = vec!["telemetry".to_string()];
                    base.extend(words.iter().map(|word| (*word).to_string()));
                    let mut with_dash = base.clone();
                    with_dash.extend(spelling);
                    let label = with_dash.join(" ");
                    let expected = spawn_words(&home, &base);
                    let record_before = fs::read(home.record()).ok();
                    let output = spawn_words(&home, &with_dash);
                    assert_eq!(output.status.code(), Some(0), "{label}");
                    assert_eq!(output.stdout, expected.stdout, "{label}: stdout");
                    assert_eq!(output.stderr, expected.stderr, "{label}: stderr");
                    assert_eq!(tree(&dash), before, "{label}: ./- changed");
                    assert_eq!(fs::read(home.record()).ok(), record_before, "{label}");
                }
            }
        }
    }
}

/// A hook given the flag before the subcommand is still a hook.
#[test]
fn a_hook_with_a_leading_bare_dash_flag_still_exits_zero() {
    for spelling in dash_spellings("--telemetry-dir") {
        let home = Home::new("dash-hook-leading");
        let dash = canary(&home);
        let before = tree(&dash);
        let mut args = vec!["telemetry".to_string()];
        args.extend(spelling);
        args.extend(["skill-invoked".to_string()]);
        let output = spawn_words(&home, &args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(text(&output.stdout), "", "{args:?}");
        assert_eq!(tree(&dash), before, "{args:?}: ./- changed");
    }
}
