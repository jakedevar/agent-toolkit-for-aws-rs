//! Gateway to the AWS `application-signals` API (`di_gateway.py`).
//!
//! The single seam where the dynamic-instrumentation tools touch the SDK.
//! Each operation issues one call, wraps any failure in a [`GatewayError`]
//! and lets the caller render it through [`render_error`]. The Python module
//! kept an allowlist (`_ALLOWED_OPERATIONS`) next to a literal dispatch so the
//! seam could not become an arbitrary-method dispatcher; here the allowlist
//! is the closed [`AppSignalsOp`] enum, and a tool can only name one of its
//! six variants.

use toolkit_core::Json;

use super::aws_client::{failure_of, AwsCalls, Service};
use super::errors::{render_failure, AwsFailure, Context, Prose};
use super::response_shape::{check, Field, Shape};

/// The members of a code location (`CodeLocation`, or the same members at
/// the top of a `Location` union).
const CODE_LOCATION: &[Field] = &[
    ("Language", Shape::Text),
    ("FilePath", Shape::Text),
    ("CodeUnit", Shape::Text),
    ("ClassName", Shape::Text),
    ("MethodName", Shape::Text),
    ("LineNumber", Shape::Int),
];

/// A `Location` union.
const LOCATION: &[Field] = &[
    ("CodeLocation", Shape::Object(CODE_LOCATION)),
    ("Language", Shape::Text),
    ("FilePath", Shape::Text),
    ("CodeUnit", Shape::Text),
    ("ClassName", Shape::Text),
    ("MethodName", Shape::Text),
    ("LineNumber", Shape::Int),
];

const CAPTURE_LIMITS: &[Field] = &[
    ("MaxHits", Shape::Int),
    ("MaxStringLength", Shape::Int),
    ("MaxCollectionWidth", Shape::Int),
    ("MaxCollectionDepth", Shape::Int),
    ("MaxStackFrames", Shape::Int),
    ("MaxStackTraceSize", Shape::Int),
    ("MaxObjectDepth", Shape::Int),
    ("MaxFieldsPerObject", Shape::Int),
];

const CODE_CAPTURE: &[Field] = &[
    ("CaptureReturn", Shape::Bool),
    ("CaptureStackTrace", Shape::Bool),
    ("CaptureArguments", Shape::StringList),
    ("CaptureLocals", Shape::StringList),
    ("CaptureLimits", Shape::Object(CAPTURE_LIMITS)),
];

/// A `CaptureConfiguration` union.
const CAPTURE: &[Field] = &[
    ("CodeCapture", Shape::Object(CODE_CAPTURE)),
    ("CaptureReturn", Shape::Bool),
    ("CaptureStackTrace", Shape::Bool),
    ("CaptureArguments", Shape::StringList),
    ("CaptureLocals", Shape::StringList),
    ("CaptureLimits", Shape::Object(CAPTURE_LIMITS)),
];

/// One instrumentation configuration (`Configuration`, an item of
/// `LatestConfigurations`).
const CONFIGURATION: &[Field] = &[
    ("InstrumentationType", Shape::Text),
    ("SignalType", Shape::Text),
    ("LocationHash", Shape::Text),
    ("Location", Shape::Object(LOCATION)),
    ("CaptureConfiguration", Shape::Object(CAPTURE)),
    ("AttributeFilters", Shape::TextMapList),
    ("Description", Shape::Text),
    ("CreatedAt", Shape::Timestamp),
    ("ExpiresAt", Shape::Timestamp),
    ("ARN", Shape::Text),
];

const DELETION: &[Field] = &[
    ("ResourceArn", Shape::Text),
    ("SignalType", Shape::Text),
    ("LocationHash", Shape::Text),
];

const DELETION_ERROR: &[Field] = &[
    ("ResourceArn", Shape::Text),
    ("Code", Shape::Text),
    ("Message", Shape::Text),
];

const STATUS_EVENT: &[Field] = &[
    ("Time", Shape::Timestamp),
    ("Status", Shape::Text),
    ("ErrorCause", Shape::Text),
];

/// The `application-signals` operations the tools may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSignalsOp {
    /// `CreateInstrumentationConfiguration` (mutating).
    Create,
    /// `ListInstrumentationConfigurations`.
    List,
    /// `GetInstrumentationConfiguration`.
    Get,
    /// `DeleteInstrumentationConfiguration` (mutating).
    Delete,
    /// `BatchDeleteInstrumentationConfigurations` (mutating).
    BatchDelete,
    /// `GetInstrumentationConfigurationStatus`.
    GetStatus,
}

