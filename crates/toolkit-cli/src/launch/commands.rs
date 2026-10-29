//! Offline Launch commands.
use super::{
    config::{self, RegionInputs},
    state,
};
use crate::{Invocation, Output};
use toolkit_aws::{Env, ProcessEnv, SystemResolver};
use toolkit_core::ToolkitError;

const GENERIC_FAILURE: &str = "The operation failed. Re-run with logging enabled for details.";

// recorded-shape: recursively scan every key and string in the constructed output for known secrets; this does not interpret optional response members.
fn ensure_no_secrets(value: &serde_json::Value, secrets: &[&str]) -> Result<(), &'static str> {
    // recorded-shape: this recursively scans every output key and value for a known secret without interpreting response fields.
    fn contains(value: &serde_json::Value, secret: &str) -> bool {
        match value {
            serde_json::Value::String(text) => text.contains(secret),
            serde_json::Value::Array(items) => items.iter().any(|v| contains(v, secret)),
            serde_json::Value::Object(map) => map
                .iter()
                .any(|(k, v)| k.contains(secret) || contains(v, secret)),
            _ => false,
        }
    }
    if secrets.iter().any(|s| s.len() >= 8 && contains(value, s)) {
        Err(GENERIC_FAILURE)
    } else {
        Ok(())
    }
}

// recorded-shape: the launch envelope and all nested response types have already been validated against the bundled service model.
fn launch_output(
    mut raw: serde_json::Value,
    region: &str,
) -> Result<serde_json::Value, super::api::ApiError> {
    let name = config::region_name(region).ok_or(super::api::ApiError::InvalidResponse)?;
    let mut launch = if let Some(launch) = raw.get_mut("launch") {
        std::mem::take(launch)
    } else {
        raw
    };
    let object = launch
        .as_object_mut()
        .ok_or(super::api::ApiError::InvalidResponse)?;
    object.remove("contextInputs");
    object.remove("uploadUrl");
    object.insert("region".into(), region.into());
    object.insert("regionName".into(), name.into());
    Ok(launch)
}

// recorded-shape: the launch and execution objects have been validated by the model; this projects them into a newly constructed status object.
fn status_output(
    raw: &serde_json::Value,
    region: &str,
) -> Result<serde_json::Value, super::api::ApiError> {
    let launch = raw
        .get("launch")
        .unwrap_or(raw)
        .as_object()
        .ok_or(super::api::ApiError::InvalidResponse)?;
    let name = config::region_name(region).ok_or(super::api::ApiError::InvalidResponse)?;
    let status = launch.get("status").cloned().unwrap_or_default();
    let mut result = serde_json::json!({"id":launch.get("id").cloned().unwrap_or_default(),"region":region,"regionName":name,"status":status,"isComplete":status == "completed","isFailed":status == "failed"});
    let object = result.as_object_mut().expect("object");
    if let Some(execution) = launch
        .get("execution")
        .and_then(serde_json::Value::as_object)
        .filter(|v| !v.is_empty())
    {
        for key in ["completedTasks", "totalTasks", "currentPhase"] {
            object.insert(key.into(), execution.get(key).cloned().unwrap_or_default());
        }
    }
    for key in ["costEstimate", "failureReason"] {
        if let Some(value) = launch
            .get(key)
            .filter(|v| !v.is_null() && *v != "" && *v != false)
        {
            object.insert(key.into(), value.clone());
        }
    }
    Ok(result)
}

