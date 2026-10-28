//! Scoped IAM policy for Bedrock model invocation (`iam_policy.py`).
//!
//! Pure functions: model ids, region and account id in, a policy document out.
//! Handles the dual-ARN pattern (foundation model plus inference profile)
//! required when a cross-region inference profile id is in use, and the
//! `bedrock-mantle` actions bare proprietary GPT ids need.

use toolkit_core::Json;

/// The geo prefixes of a cross-region inference profile id.
const GEO_PREFIXES: [&str; 6] = ["us", "eu", "apac", "au", "jp", "global"];

/// The id without ONE leading geo prefix (`_GEO_PREFIX.sub("", id)`; the
/// pattern is anchored, so it can match once).
pub(crate) fn strip_geo_prefix(model_id: &str) -> &str {
    for prefix in GEO_PREFIXES {
        if let Some(rest) = model_id
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            return rest;
        }
    }
    model_id
}

fn has_geo_prefix(model_id: &str) -> bool {
    strip_geo_prefix(model_id).len() != model_id.len()
}

/// `re.fullmatch(r"arn:aws:bedrock:[^:]+:[0-9]{12}:inference-profile/[^/]+", id)`.
fn is_profile_arn(model_id: &str) -> bool {
    let Some(rest) = model_id.strip_prefix("arn:aws:bedrock:") else {
        return false;
    };
    let Some((region, rest)) = rest.split_once(':') else {
        return false;
    };
    let Some((account, name)) = rest.split_once(':') else {
        return false;
    };
    let Some(profile) = name.strip_prefix("inference-profile/") else {
        return false;
    };
    !region.is_empty()
        && account.len() == 12
        && account.bytes().all(|b| b.is_ascii_digit())
        && !profile.is_empty()
        && !profile.contains('/')
}

/// True when the id is a cross-region inference profile: a geo-prefixed id or a
/// profile ARN.
pub(crate) fn is_inference_profile(model_id: &str) -> bool {
    has_geo_prefix(model_id) || is_profile_arn(model_id)
}

/// True for bare proprietary GPT-5/GPT-6 ids and the bare Opus 5.5 id, which
/// use `bedrock-mantle` (Responses and Messages respectively): a policy that
/// grants only `bedrock:InvokeModel` on a foundation-model ARN cannot
/// authorize them. Opus 5.5 in `eu-west-2` is a runtime in-region target.
/// Prefixed forms use the runtime. The open-weight gpt-oss models use the
/// runtime and never match.
pub(crate) fn is_mantle_model(model_id: &str, region: &str) -> bool {
    let id = model_id.to_lowercase();
    ((id.starts_with("openai.gpt-5") || id.starts_with("openai.gpt-6")) && !id.contains("oss"))
        || (id == "anthropic.claude-opus-5-5" && region != "eu-west-2")
}

/// The ARN partition of a region.
fn partition(region: &str) -> &'static str {
    if region.starts_with("us-gov-") {
        "aws-us-gov"
    } else if region.starts_with("cn-") {
        "aws-cn"
    } else {
        "aws"
    }
}

/// ARN scope for mantle inference. Mantle authorizes per project, not per
/// model, so it cannot be narrowed to model ids (use a service control policy
/// to restrict the model set).
pub(crate) fn mantle_project_arn(region: &str, account_id: &str) -> String {
    format!(
        "arn:{}:bedrock-mantle:{region}:{account_id}:project/*",
        partition(region)
    )
}

/// ARN of a plain foundation-model id (no geo prefix).
pub(crate) fn foundation_model_arn(model_id: &str, region: &str) -> String {
    format!(
        "arn:{}:bedrock:*::foundation-model/{model_id}",
        partition(region)
    )
}

/// ARN of a cross-region inference profile (a profile ARN is kept as given).
pub(crate) fn inference_profile_arn(model_id: &str, region: &str, account_id: &str) -> String {
    if model_id.starts_with("arn:") {
        return model_id.to_string();
    }
    format!(
        "arn:{}:bedrock:{region}:{account_id}:inference-profile/{model_id}",
        partition(region)
    )
}

/// ARN of the default Bedrock project of the account and region. A GPT-6 CRIS
/// id needs it next to the inference-profile ARN: every GPT-6 model card's
/// "Call the model" section grants `bedrock:InvokeModel` on both.
pub(crate) fn default_project_arn(region: &str, account_id: &str) -> String {
    format!(
        "arn:{}:bedrock:{region}:{account_id}:project/default",
        partition(region)
    )
}

/// The part of an id after its last `/` (`model_id.rsplit("/", 1)[-1]`).
fn last_segment(model_id: &str) -> &str {
    model_id.rsplit('/').next().unwrap_or(model_id)
}

