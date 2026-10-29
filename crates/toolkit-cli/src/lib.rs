//! `toolkit` command routing, output and error contracts.
//!
//! The binary in `main.rs` only forwards the process arguments and the real
//! stdout/stderr to [`run`]. Everything observable lives here so it can be
//! tested in-process with injected writers as well as through the real binary:
//!
//! - [`COMMANDS`] is the routing table and [`USAGE`] the help text. Both are
//!   assembled from the registration each slice keeps in its own module
//!   (`registry.rs`, `docs/migration/slice-registration.md`). A
//!   [`CommandSpec`] names the command path (`skills list`), the flags it
//!   accepts, how many positional arguments it takes and a [`Handler`].
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

use safe_text::CallerText;

mod ai_data;
mod ai_models;
pub mod asm_exec;
mod aurora;
mod bedrock_agent;
mod bedrock_probes;
mod bedrock_report;
mod bedrock_resolve;
pub mod cloudwatch;
pub mod docgraph;
pub mod dynamodb;
mod dynamodb_bench;
mod dynamodb_deploy;
pub mod elasticache;
pub mod manifests;
mod marketplace;
mod msk;
mod registry;
mod relational;
mod safe_text;
mod skills;
pub mod spec;
pub mod startup;
mod sync;
mod telemetry;

pub use registry::{COMMANDS, USAGE};

/// Buffered stdout lines with the closed-pipe and write-error contract.
pub struct Output<'a> {
    sink: &'a mut dyn Write,
    diagnostics: Option<&'a mut dyn Write>,
    closed: bool,
    status: u8,
    /// The command-line values the sinks must not echo as given: values with
    /// a newline, credential-shaped values and overlong values (#4639, #4941).
    caller: CallerText,
}

impl<'a> Output<'a> {
    /// Wraps a writer.
    pub fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            diagnostics: None,
            closed: false,
            status: 0,
            caller: CallerText::default(),
        }
    }

    /// Wraps a stdout writer plus a diagnostics (stderr) writer for commands
    /// that report findings line by line (#4375).
    pub fn with_diagnostics(sink: &'a mut dyn Write, diagnostics: &'a mut dyn Write) -> Self {
        Self {
            sink,
            diagnostics: Some(diagnostics),
            closed: false,
            status: 0,
            caller: CallerText::default(),
        }
    }

    /// Registers the command-line values the sinks must not echo as given:
    /// a value with a newline is escaped onto one line, a credential-shaped
    /// value is redacted and an overlong one bounded wherever a diagnostic
    /// echoes it (#4941). Stdout is not rewritten (see [`Output::line`]).
    /// Ordinary values change nothing.
    pub fn set_untrusted(&mut self, args: &[String]) {
        self.caller = CallerText::from_args(args);
    }

    /// Writes `text` and a newline to the diagnostics writer, if there is one.
    ///
    /// Failures are ignored: when stderr itself is gone there is nowhere left
    /// to report that, which is also the rule for the final error report.
    ///
    /// Control characters other than newline (ESC, BEL, CR, tab), bidirectional
    /// overrides and similar invisible characters are escaped so echoed caller
    /// text cannot drive the terminal (#4639); newlines stay because some
    /// diagnostics are multi-line blocks, except inside a command-line value
    /// that itself contains a newline (see [`Output::set_untrusted`]), which is
    /// escaped wherever it is echoed verbatim. A credential-shaped or overlong
    /// command-line value echoed here is redacted, then bounded to 200
    /// characters, then escaped (#4941). A handler that echoes other untrusted
    /// text must quote it itself. Safe text is written unchanged.
    pub fn diagnostic(&mut self, text: &str) {
        if let Some(writer) = self.diagnostics.as_mut() {
            let _ = writer
                .write_all(self.caller.diagnostic(text).as_bytes())
                .and_then(|()| writer.write_all(b"\n"));
        }
    }

    /// Sets the exit status [`run`] returns when the handler itself succeeds.
    /// Commands whose published contract uses a non-zero status together with
    /// regular output (a REJECT verdict, a legacy error code) set it here (#4387).
    pub fn set_exit_status(&mut self, status: u8) {
        self.status = status;
    }

    /// The status set with [`Output::set_exit_status`] (default 0).
    #[must_use]
    pub fn exit_status(&self) -> u8 {
        self.status
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
    ///
    /// The line is written exactly as given: it is never rewritten after it is
    /// formatted, so JSON stays valid and typed fields keep their schema. A
    /// handler redacts each command-line value where it builds stdout data
    /// (`safe_text::redacted_value`, #4941, #4981).
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
    /// Value flags that were present with their value: one entry per plain value
    /// flag (the last one wins), one entry per occurrence, in command-line order,
    /// for a [`CommandSpec::repeatable_flags`] flag.
    pub values: Vec<(String, String)>,
}

