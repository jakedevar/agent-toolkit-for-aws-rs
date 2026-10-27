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

mod msk;

/// Usage text printed by `help` (stdout) and after usage errors (stderr).
pub const USAGE: &str = "\
Usage:
  toolkit skills list [ROOT] [--json]
  toolkit msk size [--emit-static-pricing] [--use-max-partitions]
                   [--no-rack-affined-consumers] [--region REGION]
                   [--pricing-source auto|api|static]
                   [--avg-data-in-mbs N] [--peak-data-in-mbs N]
                   [--avg-data-out-mbs N] [--peak-data-out-mbs N]
                   [--num-partitions N] [--retention-hours N]
                   [--primary-retention-hours N] [--replication-factor N]
                   [--utilization-standard F] [--utilization-express F]
                   [--pst-per-broker-mbs N] [--discount-pct PCT]
                   [--broker-classes standard|express|standard,express]
                   [--broker-quota N]
  toolkit msk compat CLUSTER_CONFIG [--out-dir DIR]
  toolkit msk sim compute [--instance-type T] [--broker-count N]
                          [--kafka-version V]
  toolkit msk sim render --instance-type T --broker-count N
                         --kafka-version V --output PATH
  toolkit msk sim validate --test e2e_latency|broker_restart --config JSON
                           [--instance-type T] [--broker-count N]
  toolkit --version
  toolkit help

Commands:
  skills list   List skill directories (directories containing SKILL.md) under
                ROOT, sorted by name then relative path. ROOT defaults to
                `skills` in the current directory.
  msk size      MSK broker sizing and monthly cost report (MiB/s, GiB, USD).
                The workload options are required unless
                --emit-static-pricing is given. Only --pricing-source static
                (us-east-1) prices offline; `auto` falls back to it, `api`
                needs a build with an AWS Pricing API client.
  msk compat    Five-pillar MSK Express compatibility assessment of a
                discovery cluster-config.json. Writes
                compatibility.<cluster_name>.json into --out-dir (default:
                current directory) and reports the path and the overall
                verdict on stderr.
  msk sim       MSK Express load-test simulation: `compute` derives capacity
                and fleet sizes, `render` writes a CloudFormation template
                with the sizing baked in, `validate` returns PASS/REJECT with
                warnings for a proposed test config.

Options:
  --json        Emit a JSON array of {name, description, path} objects.
  -h, --help    Print this help.
  -V, --version Print the version.
  Value options take `--name value` or `--name=value`; the msk commands are
  documented in crates/toolkit-cli/README.md.

Exit codes:
  0  success (including help, version and an empty result)
  1  runtime error (missing root, root not a directory, unreadable files,
     stdout write failure); msk size: pricing or workload error;
     msk sim validate: decision REJECT
  2  usage error (unknown command, missing or unknown arguments,
     or a non-UTF-8 argument); msk sim: invalid sizing, config or output
     (JSON {\"error\": ...} on stderr)";


/// Buffered stdout lines with the closed-pipe and write-error contract.
pub struct Output<'a> {
    sink: &'a mut dyn Write,
    closed: bool,
    diagnostics: Vec<String>,
    status: u8,
}

impl<'a> Output<'a> {
    /// Wraps a writer.
    pub fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            closed: false,
            diagnostics: Vec::new(),
            status: 0,
        }
    }

    /// Queues a line for stderr. Diagnostics are written by [`run`] after the
    /// handler returns, whatever its outcome, so commands that print a
    /// published stderr format (warnings, a JSON error object) keep stdout
    /// and stderr separate.
    pub fn diagnostic(&mut self, text: impl Into<String>) {
        self.diagnostics.push(text.into());
    }

    /// Sets the exit status [`run`] returns when the handler itself succeeds.
    /// Commands whose published contract uses a non-zero status together with
    /// regular output (a REJECT verdict, a legacy error code) set it here.
    pub fn set_exit_status(&mut self, status: u8) {
        self.status = status;
    }

    /// The status set with [`Output::set_exit_status`] (default 0).
    #[must_use]
    pub fn exit_status(&self) -> u8 {
        self.status
    }

    /// Takes the queued stderr lines.
    pub fn take_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diagnostics)
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
    /// Value options as `(name, value)`; a repeated option keeps its last
    /// value, like the legacy argument parsers.
    pub options: Vec<(String, String)>,
}

impl Invocation {
    /// Whether `name` (for example `--json`) was passed.
    #[must_use]
    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// The value of option `name` (for example `--region`), if given.
    #[must_use]
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn set_option(&mut self, name: &str, value: &str) {
        match self.options.iter_mut().find(|(key, _)| key == name) {
            Some(entry) => entry.1 = value.to_string(),
            None => self.options.push((name.to_string(), value.to_string())),
        }
    }

    /// The positional argument at `index`, if any.
    #[must_use]
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }
}

