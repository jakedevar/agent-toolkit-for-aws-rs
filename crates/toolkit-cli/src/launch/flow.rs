//! The Launch API commands (L5, design section 1.3): `create-launch`,
//! `get-launch`, `list-launches`, `delete-launch`, `start-launch-execution`,
//! `get-launch-status` and `get-launch-download-url`.
//!
//! [`run`] is the whole command behind injected dependencies ([`Deps`]), so
//! every branch is tested offline over `FakeStreamingTransport`, a seeded
//! session directory and a fake resolver. Order of work (R-API-HTTP, D9):
//! local argument checks, region and endpoint configuration (including the
//! `LAUNCH_WITH_AWS_BASE_URL` override), then the one access-token fetch, then
//! (for `create-launch`) the archive, then the requests. Nothing that can be
//! refused locally waits for a token refresh or a request.

use std::path::Path;
use std::time::Duration;

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::recorded;
use toolkit_aws::retry::{Jitter, RetryPolicy, Sleeper};
use toolkit_aws::stream::StreamingTransport;
use toolkit_aws::{Env, Secret};
use toolkit_core::Json;

use super::api::{command_error, validate_launch_id, ApiError, ApiOperation, Client};
use super::clock::Random;
use super::config::{self, RegionInputs};
use super::http::MonotonicClock;
use super::oidc::Oidc;
use super::state;

/// The Python catch-all message (`main`, R-API-ERR last row).
pub const GENERIC_FAILURE: &str = "The operation failed. Re-run with logging enabled for details.";
/// R-API-ERR: invalid response, body over its cap.
pub const INVALID_RESPONSE: &str = "The Launch with AWS service returned an invalid response.";
/// `get-launch-download-url` without a URL (Python `_fail`).
pub const NO_DOWNLOAD_URL: &str =
    "Download URL not available yet. Ensure the launch execution has completed.";
/// The operating system random number generator failed.
pub const NO_RANDOM: &str = "The operating system random number generator is unavailable.";

/// The network commands, in the order of section 1.3.
pub const COMMANDS: [&str; 7] = [
    "create-launch",
    "get-launch",
    "list-launches",
    "delete-launch",
    "start-launch-execution",
    "get-launch-status",
    "get-launch-download-url",
];

/// One network command invocation.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    /// The command name (one of [`COMMANDS`]).
    pub command: &'a str,
    /// `<dir>` or `<id>`.
    pub first: Option<&'a str>,
    /// `[name]` or `[include]`.
    pub second: Option<&'a str>,
    /// `--region`.
    pub region: Option<&'a str>,
    /// `--aws-mcp-url`.
    pub aws_mcp_url: Option<&'a str>,
}

/// Builds the `create-launch` archive from `(source, display name)`; the
/// error is the final `ArchiveError` text.
pub type ArchiveBuilder<'a> = &'a dyn Fn(&str, &str) -> Result<Vec<u8>, String>;

/// Everything a network command touches outside its arguments.
pub struct Deps<'a> {
    /// Process environment (`LAUNCH_WITH_AWS_*`, `AWS_REGION`, ...).
    pub env: &'a dyn Env,
    /// The private state directory (session, region file).
    pub state_dir: &'a Path,
    /// `$HOME` for the region chain.
    pub home: Option<&'a Path>,
    /// The only network seam (R-API-BODY).
    pub transport: &'a dyn StreamingTransport,
    /// DNS for the base-URL override and the presigned upload host.
    pub resolver: &'a dyn HostResolver,
    /// OS CSPRNG (clientToken UUIDs, temporary names).
    pub random: &'a dyn Random,
    /// Monotonic clock for deadlines.
    pub clock: &'a dyn MonotonicClock,
    /// Retry back-off sleeper.
    pub sleeper: &'a dyn Sleeper,
    /// Retry jitter.
    pub jitter: &'a dyn Jitter,
    /// Shared retry policy.
    pub policy: RetryPolicy,
    /// Wall-clock seconds since the epoch (token expiry).
    pub now: i64,
    /// The `create-launch` archive builder.
    pub archive: ArchiveBuilder<'a>,
}