impl Invocation {
    /// Whether `name` (for example `--json`) was passed.
    #[must_use]
    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// The value of the value flag `name` (for example `--plugin`; the last one
    /// when repeated).
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(flag, _)| flag == name)
            .map(|(_, value)| value.as_str())
    }

    /// The positional argument at `index`, if any.
    #[must_use]
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    /// Every value of the value flag `name` in command-line order: one for a
    /// plain value flag (its last), one per occurrence for a
    /// [`CommandSpec::repeatable_flags`] flag, none when absent.
    pub fn values_of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.values
            .iter()
            .filter(move |(flag, _)| flag == name)
            .map(|(_, value)| value.as_str())
    }

    fn store_value(&mut self, spec: &CommandSpec, name: &str, value: &str) {
        if spec.repeatable_flags.contains(&name) {
            self.values.push((name.to_string(), value.to_string()));
        } else {
            self.set_value(name, value);
        }
    }

    fn set_value(&mut self, name: &str, value: &str) {
        if let Some(slot) = self.values.iter_mut().find(|(flag, _)| flag == name) {
            slot.1 = value.to_string();
        } else {
            self.values.push((name.to_string(), value.to_string()));
        }
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
    /// Long flags that take one value, written `--name VALUE` or
    /// `--name=VALUE`. A missing value is a usage error; see `strict_values`.
    pub value_flags: &'static [&'static str],
    /// Whether a value flag rejects an empty value and one starting with `-`
    /// (so `--plugin-root --check` is a usage error). Off for
    /// `manifests validate`, whose `--plugin` takes the next word whatever it
    /// looks like (#4375); on for `sync` (#4377).
    pub strict_values: bool,
    /// Long flags that take one value like `value_flags` (same spellings, same
    /// `strict_values` check on every value) but may be repeated, argparse
    /// `action="append"` style: every occurrence is kept in command-line order
    /// and read with [`Invocation::values_of`]. A flag listed here is a value
    /// flag without also being listed in `value_flags`; listed in both, it is
    /// repeatable. A plain `value_flags` flag keeps only its last value (#4670).
    pub repeatable_flags: &'static [&'static str],
    /// Maximum number of positional arguments.
    pub max_positionals: usize,
    /// Whether every word after the command path is a positional argument,
    /// verbatim: dash-prefixed words, `--` and `-h`/`--help` are not
    /// interpreted and `max_positionals` is ignored. For commands that run
    /// another program (`asm exec`, #4451) whose own flags must reach it.
    pub passthrough: bool,
    /// The implementation.
    pub handler: Handler,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("path", &self.path)
            .field("flags", &self.flags)
            .field("value_flags", &self.value_flags)
            .finish_non_exhaustive()
    }
}

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

/// The pseudo-flag naming a command's positional arguments in [`STDIO_DASH`].
pub const STDIO_DASH_POSITIONAL: &str = "<positional>";

/// The (command path, flag) pairs that accept the exact token `-` as the
/// argparse stdin marker (#4743, #4780).
///
/// Fail closed: `-` is accepted ONLY where a pair is listed here, as the value
/// of the named value flag (`--f -` and `--f=-`) or, for the pseudo-flag
/// [`STDIO_DASH_POSITIONAL`], as a positional. The command path is the
/// space-joined [`CommandSpec::path`] (for example `"cloudwatch snapshots"`).
/// Everywhere else `-` is a usage error (exit 2) naming the flag, for EVERY
/// value flag and positional of every command, `strict_values` or not (#4780):
/// most slices turn a path argument straight into a filesystem path, where an
/// accepted `-` would silently read or overwrite a file named `-`. `./-` stays
/// a legal explicit literal path.
///
/// The list is empty: no registered parameter reads standard input (the
/// `cloudwatch` commands' `--json -` / `--json-file -` of the Python originals are
/// deliberately not accepted, see `docs/migration/cw-api.md`). A slice that
/// wants stdin support adds its pair here in the same change that makes its
/// handler read stdin deliberately (bounded, with a finite wait) and lists it in
/// `docs/migration/cli-bare-dash.md`.
pub const STDIO_DASH: &[(&str, &str)] = &[];

/// The (command path, value flag) pairs whose exact value `-` is a legitimate
/// LITERAL value passed through to the handler unchanged (#4780, #4934).
///
/// This is not a stdio marker: each listed flag is a selector, id, name or text
/// that the Python original takes as an arbitrary string with no `-` sentinel
/// (argparse hands a lone `-` through as an ordinary value), and whose Rust
/// handler uses it only as a key, a request field or output text, never as a
/// filesystem path, not even as a component of one (a filename, a suffix, a
/// directory name, a join or a `format!` into a path). A parameter that names a
/// file or directory, or is composed into a path, MUST NOT be listed here: it
/// keeps the fail-closed refusal and `./-` is the way to name a file called `-`
/// (`bedrock-report render --date-suffix`, which selects
/// `MIGRATION_REPORT_<suffix>.md`, is refused for that reason, #4955).
/// Value flags only (`--f -` and `--f=-`);
/// a positional is never listed. A pair is listed in at most one of this list
/// and [`STDIO_DASH`].
///
/// Every entry is audited, with its Python `file:line` and its Rust consumer,
/// in `docs/migration/cli-bare-dash.md`. The contract test
/// `every_value_flag_and_positional_dash_is_refused_or_in_exactly_one_list`
/// (`tests/diag_contract.rs`) fails for a registered value flag that is neither
/// refused nor listed, and `a_literal_dash_flag_never_reaches_the_filesystem`
/// runs each pair against a file named `-` that must stay untouched.
pub const LITERAL_DASH: &[(&str, &str)] = &[
    ("startup verify-model-path", "--workload"),
    ("bedrock-agent fetch", "--agent-id"),
    ("bedrock-agent fetch", "--agent-version"),
    ("bedrock-agent fetch", "--agent-alias-id"),
    ("bedrock-agent fetch", "--region"),
    ("bedrock-agent fetch", "--profile"),
    ("bedrock-resolve identity", "--provider"),
    ("bedrock-resolve identity", "--source"),
    ("bedrock-resolve identity", "--target"),
    ("bedrock-resolve pricing", "--region"),
    ("bedrock-resolve pricing", "--models"),
    ("cloudwatch instrumentation", "--region"),
    ("cloudwatch instrumentation", "--profile"),
    ("cloudwatch snapshots", "--region"),
    ("cloudwatch snapshots", "--profile"),
    ("cloudwatch evaluate-traces", "--trace-id"),
    ("cloudwatch evaluate-traces", "--trace-ids"),
    ("cloudwatch evaluate-traces", "--session-id"),
    ("cloudwatch evaluate-traces", "--evaluator-id"),
    ("cloudwatch evaluate-traces", "--evaluator-ids"),
    ("cloudwatch evaluate-traces", "--region"),
    ("cloudwatch evaluate-traces", "--kms-key-id"),
    ("cloudwatch capture-dataset", "--trace-ids"),
    ("cloudwatch capture-dataset", "--dataset-name"),
    ("cloudwatch capture-dataset", "--dataset-id"),
    ("cloudwatch capture-dataset", "--description"),
    ("cloudwatch capture-dataset", "--region"),
    ("msk size", "--region"),
    ("ai-models search", "--region"),
];