fn execute_read_command(
    client: &super::api::Client<'_>,
    command: &str,
    id: &str,
    include: Option<&str>,
    client_token: &str,
    override_base: bool,
) -> Result<serde_json::Value, String> {
    use super::api::{command_error, ApiOperation};
    let call = |op, include: &[&str], body| {
        client
            .call(op, id, include, body)
            .map_err(|e| command_error(&e, client.region, override_base))
    };
    match command {
        "get-launch" => {
            let include: Vec<_> = include
                .map(|s| s.split(',').map(str::trim).collect())
                .unwrap_or_default();
            launch_output(
                call(ApiOperation::GetLaunch, &include, None)?,
                client.region,
            )
            .map_err(|e| e.to_string())
        }
        "delete-launch" => call(ApiOperation::Delete, &[], None),
        "start-launch-execution" => {
            let result = launch_output(
                call(
                    ApiOperation::StartExecution,
                    &[],
                    Some(serde_json::json!({"clientToken":client_token})),
                )?,
                client.region,
            )
            .map_err(|e| e.to_string())?;
            ensure_no_secrets(&result, &[client_token]).map_err(str::to_owned)?;
            Ok(result)
        }
        "get-launch-status" => status_output(
            &call(
                ApiOperation::GetLaunch,
                &["execution", "cost_estimate"],
                None,
            )?,
            client.region,
        )
        .map_err(|e| e.to_string()),
        "get-launch-download-url" => {
            let url = client
                .download_url(id, override_base)
                .map_err(|e| command_error(&e, client.region, override_base))?
                .ok_or(
                    "Download URL not available yet. Ensure the launch execution has completed.",
                )?;
            Ok(serde_json::json!({"downloadUrl":url}))
        }
        _ => Err(GENERIC_FAILURE.into()),
    }
}

// recorded-shape: the model requires items to be a list of objects; this validates it again before adding a region to each item.
fn list_output(
    regions: &[&str],
    override_base: bool,
    mut call: impl FnMut(&str) -> Result<serde_json::Value, super::api::ApiError>,
) -> Result<serde_json::Value, String> {
    let mut items = Vec::new();
    let mut errors = serde_json::Map::new();
    let mut tokens = serde_json::Map::new();
    for region in regions {
        match call(region) {
            Ok(value) => {
                let list = value
                    .get("items")
                    .and_then(serde_json::Value::as_array)
                    .ok_or("The Launch with AWS service returned an invalid response.")?;
                for item in list {
                    let mut item = item.clone();
                    let object = item
                        .as_object_mut()
                        .ok_or("The Launch with AWS service returned an invalid response.")?;
                    object.insert("region".into(), (*region).into());
                    items.push(item);
                }
                if let Some(t) = value
                    .get("nextToken")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    tokens.insert((*region).into(), t.into());
                }
            }
            Err(error) => {
                if errors.len() == regions.len() - 1 {
                    if matches!(error, super::api::ApiError::Unreachable) {
                        return Err(format!(
                            "Could not reach Launch with AWS in {}.",
                            regions.join(", ")
                        ));
                    }
                    return Err(super::api::command_error(&error, region, override_base));
                }
                errors.insert(
                    (*region).into(),
                    super::api::command_error(&error, region, override_base).into(),
                );
            }
        }
    }
    let mut result = serde_json::json!({"items":items});
    let object = result.as_object_mut().expect("object");
    if !tokens.is_empty() {
        object.insert("nextTokens".into(), tokens.into());
    }
    if !errors.is_empty() {
        object.insert("errors".into(), errors.into());
    }
    Ok(result)
}

