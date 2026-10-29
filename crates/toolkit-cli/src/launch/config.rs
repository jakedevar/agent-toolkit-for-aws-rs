//! Constants, the region chain, the AWS profile region and the base and
//! issuer URL rules (`launch_config.py`; design 1.5, 2.1, R-TOK-OIDC).
//!
//! Every input is injected: the environment through `toolkit_aws::Env`, the
//! state directory and `HOME` as paths, DNS through `HostResolver`. Nothing
//! here reads the real environment.

use std::path::{Path, PathBuf};

use toolkit_aws::{Env, HostResolver};

use super::LaunchError;

/// The default cell.
pub const DEFAULT_REGION: &str = "us-east-1";
/// The EU cell.
pub const EU_REGION: &str = "eu-central-1";
/// Regions the service runs migrations in, with the names shown to the
/// customer, in the Python dictionary order.
pub const SUPPORTED_REGIONS: [(&str, &str); 2] = [
    (DEFAULT_REGION, "US East (N. Virginia)"),
    (EU_REGION, "Europe (Frankfurt)"),
];

/// API endpoint override.
pub const ENV_BASE_URL: &str = "LAUNCH_WITH_AWS_BASE_URL";
/// IAM Identity Center issuer override.
pub const ENV_IDC_ISSUER_URL: &str = "LAUNCH_WITH_AWS_IDC_ISSUER_URL";
/// OIDC scopes override (comma list, L3).
pub const ENV_SCOPES: &str = "LAUNCH_WITH_AWS_SCOPES";
/// Region override, the second signal after `--region`.
pub const ENV_REGION: &str = "LAUNCH_WITH_AWS_REGION";

/// The default issuer.
pub const IDC_ISSUER_URL: &str = "https://view.awsapps.com/start";
/// Hosts accepted for [`ENV_BASE_URL`] (the Python `_ALLOWED_HOST_SUFFIXES`).
pub const BASE_URL_HOST_SUFFIXES: [&str; 2] = [".api.aws", ".amazonaws.com"];
/// Hosts accepted for [`ENV_IDC_ISSUER_URL`].
pub const ISSUER_HOST_SUFFIXES: [&str; 2] = [".amazonaws.com", ".awsapps.com"];

/// Where a resolved region came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionSource {
    /// `--region`.
    Explicit,
    /// [`ENV_REGION`].
    Environment,
    /// `config.json` in the state directory.
    Saved,
    /// An EU `aws-mcp.<region>.api.aws` endpoint from `--aws-mcp-url`.
    AwsMcp,
    /// `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile `region`.
    AwsProfile,
    /// Nothing else applied.
    Default,
}

impl RegionSource {
    /// The JSON `source` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Environment => "environment",
            Self::Saved => "saved",
            Self::AwsMcp => "aws-mcp",
            Self::AwsProfile => "aws-profile",
            Self::Default => "default",
        }
    }

    /// The fixed `sourceLabel` shown in the upload confirmation.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "the region you asked for",
            Self::Environment => "your LAUNCH_WITH_AWS_REGION setting",
            Self::Saved => "your saved choice",
            Self::AwsMcp => "your AWS MCP server configuration",
            Self::AwsProfile => "your AWS profile region",
            Self::Default => "the default",
        }
    }
}

/// A resolved region and the signal it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRegion {
    /// One of [`SUPPORTED_REGIONS`].
    pub region: &'static str,
    /// The signal.
    pub source: RegionSource,
}

impl ResolvedRegion {
    /// The customer-facing region name.
    #[must_use]
    pub fn region_name(&self) -> &'static str {
        region_name(self.region).unwrap_or("")
    }
}

/// The inputs of [`resolve_region`].
#[derive(Clone, Copy)]
pub struct RegionInputs<'a> {
    /// `--region`, when given (an empty value counts as not given).
    pub explicit: Option<&'a str>,
    /// `--aws-mcp-url`, when given.
    pub aws_mcp_url: Option<&'a str>,
    /// The environment.
    pub env: &'a dyn Env,
    /// The state directory holding `config.json`, when it could be located.
    pub state_dir: Option<&'a Path>,
    /// `HOME`, for `~/.aws/config`.
    pub home: Option<&'a Path>,
}

/// The customer-facing name of a supported region.
#[must_use]
pub fn region_name(region: &str) -> Option<&'static str> {
    SUPPORTED_REGIONS
        .iter()
        .find(|(name, _)| *name == region)
        .map(|(_, display)| *display)
}