/// The argument checks that need no configuration, file or network: missing
/// positionals, the repository-URL refusal, the display name and launch id
/// lengths (R-API-HTTP, D9, #4941 for the echoed source).
///
/// # Errors
/// The final `{"error": ...}` message.
pub fn validate_local(request: &Request<'_>) -> Result<(), String> {
    let command = request.command;
    if command == "list-launches" {
        return Ok(());
    }
    let Some(first) = request.first else {
        return Err(format!("Missing required argument for {command}"));
    };
    if command == "create-launch" {
        if !cfg!(unix) {
            return Err(super::PLATFORM_MESSAGE.into());
        }
        if is_repository_url(first) {
            return Err(format!(
                "create-launch takes a local directory, not a repository URL. Clone {} locally and pass the clone's directory.",
                super::text::shown(first)
            ));
        }
        if !(1..=128).contains(&launch_name(first, request.second).chars().count()) {
            return Err(ApiError::InvalidLaunchName.to_string());
        }
        return Ok(());
    }
    validate_launch_id(first).map_err(|error| error.to_string())
}

/// Python `urlparse(source).scheme in ("http", "https")`: the scheme is
/// case-insensitive and is the text before the first `:` when it starts with
/// a letter and holds only letters, digits, `+`, `-` and `.`.
fn is_repository_url(source: &str) -> bool {
    let Some((scheme, _)) = source.split_once(':') else {
        return false;
    };
    let valid = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid && matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
}

/// `name.strip()` or the basename of the source without trailing `/`, or
/// `uploaded-app` (Python `_default_repo_name`).
#[must_use]
pub fn launch_name<'a>(source: &'a str, name: Option<&'a str>) -> &'a str {
    name.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            source
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("uploaded-app")
        })
}

