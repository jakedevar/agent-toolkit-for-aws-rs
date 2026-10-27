//! `search_deployable_models.py`: exact `SageMaker` `Search` queries for public
//! Hub models, with the complete local `ListHubContents` fallback.
//!
//! Constraints are resolved against the selection-session catalog so an unknown
//! value is refused before any request and a substring request expands to every
//! exact spelling it covers. The query is a nested `And`/`Or` expression whose
//! element and value sizes stay within the documented `SearchExpression` and
//! `Filter` limits (20 elements per list, 1024 characters per value).

use std::collections::HashMap;

use toolkit_aws::{collect_pages, next_token_from, AwsError, Page, PageLimits};
use toolkit_core::{quote_untrusted, Json};

use super::client::{Operation, SageMakerClient};
use super::filter::{filter_models, sort_models_newest_first, Constraints, VALID_KEYS};
use super::hub::shown;
use super::normalize::normalize_hub_content;

/// The only Hub the Search adapter serves.
pub(super) const PUBLIC_HUB_NAME: &str = "SageMakerPublicHub";

/// Most elements of one `Filters`, `NestedFilters` or `SubExpressions` list.
const MAX_EXPRESSION_ITEMS: usize = 20;

/// Longest `Filter.Value`, in characters.
const MAX_FILTER_VALUE_LENGTH: usize = 1024;

/// `MaxResults` of every `Search` request.
const SEARCH_PAGE_SIZE: i64 = 100;

/// Paging key only; candidate order is restored from `OriginalCreationTime`.
const SEARCH_SORT_BY: &str = "HubContentName";

/// A failure of the Search adapter. The first, third and fourth kinds make the
/// caller fall back to the complete catalog; a constraint or data problem is
/// always visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchAdapterError {
    /// A constraint cannot be represented by the query contract.
    Constraint(String),
    /// The client cannot issue `HubContent` Search.
    UnsupportedSdk(String),
    /// The Search request failed.
    Service(String),
    /// The Search response is not of the expected shape.
    Response(String),
    /// The catalog or a model is malformed.
    Data(String),
}

impl SearchAdapterError {
    /// The Python exception class name, as shown in `fallback_reason.type`.
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::Constraint(_) => "ConstraintValidationError",
            Self::UnsupportedSdk(_) => "UnsupportedSdkError",
            Self::Service(_) => "SearchServiceError",
            Self::Response(_) => "SearchResponseError",
            Self::Data(_) => "ValueError",
        }
    }

    /// The (safe) message.
    pub(super) fn message(&self) -> &str {
        match self {
            Self::Constraint(text)
            | Self::UnsupportedSdk(text)
            | Self::Service(text)
            | Self::Response(text)
            | Self::Data(text) => text,
        }
    }
}

type Outcome<T> = Result<T, SearchAdapterError>;

