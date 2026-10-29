//! Constants, the region chain, the AWS profile region and the base and
//! issuer URL rules (`launch_config.py`; design 1.5, 2.1, R-TOK-OIDC).
//!
//! Every input is injected: the environment through `toolkit_aws::Env`, the
//! state directory and `HOME` as paths, DNS through `HostResolver`. Nothing
//! here reads the real environment.

use std::path::{Path, PathBuf};

use toolkit_aws::endpoint::is_non_public;
use toolkit_aws::env::non_empty;
use toolkit_aws::{Env, HostResolver};

use super::state;
use super::text::{repr, shown};
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
    SUPPORTED_REGIONS
        .iter()
        .find(|(name, _)| *name == region)
        .map(|(name, _)| *name)
        .ok_or_else(|| {
            LaunchError::Config(format!(
                "Unsupported region {}. Launch with AWS runs migrations in {}.",
                repr(region),
                supported_list()
            ))
        })
}

fn supported_list() -> String {
    SUPPORTED_REGIONS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The regional API endpoint.
///
/// # Errors
/// [`validate_region`]'s refusal.
pub fn base_url_for_region(region: &str) -> Result<String, LaunchError> {
    Ok(format!(
        "https://launch-with-aws.{}.api.aws",
        validate_region(region)?
    ))
}

/// The parts of a URL the rules look at, split the way Python's `urlparse`
/// does for these inputs.
struct UrlParts<'a> {
    /// Lower-cased scheme (empty without `scheme://`).
    scheme: String,
    /// The authority (`netloc`), raw.
    authority: &'a str,
    /// Everything after the authority (path, query, fragment).
    rest: &'a str,
}

impl<'a> UrlParts<'a> {
    fn split(url: &'a str) -> Self {
        let (scheme, after) = match url.split_once("://") {
            Some((scheme, after))
                if !scheme.is_empty()
                    && scheme
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
                    && scheme.as_bytes()[0].is_ascii_alphabetic() =>
            {
                (scheme.to_ascii_lowercase(), after)
            }
            _ => (String::new(), ""),
        };
        if scheme.is_empty() {
            return Self {
                scheme,
                authority: "",
                rest: url,
            };
        }
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        Self {
            scheme,
            authority: &after[..end],
            rest: &after[end..],
        }
    }

    /// Whether the authority carries userinfo.
    fn has_userinfo(&self) -> bool {
        self.authority.contains('@')
    }

    /// The authority without userinfo.
    fn host_port(&self) -> &'a str {
        self.authority
            .rsplit_once('@')
            .map_or(self.authority, |(_, host)| host)
    }

    /// Python `urlparse(url).hostname`: lower-cased, without userinfo, port
    /// or IPv6 brackets; `None` when empty.
    fn hostname(&self) -> Option<String> {
        let host_port = self.host_port();
        let host = if let Some(stripped) = host_port.strip_prefix('[') {
            stripped.split_once(']').map_or(stripped, |(host, _)| host)
        } else {
            host_port
                .rsplit_once(':')
                .map_or(host_port, |(host, _)| host)
        };
        (!host.is_empty()).then(|| host.to_lowercase())
    }

    /// The port text after the host, if any.
    fn port_text(&self) -> Option<&'a str> {
        let host_port = self.host_port();
        if host_port.starts_with('[') {
            return host_port.split_once("]:").map(|(_, port)| port);
        }
        host_port.rsplit_once(':').map(|(_, port)| port)
    }
}

/// The region of an AWS MCP endpoint URL, or `None` when the whole host is
/// not `aws-mcp.<region>.api.aws` (anchored, so lookalikes are no signal).
#[must_use]
pub fn region_from_aws_mcp_url(url: &str) -> Option<String> {
    let parts = UrlParts::split(url);
    if parts.scheme.is_empty() {
        return None;
    }
    let host = parts.hostname()?;
    let region = host.strip_prefix("aws-mcp.")?.strip_suffix(".api.aws")?;
    let valid = !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then(|| region.to_string())
}

/// The cell serving an AWS region: `eu-*` → [`EU_REGION`], else
/// [`DEFAULT_REGION`].
#[must_use]
pub fn cell_for_aws_region(aws_region: &str) -> &'static str {
    if aws_region.starts_with("eu-") {
        EU_REGION
    } else {
        DEFAULT_REGION
    }
}

/// Largest AWS config file read.
const MAX_AWS_CONFIG_BYTES: u64 = 1024 * 1024;

