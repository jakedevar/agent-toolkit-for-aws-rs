//! Signing scope, `AWS_REGION` metadata and the proxy configuration
//! (#4413 S3; upstream v1.7.0 `utils.get_service_name_and_region_from_endpoint`
//! and `determine_aws_region`, design 2.3 and 2.5).
//!
//! - Service: `--service`, else the endpoint host's service
//!   ([`signing_service_region`]: `bedrock-agentcore`, `<service>.<region>.api.aws`,
//!   else the first label).
//! - Signing region: `--region`, then the endpoint host's region, then a
//!   non-empty `AWS_REGION`. None is an error.
//! - `AWS_REGION` metadata (the server's default region): the session region
//!   (`AWS_DEFAULT_REGION`, then the selected profile's `region`), then
//!   `AWS_REGION`, then the endpoint host's region; a `--metadata AWS_REGION=`
//!   overrides it, and the client's own `_meta` keys win over both.
//! - `User-Agent` (Q3 ruling): a fixed product token; the MCP client's name
//!   and version are never added.

use toolkit_aws::{is_valid_region, signing_service_region, Env, Headers, SigningScope, Timeouts};

use super::cli::ProxyArgs;
use super::limits::{Limits, LimitsSpec};
use super::proxy::ProxyConfig;

/// The proxy's `User-Agent` (Q3: no client name or version).
pub const USER_AGENT: &str = concat!("agent-toolkit-mcp-proxy/", env!("CARGO_PKG_VERSION"));

/// Why no signing scope could be chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeError {
    /// No `--service` and the host yields none.
    NoService,
    /// No `--region`, none in the host, no `AWS_REGION`, or an invalid one.
    NoRegion,
}

impl ScopeError {
    /// The fixed message.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NoService => "no signing service: pass --service",
            Self::NoRegion => "no signing region: pass --region or set AWS_REGION",
        }
    }
}

fn non_empty(env: &dyn Env, name: &str) -> Option<String> {
    env.var(name).filter(|value| !value.trim().is_empty())
}

/// The signing scope for `host` (T-SCOPE).
///
/// # Errors
/// [`ScopeError`] when no service or no valid region is found.
pub fn signing_scope(
    host: &str,
    service: Option<&str>,
    region: Option<&str>,
    env: &dyn Env,
) -> Result<SigningScope, ScopeError> {
    let (host_service, host_region) = signing_service_region(host);
    let service = service.map_or(host_service, str::to_string);
    if service.is_empty() {
        return Err(ScopeError::NoService);
    }
    let region = region
        .map(str::to_string)
        .or(host_region)
        .or_else(|| non_empty(env, "AWS_REGION"))
        .ok_or(ScopeError::NoRegion)?;
    if !is_valid_region(&region) {
        return Err(ScopeError::NoRegion);
    }
    Ok(SigningScope { region, service })
}

/// The automatic `AWS_REGION` metadata value, if any.
#[must_use]
pub fn metadata_region(host: &str, profile_region: Option<&str>, env: &dyn Env) -> Option<String> {
    non_empty(env, "AWS_DEFAULT_REGION")
        .or_else(|| profile_region.map(str::to_string))
        .or_else(|| non_empty(env, "AWS_REGION"))
        .or_else(|| signing_service_region(host).1)
        .filter(|region| is_valid_region(region))
}

/// The protocol core's configuration from the command line.
///
/// # Errors
/// Never in practice: the arguments were checked by [`super::cli::parse`]; a
/// refusal here is reported as a usage error by the caller.
pub fn proxy_config(args: &ProxyArgs, auto_region: Option<String>) -> Result<ProxyConfig, String> {
    let mut config = ProxyConfig::new(&args.target.url());
    let mut headers = Headers::new();
    for (name, value) in &args.headers {
        headers
            .insert_sensitive(name, value)
            .map_err(|_| "header".to_string())?;
    }
    config.headers = headers;
    let mut metadata = Vec::new();
    if let Some(region) = auto_region {
        if !args.metadata.iter().any(|(key, _)| key == "AWS_REGION") {
            metadata.push(("AWS_REGION".to_string(), region));
        }
    }
    metadata.extend(args.metadata.iter().cloned());
    config.metadata = metadata;
    config.timeouts = Timeouts::new(args.connect_timeout, args.read_timeout)
        .map_err(|_| "timeouts".to_string())?;
    config.retries = args.retries;
    config.limits = Limits::new(LimitsSpec {
        request_timeout: args.timeout,
        tool_timeout: args.tool_timeout,
        ..LimitsSpec::from(Limits::default())
    })
    .map_err(|_| "limits".to_string())?;
    config.user_agent = Some(USER_AGENT.to_string());
    config.read_only = args.read_only;
    Ok(config)
}