fn constraint_error<T>(message: String) -> Outcome<T> {
    Err(SearchAdapterError::Constraint(message))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    Exact,
    Substring,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Casing {
    Enum,
    FreeText,
}

/// Keyword constraints: key, catalog field, keyword prefix.
const KEYWORD_CONSTRAINTS: [(&str, &str, &str); 4] = [
    ("task", "tasks", "@task:"),
    ("license", "license", "@license:"),
    ("language", "languages", "@language:"),
    ("provider", "provider", "@provider:"),
];

/// Metadata constraints: key, catalog field, `Metadata.*` filter, match mode,
/// casing of the stored document value.
const METADATA_CONSTRAINTS: [(&str, &str, &str, MatchMode, Casing); 5] = [
    (
        "data_type",
        "data_types",
        "Metadata.DataType",
        MatchMode::Substring,
        Casing::FreeText,
    ),
    (
        "input_modality",
        "input_modalities",
        "Metadata.InputModalities",
        MatchMode::Substring,
        Casing::FreeText,
    ),
    (
        "output_modality",
        "output_modalities",
        "Metadata.OutputModalities",
        MatchMode::Substring,
        Casing::FreeText,
    ),
    (
        "model_type",
        "model_type",
        "Metadata.ModelTypes",
        MatchMode::Exact,
        Casing::Enum,
    ),
    (
        "context_window",
        "context_window",
        "Metadata.ContextWindow",
        MatchMode::Exact,
        Casing::FreeText,
    ),
];

/// The filter `size` buckets expand into.
const SIZE_METADATA_FILTER: &str = "Metadata.ModelSize";

/// `bedrock` is served by `Metadata.Capabilities`; `customization` by a tag.
const METADATA_CAPABILITY: (&str, &str, &str) =
    ("bedrock", "Metadata.Capabilities", "BEDROCK_CONSOLE");
const CAPABILITY_TAG: (&str, &str) = ("customization", "@capability:customization");

fn list_repr(values: &[String]) -> String {
    let parts: Vec<String> = values.iter().map(|value| quote_untrusted(value)).collect();
    format!("[{}]", parts.join(", "))
}

/// Validates and normalizes constraints: supported keys, at least one
/// non-blank value each, values trimmed, lower-cased and de-duplicated.
pub(super) fn normalize_constraints(constraints: &Constraints) -> Outcome<Constraints> {
    let mut normalized: Constraints = Vec::new();
    for (key, values) in constraints {
        if !VALID_KEYS.contains(&key.as_str()) {
            return constraint_error(format!(
                "Unsupported constraint key {}. Supported keys: {}",
                quote_untrusted(key),
                VALID_KEYS.join(", ")
            ));
        }
        if values.is_empty() {
            return constraint_error(format!(
                "Constraint {key:?} must contain at least one value"
            ));
        }
        let mut clean: Vec<String> = Vec::new();
        for value in values {
            let trimmed = crate::manifests::py_strip(value);
            if trimmed.is_empty() {
                return constraint_error(format!(
                    "Constraint {key:?} contains an empty or non-string value"
                ));
            }
            let lowered = trimmed.to_lowercase();
            if !clean.contains(&lowered) {
                clean.push(lowered);
            }
        }
        if let Some((_, existing)) = normalized.iter_mut().find(|(name, _)| name == key) {
            for value in clean {
                if !existing.contains(&value) {
                    existing.push(value);
                }
            }
        } else {
            normalized.push((key.clone(), clean));
        }
    }
    Ok(normalized)
}

/// Parses `key:value` words into normalized constraints.
pub(super) fn parse_constraints(raw: &[&str]) -> Outcome<Constraints> {
    let mut constraints: Constraints = Vec::new();
    for word in raw {
        let Some((key, value)) = word.split_once(':') else {
            return constraint_error(format!(
                "Malformed constraint {}; expected key:value",
                quote_untrusted(word)
            ));
        };
        super::filter::add_constraint(
            &mut constraints,
            crate::manifests::py_strip(key),
            value.to_string(),
        );
    }
    normalize_constraints(&constraints)
}

fn catalog_field_values<'a>(model: &'a Json, field: &str) -> Vec<&'a str> {
    match model.get(field) {
        Some(Json::String(text)) => vec![text.as_str()],
        Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Every current catalog value the request matches, in catalog spelling and
/// case-insensitive order.
fn resolve_catalog_values(
    key: &str,
    field: &str,
    mode: MatchMode,
    requested: &[String],
    catalog: &[Json],
) -> Outcome<Vec<String>> {
    let mut resolved: Vec<String> = Vec::new();
    for model in catalog {
        for value in catalog_field_values(model, field) {
            let lowered = value.to_lowercase();
            let hit = match mode {
                MatchMode::Exact => requested.contains(&lowered),
                MatchMode::Substring => requested.iter().any(|r| lowered.contains(r.as_str())),
            };
            if hit && !resolved.iter().any(|known| known == value) {
                resolved.push(value.to_string());
            }
        }
    }
    if resolved.is_empty() {
        return constraint_error(format!(
            "Constraint {key:?} values {} do not match any current public-Hub catalog value",
            list_repr(requested)
        ));
    }
    resolved.sort_by_key(|value| value.to_lowercase());
    Ok(resolved)
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect()
    })
}

/// A lower-cased keyword value expanded to the casings the document keeps.
fn metadata_spellings(value: &str, casing: Casing) -> Vec<String> {
    let lowered = value.to_lowercase();
    if casing == Casing::Enum {
        return vec![lowered.to_uppercase()];
    }
    let mut spellings: Vec<String> = Vec::new();
    for spelling in [
        value.to_string(),
        lowered.clone(),
        capitalize(&lowered),
        lowered.to_uppercase(),
    ] {
        if !spellings.contains(&spelling) {
            spellings.push(spelling);
        }
    }
    spellings
}

