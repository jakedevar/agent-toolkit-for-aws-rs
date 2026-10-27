//! `toolkit` command routing, output and error contracts.
//!
//! The binary in `main.rs` only forwards the process arguments and the real
//! stdout/stderr to [`run`]. Everything observable lives here so it can be
//! tested in-process with injected writers as well as through the real binary:
//!
//! - [`COMMANDS`] is the routing table. A [`CommandSpec`] names the command
//!   path (`skills list`), the flags it accepts, how many positional
//!   arguments it takes and a [`Handler`].
//! - [`route`] turns arguments into a [`Routed`] decision (help, version or a
//!   handler plus an [`Invocation`]) or a usage [`ToolkitError`].
//! - [`Output`] writes stdout lines. A closed pipe (`toolkit ... | head`) is a
//!   normal stop; any other write failure becomes a runtime error that is
//!   reported on stderr with exit status 1.
//! - [`run`] maps every [`ToolkitError`] to `error: <message>` on stderr (plus
//!   the usage text for usage errors) and its exit status.
//!
//! The full user-facing contract is in `crates/toolkit-cli/README.md`.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

use toolkit_core::{discover_skills, skills_to_json, ErrorKind, ToolkitError};

pub mod dynamodb;

/// Usage text printed by `help` (stdout) and after usage errors (stderr).
pub const USAGE: &str = "\
Usage:
  toolkit skills list [ROOT] [--json]
  toolkit dynamodb calculate-costs --model FILE [--requirements|-r FILE] [--output|-o FILE]
  toolkit dynamodb iterate-design --model FILE --config FILE --loop-state FILE
      [--manifest FILE] [--apply-change FILE] [--mode MODE] [--timestamp TS]
      [--yes-deploy] [--allow-spend] [--skip-deploy] [--calculator-only] [--dry-run]
  toolkit --version
  toolkit help

Commands:
  skills list   List skill directories (directories containing SKILL.md) under
                ROOT, sorted by name then relative path. ROOT defaults to
                `skills` in the current directory.
  dynamodb calculate-costs
                Price a dynamodb_data_model.json (RRU/WRU, GSI amplification,
                storage, vector indexes) and write a cost_report.md.
  dynamodb iterate-design
                Run ONE round of the human-driven DynamoDB design loop, then stop.

Options:
  --json        Emit a JSON array of {name, description, path} objects.
  -h, --help    Print this help.
  -V, --version Print the version.

Exit codes:
  0  success (including help, version and an empty result)
  1  runtime error (missing root, root not a directory, unreadable files,
     stdout write failure)
  2  usage error (unknown command, missing or unknown arguments,
     or a non-UTF-8 argument)
  Some commands keep a published status instead (iterate-design: 2 for a
  missing input file or refused change, 4 for a deploy without --yes-deploy,
  or the failed step's own status); see crates/toolkit-cli/README.md.";

/// Buffered stdout lines with the closed-pipe and write-error contract.
pub struct Output<'a> {
    sink: &'a mut dyn Write,
    closed: bool,
    diagnostics: Vec<String>,
}

impl<'a> Output<'a> {
    /// Wraps a writer.
    pub fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            closed: false,
            diagnostics: Vec::new(),
        }
    }

    /// Queues a non-fatal diagnostic line (for example a warning). [`run`]
    /// writes the queued lines to stderr, in order, after the handler
    /// returns, whether it succeeded or failed.
    pub fn diagnostic(&mut self, text: &str) {
        self.diagnostics.push(text.to_string());
    }

    /// The diagnostic lines queued so far.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// True once the reader went away; handlers should stop producing output.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Writes `text` and a newline. After a closed pipe this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a runtime error (`stdout_write`) for any write failure other
    /// than a closed pipe, which is treated as normal completion.
    pub fn line(&mut self, text: &str) -> Result<(), ToolkitError> {
        if self.closed {
            return Ok(());
        }
        let result = self
            .sink
            .write_all(text.as_bytes())
            .and_then(|()| self.sink.write_all(b"\n"));
        self.settle(result)
    }

    /// Flushes buffered output with the same error contract as [`Output::line`].
    ///
    /// # Errors
    ///
    /// Same as [`Output::line`].
    pub fn finish(&mut self) -> Result<(), ToolkitError> {
        if self.closed {
            return Ok(());
        }
        let result = self.sink.flush();
        self.settle(result)
    }

    fn settle(&mut self, result: io::Result<()>) -> Result<(), ToolkitError> {
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(())
            }
            Err(error) => {
                self.closed = true;
                Err(ToolkitError::runtime(
                    "stdout_write",
                    format!("cannot write to stdout: {error}"),
                ))
            }
        }
    }
}

