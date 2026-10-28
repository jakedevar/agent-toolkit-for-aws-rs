//! Migration progress as `MigrationActivity` telemetry
//! (`metric_emission/migration.py`).
//!
//! Nothing is inferred from the conversation. The migration skills maintain
//! `.migration/<id>/.phase-status.json` (and `.gate-failures.json`) for their
//! own gate enforcement; this reads those files, diffs them against a snapshot
//! it keeps beside them and sends one event per transition found:
//! `RUN_STARTED`, `PHASE_COMPLETED`, `GATE_FAILED`, `RUN_COMPLETED`. The diff
//! is idempotent and the run is locked while it runs, so overlapping hooks
//! cannot report the same transition twice. `RUN_COMPLETED` is sent once per
//! `runMode`.
//!
//! Consent is the plugin's single gate, re-read per run and again per request.
//! Attribution is read from disk: a run names its owner in `owning_skill`, and
//! a run without one, or with one outside the migration set, emits nothing.
//!
//! Fail-open: nothing here reports a failure. A refusal the service may later
//! withdraw (403 while the launch gate is closed, 429, 5xx), or no connection
//! at all, leaves that event unrecorded so only it is re-sent; anything the
//! service took, or refused for good (400), is never repeated.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use toolkit_core::Json;

use super::attributes::{
    derive_attributes, get, map_enum, or_obj, py_str, read_json, Abort, Event, FAILURE_REASON,
    PHASES, R, RESOLVED_STATUS, RUN_MODE,
};
use super::client::{self, PostStatus};
use super::consent;
use super::skill::read_hook_payload;
use super::Ctx;
use crate::manifests::{is_falsy, py_eq, resolve_lenient};
use crate::startup::artifacts::pyjson;

const MIGRATION_SKILLS: [&str; 4] = [
    "AZURE_TO_AWS",
    "GCP_TO_AWS",
    "HEROKU_TO_AWS",
    "LLM_TO_BEDROCK",
];

/// A hook killed by the host mid-run leaves its lock behind; anything older
/// than this is treated as abandoned.
const LOCK_STALE_SECONDS: f64 = 60.0;
const POST_TIMEOUT_SECONDS: f64 = 3.0;
/// Claude Code gives all `SessionEnd` hooks one shared budget, 1.5 s by default.
/// A hook killed at the budget would leave accepted events unrecorded, so the
/// sweep budgets itself to the default; the reserve leaves room for the
/// snapshot write.
const SESSION_END_BUDGET_SECONDS: f64 = 1.5;
const SESSION_END_RESERVE_SECONDS: f64 = 0.4;

const STATUS_FILE: &str = ".phase-status.json";
const GATE_FILE: &str = ".gate-failures.json";
const SNAPSHOT_FILE: &str = ".telemetry-snapshot.json";
const LOCK_DIR: &str = ".telemetry-lock";

const BACKBONE: [&str; 5] = ["discover", "clarify", "design", "estimate", "generate"];

// ---------------------------------------------------------------- utilities

/// A lower-case UUID string, or `None`: the data lake accepts only lower case,
/// while macOS `uuidgen` (what the skills run) mints upper case.
fn as_uuid(value: Option<&Json>) -> Option<String> {
    match value {
        Some(Json::String(text)) if consent::is_uuid(text) => Some(text.to_lowercase()),
        _ => None,
    }
}

/// What a snapshot file held.
enum Snapshot {
    Absent,
    /// Present but not a JSON object. A missing snapshot means a new run; a
    /// corrupt one must not, or the run would be re-reported from
    /// `RUN_STARTED`. The caller rebuilds it from the current state without
    /// sending, so the run reports again from its next transition. A snapshot
    /// another user could have written (group- or other-writable, or not
    /// owned by this user) is never read: it is treated the same way, so it is
    /// replaced by a private one and nothing is sent from it.
    Unreadable,
    Data(Json),
}

/// `owner` is the user this process creates files as ([`Lock::owner`]).
fn read_snapshot(path: &Path, owner: Option<u32>) -> Snapshot {
    if !path.exists() {
        return Snapshot::Absent;
    }
    let trusted = fs::symlink_metadata(path).is_ok_and(|metadata| private_to(&metadata, owner));
    if !trusted {
        return Snapshot::Unreadable;
    }
    match read_json(path) {
        Some(data @ Json::Object(_)) => Snapshot::Data(data),
        _ => Snapshot::Unreadable,
    }
}