/// A UUID v4 string from the CSPRNG (botocore's `idempotencyToken`).
///
/// # Errors
/// [`NO_RANDOM`] when the generator fails.
pub fn client_token(random: &dyn Random) -> Result<String, String> {
    let mut bytes = [0u8; 16];
    random.fill(&mut bytes).map_err(|_| NO_RANDOM.to_owned())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::new(), |mut hex, b| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
        hex
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// Run one network command. Every rendered result and error passes the
/// known-secret scrub (R-API-CODE second layer) before it is returned.
///
/// # Errors
/// The final `{"error": ...}` message.
pub fn run(request: &Request<'_>, deps: &Deps<'_>) -> Result<Json, String> {
    validate_local(request)?;
    let resolved = config::resolve_region(&RegionInputs {
        explicit: request.region,
        aws_mcp_url: request.aws_mcp_url,
        env: deps.env,
        state_dir: Some(deps.state_dir),
        home: deps.home,
    })
    .map_err(|e| e.to_string())?;
    let region = resolved.region;
    let override_url = deps.env.var(config::ENV_BASE_URL).filter(|s| !s.is_empty());
    let override_base = override_url.is_some();
    // The override is validated (with its DNS check) before the token: an
    // unsafe endpoint never costs a refresh (D9).
    let base = match &override_url {
        Some(url) => config::validate_base_url(url, deps.resolver),
        None => config::base_url_for_region(region),
    }
    .map_err(|e| e.to_string())?;
    let list_regions: Vec<&str> = match request.region.filter(|r| !r.is_empty()) {
        Some(_) => vec![region],
        None => config::SUPPORTED_REGIONS.iter().map(|(r, _)| *r).collect(),
    };
    let mut list_bases = Vec::new();
    if request.command == "list-launches" {
        for r in &list_regions {
            list_bases.push(if override_base {
                base.clone()
            } else {
                config::base_url_for_region(r).map_err(|e| e.to_string())?
            });
        }
    }
    let mut secrets = Secrets::default();
    secrets.add_session(state::load_session(deps.state_dir).as_ref());
    let oidc = Oidc {
        transport: deps.transport,
        clock: deps.clock,
        sleeper: deps.sleeper,
        jitter: deps.jitter,
        policy: deps.policy,
    };
    let token = oidc.access_token(
        deps.state_dir,
        deps.random,
        deps.now,
        deps.clock.now() + Duration::from_secs(30),
    );
    secrets.add_session(state::load_session(deps.state_dir).as_ref());
    let token = token.map_err(|e| secrets.scrub(e.to_string()))?;
    secrets.add(token.expose());
    let client = Client {
        transport: deps.transport,
        base: &base,
        region,
        token: &token,
        clock: deps.clock,
        sleeper: deps.sleeper,
        jitter: deps.jitter,
        policy: deps.policy,
    };
    let result = dispatch(
        request,
        deps,
        &client,
        &list_regions,
        &list_bases,
        &mut secrets,
    );
    match result {
        Ok(value) if secrets.found_in(&value) => Err(GENERIC_FAILURE.into()),
        Ok(value) => Ok(value),
        Err(message) => Err(secrets.scrub(message)),
    }
}

fn dispatch(
    request: &Request<'_>,
    deps: &Deps<'_>,
    client: &Client<'_>,
    list_regions: &[&str],
    list_bases: &[String],
    secrets: &mut Secrets,
) -> Result<Json, String> {
    let override_base = deps
        .env
        .var(config::ENV_BASE_URL)
        .is_some_and(|s| !s.is_empty());
    let id = request.first.unwrap_or_default();
    let fail = |error: &ApiError| command_error(error, client.region, override_base);
    match request.command {
        "create-launch" => {
            let name = launch_name(id, request.second);
            let archive = (deps.archive)(id, name)?;
            let result = create_with_archive(client, name, &archive, deps, override_base, secrets)?;
            // Best effort, as the Python (`OSError` swallowed).
            let _ = state::save_region(deps.state_dir, client.region, deps.random);
            Ok(result)
        }
        "get-launch" => {
            let include: Vec<&str> = request
                .second
                .filter(|s| !s.is_empty())
                .map(|s| s.split(',').map(str::trim).collect())
                .unwrap_or_default();
            let raw = client
                .call(ApiOperation::GetLaunch, id, &include, None)
                .map_err(|e| fail(&e))?;
            launch_output(raw, client.region)
        }
        "delete-launch" => client
            .call(ApiOperation::Delete, id, &[], None)
            .map_err(|e| fail(&e)),
        "start-launch-execution" => {
            let token = client_token(deps.random)?;
            secrets.add(&token);
            let body = Json::object(vec![("clientToken", Json::string(token))]);
            let raw = client
                .call(ApiOperation::StartExecution, id, &[], Some(body))
                .map_err(|e| fail(&e))?;
            launch_output(raw, client.region)
        }
        "get-launch-status" => {
            let raw = client
                .call(
                    ApiOperation::GetLaunch,
                    id,
                    &["execution", "cost_estimate"],
                    None,
                )
                .map_err(|e| fail(&e))?;
            status_output(&raw, client.region)
        }
        "get-launch-download-url" => {
            let url = client
                .download_url(id, override_base)
                .map_err(|e| match e {
                    ApiError::Lookup { .. } => e.to_string(),
                    other => fail(&other),
                })?
                .ok_or(NO_DOWNLOAD_URL)?;
            Ok(Json::object(vec![("downloadUrl", Json::string(url))]))
        }
        "list-launches" => list_output(list_regions, override_base, |index, region| {
            Client {
                transport: client.transport,
                base: &list_bases[index],
                region,
                token: client.token,
                clock: client.clock,
                sleeper: client.sleeper,
                jitter: client.jitter,
                policy: client.policy,
            }
            .call(ApiOperation::ListLaunches, "", &[], None)
        }),
        _ => Err(GENERIC_FAILURE.into()),
    }
}

fn invalid() -> String {
    INVALID_RESPONSE.to_owned()
}

/// Set `key` keeping an existing member's position, else append (Python
/// `dict.__setitem__`).
fn set_member(members: &mut Vec<(String, Json)>, key: &str, value: Json) {
    match members.iter_mut().find(|(k, _)| k == key) {
        Some((_, slot)) => *slot = value,
        None => members.push((key.to_owned(), value)),
    }
}

// recorded-shape: the launch response was validated against the bundled service model, which requires the launch member to be an object; any other shape is rejected as an invalid response.
fn launch_output(raw: Json, region: &str) -> Result<Json, String> {
    let name = config::region_name(region).ok_or_else(invalid)?;
    let launch = match recorded::optional_object(&raw, "launch").map_err(|_| invalid())? {
        Some(launch) => launch.clone(),
        None => raw,
    };
    let Json::Object(mut members) = launch else {
        return Err(invalid());
    };
    members.retain(|(k, _)| k != "contextInputs" && k != "uploadUrl");
    set_member(&mut members, "region", Json::string(region));
    set_member(&mut members, "regionName", Json::string(name));
    Ok(Json::Object(members))
}

/// Python truthiness of a shaped member.
// recorded-shape: Python truthiness of a member the bundled model already validated; every JSON kind maps to a boolean and no nested member is read.
fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) => false,
        Some(Json::Bool(flag)) => *flag,
        Some(Json::String(s)) => !s.is_empty(),
        Some(Json::Int(n)) => *n != 0,
        Some(Json::Float(n)) => *n != 0.0,
        Some(Json::Array(items)) => !items.is_empty(),
        Some(Json::Object(members)) => !members.is_empty(),
    }
}