/// A command implementation.
pub type Handler = fn(&Invocation, &mut Output<'_>) -> Result<(), ToolkitError>;

/// One routable command.
#[derive(Clone, Copy)]
pub struct CommandSpec {
    /// Words that select the command, for example `["skills", "list"]`.
    pub path: &'static [&'static str],
    /// Long flags the command accepts, for example `["--json"]`.
    pub flags: &'static [&'static str],
    /// Long options that take a value (`--name value` or `--name=value`),
    /// for example `["--region"]`.
    pub value_flags: &'static [&'static str],
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
        path: &["msk", "size"],
        flags: msk::SIZE_FLAGS,
        value_flags: msk::SIZE_VALUE_FLAGS,
        max_positionals: 0,
        handler: msk::size,
    },
    CommandSpec {
        path: &["msk", "compat"],
        flags: &[],
        value_flags: msk::COMPAT_VALUE_FLAGS,
        max_positionals: 1,
        handler: msk::compat,
    },
    CommandSpec {
        path: &["msk", "sim", "compute"],
        flags: &[],
        value_flags: msk::SIM_COMPUTE_VALUE_FLAGS,
        max_positionals: 0,
        handler: msk::sim_compute,
    },
    CommandSpec {
        path: &["msk", "sim", "render"],
        flags: &[],
        value_flags: msk::SIM_RENDER_VALUE_FLAGS,
        max_positionals: 0,
        handler: msk::sim_render,
    },
    CommandSpec {
        path: &["msk", "sim", "validate"],
        flags: &[],
        value_flags: msk::SIM_VALIDATE_VALUE_FLAGS,
        max_positionals: 0,
        handler: msk::sim_validate,
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
        if let Some((name, value)) = word
            .split_once('=')
            .filter(|(name, _)| spec.value_flags.contains(name))
        {
            invocation.set_option(name, value);
            continue;
        }
        if spec.value_flags.contains(&word) {
            let Some(value) = rest.next() else {
                return Err(ToolkitError::usage(
                    "invalid_arguments",
                    format!("invalid arguments: option {word} requires a value"),
                ));
            };
            invocation.set_option(word, value);
            continue;
        }
        if word.starts_with('-') {
            if !spec.flags.contains(&word) {
                return Err(usage_error());
            }
            if !invocation.has_flag(word) {
                invocation.flags.push(arg.clone());
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
            let status = output.exit_status();
            for line in output.take_diagnostics() {
                // stderr may be closed too; there is nowhere left to report that.
                let _ = err.write_all(line.as_bytes());
                let _ = err.write_all(b"\n");
            }
            match result {
                Ok(()) => status,
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
            for flag in spec.flags.iter().chain(spec.value_flags) {
                assert!(USAGE.contains(flag), "usage lacks '{flag}'");
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
    fn output_stays_closed_after_a_failure() {
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        let mut output = Output::new(&mut sink);
        assert!(output.line("first").is_err());
        assert!(output.is_closed());
        assert!(output.line("second").is_ok(), "later lines are dropped");
        assert!(output.finish().is_ok());
    }

    #[test]
    fn value_options_accept_both_spellings_and_the_last_value_wins() {
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "msk",
            "sim",
            "compute",
            "--instance-type",
            "a",
            "--broker-count=9",
            "--instance-type",
            "b",
        ]) else {
            panic!("expected a runnable route");
        };
        assert_eq!(spec.path, &["msk", "sim", "compute"]);
        assert_eq!(invocation.option("--instance-type"), Some("b"));
        assert_eq!(invocation.option("--broker-count"), Some("9"));
        assert_eq!(invocation.option("--kafka-version"), None);
        assert_eq!(invocation.options.len(), 2);
    }

    #[test]
    fn value_options_consume_the_next_word_even_when_it_looks_like_a_flag() {
        let Ok(Routed::Run(_, invocation)) =
            route_words(&["msk", "size", "--avg-data-in-mbs", "-5", "--use-max-partitions"])
        else {
            panic!("expected a runnable route");
        };
        assert_eq!(invocation.option("--avg-data-in-mbs"), Some("-5"));
        assert!(invocation.has_flag("--use-max-partitions"));
        assert!(invocation.positionals.is_empty());
    }

    #[test]
    fn value_option_errors_are_usage_errors() {
        for values in [
            vec!["msk", "sim", "compute", "--broker-count"],
            vec!["msk", "sim", "compute", "--wat", "1"],
            vec!["msk", "sim", "compute", "extra"],
            vec!["msk", "size", "--json"],
            vec!["msk", "compat", "a", "b"],
            vec!["msk"],
            vec!["msk", "sim"],
            vec!["skills", "list", "--root", "x"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.exit_code(), 2);
        }
        let Err(error) = route_words(&["msk", "sim", "compute", "--broker-count"]) else {
            panic!("missing value must fail");
        };
        assert_eq!(
            error.message(),
            "invalid arguments: option --broker-count requires a value"
        );
    }

    #[test]
    fn handlers_can_queue_stderr_lines_and_set_the_exit_status() {
        fn handler(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
            output.line("data")?;
            output.diagnostic("warning one");
            output.diagnostic(String::from("warning two"));
            output.set_exit_status(7);
            Ok(())
        }
        let spec = CommandSpec {
            path: &["demo"],
            flags: &[],
            value_flags: &[],
            max_positionals: 0,
            handler,
        };
        let Ok(Routed::Run(spec, invocation)) = route(std::slice::from_ref(&spec), &words(&["demo"]))
        else {
            panic!("expected a runnable route");
        };
        let mut out = Vec::new();
        let mut output = Output::new(&mut out);
        (spec.handler)(&invocation, &mut output).expect("handler succeeds");
        assert_eq!(output.exit_status(), 7);
        assert_eq!(
            output.take_diagnostics(),
            vec!["warning one".to_string(), "warning two".to_string()]
        );
        assert!(output.take_diagnostics().is_empty());
        drop(output);
        assert_eq!(out, b"data\n");
    }
}