/// True for a geo-prefixed (CRIS) GPT-6 id such as `us.openai.gpt-6-astra`. A
/// bare `openai.gpt-6-*` id (mantle) does not match.
pub(crate) fn is_gpt6_cris_model(model_id: &str) -> bool {
    is_inference_profile(model_id)
        && strip_geo_prefix(last_segment(model_id))
            .to_lowercase()
            .starts_with("openai.gpt-6")
}

fn text_list(items: &[&str]) -> Json {
    Json::Array(items.iter().map(|item| Json::string(*item)).collect())
}

/// Builds a scoped policy covering exactly the given model ids.
///
/// Up to three statements, depending on the endpoints the targets use:
/// `bedrock:InvokeModel*` on foundation-model and inference-profile ARNs (the
/// runtime targets); `bedrock-mantle:CreateInference`/`Get*`/`List*` on the
/// account's mantle projects (the mantle-only targets, `SigV4`); and
/// `bedrock-mantle:CallWithBearerToken` on `*` (Bedrock API-key auth). A
/// statement is omitted when no target needs it: an all-mantle run never emits
/// an `InvokeModel` statement with an empty `Resource` list, which is an
/// invalid policy. A genuinely empty model list keeps the legacy shape (one
/// statement with an empty `Resource`).
pub(crate) fn generate_policy(model_ids: &[String], region: &str, account_id: &str) -> Json {
    let mantle_ids = model_ids.iter().any(|id| is_mantle_model(id, region));
    let mut runtime_ids: Vec<&str> = model_ids
        .iter()
        .filter(|id| !is_mantle_model(id, region))
        .map(String::as_str)
        .collect();
    runtime_ids.sort_unstable();
    runtime_ids.dedup();

    let mut resources: Vec<String> = Vec::new();
    for id in runtime_ids {
        if is_inference_profile(id) {
            resources.push(inference_profile_arn(id, region, account_id));
            resources.push(foundation_model_arn(
                strip_geo_prefix(last_segment(id)),
                region,
            ));
            // GPT-5.6 CRIS ids and every other runtime target keep the 2-ARN
            // shape; only a GPT-6 CRIS id also needs the default project.
            if is_gpt6_cris_model(id) {
                resources.push(default_project_arn(region, account_id));
            }
        } else {
            resources.push(foundation_model_arn(id, region));
        }
    }
    resources.sort_unstable();
    resources.dedup();

    let mut statements: Vec<Json> = Vec::new();
    if !resources.is_empty() || !mantle_ids {
        statements.push(Json::object(vec![
            ("Sid", Json::string("BedrockInvokeModelScoped")),
            ("Effect", Json::string("Allow")),
            (
                "Action",
                text_list(&[
                    "bedrock:InvokeModel",
                    "bedrock:InvokeModelWithResponseStream",
                ]),
            ),
            (
                "Resource",
                Json::Array(resources.into_iter().map(Json::string).collect()),
            ),
        ]));
    }
    if mantle_ids {
        statements.push(Json::object(vec![
            ("Sid", Json::string("BedrockMantleInference")),
            ("Effect", Json::string("Allow")),
            (
                "Action",
                text_list(&[
                    "bedrock-mantle:CreateInference",
                    "bedrock-mantle:Get*",
                    "bedrock-mantle:List*",
                ]),
            ),
            (
                "Resource",
                Json::string(mantle_project_arn(region, account_id)),
            ),
        ]));
        // AWS does not support narrowing CallWithBearerToken; it must be `*`.
        // Required for Bedrock API-key (bearer token) auth; omit only when the
        // application uses SigV4 exclusively.
        statements.push(Json::object(vec![
            ("Sid", Json::string("BedrockMantleCallWithBearerToken")),
            ("Effect", Json::string("Allow")),
            ("Action", text_list(&["bedrock-mantle:CallWithBearerToken"])),
            ("Resource", Json::string("*")),
        ]));
    }
    Json::object(vec![
        ("Version", Json::string("2012-10-17")),
        ("Statement", Json::Array(statements)),
    ])
}

/// Whether `text` may be part of a policy resource ARN: no IAM wildcard
/// (`*`, `?`), no policy variable (`${`) and no control character. The command
/// refuses an id, region or account id that would widen the policy; the
/// Python script pasted whatever it was given.
pub(crate) fn is_literal_policy_text(text: &str) -> bool {
    !text.contains(['*', '?']) && !text.contains("${") && !text.chars().any(char::is_control)
}

/// A 12-digit AWS account id.
pub(crate) fn is_account_id(text: &str) -> bool {
    text.len() == 12 && text.bytes().all(|b| b.is_ascii_digit())
}