fn dedup_spellings(into: &mut Vec<String>, spellings: Vec<String>) {
    for spelling in spellings {
        if !into.contains(&spelling) {
            into.push(spelling);
        }
    }
}

/// Expands size buckets to the raw `ModelSize` labels they contain.
fn resolve_size_values(requested: &[String], catalog: &[Json]) -> Outcome<Vec<String>> {
    let mut resolved: Vec<String> = Vec::new();
    for model in catalog {
        let (Some(size), Some(raw)) = (
            model.get("size").and_then(Json::as_str),
            model.get("size_raw").and_then(Json::as_str),
        ) else {
            continue;
        };
        if requested.contains(&size.to_lowercase()) && !raw.is_empty() {
            dedup_spellings(&mut resolved, metadata_spellings(raw, Casing::FreeText));
        }
    }
    if resolved.is_empty() {
        return constraint_error(format!(
            "Size values {} do not match any current public-Hub raw model-size value",
            list_repr(requested)
        ));
    }
    resolved.sort_by_key(|value| value.to_lowercase());
    Ok(resolved)
}

/// Resolves case-insensitive substrings to exact current model names.
fn resolve_model_names(requested: &[String], catalog: &[Json]) -> Outcome<Vec<String>> {
    let mut resolved: Vec<String> = Vec::new();
    for model in catalog {
        if let Some(name) = model.get("name").and_then(Json::as_str) {
            let lowered = name.to_lowercase();
            if requested.iter().any(|r| lowered.contains(r.as_str()))
                && !resolved.iter().any(|n| n == name)
            {
                resolved.push(name.to_string());
            }
        }
    }
    if resolved.is_empty() {
        return constraint_error(format!(
            "Model-name values {} do not match any current public-Hub model",
            list_repr(requested)
        ));
    }
    resolved.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    Ok(resolved)
}

/// One catalog-cased spelling of `requested` shared by every name, or `None`
/// when the casing differs, or lower-casing changes a name's length (then no
/// character position can be trusted).
fn shared_name_spelling(requested: &str, names: &[String]) -> Option<String> {
    let wanted: Vec<char> = requested.chars().collect();
    let mut spellings: Vec<String> = Vec::new();
    for name in names {
        let original: Vec<char> = name.chars().collect();
        let lowered: Vec<char> = name.to_lowercase().chars().collect();
        if lowered.len() != original.len() {
            return None;
        }
        let start = (0..=lowered.len().checked_sub(wanted.len())?)
            .find(|&at| lowered[at..at + wanted.len()] == wanted[..])?;
        let spelling: String = original[start..start + wanted.len()].iter().collect();
        if !spellings.contains(&spelling) {
            spellings.push(spelling);
        }
    }
    if spellings.len() == 1 {
        spellings.pop()
    } else {
        None
    }
}

fn search_filter(name: &str, operator: &str, value: &str) -> Json {
    Json::object(vec![
        ("Name", Json::string(name)),
        ("Operator", Json::string(operator)),
        ("Value", Json::string(value)),
    ])
}

/// Packs exact alternatives into `In` filters of at most 1024 characters and
/// `Equals` filters for values that contain a comma.
fn alternative_filters(name: &str, values: &[String]) -> Vec<Json> {
    fn flush(filters: &mut Vec<Json>, name: &str, batch: &mut Vec<&str>, length: &mut usize) {
        match batch.as_slice() {
            [] => {}
            [only] => filters.push(search_filter(name, "Equals", only)),
            many => filters.push(search_filter(name, "In", &many.join(","))),
        }
        batch.clear();
        *length = 0;
    }

    let mut filters: Vec<Json> = Vec::new();
    let mut batch: Vec<&str> = Vec::new();
    let mut batch_length = 0_usize;

    for value in values {
        if value.contains(',') {
            flush(&mut filters, name, &mut batch, &mut batch_length);
            filters.push(search_filter(name, "Equals", value));
            continue;
        }
        let characters = value.chars().count();
        let mut added = characters + usize::from(!batch.is_empty());
        if !batch.is_empty() && batch_length + added > MAX_FILTER_VALUE_LENGTH {
            flush(&mut filters, name, &mut batch, &mut batch_length);
            added = characters;
        }
        batch.push(value);
        batch_length += added;
    }
    flush(&mut filters, name, &mut batch, &mut batch_length);
    filters
}

