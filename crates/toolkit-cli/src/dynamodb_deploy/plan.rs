//! The benchmark config and the deployment plan built from it.
//!
//! The plan lists, as the exact API requests, every resource a live
//! `deploy_model.py` run would create: the `CreateTable` requests, the IAM role
//! with its trust and inline policies, and the Lambda function. It is a
//! document for review; this build never applies it.

use toolkit_core::{quote_untrusted, Json, ToolkitError};

use super::design::{build_create_table, provisioned, tables};
use super::identity::Identity;
use super::model::{die, list, object, py_int, text};
use super::names::{check_plain, check_prefix, check_region, REQUIRED_PREFIX};
use super::policy::{
    document_text, lambda_role_policy, lambda_trust_policy, DEFAULT_LAMBDA_MEMORY_MB,
    DEFAULT_LAMBDA_TIMEOUT_S, LAMBDA_HANDLER, LAMBDA_RUNTIME, VECTOR_SDK_MIN,
};
use crate::dynamodb::pyfmt;

/// One clock and one id for the whole run, so the plan's `created_at`, the
/// generated prefix and the tags agree.
#[derive(Debug, Clone)]
pub(crate) struct RunClock {
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub(crate) created_at: String,
    /// 32 lowercase hex digits.
    pub(crate) run_id: String,
    /// 8 lowercase hex digits for a generated prefix.
    pub(crate) prefix_token: String,
}

impl RunClock {
    /// `YYYYMMDD` of the run.
    pub(crate) fn today(&self) -> String {
        self.created_at
            .chars()
            .take(10)
            .filter(|c| *c != '-')
            .collect()
    }
}

/// The validated `benchmark_config.json`.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub(crate) profile: String,
    pub(crate) region: String,
    /// The prefix with exactly one trailing `-` (`rstrip("-") + "-"`).
    pub(crate) prefix: String,
    /// The prefix was generated, not supplied.
    pub(crate) prefix_generated: bool,
    /// Config tags with the run's built-in tags laid over them, in the order
    /// of `{**cfg_tags, **built_in_tags}`.
    pub(crate) tags: Vec<(String, String)>,
    pub(crate) dry_run: bool,
    /// `provisioned_capacity` of the config, as the script printed it.
    pub(crate) global_provisioned: Option<(Json, (i64, i64))>,
    pub(crate) memory_mb: i64,
    pub(crate) timeout_s: i64,
}

fn require_text<'a>(config: &'a Json, key: &str) -> Result<&'a str, ToolkitError> {
    text(config, key)?.ok_or_else(|| {
        die(format!(
            "benchmark_config.json missing required field: {key}"
        ))
    })
}

fn bounded_int(
    config: &Json,
    key: &str,
    default: i64,
    range: std::ops::RangeInclusive<i64>,
) -> Result<i64, ToolkitError> {
    match config.get(key) {
        Some(value) if pyfmt::truthy(value) => py_int(value)
            .and_then(|number| i64::try_from(number).ok())
            .filter(|number| range.contains(number))
            .ok_or_else(|| {
                die(format!(
                    "{key} must be a whole number from {} to {}, got {}",
                    range.start(),
                    range.end(),
                    quote_untrusted(&pyfmt::text(value))
                ))
            }),
        _ => Ok(default),
    }
}

/// `_ensure_resource_prefix` + `_validate_config` + the tag merge.
///
/// # Errors
///
/// Exit status 2 for a missing or malformed member.
pub(crate) fn settings(config: &Json, clock: &RunClock) -> Result<Settings, ToolkitError> {
    if config.type_name() != "object" {
        return Err(die("benchmark_config.json must be a JSON object"));
    }
    let profile = require_text(config, "aws_profile")?;
    let region = require_text(config, "region")?;
    let (supplied, prefix_generated) = match text(config, "resource_prefix")? {
        Some(prefix) => (prefix.to_string(), false),
        None => (
            format!("{REQUIRED_PREFIX}{}-{}", clock.today(), clock.prefix_token),
            true,
        ),
    };
    check_prefix(&supplied)?;
    check_plain("aws_profile", profile, 128)?;
    check_region(region)?;
    let prefix = format!("{}-", supplied.trim_end_matches('-'));

    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(given) = object(config, "tags")? {
        for (key, value) in tag_members(given) {
            check_plain("tag key", key, 128)?;
            tags.push((key.to_string(), pyfmt::text(value)));
        }
    }
    for (key, value) in [
        ("purpose", "ddb-skill-bench".to_string()),
        ("run_id", clock.run_id.clone()),
        ("created_at", clock.created_at.clone()),
        ("prefix", prefix.clone()),
    ] {
        match tags.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = value,
            None => tags.push((key.to_string(), value)),
        }
    }
    for (_, value) in &tags {
        check_plain("tag value", value, 256)?;
    }
    let global_provisioned = match config.get("provisioned_capacity") {
        Some(value) if pyfmt::truthy(value) => Some((value.clone(), provisioned(value)?)),
        _ => None,
    };
    Ok(Settings {
        profile: profile.to_string(),
        region: region.to_string(),
        prefix,
        prefix_generated,
        tags,
        dry_run: config.get("dry_run").is_some_and(pyfmt::truthy),
        global_provisioned,
        memory_mb: bounded_int(
            config,
            "lambda_memory_mb",
            DEFAULT_LAMBDA_MEMORY_MB,
            128..=10240,
        )?,
        timeout_s: bounded_int(
            config,
            "lambda_timeout_seconds",
            DEFAULT_LAMBDA_TIMEOUT_S,
            1..=900,
        )?,
    })
}

