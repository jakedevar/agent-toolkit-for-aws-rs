//! Fixture assertion programs of the AWS startup advisor (#4408, slice
//! `startup-fixtures`).
//!
//! `toolkit startup check-expected KIND RUN_DIR [SEED_DIR]` is the Rust
//! replacement of the sixteen `plugins/aws-startup-advisor/fixtures/**/
//! check_expected_*.py` programs. They are executable checks, not fixture
//! data: each one is run by a person or a replay harness against a migration
//! run directory (a committed `after-*` golden tree, or a fresh run) and exits
//! 1 with one line per failed assertion. One command keeps one registration
//! and one README section; the `KIND` selects the program:
//!
//! | KIND | Python original |
//! | --- | --- |
//! | `heroku-decide` | `heroku-decision-gate/check_expected_decide.py` |
//! | `heroku-decide-retained` | `heroku-decision-gate/check_expected_decide_with_retained_pack.py` |
//! | `gcp-decide` | `gcp-decision-gate/check_expected_decide.py` |
//! | `heroku-workshop` | `heroku-workshop/check_expected_workshop.py` |
//! | `gcp-workshop` | `gcp-workshop/check_expected_workshop.py` |
//! | `heroku-nonweb-scaling` | `heroku-nonweb-scaling/check_expected_nonweb_scaling.py` |
//! | `heroku-drift` | `heroku-live-capture/check_expected_drift.py` |
//! | `heroku-estimate` | `heroku-live-capture/check_expected_estimate.py` |
//! | `gcp-drift` | `gcp-live-capture/check_expected_drift.py` |
//! | `gcp-baseline` | `gcp-live-capture/check_expected_baseline.py` |
//! | `azure-discover` | `azure-iac-terraform/check_expected_iac_terraform.py` |
//! | `azure-clarify` | `azure-iac-terraform/check_expected_clarify.py` |
//! | `azure-clarify-complete` | `azure-iac-terraform/check_expected_clarify_complete.py` |
//! | `azure-clarify-fast-path` | `azure-iac-terraform/check_expected_clarify_fast_path.py` |
//! | `azure-design` | `azure-iac-terraform/check_expected_design.py` |
//! | `azure-estimate` | `azure-iac-terraform/check_expected_estimate.py` |
//!
//! The expectation documents (`expected-*.json`) and the golden trees stay
//! where they are, as fixture data; the command reads them from
//! `<plugin-root>/fixtures/<set>/` (default plugin root `.`) the way the
//! Python programs read their own directory. Every read refuses symbolic
//! links, every malformed expectation or run document fails the check, and
//! every echoed run value is a bounded, escaped single line. The deliberate
//! differences from the Python programs are in
//! `docs/migration/startup-fixtures.md`.

// Each fixture program is a checklist of independent assertions in the order the
// Python program ran them (its source says so: "a fixture oracle is a checklist;
// splitting it hides the list"), so the long functions are deliberate.
#![allow(clippy::too_many_lines)]

use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{neutralize_line, read_regular_file, Json, ToolkitError};

use crate::manifests::read_failure;
use crate::startup::artifacts::pyjson;
use crate::{Handler, Invocation, Output};

use super::{is_regular_file_under, read_text_under, MAX_ARTIFACT_BYTES};

mod azure;
mod capture;
mod decide;
mod ledger;
mod nonweb;
mod workshop;

#[cfg(test)]
mod tests;

use ledger::Ledger;
use ledger::Spec;

/// How a program words its final report.
#[derive(Clone, Copy)]
enum Style {
    /// `FAIL (n):` then `  - message`.
    Counted,
    /// `FAIL` then ` - message`.
    Bare,
    /// `FAIL — n problem(s)` then ` - message`.
    Problems,
}

/// What a program decided; `Stop` ends it at once with the findings so far
/// (the Python program printed the report and returned 1 mid-way).
type Flow = Option<()>;

