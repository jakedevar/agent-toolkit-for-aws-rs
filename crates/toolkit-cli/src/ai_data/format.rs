//! JSONL format detection (`format_detector.py`).
//!
//! The detector samples the first bytes of a local file or of an S3 object,
//! keeps only complete lines, parses each as JSON, classifies the first parsed
//! record into one of 11 training/evaluation formats and validates every
//! parsed record against that format. The classification order and every
//! message text are those of the Python module; the intentional differences
//! (non-object records, the error cap, the sample bound) are in
//! `docs/migration/ai-data.md`.

use std::io::Read;
use std::path::Path;

use toolkit_core::{open_regular_file, parse_json, Json};
use toolkit_msk::pyfmt::py_str;

use super::{parse_failure_text, py_type_name};
use crate::manifests::is_falsy;

/// Default number of bytes sampled from the start of the input (1 MiB).
pub(crate) const DEFAULT_SAMPLE_BYTES: u64 = 1_048_576;
/// Largest accepted `--sample-size`. The sample is held in memory together
/// with its parsed records, so it is bounded.
pub(crate) const MAX_SAMPLE_BYTES: u64 = 16 * 1024 * 1024;
/// Most errors that are kept and reported; further ones are only counted.
pub(crate) const MAX_REPORTED_ERRORS: usize = 10_000;

const PARSE_ERROR: &str = "parse_error";
const MISSING_FIELD: &str = "missing_field";
const INVALID_STRUCTURE: &str = "invalid_structure";
const SCHEMA_MISMATCH: &str = "schema_mismatch";

/// The supported formats (`FormatType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatType {
    NovaSft,
    NovaDpo,
    NovaRlvr,
    GptOssSft,
    GptOssDpo,
    OpenWeightsSft,
    OpenWeightsSftConv,
    OpenWeightsDpo,
    Verl,
    VerlLegacy,
    SagemakerEval,
    Unknown,
}

impl FormatType {
    /// The published value (`nova_sft`, ...).
    pub(crate) fn value(self) -> &'static str {
        match self {
            Self::NovaSft => "nova_sft",
            Self::NovaDpo => "nova_dpo",
            Self::NovaRlvr => "nova_rlvr",
            Self::GptOssSft => "gpt_oss_sft",
            Self::GptOssDpo => "gpt_oss_dpo",
            Self::OpenWeightsSft => "open_weights_sft",
            Self::OpenWeightsSftConv => "open_weights_sft_conv",
            Self::OpenWeightsDpo => "open_weights_dpo",
            Self::Verl => "verl",
            Self::VerlLegacy => "verl_legacy",
            Self::SagemakerEval => "sagemaker_eval",
            Self::Unknown => "unknown",
        }
    }
}

/// Confidence of a detection (`ConfidenceLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confidence {
    High,
    Low,
    None,
}

impl Confidence {
    /// The JSON value (`high`, `low`, `none`).
    pub(crate) fn value(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
            Self::None => "none",
        }
    }

    /// The enum member name used by the text report (`HIGH`, ...).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::High => "HIGH",
            Self::Low => "LOW",
            Self::None => "NONE",
        }
    }
}

/// One problem found in a sampled line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidationError {
    pub(crate) line_number: usize,
    pub(crate) error_type: &'static str,
    pub(crate) message: String,
}

