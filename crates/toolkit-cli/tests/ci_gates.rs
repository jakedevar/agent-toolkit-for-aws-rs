//! Development recipes and CI gates (#4378): the `mise` tasks and the build
//! workflow call the Rust `toolkit` commands that replaced the Python
//! validators, and no Python is installed or invoked by them.
//!
//! The tests read the real `mise.toml` files and `.github/workflows/build.yml`
//! of this repository, extract every `toolkit` command the tasks run, route it
//! through the real routing table and execute it in process against the real
//! tree (read-only). A task that drifts back to Python, names a command that
//! no longer routes, or fails on the committed tree fails here.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use toolkit_cli::{route, run, Routed, COMMANDS};

/// Words that name the command (`manifests validate`, `sync vendored`).
const COMMAND_WORDS: usize = 2;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Lines of `text` that are neither comments nor blank.
fn active_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// `toolkit` arguments of every `run = "cargo run ... -- ARGS"` task line.
fn toolkit_commands(text: &str) -> Vec<Vec<String>> {
    active_lines(text)
        .into_iter()
        .filter(|line| line.starts_with("run = \"cargo run "))
        .map(|line| {
            let command = line.trim_start_matches("run = \"").trim_end_matches('"');
            let (_, args) = command
                .split_once(" -- ")
                .unwrap_or_else(|| panic!("no `--` separator in {line}"));
            args.split_whitespace().map(str::to_string).collect()
        })
        .collect()
}

/// Resolves the path arguments (positionals after the command words and the
/// values of `--root` / `--plugin-root`) against `base`, as the task's working
/// directory would, and returns the argument vector for `run`.
fn resolve_paths(args: &[String], base: &Path) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    let mut previous_is_path_flag = false;
    for (index, arg) in args.iter().enumerate() {
        let positional = index >= COMMAND_WORDS && !arg.starts_with('-');
        if positional || previous_is_path_flag {
            out.push(base.join(arg).into_os_string());
        } else {
            out.push(OsString::from(arg));
        }
        previous_is_path_flag = arg == "--root" || arg == "--plugin-root";
    }
    out
}