/// One selectable program.
struct Program {
    name: &'static str,
    /// The fixture set directory below `fixtures/`.
    set: &'static str,
    /// Whether a second positional (the seed directory) is accepted.
    seeded: bool,
    style: Style,
    run: fn(&Context<'_>) -> Flow,
    /// The success line.
    pass: &'static str,
}

const PROGRAMS: &[Program] = &[
    Program {
        name: "heroku-decide",
        set: "heroku-decision-gate",
        seeded: false,
        style: Style::Bare,
        run: decide::heroku,
        pass: "PASS",
    },
    Program {
        name: "heroku-decide-retained",
        set: "heroku-decision-gate",
        seeded: false,
        style: Style::Bare,
        run: decide::heroku,
        pass: "PASS",
    },
    Program {
        name: "gcp-decide",
        set: "gcp-decision-gate",
        seeded: false,
        style: Style::Bare,
        run: decide::gcp,
        pass: "PASS",
    },
    Program {
        name: "heroku-workshop",
        set: "heroku-workshop",
        seeded: true,
        style: Style::Counted,
        run: workshop::heroku,
        pass: "PASS — expected-workshop.json assertions hold",
    },
    Program {
        name: "gcp-workshop",
        set: "gcp-workshop",
        seeded: true,
        style: Style::Counted,
        run: workshop::gcp,
        pass: "PASS — expected-workshop.json assertions hold",
    },
    Program {
        name: "heroku-nonweb-scaling",
        set: "heroku-nonweb-scaling",
        seeded: false,
        style: Style::Counted,
        run: nonweb::run,
        pass: "PASS — expected-nonweb-scaling.json assertions hold",
    },
    Program {
        name: "heroku-drift",
        set: "heroku-live-capture",
        seeded: false,
        style: Style::Counted,
        run: capture::heroku_drift,
        pass: "PASS — expected-drift.json assertions hold",
    },
    Program {
        name: "heroku-estimate",
        set: "heroku-live-capture",
        seeded: false,
        style: Style::Counted,
        run: capture::heroku_estimate,
        pass: "PASS — expected-estimate.json assertions hold",
    },
    Program {
        name: "gcp-drift",
        set: "gcp-live-capture",
        seeded: false,
        style: Style::Counted,
        run: capture::gcp_drift,
        pass: "PASS — expected-drift.json assertions hold",
    },
    Program {
        name: "gcp-baseline",
        set: "gcp-live-capture",
        seeded: false,
        style: Style::Counted,
        run: capture::gcp_baseline,
        pass: "PASS — expected-baseline.json assertions hold",
    },
    Program {
        name: "azure-discover",
        set: "azure-iac-terraform",
        seeded: false,
        style: Style::Counted,
        run: azure::discover::run,
        pass: "PASS — expected-iac-terraform.json assertions hold",
    },
    Program {
        name: "azure-clarify",
        set: "azure-iac-terraform",
        seeded: false,
        style: Style::Counted,
        run: azure::clarify::blocked,
        pass: "PASS — expected-clarify.json assertions hold",
    },
    Program {
        name: "azure-clarify-complete",
        set: "azure-iac-terraform",
        seeded: false,
        style: Style::Counted,
        run: azure::clarify::complete,
        pass: "PASS — expected-clarify-complete.json assertions hold",
    },
    Program {
        name: "azure-clarify-fast-path",
        set: "azure-iac-terraform",
        seeded: false,
        style: Style::Counted,
        run: azure::clarify::fast_path,
        pass: "PASS — expected-clarify-fast-path.json assertions hold",
    },
    Program {
        name: "azure-design",
        set: "azure-iac-terraform",
        seeded: false,
        style: Style::Counted,
        run: azure::design::run,
        pass: "PASS — expected-design.json assertions hold",
    },
    Program {
        name: "azure-estimate",
        set: "azure-iac-terraform",
        seeded: false,
        style: Style::Problems,
        run: azure::estimate::run,
        pass: "PASS — estimate matches expected-estimate.json (tier 1 exact, tier 2 relational, tier 3 banded)",
    },
];

/// Everything one program needs: where things are and the ledger.
pub(super) struct Context<'a> {
    pub(super) ledger: &'a Ledger,
    pub(super) plugin_root: PathBuf,
    /// `<plugin-root>/fixtures/<set>`.
    pub(super) fixture: PathBuf,
    pub(super) run: PathBuf,
    /// The caller's second positional, if the program takes one.
    pub(super) seed: Option<PathBuf>,
}

/// How a read ended.
enum Read {
    Missing,
    Unreadable(String),
    Text(String),
}