/// Result of a detection (`FormatDetectionResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormatDetectionResult {
    pub(crate) format_type: FormatType,
    pub(crate) is_valid: bool,
    pub(crate) lines_sampled: usize,
    /// At most [`MAX_REPORTED_ERRORS`] errors, in discovery order.
    pub(crate) errors: Vec<ValidationError>,
    /// Errors found beyond the reported ones.
    pub(crate) errors_omitted: usize,
    pub(crate) confidence: Confidence,
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// Complete lines of a sample: the bytes are decoded with replacement, cut
/// after the last newline (a partial last line is dropped, and a sample
/// without any newline yields nothing) and split on `\n`, skipping empty lines.
pub(crate) fn lines_from_sample(data: &[u8]) -> Vec<String> {
    if data.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(data);
    let Some(last_newline) = text.rfind('\n') else {
        return Vec::new();
    };
    text[..=last_newline]
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Samples the first `sample_size` bytes of a local file. Symbolic links and
/// non-regular files are refused; nothing beyond the sample is read.
pub(crate) fn sample_local_file(path: &Path, sample_size: u64) -> Result<Vec<String>, String> {
    let (file, _) = open_regular_file(path).map_err(|error| error.to_string())?;
    let mut data = Vec::new();
    file.take(sample_size)
        .read_to_end(&mut data)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(lines_from_sample(&data))
}

/// The ranged object read the S3 sampler needs. This build wires
/// [`NoObjectStore`]; tests inject a canned reader.
pub(crate) trait ObjectStore {
    /// Reads `range` (an HTTP `Range` value such as `bytes=0-1023`) of the
    /// object, returning its bytes.
    fn get_range(&self, bucket: &str, key: &str, range: &str) -> Result<Vec<u8>, String>;
}

/// The object store of a build without an S3 client.
pub(crate) struct NoObjectStore;

impl ObjectStore for NoObjectStore {
    fn get_range(&self, _bucket: &str, _key: &str, _range: &str) -> Result<Vec<u8>, String> {
        Err("S3 access is not available in this build; use a local file path.".to_string())
    }
}

/// Splits `s3://bucket/key` into bucket and key.
pub(crate) fn parse_s3_uri(uri: &str) -> Result<(&str, &str), String> {
    let Some(rest) = uri.strip_prefix("s3://") else {
        return Err(format!(
            "Invalid S3 URI: must start with 's3://' (got: {uri})"
        ));
    };
    match rest.split_once('/') {
        Some((bucket, key)) if !bucket.is_empty() && !key.is_empty() => Ok((bucket, key)),
        _ => Err(format!(
            "Invalid S3 URI: must contain bucket and key (got: {uri})"
        )),
    }
}

/// Samples the first `sample_size` bytes of an S3 object with a `Range` read.
pub(crate) fn sample_s3_file(
    uri: &str,
    sample_size: u64,
    store: &dyn ObjectStore,
) -> Result<Vec<String>, String> {
    let (bucket, key) = parse_s3_uri(uri)?;
    let Some(last_byte) = sample_size.checked_sub(1) else {
        return Err("Invalid sample size: an S3 range read needs at least 1 byte".to_string());
    };
    let mut data = store.get_range(bucket, key, &format!("bytes=0-{last_byte}"))?;
    // A reader that ignores the range must not widen the sample.
    if let Ok(limit) = usize::try_from(sample_size) {
        data.truncate(limit);
    }
    Ok(lines_from_sample(&data))
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

fn classify_nova_format(messages: &[Json]) -> FormatType {
    let Some(last) = messages.last() else {
        return FormatType::Unknown;
    };
    if !matches!(last, Json::Object(_)) {
        return FormatType::Unknown;
    }
    if last.get("candidates").is_some() {
        FormatType::NovaDpo
    } else if last
        .get("content")
        .is_some_and(|content| !is_falsy(content))
    {
        FormatType::NovaSft
    } else {
        FormatType::Unknown
    }
}

fn classify_messages_format(record: &Json) -> FormatType {
    let Some(Json::Array(messages)) = record.get("messages") else {
        return FormatType::Unknown;
    };
    let Some(first) = messages.first() else {
        return FormatType::Unknown;
    };
    if !matches!(first, Json::Object(_)) {
        return FormatType::Unknown;
    }
    match first.get("content") {
        Some(Json::Array(_)) => classify_nova_format(messages),
        Some(Json::String(_)) => FormatType::GptOssSft,
        _ => FormatType::Unknown,
    }
}

/// Classifies one record. A record that is not a JSON object is `Unknown`.
pub(crate) fn classify_schema(record: &Json) -> FormatType {
    if !matches!(record, Json::Object(_)) {
        return FormatType::Unknown;
    }
    let has = |name: &str| record.get(name).is_some();
    let prompt_is_list = matches!(record.get("prompt"), Some(Json::Array(_)));

    if has("query") && has("response") {
        return FormatType::SagemakerEval;
    }
    if has("prompt") && (has("reward_model") || has("extra_info")) && !has("completion") {
        return if prompt_is_list {
            FormatType::Verl
        } else {
            FormatType::VerlLegacy
        };
    }
    if has("messages") {
        return if has("reference_answer") {
            FormatType::NovaRlvr
        } else {
            classify_messages_format(record)
        };
    }
    if has("prompt") && has("chosen") && has("rejected") {
        return if prompt_is_list {
            FormatType::GptOssDpo
        } else {
            FormatType::OpenWeightsDpo
        };
    }
    if has("prompt") && has("completion") {
        return if prompt_is_list {
            FormatType::OpenWeightsSftConv
        } else {
            FormatType::OpenWeightsSft
        };
    }
    FormatType::Unknown
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Sink {
    errors: Vec<ValidationError>,
    total: usize,
    omitted: usize,
    parse_error_seen: bool,
}

impl Sink {
    fn push(&mut self, line_number: usize, error_type: &'static str, message: String) {
        self.total = self.total.saturating_add(1);
        if error_type == PARSE_ERROR {
            self.parse_error_seen = true;
        }
        if self.errors.len() < MAX_REPORTED_ERRORS {
            self.errors.push(ValidationError {
                line_number,
                error_type,
                message,
            });
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
}

#[derive(Clone, Copy)]
enum FieldType {
    List,
    Dict,
    Str,
}

impl FieldType {
    fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Dict => "dict",
            Self::Str => "str",
        }
    }

    fn accepts(self, value: &Json) -> bool {
        matches!(
            (self, value),
            (Self::List, Json::Array(_))
                | (Self::Dict, Json::Object(_))
                | (Self::Str, Json::String(_))
        )
    }
}

#[derive(Clone, Copy)]
enum MessageCheck {
    Nova { dpo: bool },
    Gpt,
    Rlvr,
}

type Required = &'static [(&'static str, FieldType)];

#[derive(Clone, Copy)]
enum Check {
    Fields {
        required: Required,
        prefix: &'static str,
        messages: Option<MessageCheck>,
    },
    Verl,
    VerlLegacy,
}

const MESSAGES_ONLY: Required = &[("messages", FieldType::List)];

fn check_for(format: FormatType) -> Option<Check> {
    let fields = |required: Required, prefix: &'static str| {
        Some(Check::Fields {
            required,
            prefix,
            messages: None,
        })
    };
    let with_messages = |required: Required, messages: MessageCheck| {
        Some(Check::Fields {
            required,
            prefix: "",
            messages: Some(messages),
        })
    };
    match format {
        FormatType::NovaSft => with_messages(MESSAGES_ONLY, MessageCheck::Nova { dpo: false }),
        FormatType::NovaDpo => with_messages(MESSAGES_ONLY, MessageCheck::Nova { dpo: true }),
        FormatType::NovaRlvr => with_messages(
            &[
                ("messages", FieldType::List),
                ("reference_answer", FieldType::Dict),
            ],
            MessageCheck::Rlvr,
        ),
        FormatType::GptOssSft => with_messages(MESSAGES_ONLY, MessageCheck::Gpt),
        FormatType::GptOssDpo => fields(
            &[
                ("prompt", FieldType::List),
                ("chosen", FieldType::List),
                ("rejected", FieldType::List),
            ],
            "GPT-OSS DPO",
        ),
        FormatType::OpenWeightsSft => fields(
            &[("prompt", FieldType::Str), ("completion", FieldType::Str)],
            "Open Weights SFT",
        ),
        FormatType::OpenWeightsSftConv => fields(
            &[("prompt", FieldType::List), ("completion", FieldType::List)],
            "Open Weights SFT Conv",
        ),
        FormatType::OpenWeightsDpo => fields(
            &[
                ("prompt", FieldType::Str),
                ("chosen", FieldType::Str),
                ("rejected", FieldType::Str),
            ],
            "Open Weights DPO",
        ),
        FormatType::SagemakerEval => fields(
            &[("query", FieldType::Str), ("response", FieldType::Str)],
            "SageMaker Eval",
        ),
        FormatType::Verl => Some(Check::Verl),
        FormatType::VerlLegacy => Some(Check::VerlLegacy),
        FormatType::Unknown => None,
    }
}