/// Parsed arguments handed to a command handler.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Positional arguments in order.
    pub positionals: Vec<String>,
    /// Flags that were present (each at most once).
    pub flags: Vec<String>,
    /// Value options as `(long name, value)`; a repeated option keeps its
    /// last value, like `argparse`.
    pub options: Vec<(String, String)>,
}

impl Invocation {
    /// The value of option `name` (its long form, for example `--model`).
    #[must_use]
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn set_option(&mut self, name: &str, value: &str) {
        if let Some(slot) = self.options.iter_mut().find(|(key, _)| key == name) {
            slot.1 = value.to_string();
        } else {
            self.options.push((name.to_string(), value.to_string()));
        }
    }

    /// Whether `name` (for example `--json`) was passed.
    #[must_use]
    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// The positional argument at `index`, if any.
    #[must_use]
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }
}

/// A command implementation.
pub type Handler = fn(&Invocation, &mut Output<'_>) -> Result<(), ToolkitError>;

/// A flag that takes one value, accepted as `--name value`, `--name=value`
/// or (when declared) `-s value`.
#[derive(Debug, Clone, Copy)]
pub struct ValueFlag {
    /// Long form, for example `--model`. Options are stored under this name.
    pub long: &'static str,
    /// Optional short form, for example `-r`.
    pub short: Option<&'static str>,
}

/// One routable command.
#[derive(Clone, Copy)]
pub struct CommandSpec {
    /// Words that select the command, for example `["skills", "list"]`.
    pub path: &'static [&'static str],
    /// Long flags the command accepts, for example `["--json"]`.
    pub flags: &'static [&'static str],
    /// Flags that take a value, for example `--model FILE`.
    pub value_flags: &'static [ValueFlag],
    /// Maximum number of positional arguments.
    pub max_positionals: usize,
    /// The implementation.
    pub handler: Handler,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("path", &self.path)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

/// The routing table. New domain slices register their commands here.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        path: &["skills", "list"],
        flags: &["--json"],
        value_flags: &[],
        max_positionals: 1,
        handler: skills_list,
    },
    CommandSpec {
        path: &["dynamodb", "calculate-costs"],
        flags: &[],
        value_flags: dynamodb::CALCULATE_COSTS_VALUE_FLAGS,
        max_positionals: 0,
        handler: dynamodb::calculate_costs_command,
    },
    CommandSpec {
        path: &["dynamodb", "iterate-design"],
        flags: dynamodb::ITERATE_DESIGN_FLAGS,
        value_flags: dynamodb::ITERATE_DESIGN_VALUE_FLAGS,
        max_positionals: 0,
        handler: dynamodb::iterate_design_command,
    },
];

/// Outcome of routing the arguments.
#[derive(Debug)]
pub enum Routed<'a> {
    /// Print the usage text on stdout.
    Help,
    /// Print the version on stdout.
    Version,
    /// Run a command.
    Run(&'a CommandSpec, Invocation),
}

fn usage_error() -> ToolkitError {
    ToolkitError::usage("invalid_arguments", "invalid arguments")
}