impl Context<'_> {
    /// The traversal anchor for a read of `path` below `root`. A path the
    /// toolkit derives below `--plugin-root` (the fixtures directory, a fixture
    /// set, a golden tree, a table directory, the shipped skills) is checked
    /// from the plugin root, so a link in any derived component is refused;
    /// `root` itself is never examined (#4759). The caller-chosen `RUN_DIR` and
    /// `SEED_DIR` keep their own explicit-root semantics.
    fn anchor<'p>(&'p self, root: &'p Path, path: &Path) -> &'p Path {
        let caller = path.starts_with(&self.run)
            || self
                .seed
                .as_ref()
                .is_some_and(|seed| path.starts_with(seed));
        if !caller && path.starts_with(&self.plugin_root) {
            &self.plugin_root
        } else {
            root
        }
    }

    /// Records a failure when a link lies below the plugin root on the way
    /// to `path`, for a path handed to another command rather than read here.
    pub(super) fn guard_derived(&self, path: &Path) -> bool {
        match super::refuse_links_below(self.anchor(&self.plugin_root, path), path) {
            Ok(()) => true,
            Err(message) => {
                self.ledger.fail(message);
                false
            }
        }
    }

    fn read(&self, root: &Path, path: &Path) -> Read {
        let root = self.anchor(root, path);
        match is_regular_file_under(root, path) {
            Err(message) => Read::Unreadable(message),
            Ok(false) => Read::Missing,
            Ok(true) => {
                match read_text_under(
                    root,
                    path,
                    &crate::safe_text::safe_path(&path.display().to_string()),
                ) {
                    Ok(text) => Read::Text(text),
                    Err(message) => Read::Unreadable(message),
                }
            }
        }
    }

    /// Whether `name` is a regular file below `root` (`Path.exists()` for a
    /// file; a link is reported, not followed).
    pub(super) fn exists(&self, root: &Path, name: &str) -> bool {
        let path = root.join(name);
        match is_regular_file_under(self.anchor(root, &path), &path) {
            Ok(found) => found,
            Err(message) => {
                self.ledger.fail(message);
                false
            }
        }
    }

    /// The text of `name` below `root`, with Python's universal newlines;
    /// `None` when it is absent (silently) or unreadable (recorded).
    pub(super) fn text_if_exists(&self, root: &Path, name: &str) -> Option<String> {
        match self.read(root, &root.join(name)) {
            Read::Text(text) => Some(text),
            Read::Missing => None,
            Read::Unreadable(message) => {
                self.ledger.fail(message);
                None
            }
        }
    }

    pub(super) fn parse_text(&self, text: &str, label: &str) -> Option<Json> {
        match pyjson::loads(text) {
            Ok(json) => Some(json),
            Err(error) => {
                self.ledger
                    .fail(format!("{label} is not valid JSON: {error}"));
                None
            }
        }
    }

    /// `json.loads(path.read_text())` of `name` below `root` when it exists.
    pub(super) fn json_if_exists(&self, root: &Path, name: &str) -> Option<Json> {
        let text = self.text_if_exists(root, name)?;
        self.parse_text(&text, name)
    }

    /// A required document, a missing one worded `missing NAME in DIR`.
    pub(super) fn json_in_dir(&self, root: &Path, name: &str) -> Option<Json> {
        self.json_in_dir_with_text(root, name).map(|(_, json)| json)
    }

    /// [`Self::json_in_dir`] that also returns the file's text.
    pub(super) fn json_in_dir_with_text(&self, root: &Path, name: &str) -> Option<(String, Json)> {
        let path = root.join(name);
        match self.read(root, &path) {
            Read::Text(text) => {
                let json = self.parse_text(&text, name)?;
                Some((text, json))
            }
            Read::Missing => {
                self.ledger
                    .fail(format!("missing {name} in {}", root.display()));
                None
            }
            Read::Unreadable(message) => {
                self.ledger.fail(message);
                None
            }
        }
    }

    /// A required document, a missing one worded `missing LABEL at PATH`.
    pub(super) fn json_labeled(&self, root: &Path, name: &str, label: &str) -> Option<Json> {
        let path = root.join(name);
        match self.read(root, &path) {
            Read::Text(text) => self.parse_text(&text, label),
            Read::Missing => {
                self.ledger
                    .fail(format!("missing {label} at {}", path.display()));
                None
            }
            Read::Unreadable(message) => {
                self.ledger.fail(message);
                None
            }
        }
    }

    /// The text of a file named by a path relative to the fixture directory
    /// (the expectation documents point at shipped skill files with
    /// `../../skills/...`); `None` when absent, a recorded failure when it
    /// is a link or unreadable.
    pub(super) fn read_relative(&self, relative: &str) -> Option<String> {
        self.text_if_exists(&self.fixture, relative)
    }

    /// The names of the files below the directory `relative` (to the fixture
    /// directory) that end with `suffix`; `None` when it is not a directory.
    /// Links are never listed.
    pub(super) fn list_relative(
        &self,
        relative: &str,
        suffix: &str,
    ) -> Option<std::collections::BTreeSet<String>> {
        let dir = self.fixture.join(relative);
        if let Err(message) = super::refuse_links_below(self.anchor(&self.fixture, &dir), &dir) {
            self.ledger.fail(message);
            return None;
        }
        if !fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.is_dir()) {
            return None;
        }
        let entries = fs::read_dir(&dir).ok()?;
        Some(
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_symlink()))
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.ends_with(suffix))
                .collect(),
        )
    }

    /// The expectation document, or an empty object and a `missing NAME`
    /// failure (the estimate program reports and stops on an empty one).
    pub(super) fn expectation_or_empty(&self, name: &str) -> Json {
        if let Some(json) = self.json_if_exists(&self.fixture, name) {
            json
        } else {
            if !self.exists(&self.fixture, name) {
                self.ledger.fail(format!("missing {name}"));
            }
            Json::Object(Vec::new())
        }
    }

    /// The program's own expectation document in the fixture set directory.
    pub(super) fn expectation(&self, name: &str) -> Option<Json> {
        self.json_labeled(&self.fixture, name, name)
    }

    /// A typed window on `exp`.
    pub(super) fn spec<'a>(&'a self, exp: &'a Json, name: &str) -> Spec<'a> {
        self.ledger.spec(exp, name)
    }

    /// The raw bytes of a file as text, without newline translation (for the
    /// byte-for-byte comparisons).
    pub(super) fn raw_text(&self, root: &Path, name: &str) -> Option<String> {
        let path = root.join(name);
        match is_regular_file_under(self.anchor(root, &path), &path) {
            Ok(true) => match read_regular_file(&path, MAX_ARTIFACT_BYTES) {
                Ok(text) => Some(text),
                Err(error) => {
                    self.ledger
                        .fail(format!("cannot read {name}: {}", read_failure(&error)));
                    None
                }
            },
            Ok(false) => None,
            Err(message) => {
                self.ledger.fail(message);
                None
            }
        }
    }
}