fn check_role(message: &Json, index: usize, line: usize, sink: &mut Sink) {
    match message.get("role") {
        None => sink.push(
            line,
            MISSING_FIELD,
            format!("Message {index} missing required field 'role'"),
        ),
        Some(role)
            if !role
                .as_str()
                .is_some_and(|name| matches!(name, "user" | "assistant" | "system")) =>
        {
            sink.push(
                line,
                INVALID_STRUCTURE,
                format!("Invalid role '{}' in message {index}", py_str(role)),
            );
        }
        Some(_) => {}
    }
}

fn check_candidates(candidates: &Json, index: usize, line: usize, sink: &mut Sink) {
    let Json::Array(items) = candidates else {
        sink.push(
            line,
            INVALID_STRUCTURE,
            format!(
                "DPO message {index} 'candidates' must be list, got {}",
                py_type_name(candidates)
            ),
        );
        return;
    };
    for (position, candidate) in items.iter().enumerate() {
        if !matches!(candidate, Json::Object(_)) {
            sink.push(
                line,
                INVALID_STRUCTURE,
                format!(
                    "DPO message {index} candidate {position} must be an object, got {}",
                    py_type_name(candidate)
                ),
            );
            continue;
        }
        match candidate.get("preferenceLabel") {
            None => sink.push(
                line,
                MISSING_FIELD,
                format!("DPO message {index} candidate {position} missing 'preferenceLabel'"),
            ),
            Some(label)
                if !label
                    .as_str()
                    .is_some_and(|name| matches!(name, "preferred" | "non-preferred")) =>
            {
                sink.push(
                    line,
                    INVALID_STRUCTURE,
                    format!(
                        "Invalid preferenceLabel '{}' in message {index} candidate {position}",
                        py_str(label)
                    ),
                );
            }
            Some(_) => {}
        }
    }
}