fn or_expression(filters: Vec<Json>) -> Json {
    if filters.len() <= MAX_EXPRESSION_ITEMS {
        return Json::object(vec![
            ("Operator", Json::string("Or")),
            ("Filters", Json::Array(filters)),
        ]);
    }
    let mut level: Vec<Json> = filters
        .chunks(MAX_EXPRESSION_ITEMS)
        .map(|chunk| {
            Json::object(vec![
                ("Operator", Json::string("Or")),
                ("Filters", Json::Array(chunk.to_vec())),
            ])
        })
        .collect();
    while level.len() > MAX_EXPRESSION_ITEMS {
        level = level
            .chunks(MAX_EXPRESSION_ITEMS)
            .map(|chunk| {
                Json::object(vec![
                    ("Operator", Json::string("Or")),
                    ("SubExpressions", Json::Array(chunk.to_vec())),
                ])
            })
            .collect();
    }
    Json::object(vec![
        ("Operator", Json::string("Or")),
        ("SubExpressions", Json::Array(level)),
    ])
}

fn append_exact_alternatives(
    filters: &mut Vec<Json>,
    subexpressions: &mut Vec<Json>,
    name: &str,
    values: &[String],
) {
    if let [only] = values {
        filters.push(search_filter(name, "Equals", only));
        return;
    }
    let mut alternatives = alternative_filters(name, values);
    if alternatives.len() == 1 {
        filters.push(alternatives.remove(0));
    } else {
        subexpressions.push(or_expression(alternatives));
    }
}

/// Validates the generated expression against the service limits.
pub(super) fn validate_search_expression(expression: &Json, path: &str) -> Outcome<()> {
    let Json::Object(_) = expression else {
        return constraint_error(format!("{path} must be an object"));
    };
    let mut has_condition = false;
    for collection in ["Filters", "NestedFilters", "SubExpressions"] {
        let Some(value) = expression.get(collection) else {
            continue;
        };
        let Json::Array(items) = value else {
            return constraint_error(format!("{path}.{collection} must be a non-empty list"));
        };
        if items.is_empty() {
            return constraint_error(format!("{path}.{collection} must be a non-empty list"));
        }
        has_condition = true;
        if items.len() > MAX_EXPRESSION_ITEMS {
            return constraint_error(format!(
                "{path}.{collection} has {} elements; the maximum is {MAX_EXPRESSION_ITEMS}",
                items.len()
            ));
        }
    }
    if let Some(Json::Array(filters)) = expression.get("Filters") {
        check_filter_values(filters, &format!("{path}.Filters"))?;
    }
    if let Some(Json::Array(subexpressions)) = expression.get("SubExpressions") {
        for (index, sub) in subexpressions.iter().enumerate() {
            validate_search_expression(sub, &format!("{path}.SubExpressions[{index}]"))?;
        }
    }
    if let Some(Json::Array(nested)) = expression.get("NestedFilters") {
        for (index, filter) in nested.iter().enumerate() {
            let nested_path = format!("{path}.NestedFilters[{index}]");
            let Json::Object(_) = filter else {
                return constraint_error(format!("{nested_path} must be an object"));
            };
            let inner = Json::object(vec![(
                "Filters",
                filter
                    .get("Filters")
                    .cloned()
                    .unwrap_or(Json::Array(Vec::new())),
            )]);
            validate_search_expression(&inner, &nested_path)?;
        }
    }
    if !has_condition {
        return constraint_error(format!("{path} must contain at least one condition"));
    }
    Ok(())
}

fn check_filter_values(filters: &[Json], path: &str) -> Outcome<()> {
    for (index, filter) in filters.iter().enumerate() {
        let Json::Object(_) = filter else {
            return constraint_error(format!("{path}[{index}] must be an object"));
        };
        match filter.get("Value") {
            Some(Json::String(value)) if !value.is_empty() => {
                if value.chars().count() > MAX_FILTER_VALUE_LENGTH {
                    return constraint_error(format!(
                        "{path}[{index}].Value exceeds {MAX_FILTER_VALUE_LENGTH} characters"
                    ));
                }
            }
            _ => {
                return constraint_error(format!(
                    "{path}[{index}].Value must be a non-empty string"
                ));
            }
        }
    }
    Ok(())
}