/// Runs one registered command handler in memory and returns its result with
/// the stdout and stderr it produced.
pub(super) fn capture(
    handler: Handler,
    positionals: &[&str],
    values: &[(&str, String)],
) -> (Result<(), ToolkitError>, String, String) {
    let invocation = Invocation {
        positionals: positionals.iter().map(|text| (*text).to_string()).collect(),
        flags: Vec::new(),
        values: values
            .iter()
            .map(|(flag, value)| ((*flag).to_string(), value.clone()))
            .collect(),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = {
        let mut output = Output::with_diagnostics(&mut stdout, &mut stderr);
        let result = handler(&invocation, &mut output);
        let _ = output.finish();
        result
    };
    (
        result,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

/// A scratch copy of a run directory, removed on drop. Symbolic links are
/// refused, not followed.
pub(super) struct Scratch(PathBuf);

impl Scratch {
    /// Copies `source` into a fresh directory below the system temp directory.
    pub(super) fn copy_of(source: &Path) -> Result<Self, String> {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let token = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!(
            "toolkit-check-expected-{}-{token:016x}",
            std::process::id()
        ));
        fs::create_dir(&root)
            .map_err(|error| format!("cannot create scratch directory: {error}"))?;
        let scratch = Self(root);
        let target = scratch.0.join("run");
        copy_tree(source, &target)?;
        Ok(scratch)
    }

    pub(super) fn run_dir(&self) -> PathBuf {
        self.0.join("run")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    let shown = crate::safe_text::safe_path(&source.display().to_string());
    let metadata =
        fs::symlink_metadata(source).map_err(|error| format!("cannot read {shown}: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "cannot read {shown}: symbolic links are not followed"
        ));
    }
    if metadata.is_dir() {
        fs::create_dir(target).map_err(|error| format!("cannot copy {shown}: {error}"))?;
        let entries =
            fs::read_dir(source).map_err(|error| format!("cannot read {shown}: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot read {shown}: {error}"))?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.is_file() {
        if metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(format!(
                "cannot copy {shown}: larger than {MAX_ARTIFACT_BYTES} bytes"
            ));
        }
        fs::copy(source, target)
            .map(|_| ())
            .map_err(|error| format!("cannot copy {shown}: {error}"))
    } else {
        Err(format!(
            "cannot copy {shown}: not a regular file or directory"
        ))
    }
}

fn find_program(name: &str) -> Option<&'static Program> {
    PROGRAMS.iter().find(|program| program.name == name)
}

fn kinds() -> String {
    PROGRAMS
        .iter()
        .map(|program| program.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Handler for `startup check-expected KIND RUN_DIR [SEED_DIR]
/// [--plugin-root DIR]`.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing or unknown `KIND`, a missing
/// `RUN_DIR`, or a `SEED_DIR` for a program that takes none; a runtime error
/// (exit status 1) after the failing report when any assertion fails.
pub fn check_expected_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(kind) = invocation.positional(0) else {
        return Err(ToolkitError::usage(
            "missing_argument",
            format!(
                "the following arguments are required: kind (one of {})",
                kinds()
            ),
        ));
    };
    let Some(program) = find_program(kind) else {
        return Err(ToolkitError::usage(
            "invalid_argument",
            format!(
                "argument kind: invalid choice: {} (choose from {})",
                toolkit_core::quote_untrusted(kind),
                kinds()
            ),
        ));
    };
    let Some(run) = invocation.positional(1) else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "the following arguments are required: run_dir",
        ));
    };
    let seed = invocation.positional(2);
    if seed.is_some() && !program.seeded {
        return Err(ToolkitError::usage(
            "invalid_argument",
            format!("kind {} takes no seed directory", program.name),
        ));
    }
    let plugin_root = PathBuf::from(invocation.value("--plugin-root").unwrap_or("."));
    let ledger = Ledger::default();
    let context = Context {
        ledger: &ledger,
        fixture: plugin_root.join("fixtures").join(program.set),
        plugin_root,
        run: PathBuf::from(run),
        seed: seed.map(PathBuf::from),
    };
    // `Stop` ends a program early with the findings so far. An early end that
    // recorded no finding evaluated nothing, so it must never read as a pass
    // (#4747).
    if (program.run)(&context).is_none() && ledger.fails().is_empty() {
        ledger.fail(format!(
            "{} stopped before evaluating its assertions",
            program.name
        ));
    }
    report(output, &ledger, program)
}

