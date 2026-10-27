//! Hub listing: `get_deployable_models.py` (`get_deployable_models`),
//! `get_model_names.py` and `get_recipes.py`, plus the identifier checks that
//! run before any request.

use toolkit_aws::{collect_pages, next_token_from, AwsError, Page, PageLimits};
use toolkit_core::{escape_untrusted, Json};

use super::client::{Operation, SageMakerClient};
use super::normalize::{keywords_of, normalize_hub_content, CUSTOMIZATION_KEYWORD};

/// `MaxResults` of every `ListHubContents` request.
const LIST_PAGE_SIZE: i64 = 100;

/// Fine-tuning techniques the skill supports.
const SUPPORTED_TECHNIQUES: [&str; 4] = ["sft", "dpo", "rlvr", "rlaif"];

/// Prefix of a recipe keyword.
const RECIPE_PREFIX: &str = "@recipe:finetuning_";

/// A failure of a hub tool: the AWS boundary or a malformed response.
#[derive(Debug)]
pub(super) enum ModelsError {
    /// A call failed.
    Aws(AwsError),
    /// A response or recorded value did not have the expected shape.
    Data(String),
}

impl From<AwsError> for ModelsError {
    fn from(error: AwsError) -> Self {
        Self::Aws(error)
    }
}

impl From<String> for ModelsError {
    fn from(message: String) -> Self {
        Self::Data(message)
    }
}

/// Whether `name` matches `[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}` (the `SageMaker`
/// Hub, Hub content and endpoint name pattern, at most 63 characters).
pub(super) fn is_resource_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
}

/// Whether `text` is `arn:<partition>:sagemaker:<region>:<account|aws>:hub/`.
fn is_hub_arn_prefix(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    let [arn, partition, service, region, account, resource] = parts[..] else {
        return false;
    };
    let word = |part: &str| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    arn == "arn"
        && (1..=63).contains(&partition.len())
        && partition
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
        && service == "sagemaker"
        && region.contains('-')
        && region.split('-').all(word)
        && (account == "aws"
            || (account.len() == 12 && account.bytes().all(|b| b.is_ascii_digit())))
        && resource == "hub/"
}

/// Checks a Hub name (or Hub ARN) before it reaches a request. The value is
/// never echoed.
///
/// # Errors
///
/// A description naming the rule.
pub(super) fn validate_hub_name(hub: &str) -> Result<(), String> {
    let name = match hub.strip_prefix("arn:") {
        Some(_) => match hub.rsplit_once("hub/") {
            Some((prefix, name)) if is_hub_arn_prefix(&format!("{prefix}hub/")) => name,
            _ => return Err(invalid_name("Hub name")),
        },
        None => hub,
    };
    if is_resource_name(name) {
        Ok(())
    } else {
        Err(invalid_name("Hub name"))
    }
}

/// Checks a Hub content (model) name before it reaches a request.
///
/// # Errors
///
/// A description naming the rule.
pub(super) fn validate_content_name(name: &str) -> Result<(), String> {
    if is_resource_name(name) {
        Ok(())
    } else {
        Err(invalid_name("model name"))
    }
}

/// Checks an endpoint name before it reaches a request or a log group name.
///
/// # Errors
///
/// A description naming the rule.
pub(super) fn validate_endpoint_name(name: &str) -> Result<(), String> {
    if is_resource_name(name) {
        Ok(())
    } else {
        Err(invalid_name("endpoint name"))
    }
}

fn invalid_name(what: &str) -> String {
    format!(
        "invalid {what}: use 1-63 letters, digits and hyphens, starting and ending with a \
         letter or digit"
    )
}

/// The `ListHubContents` request for one page.
pub(super) fn list_request(hub: &str, token: Option<&str>) -> Json {
    let mut members = vec![
        ("HubName", Json::string(hub)),
        ("HubContentType", Json::string("Model")),
        ("MaxResults", Json::Int(LIST_PAGE_SIZE)),
    ];
    if let Some(token) = token {
        members.push(("NextToken", Json::string(token)));
    }
    Json::object(members)
}

