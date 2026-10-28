//! Capture configuration for dynamic instrumentation (`di_capture.py`).
//!
//! Mirrors the location design: the two `CaptureConfiguration` variants the
//! API exposes plus an [`UnknownCapture`] fallback so renderers stay
//! forward-compatible. The type owns the API shape (`to_api_payload`,
//! [`capture_from_response`]); prose rendering stays in the renderers.
//!
//! The distinction between an *omitted* list (`None`: key absent from the
//! payload) and a *present-but-empty* list (`Some(vec![])`: key emitted as
//! `[]`) survives a parse -> payload -> parse round trip. The ADT does not
//! assert what the backend means by either shape.

use toolkit_core::Json;

use crate::dynamodb::pyfmt;

/// Optional size caps applied to a code-capture payload. A `null` value
/// means "not set"; values are JSON because a response may carry any type.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureLimits {
    /// `MaxHits`.
    pub max_hits: Json,
    /// `MaxStringLength`.
    pub max_string_length: Json,
    /// `MaxCollectionWidth`.
    pub max_collection_width: Json,
    /// `MaxCollectionDepth`.
    pub max_collection_depth: Json,
    /// `MaxStackFrames`.
    pub max_stack_frames: Json,
    /// `MaxStackTraceSize`.
    pub max_stack_trace_size: Json,
    /// `MaxObjectDepth`.
    pub max_object_depth: Json,
    /// `MaxFieldsPerObject`.
    pub max_fields_per_object: Json,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self::unset()
    }
}

impl CaptureLimits {
    /// No limit set.
    #[must_use]
    pub fn unset() -> Self {
        Self {
            max_hits: Json::Null,
            max_string_length: Json::Null,
            max_collection_width: Json::Null,
            max_collection_depth: Json::Null,
            max_stack_frames: Json::Null,
            max_stack_trace_size: Json::Null,
            max_object_depth: Json::Null,
            max_fields_per_object: Json::Null,
        }
    }

    /// Builds limits from optional integers (`None` = not set), in the
    /// order of the create operation's parameters.
    #[must_use]
    pub fn from_values(values: [Option<i64>; 8]) -> Self {
        let json = |value: Option<i64>| value.map_or(Json::Null, Json::Int);
        Self {
            max_hits: json(values[0]),
            max_string_length: json(values[1]),
            max_collection_width: json(values[2]),
            max_collection_depth: json(values[3]),
            max_stack_frames: json(values[4]),
            max_stack_trace_size: json(values[5]),
            max_object_depth: json(values[6]),
            max_fields_per_object: json(values[7]),
        }
    }

    /// Each limit as `(API key, value)`, in payload order.
    #[must_use]
    pub fn entries(&self) -> [(&'static str, &Json); 8] {
        [
            ("MaxHits", &self.max_hits),
            ("MaxStringLength", &self.max_string_length),
            ("MaxCollectionWidth", &self.max_collection_width),
            ("MaxCollectionDepth", &self.max_collection_depth),
            ("MaxStackFrames", &self.max_stack_frames),
            ("MaxStackTraceSize", &self.max_stack_trace_size),
            ("MaxObjectDepth", &self.max_object_depth),
            ("MaxFieldsPerObject", &self.max_fields_per_object),
        ]
    }

    /// True when no capture limit is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries()
            .iter()
            .all(|(_, value)| **value == Json::Null)
    }

    /// The set limits as the `CaptureLimits` payload object.
    #[must_use]
    pub fn to_api_payload(&self) -> Json {
        Json::Object(
            self.entries()
                .into_iter()
                .filter(|(_, value)| **value != Json::Null)
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
        )
    }
}

/// Capture configuration for BREAKPOINT and PROBE.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeCapture {
    /// `CaptureReturn`.
    pub capture_return: bool,
    /// `CaptureStackTrace`.
    pub capture_stack_trace: bool,
    /// `CaptureArguments`: `None` omits the key, `Some(vec![])` emits `[]`.
    pub capture_arguments: Option<Vec<String>>,
    /// `CaptureLocals`: same distinction as `capture_arguments`.
    pub capture_locals: Option<Vec<String>>,
    /// `CaptureLimits`.
    pub limits: CaptureLimits,
}

