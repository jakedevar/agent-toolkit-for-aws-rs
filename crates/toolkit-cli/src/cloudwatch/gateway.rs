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
}

/// Wraps any failure of an `application-signals` call. The failure keeps
/// what `render_client_error` needs (code, message, `str(exc)`).
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayError {
    /// The classified failure.
    pub failure: AwsFailure,
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
            Ok(response @ Json::Object(_)) => Ok(response),
            Ok(other) => Err(GatewayError {
                failure: AwsFailure::Other(format!(
                    "the {name} response is not an object (it is {})",
                    other.type_name()
                )),
            }),
            Err(error) => Err(GatewayError {
                failure: failure_of(name, &error),
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