fn emit(output: &mut Output<'_>, text: &str) -> Result<(), ToolkitError> {
    output.line(&neutralize_line(text))
}

fn report(output: &mut Output<'_>, ledger: &Ledger, program: &Program) -> Result<(), ToolkitError> {
    let fails = ledger.fails();
    let notes = ledger.notes();
    let counted = matches!(program.style, Style::Counted);
    if !fails.is_empty() {
        if counted {
            for note in &notes {
                emit(output, &format!("note: {note}"))?;
            }
        }
        match program.style {
            Style::Counted => emit(output, &format!("FAIL ({}):", fails.len()))?,
            Style::Bare => emit(output, "FAIL")?,
            Style::Problems => emit(output, &format!("FAIL — {} problem(s)", fails.len()))?,
        }
        for fail in &fails {
            match program.style {
                Style::Counted => emit(output, &format!("  - {fail}"))?,
                Style::Bare | Style::Problems => emit(output, &format!(" - {fail}"))?,
            }
        }
        return Err(ToolkitError::runtime(
            "fixture_assertions_failed",
            format!("{} assertion(s) failed for {}", fails.len(), program.name),
        ));
    }
    if counted {
        for note in &notes {
            emit(output, &format!("note: {note}"))?;
        }
    }
    emit(output, program.pass)
}