/// The members of a tags object.
// recorded-shape: the config's own tags object, not a response.
fn tag_members(tags: &Json) -> Vec<(&str, &Json)> {
    match tags {
        Json::Object(pairs) => pairs.iter().map(|(k, v)| (k.as_str(), v)).collect(),
        _ => Vec::new(),
    }
}

/// What the run prints about the plan.
#[derive(Debug, Clone)]
pub(crate) struct Summary {
    /// `(prefixed name, name in the design)` per table.
    pub(crate) tables: Vec<(String, String)>,
    pub(crate) function_name: String,
    pub(crate) role_name: String,
}

fn tag_list(tags: &[(String, String)]) -> Json {
    Json::Array(
        tags.iter()
            .map(|(key, value)| {
                Json::object(vec![
                    ("Key", Json::string(key.as_str())),
                    ("Value", Json::string(value.as_str())),
                ])
            })
            .collect(),
    )
}

fn tag_object(settings: &Settings) -> Json {
    Json::Object(
        settings
            .tags
            .iter()
            .map(|(key, value)| (key.clone(), Json::string(value.as_str())))
            .collect(),
    )
}

/// The IAM role and its inline policy, as the two API requests.
fn iam_section(settings: &Settings, identity: &Identity, clock: &RunClock) -> Json {
    let prefix = settings.prefix.as_str();
    let role_name = format!("{prefix}bench-role");
    let trust = lambda_trust_policy(&identity.account);
    let policy = lambda_role_policy(prefix, &settings.region, &identity.account);
    Json::object(vec![
        (
            "create_role",
            Json::object(vec![
                ("RoleName", Json::string(role_name.as_str())),
                (
                    "AssumeRolePolicyDocument",
                    Json::string(document_text(&trust)),
                ),
                (
                    "Description",
                    Json::string(format!("ddb-skill-bench role for run {}", clock.run_id)),
                ),
                ("Tags", tag_list(&settings.tags)),
            ]),
        ),
        (
            "put_role_policy",
            Json::object(vec![
                ("RoleName", Json::string(role_name.as_str())),
                ("PolicyName", Json::string(format!("{prefix}bench-policy"))),
                ("PolicyDocument", Json::string(document_text(&policy))),
            ]),
        ),
    ])
}

/// The Lambda request and the description of its code package.
fn lambda_section(
    settings: &Settings,
    identity: &Identity,
    clock: &RunClock,
    has_vectors: bool,
) -> Json {
    let prefix = settings.prefix.as_str();
    let role_arn = format!("arn:aws:iam::{}:role/{prefix}bench-role", identity.account);
    Json::object(vec![
        (
            "create_function",
            Json::object(vec![
                ("FunctionName", Json::string(format!("{prefix}bench"))),
                ("Runtime", Json::string(LAMBDA_RUNTIME)),
                ("Role", Json::string(role_arn)),
                ("Handler", Json::string(LAMBDA_HANDLER)),
                ("Timeout", Json::Int(settings.timeout_s)),
                ("MemorySize", Json::Int(settings.memory_mb)),
                ("Architectures", Json::Array(vec![Json::string("arm64")])),
                ("Tags", tag_object(settings)),
                (
                    "Description",
                    Json::string(format!("ddb-skill-bench Lambda for run {}", clock.run_id)),
                ),
            ]),
        ),
        (
            "code_package",
            Json::object(vec![
                ("module", Json::string("benchmark.py")),
                ("source", Json::string("scripts/benchmark_lambda.py")),
                ("bundle_vector_sdk", Json::Bool(has_vectors)),
                (
                    "vector_sdk_min_botocore",
                    if has_vectors {
                        Json::string(VECTOR_SDK_MIN)
                    } else {
                        Json::Null
                    },
                ),
            ]),
        ),
    ])
}

/// Builds the plan document and its summary.
///
/// # Errors
///
/// Exit status 2 for a table that cannot be created as declared.
pub(crate) fn build_plan(
    model: &Json,
    settings: &Settings,
    identity: &Identity,
    clock: &RunClock,
) -> Result<(Json, Summary), ToolkitError> {
    let prefix = settings.prefix.as_str();
    let global = settings
        .global_provisioned
        .as_ref()
        .map(|(_, units)| *units);
    let mut table_plans = Vec::new();
    let mut named = Vec::new();
    for table in tables(model)? {
        let request = build_create_table(table, prefix, &settings.tags, global)?;
        let original = text(table, "table_name")?.unwrap_or_default();
        let name = format!("{prefix}{original}");
        table_plans.push(Json::object(vec![
            ("name", Json::string(name.as_str())),
            ("original_name", Json::string(original)),
            ("create_table", request),
        ]));
        named.push((name, original.to_string()));
    }
    let has_vectors = tables(model)?
        .iter()
        .any(|table| list(table, "vector_indexes").is_ok_and(|v| !v.is_empty()));
    let plan = Json::object(vec![
        ("plan_version", Json::Int(1)),
        ("executed", Json::Bool(false)),
        ("account", Json::string(identity.account.as_str())),
        ("alias", Json::string(identity.alias.as_str())),
        ("aws_profile", Json::string(settings.profile.as_str())),
        ("region", Json::string(settings.region.as_str())),
        ("run_id", Json::string(clock.run_id.as_str())),
        ("created_at", Json::string(clock.created_at.as_str())),
        ("prefix", Json::string(prefix)),
        ("tags", tag_object(settings)),
        ("tables", Json::Array(table_plans)),
        ("iam", iam_section(settings, identity, clock)),
        (
            "lambda",
            lambda_section(settings, identity, clock, has_vectors),
        ),
    ]);
    Ok((
        plan,
        Summary {
            tables: named,
            function_name: format!("{prefix}bench"),
            role_name: format!("{prefix}bench-role"),
        },
    ))
}