/// The AWS region from `AWS_REGION`, then `AWS_DEFAULT_REGION`, then the
/// `region` of the selected profile (`AWS_PROFILE`, then
/// `AWS_DEFAULT_PROFILE`, else `default`) in `AWS_CONFIG_FILE` or
/// `~/.aws/config`. A missing, unreadable or malformed file is no signal.
#[must_use]
pub fn profile_region(env: &dyn Env, home: Option<&Path>) -> Option<String> {
    if let Some(region) = non_empty(env, "AWS_REGION") {
        return Some(region);
    }
    if let Some(region) = non_empty(env, "AWS_DEFAULT_REGION") {
        return Some(region);
    }
    let profile = non_empty(env, "AWS_PROFILE")
        .or_else(|| non_empty(env, "AWS_DEFAULT_PROFILE"))
        .unwrap_or_else(|| "default".to_string());
    let path = match non_empty(env, "AWS_CONFIG_FILE") {
        Some(path) => expand_user(&path, home),
        None => home?.join(".aws").join("config"),
    };
    let text = read_small_text(&path)?;
    parse_profile_region(&text, &profile)
}

/// A UTF-8 file of at most [`MAX_AWS_CONFIG_BYTES`], or `None`.
fn read_small_text(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_AWS_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_AWS_CONFIG_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The `region` of `profile` in an AWS config file's text, with botocore's
/// `RawConfigParser` rules: `[default]` or `[profile NAME]` sections (a
/// `[profile default]` section wins over `[default]`), `key = value` or
/// `key: value`, keys case-insensitive, `#`/`;` full-line comments, indented
/// continuation lines. Any line that does not parse, a key before the first
/// section, or a duplicate section or key makes the whole file no signal.
#[must_use]
pub fn parse_profile_region(text: &str, profile: &str) -> Option<String> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            // A continuation line extends the previous value.
            let (_, options) = sections.last_mut()?;
            let (_, value) = options.last_mut()?;
            value.push('\n');
            value.push_str(trimmed);
            continue;
        }
        if let Some(header) = trimmed.strip_prefix('[') {
            let name = header.strip_suffix(']')?;
            if name.is_empty() || sections.iter().any(|(existing, _)| existing == name) {
                return None;
            }
            sections.push((name.to_string(), Vec::new()));
            continue;
        }
        let delimiter = trimmed.find(['=', ':'])?;
        let key = trimmed[..delimiter].trim().to_lowercase();
        let value = trimmed[delimiter + 1..].trim().to_string();
        if key.is_empty() {
            return None;
        }
        let (_, options) = sections.last_mut()?;
        if options.iter().any(|(existing, _)| *existing == key) {
            return None;
        }
        options.push((key, value));
    }
    let wanted = format!("profile {profile}");
    let section = sections
        .iter()
        .find(|(name, _)| *name == wanted)
        .or_else(|| {
            (profile == "default")
                .then(|| sections.iter().find(|(name, _)| name == "default"))
                .flatten()
        })?;
    section
        .1
        .iter()
        .find(|(key, _)| key == "region")
        .map(|(_, value)| value.clone())
        .filter(|value| !value.is_empty())
}

/// The region chain (design 2.1): `--region` → [`ENV_REGION`] → saved
/// `config.json` → EU `--aws-mcp-url` → [`profile_region`] mapped to a cell
/// → [`DEFAULT_REGION`].
///
/// # Errors
/// [`validate_region`]'s refusal of an explicit or environment value.
pub fn resolve_region(inputs: &RegionInputs<'_>) -> Result<ResolvedRegion, LaunchError> {
    let resolved = |region, source| ResolvedRegion { region, source };
    if let Some(explicit) = inputs.explicit.filter(|value| !value.is_empty()) {
        return Ok(resolved(validate_region(explicit)?, RegionSource::Explicit));
    }
    if let Some(value) = non_empty(inputs.env, ENV_REGION) {
        return Ok(resolved(
            validate_region(&value)?,
            RegionSource::Environment,
        ));
    }
    if let Some(saved) = inputs.state_dir.and_then(state::load_saved_region) {
        return Ok(resolved(saved, RegionSource::Saved));
    }
    if let Some(region) = inputs
        .aws_mcp_url
        .filter(|url| !url.is_empty())
        .and_then(region_from_aws_mcp_url)
    {
        if region.starts_with("eu-") {
            return Ok(resolved(EU_REGION, RegionSource::AwsMcp));
        }
    }
    if let Some(region) = profile_region(inputs.env, inputs.home) {
        return Ok(resolved(
            cell_for_aws_region(&region),
            RegionSource::AwsProfile,
        ));
    }
    Ok(resolved(DEFAULT_REGION, RegionSource::Default))
}