#[cfg(unix)]
fn create_with_archive(
    client: &super::api::Client<'_>,
    name: &str,
    archive: &[u8],
    resolver: &dyn toolkit_aws::endpoint::HostResolver,
    mut make_token: impl FnMut() -> Result<String, String>,
    override_base: bool,
) -> Result<serde_json::Value, String> {
    use super::api::{command_error, ApiOperation};
    let call = |operation, body| {
        client
            .call(operation, "", &[], Some(body))
            .map_err(|e| command_error(&e, client.region, override_base))
    };
    let upload_token = make_token()?;
    let upload = call(
        ApiOperation::CreateUploadUrl,
        serde_json::json!({"clientToken":upload_token}),
    )?;
    let url = upload
        .get("uploadUrl")
        .and_then(serde_json::Value::as_str)
        .ok_or("The Launch with AWS service returned an invalid response.")?;
    let upload_id = upload
        .get("uploadId")
        .and_then(serde_json::Value::as_str)
        .ok_or("The Launch with AWS service returned an invalid response.")?;
    client
        .put_archive(url, archive, resolver)
        .map_err(|e| command_error(&e, client.region, override_base))?;
    let create_token = make_token()?;
    let result = call(
        ApiOperation::CreateLaunch,
        serde_json::json!({"name":name,"source":{"s3Upload":{"uploadId":upload_id}},"clientToken":create_token}),
    )?;
    let result = launch_output(result, client.region).map_err(|e| e.to_string())?;
    let mut upload_secrets = vec![url, upload_token.as_str(), create_token.as_str()];
    if let Some((_, query)) = url.split_once('?') {
        upload_secrets.extend(
            query
                .split('&')
                .filter_map(|part| part.split_once('=').map(|(_, value)| value)),
        );
    }
    ensure_no_secrets(&result, &upload_secrets).map_err(str::to_owned)?;
    Ok(result)
}

fn emit(output: &mut Output<'_>, value: &serde_json::Value) -> Result<(), ToolkitError> {
    output.line(&serde_json::to_string_pretty(value).expect("JSON value serializes"))
}

fn launch_name<'a>(source: &'a str, name: Option<&'a str>) -> &'a str {
    name.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::path::Path::new(source.trim_end_matches('/'))
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("uploaded-app")
        })
}
#[allow(clippy::unnecessary_wraps)]
fn failure(output: &mut Output<'_>, message: &str) -> Result<(), ToolkitError> {
    output.diagnostic(&serde_json::json!({"error": message}).to_string());
    output.set_exit_status(1);
    Ok(())
}
pub(crate) fn resolve_region(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let env = ProcessEnv;
    let dir = match state::state_dir(&env) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    let home = env.var("HOME").map(std::path::PathBuf::from);
    let region = match config::resolve_region(&RegionInputs {
        explicit: invocation.value("--region"),
        aws_mcp_url: invocation.value("--aws-mcp-url"),
        env: &env,
        state_dir: Some(&dir),
        home: home.as_deref(),
    }) {
        Ok(value) => value,
        Err(error) => return failure(output, error.message()),
    };
    let base = match env.var(config::ENV_BASE_URL).filter(|s| !s.is_empty()) {
        Some(value) => config::validate_base_url(&value, &SystemResolver),
        None => config::base_url_for_region(region.region),
    };
    let base = match base {
        Ok(value) => value,
        Err(error) => return failure(output, error.message()),
    };
    let other: Vec<_> = config::SUPPORTED_REGIONS
        .iter()
        .filter(|(name, _)| *name != region.region)
        .map(|(name, display)| serde_json::json!({"region":name,"regionName":display}))
        .collect();
    emit(
        output,
        &serde_json::json!({"region":region.region,"regionName":region.region_name(),"source":region.source.as_str(),"sourceLabel":region.source.label(),"baseUrl":base,"otherRegions":other}),
    )
}
pub(crate) fn session_status(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let dir = match state::state_dir(&ProcessEnv) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    let Some(session) = state::load_session(&dir) else {
        return emit(output, &serde_json::json!({"authenticated":false}));
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return failure(output, "The system clock is unavailable.");
    };
    let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
    emit(
        output,
        &serde_json::json!({"authenticated":session.token_expires_at > now.saturating_add(300),"tokenExpiresInSecs":session.token_expires_at.saturating_sub(now).max(0),"sessionExpiresInSecs":session.client_expires_at.saturating_sub(now).max(0),"canRefresh":session.client_expires_at > now}),
    )
}
pub(crate) fn sign_out(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let dir = match state::state_dir(&ProcessEnv) {
        Ok(dir) => dir,
        Err(error) => return failure(output, error.message()),
    };
    match state::remove_state_file(&dir, state::SESSION_FILE_NAME) {
        Ok(removed) => emit(
            output,
            &serde_json::json!({"signedOut":true,"sessionRemoved":removed}),
        ),
        Err(_) => failure(output, "Could not remove the session file."),
    }
}

