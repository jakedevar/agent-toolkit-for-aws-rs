//! The Startups Advisor plugin-telemetry event client
//! (`metric_emission/client.py`).
//!
//! ```text
//! POST /v1/plugin-telemetry-event
//! {"installId": "<uuid>", "source": "CLAUDE_CODE", "pluginVersion": "2.0.2",
//!  "occurredAt": 1759276800000, "pluginTelemetryEvent": {...}}
//! ```
//!
//! Fire and forget: nothing here reports failure to the user. The consent gate
//! is re-checked in [`prepare`], the only way to obtain a request body, so a new
//! call site cannot emit by forgetting to ask. The request itself goes through
//! an [`EventSink`]; this build ships only [`NoNetworkSink`].

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use toolkit_core::{Json, ToolkitError};

use super::consent::{self, Status};
use super::Ctx;
use crate::startup::artifacts::pyjson;
use crate::Output;

/// Prod is the only stage a customer should reach; the override chooses where
/// an event goes, never whether one is sent.
pub(super) const DEFAULT_ENDPOINT: &str =
    "https://us-east-1.prod.startup-advisor-extension.saws.activate.aws.dev/v1/plugin-telemetry-event";
pub(super) const ENDPOINT_ENV: &str = "AWS_STARTUP_ADVISOR_PLUGIN_TELEMETRY_ENDPOINT";

/// Short, because these run inline in an agent turn with a human waiting.
pub(super) const TIMEOUT: Duration = Duration::from_secs(3);

const SOURCE_CLAUDE_CODE: &str = "CLAUDE_CODE";
const SOURCE_CODEX: &str = "CODEX";
const SOURCE_CURSOR: &str = "CURSOR";
const SOURCE_KIRO: &str = "KIRO";
const SOURCE_OTHER: &str = "OTHER";

/// First marker present wins. Cursor's hook-specific markers precede the
/// Claude compatibility variables; explicit Codex/Kiro markers precede the
/// weaker, inheritable `CURSOR_AGENT`. An unmatched host reports `OTHER`
/// rather than a plausible guess.
const HOST_MARKERS: [(&str, &str); 8] = [
    ("CURSOR_VERSION", SOURCE_CURSOR),
    ("CURSOR_PROJECT_DIR", SOURCE_CURSOR),
    ("CURSOR_TRACE_ID", SOURCE_CURSOR),
    ("CODEX_SANDBOX", SOURCE_CODEX),
    ("KIRO_IDE", SOURCE_KIRO),
    ("CURSOR_AGENT", SOURCE_CURSOR),
    ("CLAUDECODE", SOURCE_CLAUDE_CODE),
    ("CLAUDE_CODE_ENTRYPOINT", SOURCE_CLAUDE_CODE),
];

/// The `PluginSkillId` enum of the service model. An unmodelled member is a
/// 400 for the whole request, so it is checked last, before anything is sent.
pub(super) const PLUGIN_SKILL_IDS: [&str; 12] = [
    "ARCHITECT_FOR_STARTUPS",
    "KNOWLEDGE_BASE_FOR_STARTUPS",
    "PROMPT_LIBRARY_FOR_STARTUPS",
    "START_BUILDING_FOR_STARTUPS",
    "AZURE_TO_AWS",
    "GCP_TO_AWS",
    "HEROKU_TO_AWS",
    "LLM_TO_BEDROCK",
    "AGENT_ADVISOR",
    "TF_BEST_PRACTICES",
    "CONTEXTUAL_OFFERS_FOR_STARTUPS",
    "OPERATE_ON_AWS",
];

const EVENT_SKILL_INVOKED: &str = "SKILL_INVOKED";

/// What a request produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostStatus {
    /// An HTTP status came back.
    #[allow(
        dead_code,
        reason = "constructed by sinks that send; the sink of this build sends nothing"
    )]
    Status(u16),
    /// No connection could be made at all (offline, DNS, refused): unlike a
    /// timeout, nothing can have reached the service.
    Unreachable,
    /// Nothing was sent or the outcome is unknown (a timeout, a connection
    /// dropped mid-request).
    Unknown,
}