/// Whether only `owner` can change what `metadata` describes: owned by
/// `owner` and neither group- nor other-writable. An unknown owner is never
/// trusted on Unix; elsewhere there is no mode to check.
#[cfg(unix)]
fn private_to(metadata: &fs::Metadata, owner: Option<u32>) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    owner == Some(metadata.uid()) && !open_to_others(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn private_to(_metadata: &fs::Metadata, _owner: Option<u32>) -> bool {
    true
}

/// Group- or other-writable.
#[cfg(unix)]
const fn open_to_others(mode: u32) -> bool {
    mode & 0o022 != 0
}

/// Temporary file plus rename, so a hook killed at the host's timeout cannot
/// leave a half-written file where [`read_snapshot`] would find it. The file
/// is created 0600 (a umask can only narrow that), so no other user can
/// rewrite what the next run trusts, and synced before it replaces the old
/// one.
fn write_json(path: &Path, value: &Json) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut text = pyjson::dumps(value, Some(2), false);
    text.push('\n');
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// `--via cli` marks an invocation made from a skill instruction instead of a
/// hook.
fn via_mode(args: &[&str]) -> &'static str {
    match args
        .iter()
        .position(|arg| *arg == "--via")
        .and_then(|i| args.get(i + 1))
    {
        Some(&"cli") => "cli",
        _ => "hook",
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` of a Unix time (`time.strftime` over `gmtime`).
fn utc_stamp(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let total = seconds.floor() as i64;
    let days = total.div_euclid(86_400);
    let rest = total.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

// ------------------------------------------------------------ run discovery

/// Run directories (`.migration/<id>/` with a state file) for the nearest
/// `.migration` tree, walking up a bounded number of levels so runs from
/// parent or sibling projects are not mixed in. Symbolic-link entries are not
/// runs; the order is by name.
fn find_run_dirs(start_dir: &Path) -> Vec<PathBuf> {
    let mut directory = resolve_lenient(start_dir);
    for _ in 0..4 {
        let root = directory.join(".migration");
        let Ok(entries) = fs::read_dir(&root) else {
            match directory.parent() {
                Some(parent) if parent != directory => directory = parent.to_path_buf(),
                _ => break,
            }
            continue;
        };
        let mut runs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .filter(|path| path.join(STATUS_FILE).exists())
            .collect();
        runs.sort();
        return runs;
    }
    Vec::new()
}

// ------------------------------------------------------------------ the lock

/// An exclusive per-run lock (directory creation as an atomic test-and-set).
/// Overlapping hooks would otherwise read the same "before" state and report
/// the same transitions twice.
///
/// The directory is created 0700 (a umask can only narrow it). One that
/// another user can write is never honoured: anyone could keep it fresh by
/// adding an entry, so it says nothing about a holder; it is replaced like an
/// abandoned one.
struct Lock {
    path: PathBuf,
    /// The user this process creates files as: the owner of the directory it
    /// just created. A snapshot is trusted only when it belongs to this user.
    owner: Option<u32>,
}

impl Lock {
    /// Takes a directory this process just created. On Unix a lock whose owner
    /// cannot be read is released at once: nothing could be checked against it.
    fn created(path: PathBuf) -> Option<Self> {
        #[cfg(unix)]
        let owner = {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(&path).ok().map(|metadata| metadata.uid())
        };
        #[cfg(not(unix))]
        let owner = None;
        let lock = Self { path, owner };
        if cfg!(unix) && lock.owner.is_none() {
            return None;
        }
        Some(lock)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(unix)]
fn writable_by_others(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    open_to_others(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn writable_by_others(_metadata: &fs::Metadata) -> bool {
    false
}

fn acquire_lock(ctx: &Ctx<'_>, run_dir: &Path) -> Option<Lock> {
    let lock = run_dir.join(LOCK_DIR);
    match create_private_dir(&lock) {
        Ok(()) => Lock::created(lock),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&lock).ok()?;
            // A link or a file is not a lock this program made: never removed.
            if !metadata.is_dir() {
                return None;
            }
            let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            let stale = (ctx.now)() - modified.as_secs_f64() > LOCK_STALE_SECONDS;
            if stale || writable_by_others(&metadata) {
                // Removal does not follow links inside the directory. If a peer
                // recreated it first, creation fails and that peer holds it.
                let _ = fs::remove_dir_all(&lock);
                create_private_dir(&lock).ok()?;
                Lock::created(lock)
            } else {
                // The holder is about to report the same transitions.
                None
            }
        }
        // An unwritable run directory: emit nothing.
        Err(_) => None,
    }
}

// ------------------------------------------------------------------- the diff

/// The skill records each failed gate in `.gate-failures.json`, keyed by
/// phase. Only phases the model knows are reportable; a null, scalar or array
/// entry is not a failure.
fn gate_failure_entries(gate_failures: Option<&Json>) -> Vec<(String, &Json)> {
    let Some(Json::Object(members)) = gate_failures else {
        return Vec::new();
    };
    members
        .iter()
        .filter(|(_, entry)| matches!(entry, Json::Object(_)))
        .map(|(name, entry)| (name.to_uppercase(), entry))
        .filter(|(phase, _)| PHASES.contains(&phase.as_str()))
        .collect()
}

/// `str(phases.get(name, "")).lower()`.
fn phase_text(phases: &Json, name: &str) -> String {
    phases
        .get(name)
        .map_or_else(String::new, |value| py_str(value).to_lowercase())
}

/// `(ended, runMode)`. The skills flip `run_mode` to `decide_and_execute`
/// before Generate runs and may leave `current_phase: complete` from the
/// decision-only finish in place, so an executed ending counts only once
/// Generate is done.
///
/// `current_phase` is optional in the shared state schema and authoritative
/// when present. Without it the owning skill evaluates the backbone in order
/// and is complete only when every backbone phase, Generate included, is
/// completed. Sidebars (workshop, feedback) never decide completion.
fn run_ended(status: &Json) -> R<(bool, Option<&'static str>)> {
    let run_mode = map_enum(&RUN_MODE, status.get("run_mode"));
    let phases = or_obj(status.get("phases"))?;
    let executing =
        run_mode == Some("DECIDE_AND_EXECUTE") && phase_text(phases, "generate") != "completed";
    let ended = match get(status, "current_phase") {
        None => BACKBONE
            .iter()
            .all(|name| phase_text(phases, name) == "completed"),
        Some(current) => current == &Json::string("complete"),
    };
    Ok((ended && !executing, run_mode))
}

/// The scalars of a Python iterable of snapshot data (`set(x or [])`,
/// `dict.fromkeys(x or [])`): list items, a string's characters, an object's
/// keys. Unhashable items and non-iterables raise.
fn iterate(value: Option<&Json>) -> R<Vec<Json>> {
    let Some(value) = value.filter(|v| !is_falsy(v)) else {
        return Ok(Vec::new());
    };
    match value {
        Json::Array(items) => {
            if items
                .iter()
                .any(|item| matches!(item, Json::Array(_) | Json::Object(_)))
            {
                return Err(Abort);
            }
            Ok(items.clone())
        }
        Json::String(text) => Ok(text.chars().map(|c| Json::String(c.to_string())).collect()),
        Json::Object(members) => Ok(members
            .iter()
            .map(|(key, _)| Json::String(key.clone()))
            .collect()),
        _ => Err(Abort),
    }
}

/// Every `runMode` this run has already been reported ending under. Older
/// snapshots kept only the last one under `completedRunMode`.
fn reported_run_modes(snapshot: Option<&Json>) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    if let Some(Json::Array(modes)) = snapshot.get("completedRunModes") {
        return modes
            .iter()
            .filter_map(|mode| match mode {
                Json::String(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
    }
    match snapshot.get("completedRunMode") {
        Some(Json::String(last)) => vec![last.clone()],
        _ => Vec::new(),
    }
}

fn snapshot_completed(snapshot: Option<&Json>) -> bool {
    snapshot
        .and_then(|snapshot| snapshot.get("completed"))
        .is_some_and(|value| !is_falsy(value))
}

/// One event per transition between the snapshot and the state file, plus one
/// `GATE_FAILED` per phase newly present in the gate record (a repeat failure
/// of the same phase is not a new event; its later success is). A run ends
/// once per `runMode`: a decision-only finish the user later turns into an
/// executed one is reported again, under `DECIDE_AND_EXECUTE`, and each mode
/// at most once.
fn diff_events(
    status: &Json,
    snapshot: Option<&Json>,
    gate_failures: Option<&Json>,
) -> R<Vec<Event>> {
    let mut events = Vec::new();
    let restarted = snapshot.is_some_and(|s| s.get("started") == Some(&Json::Bool(false)));
    if snapshot.is_none() || restarted {
        events.push(Event::named("RUN_STARTED"));
    }
    let reported: Vec<String> = iterate(snapshot.and_then(|s| s.get("gateFailures")))?
        .into_iter()
        .filter_map(|item| match item {
            Json::String(text) => Some(text),
            _ => None,
        })
        .collect();
    for (phase, entry) in gate_failure_entries(gate_failures) {
        if reported.contains(&phase) {
            continue;
        }
        let mut event = Event::named("GATE_FAILED");
        event.phase = Some(phase);
        event.status = Some("FAILED");
        event.failure_reason = map_enum(&FAILURE_REASON, entry.get("reason"));
        events.push(event);
    }
    let before = or_obj(snapshot.and_then(|s| s.get("phases")))?;
    if let Json::Object(members) = or_obj(status.get("phases"))? {
        for (name, state) in members {
            if py_eq(before.get(name).unwrap_or(&Json::Null), state) {
                continue;
            }
            let key = py_str(state).to_lowercase();
            let mapped = RESOLVED_STATUS
                .iter()
                .find(|(text, _)| *text == key)
                .map(|(_, mapped)| *mapped);
            let phase = name.to_uppercase();
            let Some(mapped) = mapped else { continue };
            if !PHASES.contains(&phase.as_str()) {
                continue;
            }
            let mut event = Event::named("PHASE_COMPLETED");
            event.phase = Some(phase);
            event.status = Some(mapped);
            event.key = Some(name.clone());
            events.push(event);
        }
    }
    let (ended, run_mode) = run_ended(status)?;
    let reported_modes = reported_run_modes(snapshot);
    // A workshop re-entry clears run_mode and reopens the decision gate while
    // the run stays completed; a mode already reported is never reported again.
    let ended_again = run_mode.is_some_and(|mode| {
        !reported_modes.is_empty() && !reported_modes.iter().any(|r| r == mode)
    });
    if ended && (!snapshot_completed(snapshot) || ended_again) {
        let mut event = Event::named("RUN_COMPLETED");
        event.status = Some("SUCCESS");
        event.run_mode = run_mode;
        events.push(event);
    }
    Ok(events)
}

/// Per-run facts shared by every event of one batch.
struct Batch<'a> {
    run_dir: &'a Path,
    status: &'a Json,
    skill: &'a str,
    initiating_skill: Option<&'a str>,
    run_id: &'a str,
    session_id: Option<&'a str>,
}

/// The `migrationActivity` union member for `event`.
fn build_activity(event: &Event, batch: &Batch<'_>) -> Json {
    let mut activity = vec![
        ("eventName".to_string(), Json::string(event.name)),
        ("skill".to_string(), Json::string(batch.skill)),
    ];
    if let Some(initiating) = batch.initiating_skill {
        activity.push(("initiatingSkill".to_string(), Json::string(initiating)));
    }
    activity.push(("runId".to_string(), Json::string(batch.run_id)));
    if let Some(session) = batch.session_id {
        activity.push(("sessionId".to_string(), Json::string(session)));
    }
    if let Some(phase) = &event.phase {
        activity.push(("phase".to_string(), Json::string(phase.as_str())));
    }
    if let Some(status) = event.status {
        activity.push(("status".to_string(), Json::string(status)));
    }
    // An artifact of an unexpected shape costs its attributes, never the event.
    if let Ok(Some(attributes)) = derive_attributes(batch.run_dir, batch.skill, event, batch.status)
    {
        activity.push(("attributes".to_string(), attributes));
    }
    Json::object(vec![("migrationActivity", Json::Object(activity))])
}

/// Outcomes that mean the service did not take the event but may later: no
/// connection at all (offline, DNS), 403 (a closed launch gate, or a WAF rate
/// limit), 429 and 5xx. Anything else is the loss the design tolerates rather
/// than risk a duplicate: a 400 the event would earn again, and a timeout or a
/// dropped connection, after which the service may have taken it.
fn is_held(status: PostStatus) -> bool {
    match status {
        PostStatus::Unreachable => true,
        PostStatus::Status(code) => code == 403 || code == 429 || code >= 500,
        PostStatus::Unknown => false,
    }
}

// ------------------------------------------------------------------ per run

fn members(value: &Json) -> &[(String, Json)] {
    match value {
        Json::Object(members) => members,
        _ => &[],
    }
}

fn dedupe(items: Vec<Json>) -> Vec<Json> {
    let mut out: Vec<Json> = Vec::new();
    for item in items {
        if !out.iter().any(|seen| py_eq(seen, &item)) {
            out.push(item);
        }
    }
    out
}

/// What one run's processing was asked to do.
struct Request<'a> {
    session_id: Option<&'a Json>,
    session_end: bool,
    url: &'a str,
    deadline: f64,
    edited_path: Option<&'a str>,
    via: &'a str,
}

/// The state files of a run: the status object (with a `migration_id`) and the
/// gate record, or `None` when the run has no usable state.
fn read_state(run_dir: &Path) -> Option<(Json, Option<Json>)> {
    let status = read_json(&run_dir.join(STATUS_FILE))?;
    if !matches!(status, Json::Object(_)) || status.get("migration_id").is_none_or(is_falsy) {
        return None;
    }
    Some((status, read_json(&run_dir.join(GATE_FILE))))
}

/// The snapshot's inputs for [`write_snapshot`].
struct SnapshotValue<'a> {
    run_id: &'a str,
    owner: Option<&'a str>,
    started_false: bool,
    phases: Json,
    gates: Vec<Json>,
    completed: bool,
    modes: &'a [String],
}

fn write_snapshot(ctx: &Ctx<'_>, file: &Path, via: &str, value: SnapshotValue<'_>) {
    let mut out = vec![("runId".to_string(), Json::string(value.run_id))];
    if let Some(owner) = value.owner {
        out.push(("sessionId".to_string(), Json::string(owner)));
    }
    if value.started_false {
        out.push(("started".to_string(), Json::Bool(false)));
    }
    out.push(("phases".to_string(), value.phases));
    out.push(("gateFailures".to_string(), Json::Array(value.gates)));
    out.push(("completed".to_string(), Json::Bool(value.completed)));
    out.push(("via".to_string(), Json::string(via)));
    out.push((
        "updatedAt".to_string(),
        Json::String(utc_stamp((ctx.now)())),
    ));
    if !value.modes.is_empty() {
        // Every runMode the run has been reported ending under.
        out.push((
            "completedRunModes".to_string(),
            Json::Array(value.modes.iter().cloned().map(Json::String).collect()),
        ));
    }
    // A snapshot that cannot be written costs a repeat later, never a failure.
    let _ = write_json(file, &Json::Object(out));
}

#[allow(clippy::too_many_lines)]
fn process_run(ctx: &Ctx<'_>, run_dir: &Path, request: &Request<'_>) -> R<()> {
    if (ctx.now)() >= request.deadline {
        return Ok(()); // out of budget: untouched, so the next trigger reports it
    }
    let Some((status, _)) = read_state(run_dir) else {
        return Ok(());
    };

    // Attribution is read from disk, never from an argument: a run that
    // declares no owner, or an owner outside the migration set, emits nothing
    // (fail closed) rather than emitting under the wrong skill.
    let Some(Json::String(skill)) = status.get("owning_skill").cloned() else {
        return Ok(());
    };
    if !MIGRATION_SKILLS.contains(&skill.as_str()) {
        return Ok(());
    }

    // Consent is the plugin's one gate; without it nothing is read into a
    // snapshot either, so a user who never agreed, or opted out, leaves no
    // trace.
    let Some(record) = consent::read_state(ctx) else {
        return Ok(());
    };
    if record.status != consent::Status::Accepted {
        return Ok(());
    }
    let Some(install_id) = as_uuid(record.install_id.map(Json::String).as_ref()) else {
        return Ok(());
    };

    let snapshot_file = run_dir.join(SNAPSHOT_FILE);
    let Some(lock) = acquire_lock(ctx, run_dir) else {
        return Ok(());
    };

    // Read again under the lock. Hooks run concurrently: one that read the
    // state before another reported a newer transition, and took the lock only
    // after that report, would otherwise diff against stale state and move the
    // snapshot backwards, and the next reconcile would re-send the newer
    // transition.
    let Some((status, gate_failures)) = read_state(run_dir) else {
        return Ok(());
    };
    if status.get("owning_skill") != Some(&Json::string(skill.as_str())) {
        return Ok(());
    }
    let (snapshot, rebuild) = match read_snapshot(&snapshot_file, lock.owner) {
        Snapshot::Absent => (None, false),
        Snapshot::Unreadable => (None, true), // re-observed below, without sending
        Snapshot::Data(data) => (Some(data), false),
    };
    let snapshot = snapshot.as_ref();
    let valid_session_id = as_uuid(request.session_id);

    // Teardown sweeps only the session that wrote the snapshot; a run last
    // touched by another session is that session's to report.
    let owner_before = match snapshot.and_then(|s| s.get("sessionId")) {
        Some(Json::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    };
    if request.session_end {
        if let (Some(owner), Some(session)) = (&owner_before, &valid_session_id) {
            if owner != session {
                return Ok(());
            }
        }
    }

    // Identifiers read back from customer-editable files are validated, not
    // trusted: the service rejects the whole event on one malformed UUID. A
    // run directory is one lifecycle, so the id the snapshot already reports
    // under stays authoritative: a state file rebuilt after corruption carries
    // a fresh run_id, which must not split the run.
    let run_id = as_uuid(snapshot.and_then(|s| s.get("runId")))
        .or_else(|| as_uuid(status.get("run_id")))
        .unwrap_or_else(consent::uuid4);

    let run_root = resolve_lenient(run_dir);
    let in_run = |path: Option<&str>| {
        path.is_some_and(|path| {
            !path.is_empty() && resolve_lenient(Path::new(path)).starts_with(&run_root)
        })
    };

    let modes_before = reported_run_modes(snapshot);
    let started_false_before =
        snapshot.is_some_and(|s| s.get("started") == Some(&Json::Bool(false)));
    let owner_pick = |first: Option<&String>, second: Option<&String>| first.or(second).cloned();

    // Record the state last observed, whether or not anything was sent.
    // Ownership moves to this session only when it changed the run or edited a
    // file inside it; a hook that merely scanned past leaves the owner alone.
    // Written only when something changed.
    let observe = |phases: &Json, completed: bool, gates: &[Json], completed_modes: &[String]| {
        let changed = match snapshot {
            None => true,
            Some(snap) => {
                let old_phases = or_obj(snap.get("phases")).unwrap_or(&Json::Null);
                let old_gates = snap
                    .get("gateFailures")
                    .filter(|v| !is_falsy(v))
                    .cloned()
                    .unwrap_or_else(|| Json::Array(Vec::new()));
                !py_eq(old_phases, phases)
                    || snapshot_completed(Some(snap)) != completed
                    || modes_before != completed_modes
                    || !py_eq(&old_gates, &Json::Array(gates.to_vec()))
            }
        };
        let touched = changed || in_run(request.edited_path);
        let owner = if touched {
            owner_pick(valid_session_id.as_ref(), owner_before.as_ref())
        } else {
            owner_pick(owner_before.as_ref(), valid_session_id.as_ref())
        };
        if !changed {
            let recorded = snapshot.and_then(|s| s.get("sessionId"));
            let same = match (&owner, recorded) {
                (Some(owner), Some(Json::String(text))) => owner == text,
                (None, None | Some(Json::Null)) => true,
                _ => false,
            };
            if same {
                return;
            }
        }
        write_snapshot(
            ctx,
            &snapshot_file,
            request.via,
            SnapshotValue {
                run_id: &run_id,
                owner: owner.as_deref(),
                started_false: started_false_before,
                phases: phases.clone(),
                gates: gates.to_vec(),
                completed,
                modes: completed_modes,
            },
        );
    };

    let status_phases = || -> R<Json> { Ok(or_obj(status.get("phases"))?.clone()) };

    if rebuild {
        // A corrupt snapshot (not this program's: its write is atomic), or one
        // another user could have written, cannot say what was reported.
        // Those transitions are lost; recording the current state privately
        // lets the run report again from the next one.
        let (ended, run_mode) = run_ended(&status)?;
        let gates: Vec<Json> = gate_failure_entries(gate_failures.as_ref())
            .into_iter()
            .map(|(phase, _)| Json::String(phase))
            .collect();
        let modes: Vec<String> = match (ended, run_mode) {
            (true, Some(mode)) => vec![mode.to_string()],
            _ => Vec::new(),
        };
        observe(&status_phases()?, ended, &gates, &modes);
        return Ok(());
    }

    let events = diff_events(&status, snapshot, gate_failures.as_ref())?;
    let known_gates = dedupe(iterate(snapshot.and_then(|s| s.get("gateFailures")))?);
    if events.is_empty() {
        // Nothing to send, but the observation may have changed: a phase reset
        // (a confirmed re-entry sets downstream phases back to pending) or a
        // run taken over by this session. Record it, or the phase's next
        // completion would read as already reported and this session's
        // teardown would skip the run.
        observe(
            &status_phases()?,
            snapshot_completed(snapshot),
            &known_gates,
            &modes_before,
        );
        return Ok(());
    }

    let initiating = match status.get("initiated_by") {
        Some(Json::String(text)) if MIGRATION_SKILLS.contains(&text.as_str()) => Some(text.clone()),
        _ => None,
    };
    let batch = Batch {
        run_dir,
        status: &status,
        skill: &skill,
        // Only a known migration skill may be named as the invoker; anything
        // else is dropped rather than risk rejecting the whole event.
        initiating_skill: initiating.as_deref(),
        run_id: &run_id,
        session_id: valid_session_id.as_deref(),
    };

    // Concurrently, under one budget: sent serially, a reconcile catching a
    // whole run could outlive the host's hook timeout and never reach the
    // snapshot write, so the next trigger would repeat the batch. Consent is
    // re-checked per request while the bodies are prepared.
    let bodies: Vec<Option<String>> = events
        .iter()
        .map(|event| client::prepare(ctx, &build_activity(event, &batch), Some(&install_id)))
        .collect();
    let remaining = request.deadline - (ctx.now)();
    let timeout = Duration::try_from_secs_f64(remaining.clamp(0.05, POST_TIMEOUT_SECONDS))
        .unwrap_or(Duration::from_millis(50));
    let results: Vec<PostStatus> = std::thread::scope(|scope| {
        let handles: Vec<_> = bodies
            .iter()
            .map(|body| {
                scope.spawn(move || {
                    body.as_deref().map_or(PostStatus::Unknown, |body| {
                        client::post(ctx, request.url, body, timeout)
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or(PostStatus::Unknown))
            .collect::<Vec<PostStatus>>()
    });
    let held: Vec<&Event> = events
        .iter()
        .zip(&results)
        .filter(|(_, result)| is_held(**result))
        .map(|(event, _)| event)
        .collect();
    if held.len() == events.len() {
        return Ok(()); // nothing taken: nothing recorded
    }

    // The snapshot records exactly what the service took. A held phase keeps
    // its previous state so only that transition is re-sent; a held
    // RUN_STARTED, GATE_FAILED or RUN_COMPLETED is re-sent alone. Accepted
    // events are never repeated, even when a later trigger replays the held
    // ones.
    let mut phases: Vec<(String, Json)> = members(&status_phases()?).to_vec();
    let before = or_obj(snapshot.and_then(|s| s.get("phases")))?;
    for event in &held {
        if event.name != "PHASE_COMPLETED" {
            continue;
        }
        let Some(key) = &event.key else { continue };
        match before.get(key) {
            Some(previous) => {
                if let Some(slot) = phases.iter_mut().find(|(name, _)| name == key) {
                    slot.1 = previous.clone();
                } else {
                    phases.push((key.clone(), previous.clone()));
                }
            }
            None => phases.retain(|(name, _)| name != key),
        }
    }
    let run_started = events.iter().find(|e| e.name == "RUN_STARTED");
    let run_completed = events.iter().find(|e| e.name == "RUN_COMPLETED");
    let sent_gates: Vec<Json> = events
        .iter()
        .filter(|e| e.name == "GATE_FAILED" && !held.contains(e))
        .filter_map(|e| e.phase.clone().map(Json::String))
        .collect();
    let ended_now = run_completed.is_some_and(|e| !held.contains(&e));
    let mut modes = modes_before.clone();
    if let Some(mode) = run_completed.filter(|_| ended_now).and_then(|e| e.run_mode) {
        if !modes.iter().any(|m| m == mode) {
            modes.push(mode.to_string());
        }
    }
    let mut gates = known_gates;
    gates.extend(sent_gates);
    write_snapshot(
        ctx,
        &snapshot_file,
        request.via,
        SnapshotValue {
            run_id: &run_id,
            owner: valid_session_id
                .as_ref()
                .or(owner_before.as_ref())
                .map(String::as_str),
            started_false: run_started.is_some_and(|e| held.contains(&e)),
            phases: Json::Object(phases),
            gates: dedupe(gates),
            completed: snapshot_completed(snapshot) || ended_now,
            modes: &modes,
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------- main

/// Report every run reachable from `start_dir`. Never fails.
fn reconcile(ctx: &Ctx<'_>, start_dir: &Path, session_end: bool, mut request: Request<'_>) {
    request.deadline = if session_end {
        (ctx.now)() + SESSION_END_BUDGET_SECONDS - SESSION_END_RESERVE_SECONDS
    } else {
        f64::INFINITY
    };
    for run_dir in find_run_dirs(start_dir) {
        // One run's trouble must not silence the others.
        let _ = process_run(ctx, &run_dir, &request);
    }
}

fn truthy(value: Option<&Json>) -> Option<&Json> {
    value.filter(|v| !is_falsy(v))
}

/// The hook entry point (`--reconcile`, `--session-end`, `--via cli`). Never
/// fails and prints nothing.
pub(super) fn main(args: &[&str], ctx: &Ctx<'_>) {
    let via = via_mode(args);
    let cli = via == "cli";
    if cli && client::migration_reporting_mode(ctx) == "hook" {
        return; // Claude Code and Cursor retain their existing hook path.
    }
    if cli && client::plugin_version(ctx).is_none() {
        return; // do not advance a snapshot when a standalone bundle is incomplete
    }
    let session_end = args.contains(&"--session-end");
    // Explicit CLI calls use the working directory; they have no hook payload
    // and must not wait for stdin or accept a caller-supplied hook session or
    // project identity.
    let payload = if cli {
        Json::Object(Vec::new())
    } else {
        read_hook_payload(ctx)
    };
    // Claude Code sends session_id on every hook; Cursor sends conversation_id.
    let session_id = truthy(payload.get("session_id")).or(payload.get("conversation_id"));
    let tool_input = match payload.get("tool_input") {
        Some(object @ Json::Object(_)) => object,
        _ => &Json::Null,
    };
    let edited = truthy(tool_input.get("file_path")).or(payload.get("file_path"));
    let edited_path = match edited {
        Some(Json::String(text)) => Some(text.as_str()),
        _ => None,
    };

    // Post-write fast path: an edit outside any .migration tree is no work at
    // all. A Bash payload carries a command, not a file path, and the agent
    // does write state files through the shell, so it falls through to a
    // reconcile of every run under the hook's cwd: a few file reads, and
    // nothing is sent unless a transition is new.
    if !session_end
        && !args.contains(&"--reconcile")
        && edited_path.is_some_and(|path| !path.is_empty() && !path.contains(".migration"))
    {
        return;
    }

    // Start discovery at the directory that owns the .migration tree, so an
    // edit deep inside a run's artifacts still resolves to its run.
    let separator = std::path::MAIN_SEPARATOR;
    let marker_text = format!("{separator}.migration{separator}");
    let owning = edited_path
        .and_then(|path| path.find(&marker_text).map(|at| &path[..at]))
        .filter(|path| !path.is_empty());
    let from_payload = |value: Option<&Json>| match value {
        Some(Json::String(text)) => Some(Some(PathBuf::from(text))),
        Some(_) => Some(None), // a non-path value: Python raised, nothing is reported
        None => None,
    };
    let roots_first = match payload.get("workspace_roots") {
        Some(Json::Array(roots)) => roots.first().filter(|v| !is_falsy(v)),
        _ => None,
    };
    let start_dir = if let Some(path) = owning {
        Some(PathBuf::from(path))
    } else if let Some(found) =
        from_payload(truthy(payload.get("cwd"))).or_else(|| from_payload(roots_first))
    {
        found
    } else {
        Some(ctx.cwd.to_path_buf())
    };
    let Some(start_dir) = start_dir else {
        return;
    };

    let url = client::endpoint(ctx);
    reconcile(
        ctx,
        &start_dir,
        session_end,
        Request {
            session_id,
            session_end,
            url: &url,
            deadline: f64::INFINITY,
            edited_path,
            via,
        },
    );
}