#[cfg(test)]
mod tests_l5 {
    use super::*;
    use serde_json::json;

    #[test]
    fn t_cmd_launch_output_removes_context_and_keeps_download_url() {
        let raw = json!({"launch":{"id":"l-1","contextInputs":{"hidden":"x"},"downloadUrl":"https://example.test/download?signature=fake"}});
        let result = launch_output(raw, "us-east-1").unwrap();
        assert!(result.get("contextInputs").is_none());
        assert!(
            result.get("downloadUrl").and_then(|v| v.as_str())
                == Some("https://example.test/download?signature=fake")
        );
        assert_eq!(result["region"], "us-east-1");
        assert_eq!(result["regionName"], "US East (N. Virginia)");
    }

    #[test]
    fn t_cmd_status_projects_progress() {
        let raw = json!({"launch":{"id":"l-1","status":"completed","execution":{"completedTasks":2,"totalTasks":3,"currentPhase":"deploy"},"costEstimate":{"estimatedMonthlyCost":12.0}}});
        let result = status_output(&raw, "eu-central-1").unwrap();
        assert_eq!(result["id"], "l-1");
        assert_eq!(result["regionName"], "Europe (Frankfurt)");
        assert_eq!(result["isComplete"], true);
        assert_eq!(result["isFailed"], false);
        assert_eq!(result["completedTasks"], 2);
        assert_eq!(result["totalTasks"], 3);
        assert_eq!(result["currentPhase"], "deploy");
        assert_eq!(result["costEstimate"]["estimatedMonthlyCost"], 12.0);
    }

    #[test]
    fn t_tok_output_rejects_seeded_token_even_in_nested_response() {
        let token = "FAKE-ACCESS-TOKEN-DO-NOT-PRINT";
        let value = json!({"launch":{"name":format!("prefix{token}suffix")}});
        assert!(ensure_no_secrets(&value, &[token]).is_err());
        assert!(ensure_no_secrets(
            &json!({"downloadUrl":"https://example.test/?sig=fake"}),
            &[token]
        )
        .is_ok());
    }
}

/// Dispatch the Launch API commands after checking their required arguments.
pub(crate) fn network_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
    command: &str,
) -> Result<(), ToolkitError> {
    let id = invocation.positional(0);
    if command != "list-launches" && id.is_none() {
        return failure(output, &format!("Missing required argument for {command}"));
    }
    if command == "create-launch" {
        #[cfg(not(unix))]
        return failure(output, super::PLATFORM_MESSAGE);
        #[cfg(unix)]
        {
            let source = id.unwrap_or_default();
            if source.starts_with("http://") || source.starts_with("https://") {
                return failure(output, &format!("create-launch takes a local directory, not a repository URL. Clone {} locally and pass the clone's directory.", super::text::shown(source)));
            }
            if !(1..=128).contains(
                &launch_name(source, invocation.positional(1))
                    .chars()
                    .count(),
            ) {
                return failure(
                    output,
                    "Invalid launch name: it must be 1 to 128 characters.",
                );
            }
        }
    } else if let Some(id) = id {
        if let Err(error) = super::api::validate_launch_id(id) {
            return failure(output, &error.to_string());
        }
    }
    #[cfg(feature = "aws-sdk")]
    {
        match run_network(invocation, command) {
            Ok(value) => emit(output, &value),
            Err(message) => failure(output, &message),
        }
    }
    #[cfg(not(feature = "aws-sdk"))]
    {
        failure(output, super::snapshot::NO_NETWORK_MESSAGE)
    }
}