impl AppSignalsOp {
    /// The whole allowlist.
    pub const ALL: [AppSignalsOp; 6] = [
        Self::Create,
        Self::List,
        Self::Get,
        Self::Delete,
        Self::BatchDelete,
        Self::GetStatus,
    ];

    /// The API operation name.
    #[must_use]
    pub fn api_name(self) -> &'static str {
        match self {
            Self::Create => "CreateInstrumentationConfiguration",
            Self::List => "ListInstrumentationConfigurations",
            Self::Get => "GetInstrumentationConfiguration",
            Self::Delete => "DeleteInstrumentationConfiguration",
            Self::BatchDelete => "BatchDeleteInstrumentationConfigurations",
            Self::GetStatus => "GetInstrumentationConfigurationStatus",
        }
    }

    /// True for the operations that change live resources.
    #[must_use]
    pub fn is_mutating(self) -> bool {
        matches!(self, Self::Create | Self::Delete | Self::BatchDelete)
    }

    /// The response members the tools read from this operation, with their
    /// shapes (#4749). [`Gateway::call`] checks them before any result is
    /// formed, so a malformed listing, event list or deletion report is a
    /// failed call, never an empty or successful result.
    #[must_use]
    pub fn response_fields(self) -> &'static [Field] {
        match self {
            Self::Create => &[
                ("LocationHash", Shape::Text),
                ("ARN", Shape::Text),
                ("CreatedAt", Shape::Timestamp),
                ("ExpiresAt", Shape::Timestamp),
            ],
            Self::List => &[
                ("LatestConfigurations", Shape::ObjectList(CONFIGURATION)),
                ("SyncedAt", Shape::Timestamp),
                ("NextToken", Shape::NullableText),
            ],
            Self::Get => &[("Configuration", Shape::Object(CONFIGURATION))],
            Self::Delete => &[],
            Self::BatchDelete => &[
                ("DeletedCount", Shape::Int),
                ("SuccessfulDeletions", Shape::ObjectList(DELETION)),
                ("Errors", Shape::ObjectList(DELETION_ERROR)),
            ],
            Self::GetStatus => &[
                ("Events", Shape::ObjectList(STATUS_EVENT)),
                ("Service", Shape::Text),
                ("Environment", Shape::Text),
                ("SignalType", Shape::Text),
                ("Status", Shape::Text),
                ("Location", Shape::Object(LOCATION)),
                ("LocationHash", Shape::Text),
                ("NextToken", Shape::NullableText),
            ],
        }
    }
}

/// Wraps any failure of an `application-signals` call. The failure keeps
/// what `render_client_error` needs (code, message, `str(exc)`).
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayError {
    /// The classified failure.
    pub failure: AwsFailure,
    /// True when the call answered but its response failed the shape check
    /// (#4818): a caller may treat a service error as best effort, but never
    /// an invalid response.
    pub invalid_response: bool,
}

/// The gateway over an injected client.
pub struct Gateway<'a> {
    client: &'a mut dyn AwsCalls,
}

impl<'a> Gateway<'a> {
    /// A gateway that sends through `client`.
    pub fn new(client: &'a mut dyn AwsCalls) -> Self {
        Self { client }
    }

    /// Calls one allowlisted operation.
    ///
    /// # Errors
    ///
    /// A [`GatewayError`] for a failed call or for a response that is not a
    /// JSON object.
    pub fn call(&mut self, op: AppSignalsOp, request: &Json) -> Result<Json, GatewayError> {
        let name = op.api_name();
        match self.client.call(Service::ApplicationSignals, name, request) {
            // recorded-shape: the response envelope must be an object; every
            // other shape is refused, and its members are checked next.
            Ok(response @ Json::Object(_)) => match check(&response, op.response_fields()) {
                Ok(()) => Ok(response),
                Err(shape) => Err(GatewayError {
                    failure: AwsFailure::Other(format!("the {name} response is invalid: {shape}")),
                    invalid_response: true,
                }),
            },
            Ok(other) => Err(GatewayError {
                failure: AwsFailure::Other(format!(
                    "the {name} response is not an object (it is {})",
                    other.type_name()
                )),
                invalid_response: true,
            }),
            Err(error) => Err(GatewayError {
                failure: failure_of(name, &error),
                invalid_response: false,
            }),
        }
    }
}

/// The tool-tailored text of a failed call (`render_error`).
///
/// A `ClientError` with any tailored prose gets the `Failed to ...` template;
/// every other class (and a plain `ClientError`) gets the canned bullets of
/// `translate_aws_error`.
#[must_use]
pub fn render_error(
    error: &GatewayError,
    action: &str,
    attempted: Context<'_>,
    prose: &Prose<'_>,
) -> String {
    render_failure(&error.failure, action, attempted, prose)
}
