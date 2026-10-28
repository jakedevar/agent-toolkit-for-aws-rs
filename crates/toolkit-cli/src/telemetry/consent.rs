//! The consent record, the approved notice and the consent commands
//! (`consent/record.py`, `notice.py`, `cli.py`, `accept.py`, `opt_out.py`,
//! `session_start.py`).
//!
//! `~/.aws-startup-advisor/plugin-telemetry.json` is the one thing that decides
//! whether telemetry may be emitted. Fail-closed on read (absent, unreadable,
//! unparseable, not an object, unknown status, `ACCEPTED` without a UUID
//! `installId` all mean "no consent"), fail-loud on write.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_core::{Json, ToolkitError};

use super::{client, Ctx};
use crate::startup::artifacts::pyjson;
use crate::Output;

pub(super) const STATE_DIR_NAME: &str = ".aws-startup-advisor";
pub(super) const STATE_FILE_NAME: &str = "plugin-telemetry.json";
pub(super) const ACCEPTED: &str = "ACCEPTED";
pub(super) const OPT_OUT: &str = "OPT_OUT";

/// Largest consent record read; a real one is under 100 bytes.
const MAX_RECORD_BYTES: u64 = 1 << 20;

/// The record as a human should see it: `~`, not an expanded home directory.
const DISPLAY_STATE_PATH: &str = "~/.aws-startup-advisor/plugin-telemetry.json";

/// The opt-out the notice names: a config setting, not a command.
const OPT_OUT_INSTRUCTION: &str =
    "setting \"consentStatus\" to \"OPT_OUT\" in ~/.aws-startup-advisor/plugin-telemetry.json";

const OPT_OUT_COMMAND: &str = "toolkit telemetry consent opt-out";
const ACCEPT_COMMAND: &str = "toolkit telemetry consent accept";
const SHOW_COMMAND: &str = "toolkit telemetry consent show";

const USAGE: &str = "usage: toolkit telemetry consent {show|status|accept|opt-out|session-start}";

/// One of the two answers; only [`Status::Accepted`] permits emitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Accepted,
    OptOut,
}

impl Status {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => ACCEPTED,
            Self::OptOut => OPT_OUT,
        }
    }
}

/// A valid consent record. `install_id` is always a UUID for `Accepted`; for
/// `OptOut` it is `None` when the (hand-edited) record carries none or a
/// malformed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub status: Status,
    pub install_id: Option<String>,
}

/// The service's UUID shape: `8-4-4-4-12` hex digits, either case.
pub(super) fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    groups.len() == lengths.len()
        && groups
            .iter()
            .zip(lengths)
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(super) fn state_dir(ctx: &Ctx<'_>) -> Option<PathBuf> {
    ctx.home.map(|home| home.join(STATE_DIR_NAME))
}

pub(super) fn state_path(ctx: &Ctx<'_>) -> Option<PathBuf> {
    state_dir(ctx).map(|dir| dir.join(STATE_FILE_NAME))
}

/// The record path for a human: the real one, or the `~` form when no home
/// directory is known.
fn shown_path(ctx: &Ctx<'_>) -> String {
    state_path(ctx).map_or_else(
        || DISPLAY_STATE_PATH.to_string(),
        |path| path.display().to_string(),
    )
}

/// The record, or `None` if absent, unreadable, unparseable or invalid.
///
/// `installId` is required only for `ACCEPTED`: the notice tells the user to
/// opt out by setting `consentStatus` to `OPT_OUT` by hand, and demanding a
/// UUID they cannot invent would read their own edit as corrupt. A UTF-8 BOM
/// (added by Windows editors) is accepted.
pub(super) fn read_state(ctx: &Ctx<'_>) -> Option<Record> {
    let path = state_path(ctx)?;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_RECORD_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let data = pyjson::loads(text).ok()?;
    let status = match data.get("consentStatus") {
        Some(Json::String(text)) if text == ACCEPTED => Status::Accepted,
        Some(Json::String(text)) if text == OPT_OUT => Status::OptOut,
        _ => return None,
    };
    let install_id = match data.get("installId") {
        Some(Json::String(text)) if is_uuid(text) => Some(text.clone()),
        _ => None,
    };
    if status == Status::Accepted && install_id.is_none() {
        return None;
    }
    Some(Record { status, install_id })
}

/// `ACCEPTED`, `OPT_OUT`, or `None` when nothing is decided.
pub(super) fn consent_status(ctx: &Ctx<'_>) -> Option<Status> {
    read_state(ctx).map(|record| record.status)
}

/// The single gate every telemetry path calls. Deliberately not
/// `!= OptOut`: a missing or corrupt record must block too.
pub(super) fn is_accepted(ctx: &Ctx<'_>) -> bool {
    consent_status(ctx) == Some(Status::Accepted)
}