/// Routes already-decoded arguments (without the program name).
///
/// `help`/`-h`/`--help` and `version`/`-V`/`--version` as the first argument
/// win over everything after them. Inside a command, `-h`/`--help` selects
/// help, any other `-`-prefixed word must be one of the command's flags and
/// there may be at most `max_positionals` other words.
///
/// # Errors
///
/// Returns a usage error (exit status 2) for no arguments, an unknown
/// command, an unknown flag or too many positional arguments.
pub fn route<'a>(specs: &'a [CommandSpec], args: &[String]) -> Result<Routed<'a>, ToolkitError> {
    let Some(first) = args.first().map(String::as_str) else {
        return Err(usage_error());
    };
    match first {
        "-h" | "--help" | "help" => return Ok(Routed::Help),
        "-V" | "--version" | "version" => return Ok(Routed::Version),
        _ => {}
    }
    let Some(spec) = specs.iter().find(|spec| {
        args.len() >= spec.path.len()
            && spec
                .path
                .iter()
                .zip(args)
                .all(|(expected, actual)| *expected == actual.as_str())
    }) else {
        return Err(usage_error());
    };

    let mut invocation = Invocation::default();
    let mut rest = args[spec.path.len()..].iter();
    while let Some(arg) = rest.next() {
        let word = arg.as_str();
        if word == "-h" || word == "--help" {
            return Ok(Routed::Help);
        }
        if word.starts_with('-') {
            if let Some(flag) = spec
                .value_flags
                .iter()
                .find(|flag| flag.long == word || flag.short == Some(word))
            {
                // Like argparse, a value may not itself look like an option.
                let Some(value) = rest.next().filter(|value| !value.starts_with('-')) else {
                    return Err(usage_error());
                };
                invocation.set_option(flag.long, value);
            } else if let Some((name, value)) = word
                .split_once('=')
                .filter(|(name, _)| spec.value_flags.iter().any(|flag| flag.long == *name))
            {
                invocation.set_option(name, value);
            } else {
                if !spec.flags.contains(&word) {
                    return Err(usage_error());
                }
                if !invocation.has_flag(word) {
                    invocation.flags.push(arg.clone());
                }
            }
        } else {
            if invocation.positionals.len() >= spec.max_positionals {
                return Err(usage_error());
            }
            invocation.positionals.push(arg.clone());
        }
    }
    Ok(Routed::Run(spec, invocation))
}

fn skills_list(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let root = PathBuf::from(invocation.positional(0).unwrap_or("skills"));
    let entries = discover_skills(&root)?;
    if invocation.has_flag("--json") {
        return output.line(&skills_to_json(&entries));
    }
    for entry in &entries {
        output.line(&entry.plain_line())?;
        if output.is_closed() {
            break;
        }
    }
    Ok(())
}

/// Writes the error report and returns the exit status.
fn report(err: &mut dyn Write, error: &ToolkitError) -> u8 {
    let text = match error.kind() {
        ErrorKind::Usage => format!("error: {}\n\n{USAGE}\n", error.message()),
        ErrorKind::Runtime => format!("error: {}\n", error.message()),
    };
    // stderr may be closed too; there is nowhere left to report that.
    let _ = err.write_all(text.as_bytes());
    error.exit_code()
}

fn print_static(out: &mut dyn Write, err: &mut dyn Write, text: &str) -> u8 {
    let mut output = Output::new(out);
    let result = output.line(text).and_then(|()| output.finish());
    match result {
        Ok(()) => 0,
        Err(error) => report(err, &error),
    }
}