fn check_nova_message(message: &Json, index: usize, dpo: bool, line: usize, sink: &mut Sink) {
    let content = message.get("content");
    let candidates = message.get("candidates");
    if content.is_none() && candidates.is_none() {
        sink.push(
            line,
            MISSING_FIELD,
            format!("Message {index} missing 'content' or 'candidates'"),
        );
    }
    if let Some(content) = content.filter(|value| !matches!(value, Json::Array(_))) {
        sink.push(
            line,
            INVALID_STRUCTURE,
            format!(
                "Nova format content must be list, got {}",
                py_type_name(content)
            ),
        );
    }
    if dpo {
        if let Some(candidates) = candidates {
            check_candidates(candidates, index, line, sink);
        }
    }
}

fn check_text_message(message: &Json, index: usize, label: &str, line: usize, sink: &mut Sink) {
    match message.get("content") {
        None => sink.push(
            line,
            MISSING_FIELD,
            format!("Message {index} missing required field 'content'"),
        ),
        Some(content) if !matches!(content, Json::String(_)) => sink.push(
            line,
            INVALID_STRUCTURE,
            format!("{label} must be string, got {}", py_type_name(content)),
        ),
        Some(_) => {}
    }
}

fn validate_messages(check: MessageCheck, messages: &[Json], line: usize, sink: &mut Sink) {
    for (index, message) in messages.iter().enumerate() {
        if !matches!(message, Json::Object(_)) {
            sink.push(
                line,
                INVALID_STRUCTURE,
                format!(
                    "Message {index} must be an object, got {}",
                    py_type_name(message)
                ),
            );
            continue;
        }
        check_role(message, index, line, sink);
        match check {
            MessageCheck::Nova { dpo } => check_nova_message(message, index, dpo, line, sink),
            MessageCheck::Gpt => {
                check_text_message(message, index, "GPT-OSS format content", line, sink);
            }
            MessageCheck::Rlvr => {
                check_text_message(message, index, "Nova RLVR content", line, sink)
            }
        }
    }
}

fn validate_fields(
    record: &Json,
    line: usize,
    required: Required,
    prefix: &str,
    messages: Option<MessageCheck>,
    sink: &mut Sink,
) {
    let mut skip_messages = false;
    for &(field, expected) in required {
        match record.get(field) {
            None => {
                sink.push(
                    line,
                    MISSING_FIELD,
                    format!("Missing required field '{field}'"),
                );
                if field == "messages" {
                    skip_messages = true;
                }
            }
            Some(value) if !expected.accepts(value) => {
                let actual = py_type_name(value);
                let wanted = expected.name();
                if field == "messages" {
                    sink.push(
                        line,
                        INVALID_STRUCTURE,
                        "Field 'messages' must be a list".to_string(),
                    );
                    skip_messages = true;
                } else if prefix.is_empty() {
                    sink.push(
                        line,
                        INVALID_STRUCTURE,
                        format!("Field '{field}' must be {wanted}, got {actual}"),
                    );
                } else {
                    sink.push(
                        line,
                        INVALID_STRUCTURE,
                        format!("{prefix} field '{field}' must be {wanted}, got {actual}"),
                    );
                }
            }
            Some(_) => {}
        }
    }
    if skip_messages {
        return;
    }
    if let (Some(check), Some(Json::Array(items))) = (messages, record.get("messages")) {
        validate_messages(check, items, line, sink);
    }
}