fn status_output(raw: &Json, region: &str) -> Result<Json, String> {
    let name = config::region_name(region).ok_or_else(invalid)?;
    let launch = recorded::optional_object(raw, "launch")
        .map_err(|_| invalid())?
        .unwrap_or(raw);
    let member = |key: &str| launch.get(key).cloned().unwrap_or(Json::Null);
    let status = member("status");
    let mut out = vec![
        ("id".to_owned(), member("id")),
        ("region".to_owned(), Json::string(region)),
        ("regionName".to_owned(), Json::string(name)),
        ("status".to_owned(), status.clone()),
        (
            "isComplete".to_owned(),
            Json::Bool(status.as_str() == Some("completed")),
        ),
        (
            "isFailed".to_owned(),
            Json::Bool(status.as_str() == Some("failed")),
        ),
    ];
    if truthy(launch.get("execution")) {
        let execution = recorded::optional_object(launch, "execution")
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        for key in ["completedTasks", "totalTasks", "currentPhase"] {
            out.push((
                key.to_owned(),
                execution.get(key).cloned().unwrap_or(Json::Null),
            ));
        }
    }
    for key in ["costEstimate", "failureReason"] {
        if truthy(launch.get(key)) {
            out.push((key.to_owned(), member(key)));
        }
    }
    Ok(Json::Object(out))
}

/// `list-launches` (section 1.3): one failing region is recorded under
/// `errors[region]` (Python `str(err)`, so no hint) unless every region
/// failed; then a connect failure names every region and a service error is
/// the top-level error with its hint. An invalid response is not a
/// per-region failure (botocore's `ResponseParserError` is not caught by the
/// loop) and ends the command.
fn list_output(
    regions: &[&str],
    override_base: bool,
    mut call: impl FnMut(usize, &str) -> Result<Json, ApiError>,
) -> Result<Json, String> {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut tokens = Vec::new();
    for (index, region) in regions.iter().enumerate() {
        match call(index, region) {
            Ok(value) => {
                for item in
                    recorded::optional_object_list(&value, "items").map_err(|_| invalid())?
                {
                    items.push(with_region(item, region)?);
                }
                if let Some(token) = recorded::optional_text(&value, "nextToken")
                    .map_err(|_| invalid())?
                    .filter(|s| !s.is_empty())
                {
                    tokens.push(((*region).to_owned(), Json::string(token)));
                }
            }
            Err(error @ (ApiError::Unreachable | ApiError::Status { .. })) => {
                if errors.len() == regions.len() - 1 {
                    if error == ApiError::Unreachable {
                        return Err(format!(
                            "Could not reach Launch with AWS in {}.",
                            regions.join(", ")
                        ));
                    }
                    return Err(command_error(&error, region, override_base));
                }
                let text = if error == ApiError::Unreachable {
                    command_error(&error, region, override_base)
                } else {
                    error.to_string()
                };
                errors.push(((*region).to_owned(), Json::String(text)));
            }
            Err(other) => return Err(command_error(&other, region, override_base)),
        }
    }
    let mut out = vec![("items".to_owned(), Json::Array(items))];
    if !tokens.is_empty() {
        out.push(("nextTokens".to_owned(), Json::Object(tokens)));
    }
    if !errors.is_empty() {
        out.push(("errors".to_owned(), Json::Object(errors)));
    }
    Ok(Json::Object(out))
}

// recorded-shape: each list item was validated as a LaunchSummary structure by the bundled service model; a non-object item is rejected as an invalid response.
fn with_region(item: &Json, region: &str) -> Result<Json, String> {
    let Json::Object(members) = item else {
        return Err(invalid());
    };
    let mut members = members.clone();
    set_member(&mut members, "region", Json::string(region));
    Ok(Json::Object(members))
}