fn execute(args: &[OsString]) -> (u8, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = run(args, &mut out, &mut err);
    (
        status,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[test]
fn the_root_recipes_run_exactly_the_replacement_commands() {
    let commands = toolkit_commands(&read("mise.toml"));
    let joined: Vec<String> = commands.iter().map(|args| args.join(" ")).collect();
    for expected in [
        "manifests validate .",
        "sync plugin-skills --check --root .",
        "sync vendored --check --plugin-root plugins/aws-startup-advisor",
        "spec validate .",
    ] {
        assert!(
            joined.iter().any(|command| command == expected),
            "mise.toml must run `toolkit {expected}`; found {joined:?}"
        );
    }
    assert_eq!(joined.len(), 4, "no other toolkit command: {joined:?}");
}

#[test]
fn the_plugin_recipe_passes_its_own_directory_as_the_plugin_root() {
    let text = read("plugins/aws-startup-advisor/mise.toml");
    let commands = toolkit_commands(&text);
    let words =
        |line: &[&str]| -> Vec<String> { line.iter().map(|word| (*word).to_string()).collect() };
    assert_eq!(
        commands,
        vec![
            words(&["sync", "vendored", "--check", "--plugin-root", "."]),
            words(&["startup", "skill-refs", "--check", "--plugin-root", "."]),
            words(&[
                "startup",
                "model-staleness",
                "--strict",
                "--plugin-root",
                "."
            ]),
        ]
    );
    assert!(
        text.contains("--manifest-path ../../Cargo.toml"),
        "the plugin task must build the workspace at the repository root"
    );
}

#[test]
fn every_recipe_command_routes_and_passes_on_the_real_tree() {
    let root = repo_root();
    let sources = [
        ("mise.toml", root.clone()),
        (
            "plugins/aws-startup-advisor/mise.toml",
            root.join("plugins/aws-startup-advisor"),
        ),
    ];
    let mut executed = 0;
    for (file, base) in sources {
        for args in toolkit_commands(&read(file)) {
            match route(COMMANDS, &args) {
                Ok(Routed::Run(..)) => {}
                other => panic!("{file}: `toolkit {args:?}` must route to a command: {other:?}"),
            }
            // `model-staleness --strict` fails once the registry ages past its window,
            // and this test cannot inject a date: run the same command without
            // `--strict` (drift and a bad contract still fail). The strict path is
            // covered with injected dates in `startup/model_staleness/tests.rs`.
            let args: Vec<String> = args.into_iter().filter(|arg| arg != "--strict").collect();
            let (status, stdout, stderr) = execute(&resolve_paths(&args, &base));
            assert_eq!(
                status, 0,
                "{file}: `toolkit {args:?}` failed on the real tree\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
            executed += 1;
        }
    }
    assert_eq!(executed, 7);
}

#[test]
fn a_recipe_command_that_cannot_use_its_root_fails() {
    // The gates only mean something if a bad invocation fails them: point each
    // validator at a directory that does not exist and require a non-zero
    // status, so a recipe can never pass vacuously.
    let missing =
        std::env::temp_dir().join(format!("toolkit-ci-gates-missing-{}", std::process::id()));
    assert!(!missing.exists());
    for args in [
        vec!["manifests", "validate"],
        vec!["spec", "validate"],
        vec!["sync", "plugin-skills", "--check", "--root"],
        vec!["sync", "vendored", "--check", "--plugin-root"],
    ] {
        let mut command: Vec<OsString> = args.iter().map(OsString::from).collect();
        command.push(missing.clone().into_os_string());
        let (status, _, _) = execute(&command);
        assert_ne!(status, 0, "{args:?} on a missing root must fail");
    }
}

#[test]
fn the_validate_task_runs_every_replacement_check_and_the_rust_tests() {
    let text = read("mise.toml");
    let lines = active_lines(&text);
    let depends_of = |header: &str| -> String {
        lines
            .iter()
            .skip_while(|line| **line != header)
            .find(|line| line.starts_with("depends = "))
            .copied()
            .unwrap_or_else(|| panic!("{header} has no depends"))
            .to_string()
    };
    let validate = depends_of("[tasks.validate]");
    for task in [
        "lint:manifests",
        "lint:skill-parity",
        "lint:vendored-parity",
        "lint:spec",
        "test:rust",
    ] {
        assert!(
            validate.contains(&format!("\"{task}\"")),
            "validate must depend on {task}: {validate}"
        );
    }
    let lint = depends_of("[tasks.lint]");
    for task in ["lint:md", "lint:rust", "lint:manifests"] {
        assert!(lint.contains(&format!("\"{task}\"")), "{lint}");
    }
    for header in [
        "[tasks.\"lint:manifests\"]",
        "[tasks.\"lint:skill-parity\"]",
        "[tasks.\"lint:vendored-parity\"]",
        "[tasks.\"lint:spec\"]",
    ] {
        assert!(
            depends_of(header).contains("\"build:toolkit\""),
            "{header} must reuse the single toolkit build"
        );
    }
}

#[test]
fn the_rust_gates_are_the_makefile_targets_with_warnings_denied() {
    let makefile = read("Makefile");
    assert!(makefile.contains("cargo fmt --all -- --check"));
    assert!(makefile.contains("cargo clippy --workspace --all-targets -- -D warnings"));
    assert!(makefile.contains("cargo test --workspace --locked"));
    let mise = read("mise.toml");
    for (task, target) in [
        ("lint:rust-fmt", "make fmt"),
        ("lint:rust-clippy", "make clippy"),
        ("test:rust", "make test"),
    ] {
        let header = format!("[tasks.\"{task}\"]");
        let wanted = format!("run = \"{target}\"");
        let body: Vec<&str> = mise
            .lines()
            .skip_while(|line| *line != header)
            .take(4)
            .collect();
        assert!(
            body.iter().any(|line| *line == wanted),
            "{task} must run `{target}`: {body:?}"
        );
    }
}

#[test]
fn no_recipe_installs_or_invokes_python() {
    for file in ["mise.toml", ".github/workflows/build.yml"] {
        let text = read(file);
        for line in active_lines(&text) {
            let lower = line.to_ascii_lowercase();
            for banned in [
                "python",
                "pip ",
                "pip3",
                "pytest",
                "unittest",
                "setup-python",
                "uv run",
                "uvx",
                ".py",
            ] {
                assert!(
                    !lower.contains(banned),
                    "{file}: `{line}` mentions {banned}"
                );
            }
            assert!(
                lower != "uv = \"latest\"",
                "{file}: uv must not be a root tool"
            );
        }
    }
}

#[test]
fn the_build_workflow_builds_once_then_runs_every_gate_and_keeps_the_scans() {
    let text = read(".github/workflows/build.yml");
    let steps: Vec<&str> = active_lines(&text)
        .into_iter()
        .filter(|line| line.starts_with("run: "))
        .collect();
    assert_eq!(
        steps,
        vec![
            "run: rustup toolchain install --profile minimal --component rustfmt --component clippy",
            "run: mise run build:toolkit",
            "run: mise run lint:md",
            "run: mise run lint:rust",
            "run: mise run validate",
            "run: mise run security",
        ],
        "build once, then fmt/clippy, validators and tests; markdown and gitleaks retained"
    );
}

#[test]
fn covered_python_is_retired_and_the_remaining_callers_are_listed() {
    let root = repo_root();
    for retired in [
        "tools/validate.py",
        "tools/tests/test_validate.py",
        "tools/validate_spec.py",
        "tools/sync-plugin-skills.py",
        "plugins/aws-startup-advisor/tools/check-skill-refs.py",
        "plugins/aws-startup-advisor/tools/test_check_skill_refs.py",
        "plugins/aws-startup-advisor/tools/model-staleness.py",
    ] {
        assert!(!root.join(retired).exists(), "{retired} must be retired");
    }
    // The shipped schemas stay: `toolkit spec validate` reads them.
    assert!(root.join("tools/schemas/1.0.0").is_dir());
    // `sync-vendored.py` still has Python callers owned by another slice (the
    // startup-advisor contract tests run it as a subprocess), so it stays
    // until those tests are ported and is recorded in the CI-gates document.
    for retained in [
        "plugins/aws-startup-advisor/tools/sync-vendored.py",
        "plugins/aws-startup-advisor/tools/test_sync_vendored.py",
    ] {
        assert!(root.join(retained).is_file(), "{retained} is still needed");
    }
    let document = read("docs/migration/ci-gates.md");
    assert!(document.contains("plugins/aws-startup-advisor/tools/sync-vendored.py"));
    for caller in [
        "test_generation_ai_contracts.py",
        "test_estimation_ai_contracts.py",
    ] {
        let source = read(&format!("plugins/aws-startup-advisor/tests/{caller}"));
        assert!(
            source.contains("sync-vendored.py"),
            "{caller} no longer calls sync-vendored.py; retire the tool"
        );
        assert!(document.contains(caller), "{caller} must be listed");
    }
}
