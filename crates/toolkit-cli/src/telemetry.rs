//! `toolkit telemetry ...`: plugin usage-data consent, persistence and
//! emission (#4398).
//!
//! Rust port of `plugins/aws-startup-advisor/scripts/telemetry` (the consent
//! record and notice, the Startups Advisor event client, the skill-invoked and
//! migration hooks, the bundle generator). See `docs/migration/telemetry.md`
//! for the contract, the intentional changes and the cutover notes.
//!
//! One command, [`telemetry_command`], takes its words verbatim (a registered
//! `passthrough` command) because the hook subcommands must never turn a bad
//! word into a usage failure that could block the host tool:
//!
//! ```text
//! toolkit telemetry consent show|status|accept|opt-out|session-start
//! toolkit telemetry status | reconcile
//! toolkit telemetry skill-invoked [--skill NAME]
//! toolkit telemetry migration [--reconcile] [--session-end] [--via cli]
//! toolkit telemetry bundles [--write | --check] [--plugin-root DIR]
//! ```
//!
//! Fail-closed consent: the only thing that permits an event is a readable
//! `~/.aws-startup-advisor/plugin-telemetry.json` whose `consentStatus` is
//! `ACCEPTED` with a UUID `installId`. A missing, unreadable or malformed
//! record, an unknown home directory and an opt-out all mean no emission, and
//! every emission path re-checks the record itself (`client::prepare`).
//!
//! Everything observable is injected through [`Ctx`]: the environment, the
//! home directory, the clock, stdin and the [`EventSink`] that actually sends a
//! request. This build has no HTTP client, so the shipped sink
//! ([`NoNetworkSink`]) reports every request as unreachable and nothing ever
//! leaves the process; tests record the requests in their own sink.

use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toolkit_core::ToolkitError;

use crate::{Invocation, Output};

mod attributes;
mod bundles;
mod client;
mod consent;
mod migration;
mod skill;
#[cfg(test)]
mod tests;

pub(crate) use client::{EventSink, NoNetworkSink};

/// Registration of the `telemetry` slice: its one passthrough route and its
/// help text (see `crate::registry`). Added after the registry, so both ranks
/// are [`crate::registry::NEW_RANK`].
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "telemetry",
    commands: &[crate::CommandSpec {
        path: &["telemetry"],
        flags: &[],
        // Declared for the #4780 contract test and the audit; the router never
        // reads them for a passthrough command. `screen` applies the bare `-`
        // policy to exactly these flags.
        value_flags: &PATH_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: usize::MAX,
        passthrough: true,
        handler: telemetry_command,
    }],
    synopsis: r"  toolkit telemetry (consent show|status|accept|opt-out|session-start | status | reconcile
                    | skill-invoked [--skill NAME] | migration [--reconcile] [--session-end]
                    [--via cli] | bundles [--write | --check] [--plugin-root DIR])
                    [--telemetry-dir DIR]
",
    description: r"  telemetry     Plugin usage-data consent, persistence and emission (#4398), the
                Rust port of aws-startup-advisor scripts/telemetry. Words are taken
                verbatim so a hook can never fail on a bad word. consent shows the
                approved notice, reports the setting, records an acknowledgement
                (accept) or an opt-out, or prints the SessionStart instruction;
                status prints the host source, reporting mode, consent and plugin
                version as JSON and sends nothing; reconcile and migration report
                `.migration/<id>/` progress; skill-invoked reports a skill use;
                bundles generates or checks the vendored runtime copies. Nothing
                is sent without an ACCEPTED record, and this build has no HTTP
                client, so nothing leaves the process. See docs/migration/telemetry.md.
",
    exit_notes: "",
    doc: "docs/migration/telemetry.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// Environment lookup, injectable for tests.
pub(crate) type EnvFn<'a> = dyn Fn(&str) -> Option<String> + Sync + 'a;
/// Seconds since the Unix epoch, injectable for tests.
pub(crate) type ClockFn<'a> = dyn Fn() -> f64 + Sync + 'a;
/// Reads standard input for at most the given time; `""` when it cannot.
pub(crate) type StdinFn<'a> = dyn Fn(Duration) -> String + Sync + 'a;

/// Longest hook payload read from stdin.
const STDIN_MAX_BYTES: u64 = 1 << 20;

/// Everything a telemetry action observes or causes outside the process.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    /// The user's home directory (`None`: no consent record can be located).
    pub home: Option<&'a std::path::Path>,
    /// Environment lookup.
    pub env: &'a EnvFn<'a>,
    /// The working directory.
    pub cwd: &'a std::path::Path,
    /// The directory holding `version.json` (the bundle or the canonical
    /// `scripts/telemetry` directory), from `--telemetry-dir`.
    pub telemetry_dir: Option<&'a std::path::Path>,
    /// The clock.
    pub now: &'a ClockFn<'a>,
    /// Where requests go.
    pub sink: &'a dyn EventSink,
    /// Standard input.
    pub stdin: &'a StdinFn<'a>,
}

const USAGE_ERROR: &str = "invalid arguments";

fn usage() -> ToolkitError {
    ToolkitError::usage("invalid_arguments", USAGE_ERROR)
}