fn check_reward_source(record: &Json, line: usize, sink: &mut Sink) {
    if record.get("reward_model").is_none() && record.get("extra_info").is_none() {
        sink.push(
            line,
            MISSING_FIELD,
            "Missing required field 'reward_model' or 'extra_info'".to_string(),
        );
    }
}

fn validate_verl(record: &Json, line: usize, sink: &mut Sink) {
    match record.get("prompt") {
        None => sink.push(
            line,
            MISSING_FIELD,
            "Missing required field 'prompt'".to_string(),
        ),
        Some(Json::Array(items)) => {
            for (index, message) in items.iter().enumerate() {
                let complete = matches!(message, Json::Object(_))
                    && message.get("role").is_some()
                    && message.get("content").is_some();
                if !complete {
                    sink.push(
                        line,
                        INVALID_STRUCTURE,
                        format!("Prompt message {index} must have 'role' and 'content'"),
                    );
                }
            }
        }
        Some(other) => sink.push(
            line,
            INVALID_STRUCTURE,
            format!(
                "Verl field 'prompt' must be list, got {}",
                py_type_name(other)
            ),
        ),
    }
    check_reward_source(record, line, sink);
}

fn validate_verl_legacy(record: &Json, line: usize, sink: &mut Sink) {
    match record.get("prompt") {
        None => sink.push(
            line,
            MISSING_FIELD,
            "Missing required field 'prompt'".to_string(),
        ),
        Some(Json::String(_)) => {}
        Some(other) => sink.push(
            line,
            INVALID_STRUCTURE,
            format!(
                "Verl Legacy field 'prompt' must be string, got {}",
                py_type_name(other)
            ),
        ),
    }
    check_reward_source(record, line, sink);
}

fn validate_samples(records: &[(usize, Json)], expected: FormatType, sink: &mut Sink) {
    let check = check_for(expected);
    for (line, record) in records {
        let line = *line;
        if !matches!(record, Json::Object(_)) {
            sink.push(
                line,
                INVALID_STRUCTURE,
                format!("Record must be a JSON object, got {}", py_type_name(record)),
            );
            continue;
        }
        let detected = classify_schema(record);
        if detected != expected {
            sink.push(
                line,
                SCHEMA_MISMATCH,
                format!(
                    "Expected {} but found {}",
                    expected.value(),
                    detected.value()
                ),
            );
            continue;
        }
        match check {
            None => {}
            Some(Check::Verl) => validate_verl(record, line, sink),
            Some(Check::VerlLegacy) => validate_verl_legacy(record, line, sink),
            Some(Check::Fields {
                required,
                prefix,
                messages,
            }) => validate_fields(record, line, required, prefix, messages, sink),
        }
    }
}

/// Detects and validates the format of already sampled lines.
pub(crate) fn detect_lines(lines: &[String]) -> FormatDetectionResult {
    let mut sink = Sink::default();
    let mut records: Vec<(usize, Json)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let line_number = index.saturating_add(1);
        match parse_json(line) {
            Ok(value) => records.push((line_number, value)),
            Err(error) => sink.push(
                line_number,
                PARSE_ERROR,
                format!("Invalid JSON: {}", parse_failure_text(&error)),
            ),
        }
    }

    let format_type = match records.first() {
        Some((_, first)) => classify_schema(first),
        None => FormatType::Unknown,
    };
    if !records.is_empty() {
        validate_samples(&records, format_type, &mut sink);
    }

    let confidence = if sink.total == 0 {
        Confidence::High
    } else if records.is_empty() || sink.parse_error_seen {
        Confidence::None
    } else {
        Confidence::Low
    };
    FormatDetectionResult {
        format_type,
        is_valid: sink.total == 0,
        lines_sampled: lines.len(),
        errors: sink.errors,
        errors_omitted: sink.omitted,
        confidence,
    }
}