/// Translates confirmed constraints into a `SearchExpression`.
///
/// # Errors
///
/// A constraint error for a private Hub, an unsupported or unresolvable
/// constraint, or an expression beyond the service limits.
pub(super) fn build_search_expression(
    hub_name: &str,
    constraints: &Constraints,
    catalog: &[Json],
) -> Outcome<Json> {
    if hub_name != PUBLIC_HUB_NAME {
        return constraint_error(
            "The Search adapter supports SageMakerPublicHub only; private Hubs must continue \
             using ListHubContents"
                .to_string(),
        );
    }
    let normalized = normalize_constraints(constraints)?;
    let mut filters = vec![
        search_filter("HubName", "Equals", PUBLIC_HUB_NAME),
        search_filter("HubContentType", "Equals", "Model"),
    ];
    let mut subexpressions: Vec<Json> = Vec::new();

    for key in VALID_KEYS {
        let Some((_, values)) = normalized.iter().find(|(name, _)| name == key) else {
            continue;
        };
        if let Some((_, field, filter_name, mode, casing)) =
            METADATA_CONSTRAINTS.iter().find(|row| row.0 == key)
        {
            let resolved = resolve_catalog_values(key, field, *mode, values, catalog)?;
            let mut spellings: Vec<String> = Vec::new();
            for value in &resolved {
                dedup_spellings(&mut spellings, metadata_spellings(value, *casing));
            }
            append_exact_alternatives(&mut filters, &mut subexpressions, filter_name, &spellings);
        } else if let Some((_, field, prefix)) = KEYWORD_CONSTRAINTS.iter().find(|row| row.0 == key)
        {
            let resolved =
                resolve_catalog_values(key, field, MatchMode::Substring, values, catalog)?;
            let tags: Vec<String> = resolved
                .iter()
                .map(|value| format!("{prefix}{value}"))
                .collect();
            append_exact_alternatives(
                &mut filters,
                &mut subexpressions,
                "HubContentSearchKeywords",
                &tags,
            );
        } else if key == "size" {
            let raw = resolve_size_values(values, catalog)?;
            append_exact_alternatives(
                &mut filters,
                &mut subexpressions,
                SIZE_METADATA_FILTER,
                &raw,
            );
        } else if key == METADATA_CAPABILITY.0 || key == CAPABILITY_TAG.0 {
            // Capabilities are positive exact values: absence-based `false`
            // matching is not enabled until its Search semantics are verified.
            if values.as_slice() != ["true"] {
                return constraint_error(format!(
                    "The Search adapter supports {key}:true only; absence-based false matching \
                     is not yet verified"
                ));
            }
            if key == METADATA_CAPABILITY.0 {
                filters.push(search_filter(
                    METADATA_CAPABILITY.1,
                    "Equals",
                    METADATA_CAPABILITY.2,
                ));
            } else {
                append_exact_alternatives(
                    &mut filters,
                    &mut subexpressions,
                    "HubContentSearchKeywords",
                    &[CAPABILITY_TAG.1.to_string()],
                );
            }
        } else if key == "name" {
            let names = resolve_model_names(values, catalog)?;
            let spelling = match values.as_slice() {
                [only] => shared_name_spelling(only, &names),
                _ => None,
            };
            if let Some(spelling) = spelling {
                filters.push(search_filter("HubContentName", "Contains", &spelling));
            } else {
                // Several substrings or inconsistent casing cannot share the
                // service's single case-sensitive Contains: use exact names.
                append_exact_alternatives(
                    &mut filters,
                    &mut subexpressions,
                    "HubContentName",
                    &names,
                );
            }
        }
    }

    let mut members = vec![
        ("Operator", Json::string("And")),
        ("Filters", Json::Array(filters)),
    ];
    if !subexpressions.is_empty() {
        members.push(("SubExpressions", Json::Array(subexpressions)));
    }
    let expression = Json::object(members);
    validate_search_expression(&expression, "SearchExpression")?;
    Ok(expression)
}

/// The `Search` request for one page.
pub(super) fn search_request(expression: &Json, token: Option<&str>) -> Json {
    let mut members = vec![
        ("Resource", Json::string("HubContent")),
        ("SearchExpression", expression.clone()),
        ("MaxResults", Json::Int(SEARCH_PAGE_SIZE)),
        // Pages of an unsorted Search are not stable: the name key makes the
        // page boundaries deterministic; the visible order is restored after.
        ("SortBy", Json::string(SEARCH_SORT_BY)),
        ("SortOrder", Json::string("Ascending")),
    ];
    if let Some(token) = token {
        members.push(("NextToken", Json::string(token)));
    }
    Json::object(members)
}