/// The directory with symbolic links resolved (the layout checks look at real
/// parent names), or as given when it cannot be resolved.
fn resolve(value: &str) -> PathBuf {
    std::fs::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value))
}

/// The value flags that name a filesystem path: `--telemetry-dir` for every
/// subcommand and `--plugin-root` for `bundles`.
const PATH_FLAGS: [&str; 2] = ["--plugin-root", "--telemetry-dir"];

/// Whether `words` (path flags already removed) select a hook entry point, which
/// must never fail: `skill-invoked`, `migration` and `consent session-start`.
fn is_hook(words: &[String]) -> bool {
    match words {
        [first, ..] if first == "skill-invoked" || first == "migration" => true,
        [first, second, ..] => first == "consent" && second == "session-start",
        _ => false,
    }
}

/// Removes every path flag whose value is exactly `-` (`--flag -` and
/// `--flag=-`) from `words` and names the first one removed. A bare `-` is the
/// standard-input/output marker, never a path (#4780); other values pass
/// through untouched, the flag's value word is never rescanned as a flag.
fn screen(words: &[String]) -> (Vec<String>, Option<&'static str>) {
    let mut kept = Vec::new();
    let mut dash = None;
    let mut iter = words.iter().peekable();
    while let Some(word) = iter.next() {
        if let Some(flag) = PATH_FLAGS.iter().find(|flag| **flag == word) {
            if iter.peek().is_some_and(|value| *value == "-") {
                dash.get_or_insert(*flag);
                iter.next();
                continue;
            }
            kept.push(word.clone());
            kept.extend(iter.next().cloned());
        } else if let Some(flag) = PATH_FLAGS
            .iter()
            .find(|flag| word.strip_prefix(**flag) == Some("=-"))
        {
            dash.get_or_insert(*flag);
        } else {
            kept.push(word.clone());
        }
    }
    (kept, dash)
}

/// Splits `--telemetry-dir DIR` / `--telemetry-dir=DIR` out of `words`.
fn split_telemetry_dir(words: &[String]) -> (Vec<String>, Option<PathBuf>) {
    let mut rest = Vec::new();
    let mut dir = None;
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        if word == "--telemetry-dir" {
            if let Some(value) = iter.next() {
                dir = Some(resolve(value));
            }
        } else if let Some(value) = word.strip_prefix("--telemetry-dir=") {
            dir = Some(resolve(value));
        } else {
            rest.push(word.clone());
        }
    }
    (rest, dir)
}

/// Runs one telemetry action against `ctx`.
///
/// # Errors
///
/// A usage error for an unknown or missing subcommand of the interactive
/// commands (`consent`, `status`, `reconcile`, `bundles`); the hook
/// subcommands and `consent session-start` never fail.
pub(crate) fn run_action(
    words: &[String],
    ctx: &Ctx<'_>,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    // The #4780 bare `-` policy, applied here because a passthrough command's
    // words never reach the router: an interactive subcommand refuses it (exit
    // 2, the router's message, no path read or written); a hook treats the flag
    // as absent so it still emits nothing, exits 0 and touches no file (#4961).
    let (words, dash) = screen(words);
    if !is_hook(&words) {
        if let Some(flag) = dash {
            return Err(crate::dash_value_error(flag));
        }
        if words.iter().any(|word| word == "-") {
            return Err(crate::dash_positional_error());
        }
    }
    let (words, dir) = split_telemetry_dir(&words);
    let ctx = Ctx {
        telemetry_dir: dir.as_deref().or(ctx.telemetry_dir),
        ..*ctx
    };
    let args: Vec<&str> = words.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["consent", rest @ ..] => consent::run(rest, &ctx, output),
        ["status"] => client::status(&ctx, output),
        ["reconcile"] => {
            migration::main(&["--reconcile", "--via", "cli"], &ctx);
            Ok(())
        }
        ["skill-invoked", rest @ ..] => {
            skill::main(rest, &ctx);
            Ok(())
        }
        ["migration", rest @ ..] => {
            migration::main(rest, &ctx);
            Ok(())
        }
        ["bundles", rest @ ..] => bundles::run(rest, output),
        _ => Err(usage()),
    }
}

/// The registered handler of `toolkit telemetry ...`.
pub(crate) fn telemetry_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let home = home_dir();
    let env = |name: &str| std::env::var(name).ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = Ctx {
        home: home.as_deref(),
        env: &env,
        cwd: &cwd,
        telemetry_dir: None,
        now: &system_now,
        sink: &NoNetworkSink,
        stdin: &real_stdin,
    };
    run_action(&invocation.positionals, &ctx, output)
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn system_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Reads stdin on a helper thread so a host that holds the pipe open without
/// ever closing it cannot stall the turn: after `timeout` the helper is
/// abandoned and nothing was read.
fn real_stdin(timeout: Duration) -> String {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let read = std::io::stdin()
            .lock()
            .take(STDIN_MAX_BYTES)
            .read_to_string(&mut text);
        let _ = sender.send(if read.is_ok() { text } else { String::new() });
    });
    receiver.recv_timeout(timeout).unwrap_or_default()
}