macro_rules! network_route {
    ($name:ident, $command:literal) => {
        pub(crate) fn $name(
            invocation: &Invocation,
            output: &mut Output<'_>,
        ) -> Result<(), ToolkitError> {
            network_command(invocation, output, $command)
        }
    };
}
network_route!(create_launch, "create-launch");
network_route!(get_launch, "get-launch");
network_route!(list_launches, "list-launches");
network_route!(delete_launch, "delete-launch");
network_route!(start_execution, "start-launch-execution");
network_route!(get_status, "get-launch-status");
network_route!(get_download_url, "get-launch-download-url");

#[cfg(feature = "aws-sdk")]
#[allow(clippy::too_many_lines)]
fn run_network(invocation: &Invocation, command: &str) -> Result<serde_json::Value, String> {
    use super::api::{ApiOperation, Client};
    use super::clock::{OsRandom, Random};
    use super::http::SystemClock;
    use super::oidc::Oidc;
    use toolkit_aws::retry::{FullJitter, RetryPolicy, ThreadSleeper};
    use toolkit_aws::Env;
    let env = ProcessEnv;
    let dir = state::state_dir(&env).map_err(|e| e.to_string())?;
    let home = env.var("HOME").map(std::path::PathBuf::from);
    let resolved = config::resolve_region(&RegionInputs {
        explicit: invocation.value("--region"),
        aws_mcp_url: invocation.value("--aws-mcp-url"),
        env: &env,
        state_dir: Some(&dir),
        home: home.as_deref(),
    })
    .map_err(|e| e.to_string())?;
    let override_url = env.var(config::ENV_BASE_URL).filter(|s| !s.is_empty());
    let network = super::snapshot::Network::open().map_err(|e| e.to_string())?;
    let jitter = FullJitter::new();
    let oidc = Oidc {
        transport: network.transport(),
        clock: &SystemClock,
        sleeper: &ThreadSleeper,
        jitter: &jitter,
        policy: RetryPolicy::default(),
    };
    let previous_session = state::load_session(&dir);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or("The system clock is unavailable.")?;
    let token = oidc
        .access_token(
            &dir,
            &OsRandom,
            now,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
        .map_err(|e| e.to_string())?;
    let current_session = state::load_session(&dir);
    let mut secrets = vec![token.expose()];
    for session in [previous_session.as_ref(), current_session.as_ref()]
        .into_iter()
        .flatten()
    {
        secrets.extend([
            session.access_token.expose(),
            session.refresh_token.expose(),
            session.client_secret.expose(),
        ]);
    }
    let base_for = |region: &str| -> Result<String, String> {
        match &override_url {
            Some(url) => config::validate_base_url(url, &SystemResolver),
            None => config::base_url_for_region(region),
        }
        .map_err(|e| e.to_string())
    };
    let make_token = || -> Result<String, String> {
        let mut bytes = [0u8; 16];
        OsRandom.fill(&mut bytes).map_err(|e| e.to_string())?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", bytes[0],bytes[1],bytes[2],bytes[3],bytes[4],bytes[5],bytes[6],bytes[7],bytes[8],bytes[9],bytes[10],bytes[11],bytes[12],bytes[13],bytes[14],bytes[15]))
    };
    let region = resolved.region;
    let base = base_for(region)?;
    let client = Client {
        transport: network.transport(),
        base: &base,
        region,
        token: &token,
        clock: &SystemClock,
        sleeper: &ThreadSleeper,
        jitter: &jitter,
        policy: RetryPolicy::default(),
    };
    let id = invocation.positional(0).unwrap_or("");
    let result = (|| -> Result<serde_json::Value, String> {
        match command {
            "create-launch" => {
                #[cfg(not(unix))]
                {
                    Err(super::PLATFORM_MESSAGE.into())
                }
                #[cfg(unix)]
                {
                    use super::archive::{
                        git::GitRunner,
                        select::{build_archive, BuildOptions, NoHook},
                        skipped::SkippedBudget,
                        ArchiveLimits,
                    };
                    let name = launch_name(id, invocation.positional(1));
                    let cwd = std::env::current_dir()
                        .map_err(|_| "Could not determine the current directory.")?;
                    let temp = std::env::temp_dir();
                    let git = GitRunner::system();
                    let archive = build_archive(
                        id,
                        name,
                        &BuildOptions {
                            git: &git,
                            hook: &NoHook,
                            limits: ArchiveLimits::PRODUCTION,
                            skipped: SkippedBudget::PRODUCTION,
                            home: home.as_deref(),
                            cwd: &cwd,
                            temp_parent: &temp,
                            random: &OsRandom,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                    let result = create_with_archive(
                        &client,
                        name,
                        &archive.bytes,
                        &SystemResolver,
                        make_token,
                        override_url.is_some(),
                    )?;
                    let _ = state::save_region(&dir, region, &OsRandom);
                    Ok(result)
                }
            }
            "get-launch"
            | "delete-launch"
            | "start-launch-execution"
            | "get-launch-status"
            | "get-launch-download-url" => {
                let token = if command == "start-launch-execution" {
                    make_token()?
                } else {
                    String::new()
                };
                execute_read_command(
                    &client,
                    command,
                    id,
                    invocation.positional(1),
                    &token,
                    override_url.is_some(),
                )
            }
            "list-launches" => {
                let regions: Vec<&str> = invocation.value("--region").map_or_else(
                    || config::SUPPORTED_REGIONS.iter().map(|(r, _)| *r).collect(),
                    |r| vec![r],
                );
                let bases: Vec<String> = regions
                    .iter()
                    .map(|r| base_for(r))
                    .collect::<Result<_, _>>()?;
                let mut index = 0;
                list_output(&regions, override_url.is_some(), |r| {
                    let base = &bases[index];
                    index += 1;
                    let regional = Client {
                        transport: network.transport(),
                        base,
                        region: r,
                        token: &token,
                        clock: &SystemClock,
                        sleeper: &ThreadSleeper,
                        jitter: &jitter,
                        policy: RetryPolicy::default(),
                    };
                    regional.call(ApiOperation::ListLaunches, "", &[], None)
                })
            }
            _ => Err("The operation failed. Re-run with logging enabled for details.".into()),
        }
    })();
    match result {
        Ok(value) => {
            ensure_no_secrets(&value, &secrets).map_err(str::to_owned)?;
            Ok(value)
        }
        Err(message) => {
            if secrets.iter().any(|s| s.len() >= 8 && message.contains(s)) {
                Err(GENERIC_FAILURE.into())
            } else {
                Err(message)
            }
        }
    }
}

#[cfg(test)]
mod tests_network_commands {
    use super::*;
    use crate::launch::{api::Client, http::SystemClock};
    use toolkit_aws::{
        retry::{NoJitter, RetryPolicy},
        testing::{FakeChunk, FakeStreamingTransport, RecordingSleeper},
        Secret,
    };

    #[test]
    fn t_cmd_download_url_is_printed_and_missing_url_is_an_error() {
        let launch = r#"{"launch":{"id":"l","name":"n","status":"completed","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z","downloadUrl":"https://bucket.example/download?sig=fake"}}"#;
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[],
            vec![FakeChunk::data(launch.as_bytes())],
        );
        let sleeper = RecordingSleeper::new();
        let token = Secret::new("FAKE-ACCESS-TOKEN");
        let client = Client {
            transport: &fake,
            base: "https://example.test",
            region: "us-east-1",
            token: &token,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let result =
            execute_read_command(&client, "get-launch-download-url", "l", None, "", false).unwrap();
        assert!(
            result
                .get("downloadUrl")
                .and_then(serde_json::Value::as_str)
                == Some("https://bucket.example/download?sig=fake")
        );
        assert!(fake.requests()[0].url.ends_with("?include=download_url"));
        let missing = launch.replace(
            ",\"downloadUrl\":\"https://bucket.example/download?sig=fake\"",
            "",
        );
        let fake = FakeStreamingTransport::new().respond(
            200,
            &[],
            vec![FakeChunk::data(missing.as_bytes())],
        );
        let client = Client {
            transport: &fake,
            ..client
        };
        let error = execute_read_command(&client, "get-launch-download-url", "l", None, "", false)
            .unwrap_err();
        assert_eq!(
            error,
            "Download URL not available yet. Ensure the launch execution has completed."
        );
    }

    #[test]
    fn t_cmd_get_and_start_use_expected_requests_and_shape() {
        let launch = r#"{"launch":{"id":"l","name":"n","status":"created","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z","contextInputs":[]}}"#;
        let fake = FakeStreamingTransport::new()
            .respond(200, &[], vec![FakeChunk::data(launch.as_bytes())])
            .respond(503, &[], vec![FakeChunk::data(b"{}")])
            .respond(202, &[], vec![FakeChunk::data(launch.as_bytes())]);
        let sleeper = RecordingSleeper::new();
        let token = Secret::new("FAKE-ACCESS-TOKEN");
        let client = Client {
            transport: &fake,
            base: "https://example.test",
            region: "us-east-1",
            token: &token,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::default(),
        };
        let output = execute_read_command(
            &client,
            "get-launch",
            "l/1",
            Some("execution, cost_estimate"),
            "",
            false,
        )
        .unwrap();
        assert!(output.get("contextInputs").is_none());
        assert_eq!(output["region"], "us-east-1");
        assert!(fake.requests()[0]
            .url
            .ends_with("/l%2F1?include=execution&include=cost_estimate"));
        execute_read_command(
            &client,
            "start-launch-execution",
            "l/1",
            None,
            "fixture-client-token",
            false,
        )
        .unwrap();
        let requests = fake.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].body == requests[2].body);
    }

    #[test]
    fn t_cmd_delete_and_status_contracts() {
        let launch = r#"{"launch":{"id":"l","name":"n","status":"completed","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z"}}"#;
        let fake = FakeStreamingTransport::new()
            .respond(204, &[], vec![])
            .respond(200, &[], vec![FakeChunk::data(launch.as_bytes())]);
        let sleeper = RecordingSleeper::new();
        let token = Secret::new("FAKE-ACCESS-TOKEN");
        let client = Client {
            transport: &fake,
            base: "https://example.test",
            region: "us-east-1",
            token: &token,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let deleted = execute_read_command(&client, "delete-launch", "l", None, "", false).unwrap();
        assert_eq!(
            deleted,
            serde_json::json!({"deleted":true,"id":"l","region":"us-east-1"})
        );
        let status =
            execute_read_command(&client, "get-launch-status", "l", None, "", false).unwrap();
        assert_eq!(
            status,
            serde_json::json!({"id":"l","region":"us-east-1","regionName":"US East (N. Virginia)","status":"completed","isComplete":true,"isFailed":false})
        );
        assert!(fake.requests()[1]
            .url
            .ends_with("?include=execution&include=cost_estimate"));
    }

    #[test]
    fn t_cmd_list_partial_and_all_unreachable() {
        let regions = ["us-east-1", "eu-central-1"];
        let result = list_output(&regions, false, |region| {
            if region == "us-east-1" {
                Err(super::super::api::ApiError::Unreachable)
            } else {
                Ok(serde_json::json!({"items":[{"id":"l"}],"nextToken":"next"}))
            }
        })
        .unwrap();
        assert_eq!(result["items"][0]["region"], "eu-central-1");
        assert!(result["errors"]["us-east-1"]
            .as_str()
            .unwrap()
            .contains("us-east-1"));
        assert_eq!(result["nextTokens"]["eu-central-1"], "next");
        let error = list_output(&regions, false, |_| {
            Err(super::super::api::ApiError::Unreachable)
        })
        .unwrap_err();
        assert_eq!(
            error,
            "Could not reach Launch with AWS in us-east-1, eu-central-1."
        );
    }

    #[cfg(unix)]
    #[test]
    fn t_cmd_create_uploads_once_and_never_emits_upload_url() {
        let upload = r#"{"uploadUrl":"https://bucket.amazonaws.com/file?sig=FAKE-UPLOAD-SIGNATURE","uploadId":"u"}"#;
        let launch = r#"{"launch":{"id":"l","name":"n","status":"created","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z"}}"#;
        let fake = FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(upload.as_bytes())])
            .respond(200, &[], vec![])
            .respond(201, &[], vec![FakeChunk::data(launch.as_bytes())]);
        let sleeper = RecordingSleeper::new();
        let token = Secret::new("FAKE-ACCESS-TOKEN");
        let client = Client {
            transport: &fake,
            base: "https://example.test",
            region: "us-east-1",
            token: &token,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let resolver = toolkit_aws::testing::FixedResolver::new().answer_any(&["8.8.8.8"]);
        let mut next = ["upload-token", "create-token"].into_iter();
        let value = create_with_archive(
            &client,
            "n",
            b"zip",
            &resolver,
            || Ok(next.next().unwrap().to_owned()),
            false,
        )
        .unwrap();
        assert!(!value.to_string().contains("FAKE-UPLOAD-SIGNATURE"));
        let requests = fake.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].method, toolkit_aws::transport::Method::Put);
        assert!(requests[1].headers.get("authorization").is_none());
        assert_eq!(value["region"], "us-east-1");
    }

    #[cfg(unix)]
    #[test]
    fn t_tok_upload_signature_echo_in_launch_is_rejected() {
        let upload = r#"{"uploadUrl":"https://bucket.amazonaws.com/file?X-Amz-Signature=FAKE-UPLOAD-SIGNATURE","uploadId":"u"}"#;
        let launch = r#"{"launch":{"id":"l","name":"FAKE-UPLOAD-SIGNATURE","status":"created","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z"}}"#;
        let fake = FakeStreamingTransport::new()
            .respond(201, &[], vec![FakeChunk::data(upload.as_bytes())])
            .respond(200, &[], vec![])
            .respond(201, &[], vec![FakeChunk::data(launch.as_bytes())]);
        let sleeper = RecordingSleeper::new();
        let token = Secret::new("FAKE-ACCESS-TOKEN");
        let client = Client {
            transport: &fake,
            base: "https://example.test",
            region: "us-east-1",
            token: &token,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let resolver = toolkit_aws::testing::FixedResolver::new().answer_any(&["8.8.8.8"]);
        let result = create_with_archive(
            &client,
            "n",
            b"zip",
            &resolver,
            || Ok("fixture-token".to_string()),
            false,
        );
        assert!(result.is_err());
        assert!(!result.err().unwrap().contains("FAKE-UPLOAD-SIGNATURE"));
    }

    #[test]
    fn t_tok_client_token_echo_in_launch_is_rejected() {
        let launch = r#"{"launch":{"id":"l","name":"FIXTURE-CLIENT-TOKEN","status":"created","source":{"s3Upload":{"uploadId":"u"}},"createdAt":"2026-06-15T12:34:56Z","updatedAt":"2026-06-15T12:34:56Z"}}"#;
        let fake = FakeStreamingTransport::new().respond(
            202,
            &[],
            vec![FakeChunk::data(launch.as_bytes())],
        );
        let sleeper = RecordingSleeper::new();
        let token = Secret::new("FAKE-ACCESS-TOKEN");
        let client = Client {
            transport: &fake,
            base: "https://example.test",
            region: "us-east-1",
            token: &token,
            clock: &SystemClock,
            sleeper: &sleeper,
            jitter: &NoJitter,
            policy: RetryPolicy::none(),
        };
        let result = execute_read_command(
            &client,
            "start-launch-execution",
            "l",
            None,
            "FIXTURE-CLIENT-TOKEN",
            false,
        );
        assert!(result.is_err());
        assert!(!result.err().unwrap().contains("FIXTURE-CLIENT-TOKEN"));
    }
}