/// Every `HubContentSummaries` record of the Hub, following `NextToken`.
///
/// # Errors
///
/// The first failed call, a repeated token, a runaway listing or a response
/// whose summaries are not a list of objects.
pub(super) fn list_hub_contents(
    client: &mut dyn SageMakerClient,
    hub: &str,
) -> Result<Vec<Json>, AwsError> {
    collect_pages(&PageLimits::default(), |token| {
        let response = client.call(Operation::ListHubContents, &list_request(hub, token))?;
        let items = match response.get("HubContentSummaries") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Array(items))
                if items.iter().all(|item| matches!(item, Json::Object(_))) =>
            {
                items.clone()
            }
            Some(_) => {
                return Err(AwsError::invalid_response(
                    "HubContentSummaries must be a list of objects",
                ))
            }
        };
        Ok(Page {
            items,
            next_token: next_token_from(&response, "NextToken")?,
        })
    })
}

/// `get_deployable_models`: the Hub's models in the normalized shape.
///
/// # Errors
///
/// A failed call or a record with malformed keywords.
pub(super) fn get_deployable_models(
    client: &mut dyn SageMakerClient,
    hub: &str,
) -> Result<Vec<Json>, ModelsError> {
    list_hub_contents(client, hub)?
        .iter()
        .enumerate()
        .map(|(index, record)| {
            normalize_hub_content(record).map_err(|message| format!("model {index}: {message}"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(ModelsError::Data)
}

/// `get_model_names.py`: the names of the customization-capable models, sorted
/// case-insensitively.
///
/// # Errors
///
/// A failed call or a record with malformed keywords.
pub(super) fn customization_model_names(
    client: &mut dyn SageMakerClient,
    hub: &str,
) -> Result<Json, ModelsError> {
    let mut names: Vec<Json> = Vec::new();
    for (index, record) in list_hub_contents(client, hub)?.iter().enumerate() {
        let keywords =
            keywords_of(record).map_err(|message| format!("model {index}: {message}"))?;
        if keywords.contains(&CUSTOMIZATION_KEYWORD) {
            names.push(record.get("HubContentName").cloned().unwrap_or(Json::Null));
        }
    }
    // Stable, case-insensitive; a missing name sorts as "".
    names.sort_by_key(|name| name.as_str().unwrap_or("").to_lowercase());
    Ok(Json::Array(names))
}

/// The `DescribeHubContent` request of `get_recipes.py`.
pub(super) fn describe_request(hub: &str, model: &str) -> Json {
    Json::object(vec![
        ("HubName", Json::string(hub)),
        ("HubContentType", Json::string("Model")),
        ("HubContentName", Json::string(model)),
    ])
}

/// `get_recipes.py`: the supported fine-tuning techniques of a model, sorted
/// and unique.
///
/// # Errors
///
/// A failed call (an unknown model is a `ResourceNotFound` service error) or
/// malformed keywords.
pub(super) fn finetuning_techniques(
    client: &mut dyn SageMakerClient,
    hub: &str,
    model: &str,
) -> Result<Json, ModelsError> {
    let detail = client.call(Operation::DescribeHubContent, &describe_request(hub, model))?;
    let keywords = keywords_of(&detail)?;
    let mut techniques: Vec<String> = keywords
        .iter()
        .filter(|keyword| keyword.starts_with(RECIPE_PREFIX))
        .map(|keyword| {
            keyword
                .replace(RECIPE_PREFIX, "")
                .split('_')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    techniques.sort();
    techniques.dedup();
    techniques.retain(|technique| SUPPORTED_TECHNIQUES.contains(&technique.as_str()));
    Ok(Json::Array(
        techniques.into_iter().map(Json::String).collect(),
    ))
}

/// The one-line, escaped text of a service code or message for a diagnostic.
pub(super) fn shown(text: &str) -> String {
    escape_untrusted(text, 400)
}