/// Whether `-` is an accepted marker for `flag` (a value flag name, or
/// [`STDIO_DASH_POSITIONAL`]) of `spec` according to `allowed`.
fn dash_listed(allowed: &[(&str, &str)], spec: &CommandSpec, flag: &str) -> bool {
    let path = spec.path.join(" ");
    allowed
        .iter()
        .any(|(command, listed)| *command == path && *listed == flag)
}

/// The two allowlists of the exact value `-`: stdio markers and literals.
#[derive(Clone, Copy)]
struct DashLists<'a> {
    stdio: &'a [(&'a str, &'a str)],
    literal: &'a [(&'a str, &'a str)],
}

/// Whether `value` may follow the value flag `flag` of `spec` (see
/// [`CommandSpec::strict_values`]). The exact token `-` is refused for every
/// command unless the pair is listed in [`STDIO_DASH`] or [`LITERAL_DASH`]; a
/// listed pair takes it as an ordinary value, whatever `strict_values` says.
fn value_allowed(
    spec: &CommandSpec,
    lists: DashLists<'_>,
    flag: &str,
    value: &str,
) -> Result<(), ToolkitError> {
    if value == "-" {
        return if dash_listed(lists.stdio, spec, flag) || dash_listed(lists.literal, spec, flag) {
            Ok(())
        } else {
            Err(dash_value_error(flag))
        };
    }
    if spec.strict_values && (value.is_empty() || value.starts_with('-')) {
        return Err(usage_error());
    }
    Ok(())
}

/// The usage error for a bare `-` given to `flag`: one line naming the flag
/// (always a registered flag name, never caller text).
pub(crate) fn dash_value_error(flag: &str) -> ToolkitError {
    ToolkitError::usage(
        "invalid_arguments",
        format!(
            "invalid arguments: {flag} does not accept - (standard input/output); \
             use ./- for a file named -"
        ),
    )
}

/// The usage error for a bare `-` positional argument.
pub(crate) fn dash_positional_error() -> ToolkitError {
    ToolkitError::usage(
        "invalid_arguments",
        "invalid arguments: a bare - argument is not accepted (standard input/output); \
         use ./- for a file named -",
    )
}

/// Whether `name` is a value flag of `spec`, plain or repeatable.
fn is_value_flag(spec: &CommandSpec, name: &str) -> bool {
    spec.value_flags.contains(&name) || spec.repeatable_flags.contains(&name)
}

/// Routes already-decoded arguments (without the program name).
///
/// `help`/`-h`/`--help` and `version`/`-V`/`--version` as the first argument
/// win over everything after them. Inside a command, `-h`/`--help` selects
/// help, any other `-`-prefixed word must be one of the command's flags or
/// value flags and there may be at most `max_positionals` other words. A value
/// flag takes the next word (or the text after `=`) as its value; repeating it
/// keeps the last value (every value, in order, for a flag in
/// [`CommandSpec::repeatable_flags`]), and a value flag at the end of the
/// arguments with no value is a usage error.
///
/// # Errors
///
/// Returns a usage error (exit status 2) for no arguments, an unknown
/// command, an unknown flag or too many positional arguments.
pub fn route<'a>(specs: &'a [CommandSpec], args: &[String]) -> Result<Routed<'a>, ToolkitError> {
    route_lists(
        specs,
        DashLists {
            stdio: STDIO_DASH,
            literal: LITERAL_DASH,
        },
        args,
    )
}