fn search_error(error: &AwsError) -> SearchAdapterError {
    match error {
        AwsError::Service(service) => SearchAdapterError::Service(format!(
            "SageMaker Search failed ({}): {}",
            shown(service.code().unwrap_or("Unknown")),
            shown(service.message())
        )),
        AwsError::Pagination { .. } | AwsError::InvalidResponse { .. } => {
            SearchAdapterError::Response(shown(&error.to_string()))
        }
        other => SearchAdapterError::Service(format!(
            "SageMaker Search failed: {}",
            shown(&other.to_string())
        )),
    }
}

/// Searches the public Hub and returns the normalized candidate models, each
/// name once.
///
/// # Errors
///
/// A constraint error (never a fallback), or an unsupported client, a failed
/// request or a malformed response.
pub(super) fn search_deployable_models(
    hub_name: &str,
    constraints: &Constraints,
    catalog: &[Json],
    client: &mut dyn SageMakerClient,
) -> Outcome<Vec<Json>> {
    let expression = build_search_expression(hub_name, constraints, catalog)?;
    client
        .check_search_support()
        .map_err(|reason| SearchAdapterError::UnsupportedSdk(shown(&reason)))?;
    let records = collect_pages(&PageLimits::default(), |token| {
        let response = client.call(Operation::Search, &search_request(&expression, token))?;
        let results = match response.get("Results") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Array(results)) => results.clone(),
            Some(_) => {
                return Err(AwsError::invalid_response(
                    "Search response Results must be a list",
                ))
            }
        };
        let mut contents = Vec::with_capacity(results.len());
        for (index, record) in results.iter().enumerate() {
            match record.get("HubContent") {
                Some(content @ Json::Object(_)) => contents.push(content.clone()),
                _ => {
                    return Err(AwsError::invalid_response(format!(
                        "Search result {index} does not contain a HubContent object"
                    )))
                }
            }
        }
        Ok(Page {
            items: contents,
            next_token: next_token_from(&response, "NextToken")?,
        })
    })
    .map_err(|error| search_error(&error))?;

    let mut models: Vec<Json> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let model = normalize_hub_content(record).map_err(|message| {
            SearchAdapterError::Response(format!("Search result {index}: {}", shown(&message)))
        })?;
        // A model repeated across pages means paging was not stable: keep the
        // candidate set a set.
        if let Some(name) = model.get("name").and_then(Json::as_str) {
            if seen.iter().any(|known| known == name) {
                continue;
            }
            seen.push(name.to_string());
        }
        models.push(model);
    }
    Ok(models)
}

/// Timestamp coverage and provenance of the candidate ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OrderingSummary {
    pub(super) timestamp_source: &'static str,
    pub(super) dated_models: usize,
    pub(super) undated_models: usize,
}

impl OrderingSummary {
    fn to_json(&self) -> Json {
        let count = |n: usize| Json::Int(i64::try_from(n).unwrap_or(i64::MAX));
        Json::object(vec![
            ("field", Json::string("OriginalCreationTime")),
            ("timestamp_source", Json::string(self.timestamp_source)),
            ("dated_models", count(self.dated_models)),
            ("undated_models", count(self.undated_models)),
        ])
    }
}

fn ordering_summary(models: &[Json], timestamp_source: &'static str) -> OrderingSummary {
    let dated = models
        .iter()
        .filter(|model| !matches!(model.get("original_creation_time"), None | Some(Json::Null)))
        .count();
    OrderingSummary {
        timestamp_source,
        dated_models: dated,
        undated_models: models.len() - dated,
    }
}