/// Runs the CLI with the given arguments (without the program name) and
/// writers, returning the process exit status.
pub fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let mut text_args = Vec::with_capacity(args.len());
    for arg in args {
        let Some(text) = arg.to_str() else {
            return report(
                err,
                &ToolkitError::usage(
                    "invalid_arguments",
                    "invalid arguments: arguments must be valid UTF-8",
                ),
            );
        };
        text_args.push(text.to_string());
    }
    match route(COMMANDS, &text_args) {
        Ok(Routed::Help) => print_static(out, err, USAGE),
        Ok(Routed::Version) => {
            print_static(out, err, &format!("toolkit {}", env!("CARGO_PKG_VERSION")))
        }
        Ok(Routed::Run(spec, invocation)) => {
            let mut output = Output::new(out);
            let result = (spec.handler)(&invocation, &mut output).and_then(|()| output.finish());
            // Diagnostics (warnings) reach stderr before any error report, so
            // the final `error:` line is always the last thing printed.
            for line in output.diagnostics() {
                let _ = err.write_all(line.as_bytes());
                let _ = err.write_all(b"\n");
            }
            match result {
                Ok(()) => 0,
                Err(error) => report(err, &error),
            }
        }
        Err(error) => report(err, &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "toolkit-cli-lib-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn skill(root: &Path, relative: &str, content: &str) {
        let full = root.join(relative);
        fs::create_dir_all(&full).expect("create skill dir");
        fs::write(full.join("SKILL.md"), content).expect("write SKILL.md");
    }

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn os_args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    /// Runs the CLI in-process; returns (status, stdout, stderr).
    fn exec(values: &[&str]) -> (u8, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = run(&os_args(values), &mut out, &mut err);
        (
            status,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    /// Writer that fails every call with the given kind.
    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(self.0))
        }
    }

    fn route_words(values: &[&str]) -> Result<Routed<'static>, ToolkitError> {
        route(COMMANDS, &words(values))
    }

    #[test]
    fn routes_skills_list_with_flags_and_root_in_any_order() {
        for values in [
            vec!["skills", "list", "somewhere", "--json"],
            vec!["skills", "list", "--json", "somewhere"],
        ] {
            let Ok(Routed::Run(spec, invocation)) = route_words(&values) else {
                panic!("expected a runnable route for {values:?}");
            };
            assert_eq!(spec.path, &["skills", "list"]);
            assert_eq!(invocation.positional(0), Some("somewhere"));
            assert!(invocation.has_flag("--json"));
        }
    }

    #[test]
    fn duplicate_flags_are_collapsed_and_defaults_are_empty() {
        let Ok(Routed::Run(_, invocation)) = route_words(&["skills", "list", "--json", "--json"])
        else {
            panic!("expected a runnable route");
        };
        assert_eq!(invocation.flags, vec!["--json".to_string()]);
        assert_eq!(invocation.positional(0), None);
        assert!(!Invocation::default().has_flag("--json"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        for values in [
            vec!["--help"],
            vec!["-h"],
            vec!["help"],
            vec!["help", "ignored", "--wat"],
            vec!["skills", "list", "-h"],
            vec!["skills", "list", "--help"],
        ] {
            assert!(
                matches!(route_words(&values), Ok(Routed::Help)),
                "{values:?}"
            );
        }
        for values in [vec!["--version"], vec!["-V"], vec!["version", "x"]] {
            assert!(
                matches!(route_words(&values), Ok(Routed::Version)),
                "{values:?}"
            );
        }
    }

    #[test]
    fn usage_errors_are_exit_two_with_a_stable_code() {
        for values in [
            vec![],
            vec!["unknown"],
            vec!["skills"],
            vec!["skills", "export"],
            vec!["skills", "list", "--wat"],
            vec!["skills", "list", "-"],
            vec!["skills", "list", "a", "b"],
            vec!["list"],
            vec!["--json"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.code(), "invalid_arguments");
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn registry_commands_are_documented_in_usage() {
        for spec in COMMANDS {
            let joined = spec.path.join(" ");
            assert!(USAGE.contains(&joined), "usage lacks '{joined}'");
            for flag in spec.flags {
                assert!(USAGE.contains(flag), "usage lacks '{flag}'");
            }
            for flag in spec.value_flags {
                assert!(USAGE.contains(flag.long), "usage lacks '{}'", flag.long);
                if let Some(short) = flag.short {
                    assert!(USAGE.contains(short), "usage lacks '{short}'");
                }
            }
        }
        let paths: Vec<&[&str]> = COMMANDS.iter().map(|spec| spec.path).collect();
        for (index, path) in paths.iter().enumerate() {
            assert!(!paths[..index].contains(path), "duplicate route {path:?}");
        }
    }

    #[test]
    fn help_and_version_write_to_stdout_only() {
        let (status, stdout, stderr) = exec(&["--help"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(stdout, format!("{USAGE}\n"));
        let (status, stdout, stderr) = exec(&["-V"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(stdout, format!("toolkit {}\n", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn usage_error_report_has_message_then_usage_on_stderr() {
        let (status, stdout, stderr) = exec(&["bogus"]);
        assert_eq!(status, 2);
        assert_eq!(stdout, "");
        assert_eq!(stderr, format!("error: invalid arguments\n\n{USAGE}\n"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_are_usage_errors() {
        use std::os::unix::ffi::OsStringExt;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = run(
            &[OsString::from_vec(b"skills\xff".to_vec())],
            &mut out,
            &mut err,
        );
        assert_eq!(status, 2);
        assert!(out.is_empty());
        let stderr = String::from_utf8(err).expect("utf-8 stderr");
        assert!(stderr.starts_with("error: invalid arguments: arguments must be valid UTF-8"));
        assert!(stderr.contains("Usage:"));
    }

    #[test]
    fn skills_list_prints_plain_and_json_in_sorted_order() {
        let root = TempDir::new("list");
        skill(root.path(), "z/zulu", "---\ndescription: last\n---\n");
        skill(
            root.path(),
            "a/alpha",
            "---\nname: alpha\ndescription: first\n---\n",
        );
        let root_arg = root.path().to_str().expect("utf-8 temp path");

        let (status, stdout, stderr) = exec(&["skills", "list", root_arg]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(stdout, "alpha\tfirst\nzulu\tlast\n");

        let (status, stdout, stderr) = exec(&["skills", "list", root_arg, "--json"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(
            stdout,
            "[\n  {\n    \"name\": \"alpha\",\n    \"description\": \"first\",\n    \"path\": \"a/alpha\"\n  },\n  {\n    \"name\": \"zulu\",\n    \"description\": \"last\",\n    \"path\": \"z/zulu\"\n  }\n]\n"
        );
    }

    #[test]
    fn runtime_errors_exit_one_with_message_only() {
        let root = TempDir::new("missing");
        let missing = root.path().join("absent");
        let (status, stdout, stderr) =
            exec(&["skills", "list", missing.to_str().expect("utf-8 temp path")]);
        assert_eq!(status, 1);
        assert_eq!(stdout, "");
        assert!(stderr.starts_with("error: root does not exist: "));
        assert!(!stderr.contains("Usage:"), "runtime errors omit usage text");
    }

    #[test]
    fn closed_pipe_is_success_and_stops_output() {
        let root = TempDir::new("pipe");
        skill(root.path(), "one", "---\nname: one\n---\n");
        let mut sink = FailingWriter(io::ErrorKind::BrokenPipe);
        let mut err = Vec::new();
        let args = os_args(&["skills", "list", root.path().to_str().expect("utf-8 path")]);
        assert_eq!(run(&args, &mut sink, &mut err), 0);
        assert!(err.is_empty(), "a closed pipe prints nothing on stderr");

        let json_args = os_args(&[
            "skills",
            "list",
            root.path().to_str().expect("utf-8 path"),
            "--json",
        ]);
        assert_eq!(run(&json_args, &mut sink, &mut err), 0);
        assert!(err.is_empty());
    }

    #[test]
    fn other_stdout_write_errors_are_reported_with_exit_one() {
        let root = TempDir::new("write-error");
        skill(root.path(), "one", "---\nname: one\n---\n");
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        let mut err = Vec::new();
        let args = os_args(&["skills", "list", root.path().to_str().expect("utf-8 path")]);
        assert_eq!(run(&args, &mut sink, &mut err), 1);
        let stderr = String::from_utf8(err).expect("utf-8 stderr");
        assert!(
            stderr.starts_with("error: cannot write to stdout: "),
            "{stderr}"
        );
        assert!(!stderr.contains("Usage:"));

        // Help goes through the same contract.
        let mut err = Vec::new();
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        assert_eq!(run(&os_args(&["--help"]), &mut sink, &mut err), 1);
        assert!(String::from_utf8(err)
            .expect("utf-8 stderr")
            .contains("cannot write to stdout"));
    }

    #[test]
    fn value_options_accept_three_spellings_and_keep_the_last() {
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "dynamodb",
            "calculate-costs",
            "--model",
            "a.json",
            "-o",
            "x.md",
            "--output=y.md",
            "-r",
            "r.json",
            "--requirements=",
        ]) else {
            panic!("expected a runnable route");
        };
        assert_eq!(spec.path, &["dynamodb", "calculate-costs"]);
        assert_eq!(invocation.option("--model"), Some("a.json"));
        assert_eq!(invocation.option("--output"), Some("y.md"), "last wins");
        assert_eq!(invocation.option("--requirements"), Some(""), "empty value");
        assert_eq!(invocation.option("--absent"), None);
        assert!(invocation.positionals.is_empty());
        assert!(invocation.flags.is_empty());
    }

    #[test]
    fn malformed_value_options_are_usage_errors() {
        for values in [
            // No value, or a value that looks like another option.
            vec!["dynamodb", "calculate-costs", "--model"],
            vec!["dynamodb", "calculate-costs", "--model", "--output", "x"],
            vec!["dynamodb", "calculate-costs", "-o"],
            vec!["dynamodb", "calculate-costs", "-o", "-r"],
            // `=` only splits a declared value option; a short form has no `=`.
            vec!["dynamodb", "calculate-costs", "--bogus=1"],
            vec!["dynamodb", "calculate-costs", "-o=x"],
            // A value option without a short form, and abbreviations.
            vec!["dynamodb", "iterate-design", "-m", "x"],
            vec!["dynamodb", "calculate-costs", "--req", "x"],
            // A stray positional.
            vec!["dynamodb", "calculate-costs", "--model", "a.json", "extra"],
            // A value option is not a boolean flag.
            vec!["dynamodb", "iterate-design", "--model=a", "--dry-run=1"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.exit_code(), 2);
        }
        // Boolean flags still work beside value options.
        let Ok(Routed::Run(_, invocation)) = route_words(&[
            "dynamodb",
            "iterate-design",
            "--dry-run",
            "--model",
            "m",
            "--yes-deploy",
        ]) else {
            panic!("expected a runnable route");
        };
        assert!(invocation.has_flag("--dry-run") && invocation.has_flag("--yes-deploy"));
        assert_eq!(invocation.option("--model"), Some("m"));
    }

    #[test]
    fn diagnostics_reach_stderr_even_when_the_command_succeeds() {
        let root = TempDir::new("diagnostics");
        fs::write(
            root.path().join("model.json"),
            r#"{"tables":[],"access_patterns":[{"pattern_id":"P","operation":"GetItem"}]}"#,
        )
        .expect("write model");
        let model = root.path().join("model.json");
        let missing = root.path().join("absent.json");
        let report = root.path().join("report.md");
        let (status, stdout, stderr) = exec(&[
            "dynamodb",
            "calculate-costs",
            "--model",
            model.to_str().expect("utf-8 path"),
            "--requirements",
            missing.to_str().expect("utf-8 path"),
            "--output",
            report.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(status, 0, "{stderr}");
        assert_eq!(
            stderr,
            format!(
                "Warning: Could not load requirements (path does not exist: {}), using defaults.\n",
                missing.display()
            )
        );
        assert_eq!(
            stdout,
            format!(
                "Cost report written to {}\nAnalyzed 1 access patterns across 0 tables.\n",
                report.display()
            )
        );
        assert!(report.exists());
    }

    #[test]
    fn output_stays_closed_after_a_failure() {
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        let mut output = Output::new(&mut sink);
        assert!(output.line("first").is_err());
        assert!(output.is_closed());
        assert!(output.line("second").is_ok(), "later lines are dropped");
        assert!(output.finish().is_ok());
    }
}