/// The checks the two override URLs share: scheme, userinfo, hostname,
/// suffix allowlist, port, characters and host syntax. Returns the
/// lower-cased hostname.
fn check_override_url(
    url: &str,
    field: &str,
    suffixes: &[&str],
    scheme_tail: &str,
    allowed_label: &str,
) -> Result<String, LaunchError> {
    let invalid = |reason: String| LaunchError::Config(format!("Invalid {field}: {reason}"));
    let parts = UrlParts::split(url);
    if parts.scheme != "https" {
        return Err(invalid(format!(
            "scheme must be https, got {}. {scheme_tail}",
            repr(&parts.scheme)
        )));
    }
    if parts.has_userinfo() {
        return Err(invalid(
            "userinfo (credentials) in the URL is not allowed.".to_string(),
        ));
    }
    let Some(host) = parts.hostname() else {
        return Err(invalid(format!("no hostname in {}.", repr(url))));
    };
    if !suffixes
        .iter()
        .any(|suffix| host == suffix.trim_start_matches('.') || host.ends_with(suffix))
    {
        return Err(invalid(format!(
            "host {} is not in the allowed {allowed_label} ({}). {}",
            repr(&host),
            suffixes.join(", "),
            if allowed_label == "domains" {
                "Only official AWS endpoints are permitted."
            } else {
                "Only official AWS IAM Identity Center endpoints are permitted."
            }
        )));
    }
    let host_ok = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
        });
    if !host_ok {
        return Err(invalid(format!("malformed hostname {}.", repr(&host))));
    }
    if let Some(port) = parts.port_text() {
        if !matches!(port.parse::<u16>(), Ok(port) if port != 0) {
            return Err(invalid(
                "port must be a number from 1 to 65535.".to_string(),
            ));
        }
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) || parts.rest.contains('\\') {
        return Err(invalid(
            "the URL contains whitespace, control characters or a backslash.".to_string(),
        ));
    }
    Ok(host)
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
    let host = check_override_url(
        url,
        ENV_BASE_URL,
        &BASE_URL_HOST_SUFFIXES,
        "Refusing to send credentials over a non-HTTPS connection.",
        "domains",
    )?;
    let invalid = |reason: String| LaunchError::Config(format!("Invalid {ENV_BASE_URL}: {reason}"));
    let addresses = resolver.resolve(&host).map_err(|error| {
        invalid(format!(
            "DNS resolution failed for {}: {}. Refusing to proceed with an unresolvable host.",
            repr(&host),
            shown(&error)
        ))
    })?;
    if addresses.is_empty() {
        return Err(invalid(format!(
            "DNS resolution failed for {}: no addresses. Refusing to proceed with an \
             unresolvable host.",
            repr(&host)
        )));
    }
    if let Some(address) = addresses.iter().find(|address| is_non_public(**address)) {
        return Err(invalid(format!(
            "host {} resolves to private/loopback/link-local address {address}. Refusing to send \
             credentials to a non-public address.",
            repr(&host)
        )));
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Validate [`ENV_IDC_ISSUER_URL`] (Python `validate_issuer_url`) with the
/// R-TOK-OIDC additions; returns it unchanged. No DNS check (parity).
///
/// # Errors
/// [`LaunchError::Config`] with the Python message shape.
pub fn validate_issuer_url(url: &str) -> Result<String, LaunchError> {
    check_override_url(
        url,
        ENV_IDC_ISSUER_URL,
        &ISSUER_HOST_SUFFIXES,
        "Refusing to anchor the authentication flow on a non-HTTPS issuer.",
        "issuer domains",
    )?;
    Ok(url.to_string())
}

/// The issuer URL and, for an override, the stderr warning line
/// `Using non-default IdC issuer URL: <shown>`.
///
/// # Errors
/// [`validate_issuer_url`]'s refusal.
pub fn resolve_issuer_url(env: &dyn Env) -> Result<(String, Option<String>), LaunchError> {
    match non_empty(env, ENV_IDC_ISSUER_URL) {
        Some(url) => {
            let validated = validate_issuer_url(&url)?;
            let warning = format!("Using non-default IdC issuer URL: {}", shown(&validated));
            Ok((validated, Some(warning)))
        }
        None => Ok((IDC_ISSUER_URL.to_string(), None)),
    }
}

/// `~` and `~/...` expanded from `home` (Python `expanduser`); `~user` stays
/// literal (recorded divergence).
#[must_use]
pub fn expand_user(path: &str, home: Option<&Path>) -> PathBuf {
    match (path, home) {
        ("~", Some(home)) => home.to_path_buf(),
        (path, Some(home)) if path.starts_with("~/") => home.join(&path[2..]),
        (path, _) => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests_config;