/// Attaches the catalog's `OriginalCreationTime` to the Search matches and
/// applies the newest-first order. A Search-only model stays undated.
pub(super) fn restore_catalog_order(
    search_models: &[Json],
    catalog: &[Json],
    timestamp_source: &'static str,
) -> Result<(Vec<Json>, OrderingSummary), String> {
    let mut dates: HashMap<&str, &str> = HashMap::new();
    for model in catalog {
        if let (Some(name), Some(date)) = (
            model.get("name").and_then(Json::as_str),
            model.get("original_creation_time").and_then(Json::as_str),
        ) {
            dates.insert(name, date);
        }
    }
    let restored: Vec<Json> = search_models
        .iter()
        .map(|model| {
            let Json::Object(members) = model else {
                return model.clone();
            };
            let mut members: Vec<(String, Json)> = members
                .iter()
                .filter(|(key, _)| key != "original_creation_time")
                .cloned()
                .collect();
            let date = model
                .get("name")
                .and_then(Json::as_str)
                .and_then(|name| dates.get(name));
            if let Some(date) = date {
                members.push(("original_creation_time".to_string(), Json::string(*date)));
            }
            Json::Object(members)
        })
        .collect();
    let ordered = sort_models_newest_first(&restored)?;
    let summary = ordering_summary(&ordered, timestamp_source);
    Ok((ordered, summary))
}

/// The typed result of a public-Hub selection.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PublicHubSelection {
    pub(super) retrieval_backend: &'static str,
    pub(super) ordering: OrderingSummary,
    pub(super) warnings: Vec<String>,
    pub(super) models: Vec<Json>,
    pub(super) fallback_reason: Option<(&'static str, String)>,
}

impl PublicHubSelection {
    /// The members merged into the command output, in the dataclass order; an
    /// absent fallback reason is omitted.
    pub(super) fn output_members(&self) -> Vec<(String, Json)> {
        let mut members = vec![
            (
                "retrieval_backend".to_string(),
                Json::string(self.retrieval_backend),
            ),
            ("ordering".to_string(), self.ordering.to_json()),
            (
                "warnings".to_string(),
                Json::Array(
                    self.warnings
                        .iter()
                        .map(|w| Json::string(w.as_str()))
                        .collect(),
                ),
            ),
            ("models".to_string(), Json::Array(self.models.clone())),
        ];
        if let Some((kind, message)) = &self.fallback_reason {
            members.push((
                "fallback_reason".to_string(),
                Json::object(vec![
                    ("type", Json::string(*kind)),
                    ("message", Json::string(message.as_str())),
                ]),
            ));
        }
        members
    }
}

/// Uses Search for candidates with a visible, complete local fallback.
///
/// A successful empty Search is authoritative and does not fall back. Only an
/// unsupported client, a service or transport failure or a malformed response
/// uses the `ListHubContents` catalog; constraint and catalog problems stay
/// visible because falling back would hide a query or workflow defect.
///
/// # Errors
///
/// A constraint error, an unknown `catalog_source` or a malformed catalog.
pub(super) fn select_public_hub_models(
    hub_name: &str,
    constraints: &Constraints,
    catalog: &[Json],
    client: &mut dyn SageMakerClient,
    catalog_source: &str,
) -> Outcome<PublicHubSelection> {
    let (search_source, fallback_source): (&'static str, &'static str) = match catalog_source {
        "snapshot" => ("selection_session_snapshot", "list_hub_contents_snapshot"),
        "live_list" => ("live_list", "live_list"),
        other => {
            return constraint_error(format!(
                "Unsupported catalog source: {}",
                quote_untrusted(other)
            ))
        }
    };
    match search_deployable_models(hub_name, constraints, catalog, client) {
        Ok(search_models) => {
            let (models, ordering) = restore_catalog_order(&search_models, catalog, search_source)
                .map_err(SearchAdapterError::Data)?;
            let mut warnings = Vec::new();
            if ordering.undated_models > 0 {
                warnings.push(format!(
                    "{} of {} Search results had no cached OriginalCreationTime and were ordered \
                     after dated models.",
                    ordering.undated_models,
                    models.len()
                ));
            }
            Ok(PublicHubSelection {
                retrieval_backend: "search",
                ordering,
                warnings,
                models,
                fallback_reason: None,
            })
        }
        Err(
            error @ (SearchAdapterError::UnsupportedSdk(_)
            | SearchAdapterError::Service(_)
            | SearchAdapterError::Response(_)),
        ) => {
            let models = filter_models(catalog, constraints).map_err(SearchAdapterError::Data)?;
            Ok(PublicHubSelection {
                retrieval_backend: "list_fallback",
                ordering: ordering_summary(&models, fallback_source),
                warnings: vec![
                    "SageMaker Search was unavailable; used the complete ListHubContents snapshot \
                     and local filtering instead."
                        .to_string(),
                ],
                models,
                fallback_reason: Some((error.kind(), error.message().to_string())),
            })
        }
        Err(other) => Err(other),
    }
}