/// `region` when the service runs migrations there.
///
/// # Errors
/// [`LaunchError::Config`] naming the shown value and the supported regions.
pub fn validate_region(region: &str) -> Result<&'static str, LaunchError> {
    let _ = region;
    todo!("L1: validate_region")
}

/// The regional API endpoint.
///
/// # Errors
/// [`validate_region`]'s refusal.
pub fn base_url_for_region(region: &str) -> Result<String, LaunchError> {
    let _ = region;
    todo!("L1: base_url_for_region")
}

/// The region of an AWS MCP endpoint URL, or `None` when the whole host is
/// not `aws-mcp.<region>.api.aws` (anchored, so lookalikes are no signal).
#[must_use]
pub fn region_from_aws_mcp_url(url: &str) -> Option<String> {
    let _ = url;
    todo!("L1: aws-mcp host")
}

/// The cell serving an AWS region: `eu-*` → [`EU_REGION`], else
/// [`DEFAULT_REGION`].
#[must_use]
pub fn cell_for_aws_region(aws_region: &str) -> &'static str {
    let _ = aws_region;
    todo!("L1: cell")
}

/// The AWS region from `AWS_REGION`, then `AWS_DEFAULT_REGION`, then the
/// `region` of the selected profile (`AWS_PROFILE`, then
/// `AWS_DEFAULT_PROFILE`, else `default`) in `AWS_CONFIG_FILE` or
/// `~/.aws/config`. A missing, unreadable or malformed file is no signal.
#[must_use]
pub fn profile_region(env: &dyn Env, home: Option<&Path>) -> Option<String> {
    let _ = (env, home);
    todo!("L1: profile region")
}

/// The `region` of `profile` in an AWS config file's text, with botocore's
/// `RawConfigParser` rules: `[default]` or `[profile NAME]` sections (a
/// `[profile default]` section wins over `[default]`), `key = value` or
/// `key: value`, keys case-insensitive, `#`/`;` full-line comments, indented
/// continuation lines. Any line that does not parse, a key before the first
/// section, or a duplicate section or key makes the whole file no signal.
#[must_use]
pub fn parse_profile_region(text: &str, profile: &str) -> Option<String> {
    let _ = (text, profile);
    todo!("L1: profile parser")
}

/// The region chain (design 2.1): `--region` → [`ENV_REGION`] → saved
/// `config.json` → EU `--aws-mcp-url` → [`profile_region`] mapped to a cell
/// → [`DEFAULT_REGION`].
///
/// # Errors
/// [`validate_region`]'s refusal of an explicit or environment value.
pub fn resolve_region(inputs: &RegionInputs<'_>) -> Result<ResolvedRegion, LaunchError> {
    let _ = inputs;
    todo!("L1: region chain")
}

/// Validate [`ENV_BASE_URL`] (Python `_validate_base_url`): `https`, a host
/// equal to or ending in an allowed suffix, and only public DNS answers;
/// additionally (D8-style hardening) no userinfo, a port in 1..=65535, no
/// whitespace or control characters and a well-formed host. Returns the URL
/// without trailing `/`. The URL itself is never echoed raw: messages name
/// only shown (redacted, bounded, escaped) parts.
///
/// # Errors
/// [`LaunchError::Config`] with the Python message shape
/// `Invalid LAUNCH_WITH_AWS_BASE_URL: ...`.
pub fn validate_base_url(url: &str, resolver: &dyn HostResolver) -> Result<String, LaunchError> {
    let _ = (url, resolver);
    todo!("L1: base url")
}

/// Validate [`ENV_IDC_ISSUER_URL`] (Python `validate_issuer_url`) with the
/// R-TOK-OIDC additions; returns it unchanged. No DNS check (parity).
///
/// # Errors
/// [`LaunchError::Config`] with the Python message shape.
pub fn validate_issuer_url(url: &str) -> Result<String, LaunchError> {
    let _ = url;
    todo!("L1: issuer url")
}

/// The issuer URL and, for an override, the stderr warning line
/// `Using non-default IdC issuer URL: <shown>`.
///
/// # Errors
/// [`validate_issuer_url`]'s refusal.
pub fn resolve_issuer_url(env: &dyn Env) -> Result<(String, Option<String>), LaunchError> {
    let _ = env;
    todo!("L1: issuer chain")
}

/// `~` and `~/...` expanded from `home` (Python `expanduser`); `~user` stays
/// literal (recorded divergence).
#[must_use]
pub fn expand_user(path: &str, home: Option<&Path>) -> PathBuf {
    let _ = (path, home);
    todo!("L1: expand_user")
}

#[cfg(test)]
mod tests_config;