/// `CreateUploadUrl`, one presigned `PUT`, `CreateLaunch`. The upload URL,
/// each of its query values and both clientTokens are registered as secrets
/// first, so neither the output nor an error can carry them.
fn create_with_archive(
    client: &Client<'_>,
    name: &str,
    archive: &[u8],
    deps: &Deps<'_>,
    override_base: bool,
    secrets: &mut Secrets,
) -> Result<Json, String> {
    let fail = |error: &ApiError| command_error(error, client.region, override_base);
    let upload_token = client_token(deps.random)?;
    secrets.add(&upload_token);
    let upload = client
        .call(
            ApiOperation::CreateUploadUrl,
            "",
            &[],
            Some(Json::object(vec![(
                "clientToken",
                Json::string(upload_token),
            )])),
        )
        .map_err(|e| fail(&e))?;
    let url = recorded::optional_text(&upload, "uploadUrl")
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    secrets.add_url(url);
    let upload_id = recorded::optional_text(&upload, "uploadId")
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    #[cfg(unix)]
    client
        .put_archive(url, archive, deps.resolver)
        .map_err(|e| fail(&e))?;
    #[cfg(not(unix))]
    let _ = archive;
    let create_token = client_token(deps.random)?;
    secrets.add(&create_token);
    let body = Json::object(vec![
        ("name", Json::string(name)),
        (
            "source",
            Json::object(vec![(
                "s3Upload",
                Json::object(vec![("uploadId", Json::string(upload_id))]),
            )]),
        ),
        ("clientToken", Json::string(create_token)),
    ]);
    let raw = client
        .call(ApiOperation::CreateLaunch, "", &[], Some(body))
        .map_err(|e| fail(&e))?;
    launch_output(raw, client.region)
}

/// The live secret values of one command (R-API-CODE second layer): the
/// access token, the session's refresh token and client secret (before and
/// after a refresh), each clientToken and the presigned upload URL with each
/// of its query values (raw and percent-decoded). Values shorter than 8
/// bytes are ignored.
#[derive(Default)]
pub struct Secrets(Vec<String>);

impl Secrets {
    /// Register one value.
    pub fn add(&mut self, value: &str) {
        if value.len() >= 8 && !self.0.iter().any(|v| v == value) {
            self.0.push(value.to_owned());
        }
    }

    fn add_secret(&mut self, value: &Secret) {
        self.add(value.expose());
    }

    /// Register a stored session's secrets.
    pub fn add_session(&mut self, session: Option<&state::StoredSession>) {
        if let Some(session) = session {
            self.add_secret(&session.access_token);
            self.add_secret(&session.refresh_token);
            self.add_secret(&session.client_secret);
        }
    }

    /// Register a presigned URL, its query string and each query value, raw
    /// and percent-decoded.
    pub fn add_url(&mut self, url: &str) {
        self.add(url);
        if let Some((_, query)) = url.split_once('?') {
            let query = query.split('#').next().unwrap_or_default();
            self.add(query);
            for part in query.split('&') {
                if let Some((_, value)) = part.split_once('=') {
                    self.add(value);
                    if let Some(decoded) = percent_decode(value) {
                        self.add(&decoded);
                    }
                }
            }
        }
    }

    /// Whether `text` contains any registered secret.
    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        self.0.iter().any(|secret| text.contains(secret.as_str()))
    }

    /// `message`, or the generic message when it carries a secret.
    #[must_use]
    pub fn scrub(&self, message: String) -> String {
        if self.contains(&message) {
            GENERIC_FAILURE.to_owned()
        } else {
            message
        }
    }

    /// Whether any key or string anywhere in `value` contains a secret.
    #[must_use]
    // recorded-shape: a recursive scan of every key and string the command built for output; it interprets no response member and accepts every shape.
    pub fn found_in(&self, value: &Json) -> bool {
        match value {
            Json::String(text) => self.contains(text),
            Json::Array(items) => items.iter().any(|v| self.found_in(v)),
            Json::Object(members) => members
                .iter()
                .any(|(k, v)| self.contains(k) || self.found_in(v)),
            _ => false,
        }
    }
}

/// `%XX` decoding (and `+` as space) of a query value; `None` when nothing
/// changes or the result is not UTF-8.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    (decoded != value).then_some(decoded)
}

#[cfg(test)]
#[path = "flow/tests_flow.rs"]
mod tests_flow;

#[cfg(test)]
#[path = "flow/tests_tok.rs"]
mod tests_tok;