/// One prepared request: a JSON `POST` of `body` to `url`.
#[allow(
    dead_code,
    reason = "read by sinks that send; the sink of this build sends nothing"
)]
pub(crate) struct PostRequest<'a> {
    pub url: &'a str,
    pub body: &'a str,
    /// Always `application/json`.
    pub content_type: &'static str,
    /// Always `aws-startup-advisor-plugin`.
    pub user_agent: &'static str,
    pub timeout: Duration,
}

/// Sends one prepared request. Implementations must not panic and must treat
/// every failure as a status; they never see anything the consent gate did not
/// already allow.
pub(crate) trait EventSink: Sync {
    /// POST the request.
    fn post(&self, request: &PostRequest<'_>) -> PostStatus;
}

/// The sink of this build: it has no HTTP client, so every request is
/// reported as unreachable and nothing leaves the process.
pub(crate) struct NoNetworkSink;

impl EventSink for NoNetworkSink {
    fn post(&self, _request: &PostRequest<'_>) -> PostStatus {
        PostStatus::Unreachable
    }
}

/// The URL to POST to, honoring the override.
pub(super) fn endpoint(ctx: &Ctx<'_>) -> String {
    (ctx.env)(ENDPOINT_ENV)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
}

/// Best-effort `PluginSource` for the host, or `OTHER`.
pub(super) fn detect_source(ctx: &Ctx<'_>) -> &'static str {
    HOST_MARKERS
        .iter()
        .find(|(name, _)| (ctx.env)(name).is_some_and(|value| !value.trim().is_empty()))
        .map_or(SOURCE_OTHER, |(_, source)| source)
}

/// The plugin root of a recognised layout: `<plugin>/scripts/telemetry` or
/// `<plugin>/skills/<skill>/references/vendored/telemetry`. A bare
/// `references/` directory is never a plugin.
fn plugin_root(telemetry_dir: &Path) -> Option<PathBuf> {
    let parent = telemetry_dir.parent()?;
    let name_of = |path: &Path| path.file_name().map(std::ffi::OsStr::to_os_string);
    if name_of(parent).is_some_and(|name| name == "scripts") {
        return parent.parent().map(Path::to_path_buf);
    }
    let ancestors: Vec<&Path> = telemetry_dir.ancestors().collect();
    // ancestors: [0]=telemetry, [1]=vendored, [2]=references, [3]=<skill>,
    // [4]=skills, [5]=<plugin>
    if ancestors.len() > 5
        && name_of(ancestors[1]).is_some_and(|name| name == "vendored")
        && name_of(ancestors[2]).is_some_and(|name| name == "references")
        && name_of(ancestors[4]).is_some_and(|name| name == "skills")
    {
        return Some(ancestors[5].to_path_buf());
    }
    None
}

/// Manifests that may carry the plugin version, in order. A bundle reports
/// its own generated `version.json`; only recognised plugin layouts fall back
/// to plugin manifests.
fn manifest_paths(ctx: &Ctx<'_>) -> Vec<PathBuf> {
    let Some(dir) = ctx.telemetry_dir else {
        return Vec::new();
    };
    let mut paths = vec![dir.join("version.json")];
    if let Some(root) = plugin_root(dir) {
        paths.push(root.join(".claude-plugin").join("plugin.json"));
        paths.push(root.join(".cursor-plugin").join("plugin.json"));
        paths.push(root.join("plugin.json"));
    }
    paths
}

/// Largest manifest or hook file read.
const MAX_JSON_BYTES: u64 = 1 << 20;

fn read_bounded(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_JSON_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_JSON_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn read_json_file(path: &Path) -> Option<Json> {
    pyjson::loads(&read_bounded(path)?).ok()
}

/// The plugin's version, or `None`. `None` is a refusal to send, not a
/// default: a made-up `0.0.0` would pass the semver pattern and misattribute
/// the event to a version that does not exist.
pub(super) fn plugin_version(ctx: &Ctx<'_>) -> Option<String> {
    for manifest in manifest_paths(ctx) {
        let Some(Json::String(version)) = read_json_file(&manifest)
            .as_ref()
            .and_then(|data| data.get("version").cloned())
        else {
            continue;
        };
        let version = version.trim();
        if !version.is_empty() {
            return Some(version.to_string());
        }
    }
    None
}

/// Whether `command` is a hook command that runs the migration reporter:
/// either the Python script (which must exist next to the plugin) or this
/// binary's `telemetry migration` action.
fn runs_migration_reporter(command: &str, root: &Path) -> bool {
    const SCRIPT: &str = "scripts/telemetry/metric_emission/migration.py";
    (command.contains(SCRIPT) && root.join(SCRIPT).is_file())
        || command.contains("telemetry migration")
}

/// A hook shape the Python code would have raised on: the whole check then
/// answers `cli`.
struct BadShape;

fn registered_hook(source: &str, hooks: &Json, root: &Path) -> Result<bool, BadShape> {
    let claude = source == SOURCE_CLAUDE_CODE;
    let events: &[&str] = if claude {
        &["PostToolUse", "Stop"]
    } else {
        &["afterFileEdit", "afterShellExecution", "stop"]
    };
    for event in events {
        let entries = match hooks.get(event) {
            None => continue,
            Some(Json::Array(entries)) => entries,
            Some(_) => return Err(BadShape),
        };
        for entry in entries {
            let commands: Vec<&Json> = if claude {
                match entry.get("hooks") {
                    None => Vec::new(),
                    Some(Json::Array(list)) => list.iter().collect(),
                    Some(_) => return Err(BadShape),
                }
            } else {
                vec![entry]
            };
            for command in commands {
                if !matches!(command, Json::Object(_)) {
                    return Err(BadShape);
                }
                match command.get("command") {
                    None => {}
                    Some(Json::String(text)) => {
                        if runs_migration_reporter(text, root) {
                            return Ok(true);
                        }
                    }
                    Some(_) => return Err(BadShape),
                }
            }
        }
    }
    Ok(false)
}

/// Use configured plugin hooks when reachable, otherwise the bundled CLI.
/// This checks the install layout, not whether a host enabled or ran its hooks.
pub(super) fn migration_reporting_mode(ctx: &Ctx<'_>) -> &'static str {
    let source = detect_source(ctx);
    let manifest_name = match source {
        SOURCE_CLAUDE_CODE => ".claude-plugin/plugin.json",
        SOURCE_CURSOR => ".cursor-plugin/plugin.json",
        _ => return "cli",
    };
    let Some(root) = ctx.telemetry_dir.and_then(plugin_root) else {
        return "cli";
    };
    let hooked = (|| {
        let manifest = read_json_file(&root.join(manifest_name))?;
        if manifest.get("name") != Some(&Json::string("aws-startup-advisor")) {
            return None;
        }
        let Some(Json::String(hook_name)) = manifest.get("hooks") else {
            return None;
        };
        let hook_file = fs::canonicalize(root.join(hook_name)).ok()?;
        if !hook_file.starts_with(fs::canonicalize(&root).ok()?) {
            return None;
        }
        let document = read_json_file(&hook_file)?;
        let hooks = document.get("hooks")?;
        if !matches!(hooks, Json::Object(_)) {
            return None;
        }
        registered_hook(source, hooks, &root).ok()
    })();
    if hooked == Some(true) {
        "hook"
    } else {
        "cli"
    }
}

/// The request body for `event`, or `None` if a required field is missing.
pub(super) fn build_payload(ctx: &Ctx<'_>, event: &Json, install_id: Option<&str>) -> Option<Json> {
    let version = plugin_version(ctx)?;
    let install_id = install_id.filter(|id| !id.is_empty())?;
    // The service drops timestamps more than five minutes ahead, silently and
    // with a 200, so the clock is read once, in milliseconds.
    #[allow(clippy::cast_possible_truncation)]
    let occurred_at = ((ctx.now)() * 1000.0).floor() as i64;
    Some(Json::object(vec![
        ("installId", Json::string(install_id)),
        ("source", Json::string(detect_source(ctx))),
        ("pluginVersion", Json::string(version)),
        ("occurredAt", Json::Int(occurred_at)),
        ("pluginTelemetryEvent", event.clone()),
    ]))
}

/// The consent gate and the body, in one step: `None` unless consent is
/// currently `ACCEPTED` and every required field is known. Nothing may be
/// sent without going through here.
pub(super) fn prepare(ctx: &Ctx<'_>, event: &Json, install_id: Option<&str>) -> Option<String> {
    if !consent::is_accepted(ctx) {
        return None;
    }
    let payload = build_payload(ctx, event, install_id)?;
    Some(pyjson::dumps(&payload, None, false))
}

/// POST one event and report what happened; never fails.
pub(super) fn post_event_status(
    ctx: &Ctx<'_>,
    event: &Json,
    install_id: Option<&str>,
    url: &str,
    timeout: Duration,
) -> PostStatus {
    match prepare(ctx, event, install_id) {
        Some(body) => post(ctx, url, &body, timeout),
        None => PostStatus::Unknown,
    }
}

/// POST one event. True only if the service accepted it.
pub(super) fn post_event(ctx: &Ctx<'_>, event: &Json, install_id: Option<&str>, url: &str) -> bool {
    matches!(
        post_event_status(ctx, event, install_id, url, TIMEOUT),
        PostStatus::Status(200..=299)
    )
}

/// POST one event, reading the install ID and endpoint for the caller. An
/// opted-out record is valid without an `installId`, and [`prepare`] re-checks
/// consent anyway, so `None` simply means nothing is sent.
pub(super) fn send_event(ctx: &Ctx<'_>, event: &Json) -> bool {
    let record = consent::read_state(ctx);
    let install_id = record
        .as_ref()
        .and_then(|record| record.install_id.as_deref());
    post_event(ctx, event, install_id, &endpoint(ctx))
}

/// Report that `skill_id` was invoked. A skill id that is not a
/// `PluginSkillId` member is dropped rather than sent.
pub(super) fn emit_skill_invocation_metric(
    ctx: &Ctx<'_>,
    install_id: &str,
    skill_id: &str,
    url: &str,
) -> bool {
    if !PLUGIN_SKILL_IDS.contains(&skill_id) {
        return false;
    }
    let event = Json::object(vec![(
        "skillInvoked",
        Json::object(vec![
            ("skill", Json::string(skill_id)),
            ("eventName", Json::string(EVENT_SKILL_INVOKED)),
        ]),
    )]);
    post_event(ctx, &event, Some(install_id), url)
}

/// Report that consent was recorded. Only ever sent on acknowledgement:
/// `ConsentRecordedDetails` is empty and cannot say which way the user
/// answered, so a request from the opt-out would be indistinguishable from a
/// yes.
pub(super) fn send_consent_recorded(ctx: &Ctx<'_>) -> bool {
    let event = Json::object(vec![("consentRecorded", Json::Object(Vec::new()))]);
    send_event(ctx, &event)
}

/// `toolkit telemetry status`: read-only; sends nothing, creates no consent.
pub(super) fn status(ctx: &Ctx<'_>, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let consent = match consent::consent_status(ctx) {
        Some(Status::Accepted) => Json::string(consent::ACCEPTED),
        Some(Status::OptOut) => Json::string(consent::OPT_OUT),
        None => Json::Null,
    };
    let version = plugin_version(ctx).map_or(Json::Null, Json::String);
    let report = Json::object(vec![
        ("source", Json::string(detect_source(ctx))),
        ("reportingMode", Json::string(migration_reporting_mode(ctx))),
        ("consentStatus", consent),
        ("pluginVersion", version),
    ]);
    output.line(&pyjson::dumps(&report, None, false))
}

/// Hands one prepared body to the sink with the fixed headers.
pub(super) fn post(ctx: &Ctx<'_>, url: &str, body: &str, timeout: Duration) -> PostStatus {
    ctx.sink.post(&PostRequest {
        url,
        body,
        content_type: "application/json",
        user_agent: "aws-startup-advisor-plugin",
        timeout,
    })
}