/// [`route`] with explicit allowlists, so tests can exercise the opt-ins
/// independently of the shipped constants.
fn route_lists<'a>(
    specs: &'a [CommandSpec],
    lists: DashLists<'_>,
    args: &[String],
) -> Result<Routed<'a>, ToolkitError> {
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
    if spec.passthrough {
        invocation.positionals = args[spec.path.len()..].to_vec();
        return Ok(Routed::Run(spec, invocation));
    }
    let mut rest = args[spec.path.len()..].iter();
    while let Some(arg) = rest.next() {
        let word = arg.as_str();
        if word == "-h" || word == "--help" {
            return Ok(Routed::Help);
        }
        // A bare `-` is a positional only where STDIO_DASH lists the command's
        // `<positional>`; otherwise it is an unknown flag, as before #4743.
        if word.starts_with('-')
            && !(word == "-" && dash_listed(lists.stdio, spec, STDIO_DASH_POSITIONAL))
        {
            if is_value_flag(spec, word) {
                // `--name value`: the next word is the value.
                let Some(value) = rest.next() else {
                    return Err(usage_error());
                };
                value_allowed(spec, lists, word, value)?;
                invocation.store_value(spec, word, value);
            } else if let Some((name, value)) = word
                .split_once('=')
                .filter(|(name, _)| is_value_flag(spec, name))
            {
                value_allowed(spec, lists, name, value)?;
                invocation.store_value(spec, name, value);
            } else if word == "-" {
                return Err(dash_positional_error());
            } else if !spec.flags.contains(&word) {
                return Err(usage_error());
            } else if !invocation.has_flag(word) {
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

/// [`route_lists`] with only a [`STDIO_DASH`]-style allowlist (tests).
#[cfg(test)]
fn route_with<'a>(
    specs: &'a [CommandSpec],
    stdio_dash: &[(&str, &str)],
    args: &[String],
) -> Result<Routed<'a>, ToolkitError> {
    route_lists(
        specs,
        DashLists {
            stdio: stdio_dash,
            literal: &[],
        },
        args,
    )
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

/// Writes the error report and returns the exit status. The message passes
/// through [`CallerText::diagnostic`]: a command-line value it echoes is shown
/// escaped and, when credential-shaped or overlong, redacted then bounded
/// (#4639, #4941).
fn report(err: &mut dyn Write, error: &ToolkitError, caller: &CallerText) -> u8 {
    let message = caller.diagnostic(error.message());
    let text = match error.kind() {
        ErrorKind::Usage => format!("error: {message}\n\n{USAGE}\n"),
        ErrorKind::Runtime => format!("error: {message}\n"),
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
        Err(error) => report(err, &error, &CallerText::default()),
    }
}

/// Runs the CLI with the given arguments (without the program name) and
/// writers, returning the process exit status.
pub fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    // A path named in a shared file-contract error is redacted, then bounded,
    // where the message is formatted, not only at the sinks (#4941).
    toolkit_core::set_path_display(|path: &str| safe_text::shown_value(path).into_owned());
    let mut text_args = Vec::with_capacity(args.len());
    for arg in args {
        let Some(text) = arg.to_str() else {
            return report(
                err,
                &ToolkitError::usage(
                    "invalid_arguments",
                    "invalid arguments: arguments must be valid UTF-8",
                ),
                &CallerText::default(),
            );
        };
        text_args.push(text.to_string());
    }
    let caller = CallerText::from_args(&text_args);
    match route(COMMANDS, &text_args) {
        Ok(Routed::Help) => print_static(out, err, USAGE),
        Ok(Routed::Version) => {
            print_static(out, err, &format!("toolkit {}", env!("CARGO_PKG_VERSION")))
        }
        Ok(Routed::Run(spec, invocation)) => {
            // A passthrough command's words belong to another program and may
            // be secret references, not secrets; it redacts what it resolves
            // itself, so only the newline rule applies (#4941).
            let caller = if spec.passthrough {
                CallerText::newlines_only(&text_args)
            } else {
                caller
            };
            let mut output = Output::with_diagnostics(out, err);
            output.caller = caller.clone();
            let result = (spec.handler)(&invocation, &mut output).and_then(|()| output.finish());
            match result {
                Ok(()) => output.exit_status(),
                Err(error) => report(err, &error, &caller),
            }
        }
        Err(error) => report(err, &error, &caller),
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
    fn value_flags_take_one_value_in_either_spelling() {
        for values in [
            vec![
                "sync",
                "plugin-skills",
                "--root",
                "r",
                "--plugin",
                "p",
                "--check",
            ],
            vec!["sync", "plugin-skills", "--check", "--plugin=p", "--root=r"],
        ] {
            let Ok(Routed::Run(spec, invocation)) = route_words(&values) else {
                panic!("expected a runnable route for {values:?}");
            };
            assert_eq!(spec.path, &["sync", "plugin-skills"]);
            assert_eq!(invocation.value("--root"), Some("r"));
            assert_eq!(invocation.value("--plugin"), Some("p"));
            assert!(invocation.has_flag("--check"));
            assert!(invocation.positionals.is_empty());
        }
        let Ok(Routed::Run(_, invocation)) =
            route_words(&["sync", "vendored", "--plugin-root", "a", "--plugin-root=b"])
        else {
            panic!("expected a runnable route");
        };
        assert_eq!(
            invocation.value("--plugin-root"),
            Some("b"),
            "last one wins"
        );
        assert_eq!(invocation.value("--root"), None);
    }

    #[test]
    fn value_flag_misuse_is_a_usage_error() {
        for values in [
            vec!["sync", "vendored", "--plugin-root"],
            vec!["sync", "vendored", "--plugin-root", "--check"],
            vec!["sync", "vendored", "--plugin-root", ""],
            vec!["sync", "vendored", "--plugin-root="],
            vec!["sync", "vendored", "--plugin-root=-x"],
            vec!["sync", "vendored", "--check=1"],
            vec!["sync", "vendored", "--root", "x"],
            vec!["sync", "vendored", "positional"],
            vec!["sync", "plugin-skills", "--json"],
            vec!["skills", "list", "--root", "x"],
            vec!["sync"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.exit_code(), 2);
        }
    }

    /// Two commands with the same flags: `strict` rejects empty and dash values,
    /// `loose` takes the next word whatever it looks like.
    const REPEATABLE_SPECS: &[CommandSpec] = &[
        CommandSpec {
            path: &["strict"],
            flags: &["--json"],
            value_flags: &["--mode"],
            strict_values: true,
            repeatable_flags: &["--workload"],
            max_positionals: 1,
            passthrough: false,
            handler: skills_list,
        },
        CommandSpec {
            path: &["loose"],
            flags: &["--json"],
            value_flags: &["--mode", "--both"],
            strict_values: false,
            repeatable_flags: &["--workload", "--both"],
            max_positionals: 1,
            passthrough: false,
            handler: skills_list,
        },
    ];

    fn route_repeatable(values: &[&str]) -> Result<Invocation, ToolkitError> {
        match route(REPEATABLE_SPECS, &words(values))? {
            Routed::Run(_, invocation) => Ok(invocation),
            other => panic!("expected a runnable route, got {other:?}"),
        }
    }

    fn workloads(values: &[&str]) -> Vec<String> {
        let invocation = route_repeatable(values).expect("routes");
        invocation
            .values_of("--workload")
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_repeatable_flag_keeps_every_value_in_order_in_either_spelling() {
        assert_eq!(
            workloads(&[
                "strict",
                "--workload",
                "a",
                "--workload=b",
                "--workload",
                "c"
            ]),
            ["a", "b", "c"]
        );
        // The same value twice is two values, not a collapse.
        assert_eq!(
            workloads(&["strict", "--workload=a", "--workload", "a"]),
            ["a", "a"]
        );
        // `value` is still the last one.
        let invocation =
            route_repeatable(&["strict", "--workload", "a", "--workload", "b"]).expect("routes");
        assert_eq!(invocation.value("--workload"), Some("b"));
    }

    #[test]
    fn a_single_or_absent_repeatable_flag_yields_one_or_no_value() {
        assert_eq!(workloads(&["strict", "--workload", "only"]), ["only"]);
        assert_eq!(workloads(&["strict", "--workload=only"]), ["only"]);
        let invocation = route_repeatable(&["strict", "--json"]).expect("routes");
        assert_eq!(invocation.values_of("--workload").count(), 0);
        assert_eq!(invocation.value("--workload"), None);
        assert_eq!(Invocation::default().values_of("--workload").count(), 0);
    }

    #[test]
    fn repeated_values_interleave_with_other_flags_and_positionals() {
        let invocation = route_repeatable(&[
            "strict",
            "--workload",
            "a",
            "--mode",
            "m1",
            "pos",
            "--json",
            "--workload=b",
            "--mode=m2",
            "--json",
            "--workload",
            "c",
        ])
        .expect("routes");
        assert_eq!(
            invocation.values_of("--workload").collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        // A plain value flag between them still keeps only its last value.
        assert_eq!(invocation.values_of("--mode").collect::<Vec<_>>(), ["m2"]);
        assert_eq!(invocation.value("--mode"), Some("m2"));
        assert_eq!(invocation.flags, vec!["--json".to_string()]);
        assert_eq!(invocation.positionals, vec!["pos".to_string()]);
        // Entries of the repeatable flag keep their order relative to each other.
        let order: Vec<_> = invocation
            .values
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("--workload", "a"),
                ("--mode", "m2"),
                ("--workload", "b"),
                ("--workload", "c"),
            ]
        );
    }

    /// Routes `values` against `REPEATABLE_SPECS` with an explicit `-` allowlist.
    fn route_dash(allowed: &[(&str, &str)], values: &[&str]) -> Result<Invocation, ToolkitError> {
        match route_with(REPEATABLE_SPECS, allowed, &words(values))? {
            Routed::Run(_, invocation) => Ok(invocation),
            other => panic!("expected a runnable route, got {other:?}"),
        }
    }

    #[test]
    fn the_shipped_stdio_dash_list_is_empty() {
        // No registered parameter reads standard input (#4935): a bare `-` is a
        // stdio marker nowhere.
        assert!(STDIO_DASH.is_empty(), "{STDIO_DASH:?}");
    }

    #[test]
    fn the_literal_dash_list_is_value_flags_only_and_disjoint_from_stdio() {
        let mut seen = std::collections::BTreeSet::new();
        for (command, flag) in LITERAL_DASH {
            assert!(seen.insert((*command, *flag)), "duplicate {command} {flag}");
            assert_ne!(*flag, STDIO_DASH_POSITIONAL, "{command}: positional");
            assert!(flag.starts_with("--"), "{command} {flag}");
            assert!(
                !STDIO_DASH.contains(&(*command, *flag)),
                "{command} {flag} is in both lists"
            );
        }
    }

    /// Routes `values` against `REPEATABLE_SPECS` with an explicit literal list.
    fn route_literal(
        literal: &[(&str, &str)],
        values: &[&str],
    ) -> Result<Invocation, ToolkitError> {
        let lists = DashLists {
            stdio: &[],
            literal,
        };
        match route_lists(REPEATABLE_SPECS, lists, &words(values))? {
            Routed::Run(_, invocation) => Ok(invocation),
            other => panic!("expected a runnable route, got {other:?}"),
        }
    }

    #[test]
    fn a_literal_dash_pair_takes_the_value_unchanged_and_opens_only_itself() {
        let literal = [("strict", "--mode"), ("loose", "--both")];
        // Plain and attached spelling, strict and non-strict, the value is `-`.
        let invocation = route_literal(&literal, &["strict", "--mode", "-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        let invocation = route_literal(&literal, &["strict", "--mode=-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        let invocation = route_literal(&literal, &["loose", "--both", "-"]).expect("routes");
        assert_eq!(invocation.values_of("--both").collect::<Vec<_>>(), ["-"]);
        // Another flag of the same command, another command and a positional stay refused.
        assert!(route_literal(&literal, &["strict", "--workload", "-"]).is_err());
        assert!(route_literal(&literal, &["strict", "--workload=-"]).is_err());
        assert!(route_literal(&literal, &["loose", "--mode", "-"]).is_err());
        assert!(route_literal(&literal, &["strict", "-"]).is_err());
        // The literal pair does not loosen the other dash rules.
        assert!(route_literal(&literal, &["strict", "--mode", "-x"]).is_err());
        assert!(route_literal(&literal, &["strict", "--mode", ""]).is_err());
        assert!(route_literal(&literal, &["strict", "--mode", "--"]).is_err());
    }

    #[test]
    fn an_unlisted_bare_dash_is_refused_as_before_4743() {
        // Strict command: value and positional `-` are refused, in both spellings.
        for values in [
            ["strict", "--mode", "-"].as_slice(),
            &["strict", "--mode=-"],
            &["strict", "--workload", "-"],
            &["strict", "--workload=-"],
            &["strict", "-"],
            &["strict", "--mode", "m", "-", "-"],
        ] {
            let Err(error) = route_repeatable(values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.code(), "invalid_arguments", "{values:?}");
            assert!(
                error.message().starts_with("invalid arguments: "),
                "{values:?}: {}",
                error.message()
            );
            assert!(!error.message().contains('\n'), "single line");
        }
        // A listed pair does not open its neighbours: another flag on the same
        // command, the same flag on another command, or the positional.
        let allowed = [("strict", "--mode")];
        assert!(route_dash(&allowed, &["strict", "--workload", "-"]).is_err());
        assert!(route_dash(&allowed, &["strict", "--workload=-"]).is_err());
        assert!(route_dash(&allowed, &["strict", "-"]).is_err());
        let other_command = [("loose", "<positional>")];
        assert!(route_dash(&other_command, &["strict", "-"]).is_err());
        let other_flag = [("loose", "--workload")];
        assert!(route_dash(&other_flag, &["strict", "--workload", "-"]).is_err());
        // Listing the positional does not allow `-` as a flag value.
        let positional_only = [("strict", STDIO_DASH_POSITIONAL)];
        assert!(route_dash(&positional_only, &["strict", "--mode", "-"]).is_err());
        assert!(route_dash(&positional_only, &["strict", "--mode=-"]).is_err());
    }

    #[test]
    fn a_non_strict_command_refuses_a_bare_dash_value_too() {
        // #4780: `loose` takes any other value verbatim, but a lone `-` reaches a
        // handler only through STDIO_DASH, in both spellings, plain or repeatable.
        for values in [
            ["loose", "--mode", "-"].as_slice(),
            &["loose", "--mode=-"],
            &["loose", "--both", "-"],
            &["loose", "--workload=-"],
            &["loose", "-"],
        ] {
            let Err(error) = route_repeatable(values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.exit_code(), 2, "{values:?}");
            assert!(
                error.message().starts_with("invalid arguments: "),
                "{values:?}: {}",
                error.message()
            );
        }
        // The message names the flag.
        let error = route_repeatable(&["loose", "--mode=-"]).expect_err("refused");
        assert!(error.message().contains("--mode"), "{}", error.message());
        // Every other value is still taken verbatim, dashes included.
        let invocation = route_repeatable(&["loose", "--mode", "-x"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-x"));
        let invocation = route_repeatable(&["loose", "--mode", ""]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some(""));
        // `./-` is an ordinary literal.
        let invocation = route_repeatable(&["loose", "--mode", "./-", "./-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("./-"));
        assert_eq!(invocation.positionals, ["./-"]);
        // A listed pair opens only itself.
        let allowed = [("loose", "--mode")];
        assert!(route_dash(&allowed, &["loose", "--mode", "-"]).is_ok());
        assert!(route_dash(&allowed, &["loose", "--both", "-"]).is_err());
    }

    #[test]
    fn a_listed_pair_accepts_a_bare_dash_value_in_both_spellings() {
        let allowed = [("strict", "--mode"), ("strict", "--workload")];
        let invocation = route_dash(&allowed, &["strict", "--mode", "-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        let invocation = route_dash(&allowed, &["strict", "--mode=-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        let invocation = route_dash(
            &allowed,
            &[
                "strict",
                "--workload",
                "-",
                "--workload=-",
                "--workload",
                "a",
            ],
        )
        .expect("routes");
        assert_eq!(
            invocation.values_of("--workload").collect::<Vec<_>>(),
            ["-", "-", "a"]
        );
    }

    #[test]
    fn a_listed_pair_still_refuses_every_other_dashed_or_empty_value() {
        let allowed = [("strict", "--mode"), ("strict", "--workload")];
        for bad in ["-x", "--foo", "--", "-=", "-- ", "-\u{0}", "--json", ""] {
            for flag in ["--mode", "--workload"] {
                assert!(
                    route_dash(&allowed, &["strict", flag, bad]).is_err(),
                    "{flag} {bad:?}"
                );
                let attached = format!("{flag}={bad}");
                assert!(
                    route_dash(&allowed, &["strict", &attached]).is_err(),
                    "{attached:?}"
                );
            }
        }
        // `--mode -` does not swallow the next flag as a value either.
        assert!(route_dash(&allowed, &["strict", "--mode", "-", "--bogus"]).is_err());
    }

    #[test]
    fn a_listed_positional_accepts_a_bare_dash_and_counts_it() {
        let allowed = [("strict", STDIO_DASH_POSITIONAL)];
        let invocation = route_dash(&allowed, &["strict", "-"]).expect("routes");
        assert_eq!(invocation.positionals, ["-"]);
        // Beside a switch and a value flag, in any order.
        let invocation =
            route_dash(&allowed, &["strict", "--json", "--mode", "m", "-"]).expect("routes");
        assert_eq!(invocation.positionals, ["-"]);
        assert!(invocation.has_flag("--json"));
        assert_eq!(invocation.value("--mode"), Some("m"));
        // It counts against `max_positionals`.
        assert!(route_dash(&allowed, &["strict", "-", "-"]).is_err());
        assert!(route_dash(&allowed, &["strict", "a", "-"]).is_err());
        // Other dash-prefixed words are still unknown flags.
        assert!(route_dash(&allowed, &["strict", "-x"]).is_err());
        assert!(route_dash(&allowed, &["strict", "--"]).is_err());
        // A command that takes no positionals refuses it even when listed.
        let none = &[CommandSpec {
            path: &["none"],
            flags: &[],
            value_flags: &[],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: skills_list,
        }];
        let listed = [("none", STDIO_DASH_POSITIONAL)];
        assert!(route_with(none, &listed, &words(&["none", "-"])).is_err());
    }

    #[test]
    fn a_listed_multi_word_command_path_is_matched_space_joined() {
        let specs = &[CommandSpec {
            path: &["group", "leaf"],
            flags: &[],
            value_flags: &["--json"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: skills_list,
        }];
        let args = words(&["group", "leaf", "--json", "-"]);
        assert!(route_with(specs, &[], &args).is_err());
        assert!(route_with(specs, &[("group", "--json")], &args).is_err());
        assert!(route_with(specs, &[("group leaf", "--json")], &args).is_ok());
    }

    #[test]
    fn strict_values_refuse_each_repeated_value_that_is_empty_or_dashed() {
        for bad in [
            ["--workload", ""].as_slice(),
            &["--workload", "-x"],
            &["--workload", "--json"],
            &["--workload="],
            &["--workload=-x"],
            &["--workload"],
        ] {
            for position in [0, 1] {
                let mut values = vec!["strict"];
                if position == 1 {
                    values.extend(["--workload", "ok"]);
                }
                values.extend_from_slice(bad);
                if position == 0 {
                    values.extend(["--workload", "ok"]);
                }
                let Err(error) = route_repeatable(&values) else {
                    panic!("{values:?} must be a usage error");
                };
                assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
                assert_eq!(error.exit_code(), 2);
            }
        }
    }

    #[test]
    fn a_non_strict_command_takes_any_next_word_for_every_repeated_value() {
        let invocation = route_repeatable(&[
            "loose",
            "--workload",
            "--json",
            "--workload=",
            "--workload",
            "",
            "--workload=-x",
        ])
        .expect("routes");
        assert_eq!(
            invocation.values_of("--workload").collect::<Vec<_>>(),
            ["--json", "", "", "-x"]
        );
        assert!(invocation.flags.is_empty());
    }

    #[test]
    fn a_plain_value_flag_still_keeps_only_its_last_value_when_repeated() {
        let invocation = route_repeatable(&["strict", "--mode", "a", "--mode=b", "--mode", "c"])
            .expect("routes");
        assert_eq!(invocation.values_of("--mode").collect::<Vec<_>>(), ["c"]);
        assert_eq!(invocation.value("--mode"), Some("c"));
        assert_eq!(invocation.values.len(), 1);
        // A flag in both lists is repeatable.
        let invocation = route_repeatable(&["loose", "--both", "a", "--both=b"]).expect("routes");
        assert_eq!(
            invocation.values_of("--both").collect::<Vec<_>>(),
            ["a", "b"]
        );
        // Repeatable flags are only known to the commands that declare them, and
        // `--help` still wins.
        assert!(route(COMMANDS, &words(&["sync", "vendored", "--workload", "a"])).is_err());
        assert!(matches!(
            route(
                REPEATABLE_SPECS,
                &words(&["strict", "--workload", "a", "--help"])
            ),
            Ok(Routed::Help)
        ));
    }

    #[test]
    fn registered_repeatable_flags_are_not_also_plain_flags() {
        for spec in COMMANDS {
            for flag in spec.repeatable_flags {
                assert!(
                    !spec.flags.contains(flag),
                    "{:?}: {flag} is both a switch and a repeatable value flag",
                    spec.path
                );
            }
        }
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
    fn passthrough_commands_take_every_word_verbatim() {
        let routed = route_words(&["asm", "exec", "--", "ls", "-la", "--help", "-h", "x=1"]);
        let Ok(Routed::Run(spec, invocation)) = routed else {
            panic!("asm exec must route to its handler");
        };
        assert_eq!(spec.path, ["asm", "exec"]);
        assert_eq!(
            invocation.positionals,
            words(&["--", "ls", "-la", "--help", "-h", "x=1"])
        );
        assert!(invocation.flags.is_empty() && invocation.values.is_empty());
        // No words at all still routes (the handler prints the usage line).
        assert!(matches!(
            route_words(&["asm", "exec"]),
            Ok(Routed::Run(_, invocation)) if invocation.positionals.is_empty()
        ));
        // Other commands still reject unknown flags.
        assert!(route_words(&["skills", "list", "--bogus"]).is_err());
    }

    #[test]
    fn a_handler_can_return_a_status_without_an_error_report() {
        let (status, stdout, stderr) = exec(&["asm", "exec"]);
        assert_eq!(
            (status, stdout.as_str(), stderr.as_str()),
            (1, "", "Usage: asm-exec <command> [args...]\n")
        );
        #[cfg(unix)]
        {
            let (status, stdout, stderr) = exec(&["asm", "exec", "/bin/sh", "-c", "exit 3"]);
            assert_eq!((status, stdout.as_str(), stderr.as_str()), (3, "", ""));
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
    fn dynamodb_value_flags_route_with_the_canonical_router() {
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "dynamodb",
            "calculate-costs",
            "--model",
            "a.json",
            "--output=x.md",
            "--output=y.md",
        ]) else {
            panic!("expected a runnable route");
        };
        assert_eq!(spec.path, &["dynamodb", "calculate-costs"]);
        assert_eq!(invocation.value("--model"), Some("a.json"));
        assert_eq!(invocation.value("--output"), Some("y.md"), "last wins");
        assert_eq!(invocation.value("--absent"), None);
        assert!(invocation.positionals.is_empty() && invocation.flags.is_empty());
        for values in [
            vec!["dynamodb", "calculate-costs", "--model"],
            vec!["dynamodb", "calculate-costs", "--model", "--output", "x"],
            vec!["dynamodb", "calculate-costs", "-o", "x"],
            vec!["dynamodb", "calculate-costs", "--req", "x"],
            vec!["dynamodb", "calculate-costs", "--model", "a.json", "extra"],
            vec!["dynamodb", "iterate-design", "--model=a", "--dry-run=1"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.exit_code(), 2, "{values:?}");
        }
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
        assert_eq!(invocation.value("--model"), Some("m"));
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

    #[test]
    fn msk_commands_route_with_value_options_in_both_spellings() {
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
        assert_eq!(invocation.value("--instance-type"), Some("b"));
        assert_eq!(invocation.value("--broker-count"), Some("9"));
        assert_eq!(invocation.value("--kafka-version"), None);
        assert_eq!(invocation.values.len(), 2);
    }

    #[test]
    fn msk_value_options_consume_the_next_word_even_when_it_looks_like_a_flag() {
        // argparse semantics: a negative number is a value (the legacy script
        // then rejects it), so msk uses the lenient value rule.
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "msk",
            "size",
            "--avg-data-in-mbs",
            "-5",
            "--use-max-partitions",
        ]) else {
            panic!("expected a runnable route");
        };
        assert!(!spec.strict_values);
        assert_eq!(invocation.value("--avg-data-in-mbs"), Some("-5"));
        assert!(invocation.has_flag("--use-max-partitions"));
        assert!(invocation.positionals.is_empty());
    }

    #[test]
    fn msk_argument_errors_are_usage_errors() {
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
    }

    #[test]
    fn handlers_can_write_stderr_lines_and_set_the_exit_status() {
        fn handler(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
            output.line("data")?;
            output.diagnostic("warning one");
            output.diagnostic("warning two");
            output.set_exit_status(7);
            Ok(())
        }
        let spec = CommandSpec {
            path: &["demo"],
            flags: &[],
            value_flags: &[],
            strict_values: false,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler,
        };
        let Ok(Routed::Run(spec, invocation)) =
            route(std::slice::from_ref(&spec), &words(&["demo"]))
        else {
            panic!("expected a runnable route");
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        (spec.handler)(&invocation, &mut output).expect("handler succeeds");
        assert_eq!(output.exit_status(), 7);
        assert_eq!(out, b"data\n");
        assert_eq!(err, b"warning one\nwarning two\n");
    }

    #[test]
    fn existing_command_families_still_route_next_to_msk() {
        for values in [
            vec!["manifests", "validate", "--plugin", "p"],
            vec!["spec", "validate"],
            vec!["sync", "plugin-skills", "--check", "--root", "r"],
            vec!["sync", "vendored", "--plugin-root=x"],
        ] {
            assert!(
                matches!(route_words(&values), Ok(Routed::Run(..))),
                "{values:?}"
            );
        }
        let Err(error) = route_words(&["sync", "vendored", "--plugin-root", "--check"]) else {
            panic!("strict sync values must stay strict");
        };
        assert_eq!(error.exit_code(), 2);
    }
}