impl CodeCapture {
    /// The `CodeCapture` create-request payload.
    #[must_use]
    pub fn to_api_payload(&self) -> Json {
        let mut config: Vec<(String, Json)> = vec![
            ("CaptureReturn".to_string(), Json::Bool(self.capture_return)),
            (
                "CaptureStackTrace".to_string(),
                Json::Bool(self.capture_stack_trace),
            ),
            ("CaptureLimits".to_string(), self.limits.to_api_payload()),
        ];
        if let Some(arguments) = &self.capture_arguments {
            config.push((
                "CaptureArguments".to_string(),
                super::pytext::string_array(arguments),
            ));
        }
        if let Some(locals) = &self.capture_locals {
            config.push((
                "CaptureLocals".to_string(),
                super::pytext::string_array(locals),
            ));
        }
        Json::Object(vec![("CodeCapture".to_string(), Json::Object(config))])
    }
}

/// A `CaptureConfiguration` union that did not match a known variant.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownCapture {
    /// The raw members of the union.
    pub raw: Vec<(String, Json)>,
}

/// Any capture configuration a renderer can meet.
#[derive(Debug, Clone, PartialEq)]
pub enum Capture {
    /// A code capture.
    Code(CodeCapture),
    /// An unrecognized union.
    Unknown(UnknownCapture),
}

const CODE_CAPTURE_HINT_KEYS: [&str; 4] = [
    "CaptureReturn",
    "CaptureLimits",
    "CaptureArguments",
    "CaptureStackTrace",
];

/// Parses a `CaptureConfiguration` union returned by the API.
///
/// A CodeCapture-shaped object without the `CodeCapture` wrapper key is
/// still read as a code capture (the legacy `extract_capture_variant`
/// fallback some response shapes relied on).
#[must_use]
pub fn capture_from_response(union: &Json) -> Capture {
    // recorded-shape: display-only union parse; Gateway::call shape-checks a response's CaptureConfiguration first (#4749).
    let Json::Object(members) = union else {
        return Capture::Unknown(UnknownCapture { raw: Vec::new() });
    };

    // recorded-shape: display-only union parse; Gateway::call shape-checks a response's CaptureConfiguration first (#4749).
    if let Some(code @ Json::Object(_)) = union.get("CodeCapture") {
        return Capture::Code(code_capture_from(code));
    }

    if CODE_CAPTURE_HINT_KEYS
        .iter()
        .any(|key| union.get(key).is_some())
    {
        return Capture::Code(code_capture_from(union));
    }

    Capture::Unknown(UnknownCapture {
        raw: members.clone(),
    })
}

/// A name list as the renderers show it: an array of values rendered with
/// `str()`. A missing key, a `null` and a non-array all read as "not set".
fn names_from(payload: &Json, key: &str) -> Option<Vec<String>> {
    match payload.get(key) {
        // recorded-shape: display-only union parse; Gateway::call shape-checks a response's CaptureConfiguration first (#4749).
        Some(Json::Array(items)) => Some(items.iter().map(pyfmt::text).collect()),
        _ => None,
    }
}

fn code_capture_from(payload: &Json) -> CodeCapture {
    let raw_limits = match payload.get("CaptureLimits") {
        // recorded-shape: display-only union parse; Gateway::call shape-checks a response's CaptureConfiguration first (#4749).
        Some(limits @ Json::Object(_)) => limits,
        _ => &Json::Null,
    };
    let limit = |key: &str| raw_limits.get(key).cloned().unwrap_or(Json::Null);
    CodeCapture {
        capture_return: payload.get("CaptureReturn").is_some_and(pyfmt::truthy),
        capture_stack_trace: payload.get("CaptureStackTrace").is_some_and(pyfmt::truthy),
        capture_arguments: names_from(payload, "CaptureArguments"),
        capture_locals: names_from(payload, "CaptureLocals"),
        limits: CaptureLimits {
            max_hits: limit("MaxHits"),
            max_string_length: limit("MaxStringLength"),
            max_collection_width: limit("MaxCollectionWidth"),
            max_collection_depth: limit("MaxCollectionDepth"),
            max_stack_frames: limit("MaxStackFrames"),
            max_stack_trace_size: limit("MaxStackTraceSize"),
            max_object_depth: limit("MaxObjectDepth"),
            max_fields_per_object: limit("MaxFieldsPerObject"),
        },
    }
}