/// A random (version 4) UUID string. Only an anonymous install identifier, so
/// it is derived from the process-random hasher keys, the clock and a counter
/// rather than a dependency.
pub(super) fn uuid4() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut bytes = [0u8; 16];
    for (index, half) in (0u8..).zip(bytes.chunks_mut(8)) {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u8(index);
        half.copy_from_slice(&hasher.finish().to_be_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn no_home() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no home directory")
}

/// Records `status` and returns the record written. An existing UUID
/// `installId` is preserved (changing the answer is not a new install).
///
/// Atomic (temporary file in the same directory, fsync, rename) so an
/// interrupted write cannot leave a record that reads as corrupt and re-raises
/// the notice forever.
///
/// # Errors
///
/// Any I/O failure, including an unknown home directory.
pub(super) fn write_state(ctx: &Ctx<'_>, status: Status) -> io::Result<Record> {
    let (directory, path) = state_dir(ctx).zip(state_path(ctx)).ok_or_else(no_home)?;
    let existing = read_state(ctx).and_then(|record| record.install_id);
    let install_id = existing.unwrap_or_else(uuid4);
    let value = Json::object(vec![
        ("installId", Json::string(install_id.clone())),
        ("consentStatus", Json::string(status.as_str())),
    ]);
    let mut text = pyjson::dumps(&value, Some(2), false);
    text.push('\n');

    fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(
        "{STATE_FILE_NAME}.{}-{}.tmp",
        std::process::id(),
        &uuid4()[..8]
    ));
    let result =
        write_synced(&temporary, text.as_bytes()).and_then(|()| fs::rename(&temporary, &path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(Record {
        status,
        install_id: Some(install_id),
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

// ------------------------------------------------------------------ notice

const NOTICE_TITLE: &str = "AWS Startup Advisor plugin \u{2014} usage data notice";

/// One unwrapped paragraph; [`render_notice`] wraps it. Do not rephrase,
/// reorder or trim: line breaks are the only formatting liberty.
const NOTICE_BODY: &str =
    "You've installed the AWS Startup Advisor plugin, which provides skills that \
help you build on AWS through your AI coding agent. To help us improve it, \
we'd like to collect optional usage data about how the plugin is used. What \
we collect: which skill you invoked, the plugin version, the host tool \
you're using, a randomly generated install identifier, and, for migration \
skills, the source cloud provider and which migration phase you start and \
complete. The usage data above is not linked to your identity. You can \
opt-out by {opt_out}. Opting out does not change any plugin functionality.";

const ACKNOWLEDGE_PROMPT: &str =
    "Do you acknowledge this notice? Reply \"yes\" to acknowledge, or \"opt out\" if \
you would rather no usage data was collected. Either answer is fine, and \
nothing is collected until you answer.";

/// 79, not 80, so a terminal reserving a cell for the cursor does not re-wrap.
const WRAP_WIDTH: usize = 79;

/// `textwrap.fill(text, width=79, break_long_words=False,
/// break_on_hyphens=False)` for single-space-separated text: greedy, a word
/// longer than the width stays whole on its own line.
fn wrap(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_ascii_whitespace() {
        let length = word.chars().count();
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + length <= WRAP_WIDTH => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines.join("\n")
}

fn render_notice(opt_out: &str) -> String {
    let body = wrap(&NOTICE_BODY.replace("{opt_out}", opt_out));
    format!("{NOTICE_TITLE}\n\n{body}\n\n{}\n", wrap(ACKNOWLEDGE_PROMPT))
}

/// The notice, with the opt-out mechanism filled in.
pub(super) fn disclaimer() -> String {
    render_notice(OPT_OUT_INSTRUCTION)
}

/// The copy-pasteable opt-out, for a human reading command output.
fn opt_out_help(ctx: &Ctx<'_>) -> String {
    [
        "To opt out later, run:".to_string(),
        format!("  {OPT_OUT_COMMAND}"),
        format!(
            "Or edit {} by hand and set \"consentStatus\" to \"{OPT_OUT}\".",
            shown_path(ctx)
        ),
        "That file is the only thing that decides whether anything is sent.".to_string(),
        "Opting out disables no plugin features.".to_string(),
    ]
    .join("\n")
}

/// One human-readable paragraph describing the current setting.
fn status_report(ctx: &Ctx<'_>) -> String {
    let path = shown_path(ctx);
    match read_state(ctx) {
        Some(Record {
            status: Status::Accepted,
            install_id,
        }) => format!(
            "Usage data collection: ON. You acknowledged the notice.\nInstall ID: {}\nRecorded at: {path}\n\n{}",
            install_id.unwrap_or_default(),
            opt_out_help(ctx)
        ),
        Some(Record {
            status: Status::OptOut,
            ..
        }) => format!(
            "Usage data collection: OFF. You opted out, and nothing is sent.\nRecorded at: {path}\nTo turn it back on, run: {ACCEPT_COMMAND}"
        ),
        // Also what a corrupt record reports: the gate treats the two identically.
        None => format!(
            "Usage data collection: OFF. Nothing is recorded yet, so nothing is sent.\nThe notice will be shown at the start of your next session.\nExpected record location: {path}"
        ),
    }
}

/// The `SessionStart` hook's instruction to the agent. A host injects plain
/// stdout as context, so that is all this is.
fn session_instruction() -> String {
    format!(
        "\
[AWS Startup Advisor] This user has not yet been shown the usage-data notice.

The first time in this session that a request would use an AWS Startup Advisor
skill, stop before doing that work and run this exchange instead:

1. Run: {SHOW_COMMAND}
2. Reproduce that command's output to the user VERBATIM. Do not summarize,
   shorten, translate, reorder, or rewrite it \u{2014} it is a legal notice and the
   exact wording is the point. Its last paragraph already asks the question and
   offers both answers, so do not restate it, re-ask it in your own words, or
   add your own version underneath. Relaying it is the whole job.
3. END YOUR TURN THERE. Send the notice and the question, and nothing else: do
   not answer their request in the same message, do not call any other tool, and
   do not begin the work. A question delivered alongside its own answer gets
   skipped \u{2014} the user reads the answer they asked for and never replies to the
   notice, which makes showing it pointless.
4. When they reply, record it by running exactly one of:
     acknowledged     -> {ACCEPT_COMMAND}
     wants to opt out -> {OPT_OUT_COMMAND}
5. Then answer their original request in full, in that same turn. They asked
   once; do not make them repeat themselves or re-ask.

Rules: Do not guess or assume a response, and do not run either command until the
user has actually responded. If they ignore the notice, change the subject, or
tell you to get on with it, then get on with it: record nothing, answer them, and
do not raise this again in this session. Nothing recorded means no usage data is
sent and the notice will simply be raised again next session, so an unanswered
notice costs nothing and is never worth nagging over.

This pause is one turn, once per install. It is not a gate on using the plugin:
every feature works identically whatever they choose, and opting out disables
nothing. There is no environment variable: the record file the commands above
write is the only thing that decides whether anything is collected, and the
notice tells the user they may edit it themselves instead.
"
    )
}

// ---------------------------------------------------------------- commands

/// Prints `text` exactly: it already ends with its newline.
fn print_exact(output: &mut Output<'_>, text: &str) -> Result<(), ToolkitError> {
    output.line(text.strip_suffix('\n').unwrap_or(text))
}

pub(super) fn run(
    args: &[&str],
    ctx: &Ctx<'_>,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    match args {
        ["show"] => print_exact(output, &disclaimer()),
        ["status"] => output.line(&status_report(ctx)),
        ["accept"] => accept(ctx, output),
        ["opt-out"] => opt_out(ctx, output),
        // A consent prompt is not worth breaking a session over: never fails.
        ["session-start"] => {
            if consent_status(ctx).is_none() {
                let _ = print_exact(output, &session_instruction());
            }
            Ok(())
        }
        ["-h" | "--help"] => {
            output.diagnostic(USAGE);
            Ok(())
        }
        _ => Err(ToolkitError::usage(
            "invalid_arguments",
            "invalid arguments",
        )),
    }
}

/// Records that the user ACKNOWLEDGED the notice, then (fire and forget) sends
/// `consentRecorded`. Exit 1 and an explanation if the write fails, so a
/// failure is never silently taken as consent.
fn accept(ctx: &Ctx<'_>, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let written = match write_state(ctx, Status::Accepted) {
        Ok(record) => record,
        Err(error) => {
            output.diagnostic(&format!(
                "Could not record your choice at {}: {}\nNothing was saved, so no usage data will be collected. The notice will be shown again next session.",
                shown_path(ctx),
                io_reason(&error)
            ));
            output.set_exit_status(1);
            return Ok(());
        }
    };
    client::send_consent_recorded(ctx);
    output.line(&format!(
        "Acknowledged \u{2014} usage data collection is ON.\nRecorded at {} (install ID {}).\n\n{}",
        shown_path(ctx),
        written.install_id.unwrap_or_default(),
        opt_out_help(ctx)
    ))
}

/// Records the opt-out. Sends nothing: `ConsentRecordedDetails` is empty and
/// cannot say which way the user answered.
fn opt_out(ctx: &Ctx<'_>, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    if let Err(error) = write_state(ctx, Status::OptOut) {
        output.diagnostic(&format!(
            "Could not record your choice at {}: {}\nNo usage data will be collected either way \u{2014} the plugin only emits when an acknowledged record exists. But because nothing was saved, the notice will be shown again next session.",
            shown_path(ctx),
            io_reason(&error)
        ));
        output.set_exit_status(1);
        return Ok(());
    }
    output.line(&format!(
        "You are opted out \u{2014} usage data collection is OFF and no data will be sent.\nRecorded at {} so you are not asked again.\nEvery feature of the plugin still works.",
        shown_path(ctx)
    ))
}

/// The operating-system reason of a failed write, without any path or file
/// content that the platform message could carry.
fn io_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "not found".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        io::ErrorKind::AlreadyExists => "already exists".to_string(),
        other => format!("{other}"),
    }
}
