//! Region, profile and endpoint resolution into one [`ClientConfig`].
//!
//! Policy carried over from the Python `di_region` / `di_session`:
//!
//! - region: explicit flag, then `AWS_REGION`, then `AWS_DEFAULT_REGION`, then
//!   `us-east-1`; an empty value counts as unset; the profile's configured
//!   region is **ignored** so the target region is always explicit;
//! - `AWS_PROFILE` selects credentials only (the SDK binding applies it to the
//!   provider chain; it never selects the region);
//! - endpoint overrides: explicit, then `AWS_ENDPOINT_URL_<SERVICE>`, then
//!   `AWS_ENDPOINT_URL`, each validated by [`validate_endpoint`].
//!
//! Intentional strictness over Python: the resolved region must look like an AWS
//! region name, so a typo fails here instead of building a bogus host.

use std::fmt;

use crate::endpoint::{
    is_valid_region, is_valid_service_label, validate_endpoint, Endpoint, EndpointPolicy,
    HostResolver,
};
use crate::env::{non_empty, Env};
use crate::error::AwsError;
use crate::redact::{snippet, write_scrubbed_debug, Scrubbed};
use crate::retry::RetryPolicy;
use crate::transport::{SigningScope, Timeouts};

/// Region used when nothing selects one (the Python default).
pub const DEFAULT_REGION: &str = "us-east-1";
/// Primary region variable.
pub const ENV_REGION: &str = "AWS_REGION";
/// Secondary region variable (boto3 honors both).
pub const ENV_DEFAULT_REGION: &str = "AWS_DEFAULT_REGION";
/// Credential profile variable.
pub const ENV_PROFILE: &str = "AWS_PROFILE";
/// Global endpoint override variable.
pub const ENV_ENDPOINT_URL: &str = "AWS_ENDPOINT_URL";

/// Where the region came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionSource {
    /// A flag or caller-supplied value.
    Explicit,
    /// `AWS_REGION`.
    AwsRegion,
    /// `AWS_DEFAULT_REGION`.
    AwsDefaultRegion,
    /// The built-in default.
    Default,
}

impl fmt::Display for RegionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Explicit => "the region you asked for",
            Self::AwsRegion => "AWS_REGION",
            Self::AwsDefaultRegion => "AWS_DEFAULT_REGION",
            Self::Default => "the default",
        })
    }
}

/// A validated region and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRegion {
    region: String,
    source: RegionSource,
}

impl ResolvedRegion {
    /// Region name such as `us-east-1`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.region
    }

    /// Which setting selected it.
    #[must_use]
    pub fn source(&self) -> RegionSource {
        self.source
    }
}

/// Resolve the region: `explicit` > `AWS_REGION` > `AWS_DEFAULT_REGION` >
/// `us-east-1`. Empty values are skipped.
///
/// # Errors
/// `InvalidConfig` when the selected value is not shaped like an AWS region.
pub fn resolve_region(explicit: Option<&str>, env: &dyn Env) -> Result<ResolvedRegion, AwsError> {
    let (region, source) = if let Some(value) = explicit.filter(|value| !value.is_empty()) {
        (value.to_string(), RegionSource::Explicit)
    } else if let Some(value) = non_empty(env, ENV_REGION) {
        (value, RegionSource::AwsRegion)
    } else if let Some(value) = non_empty(env, ENV_DEFAULT_REGION) {
        (value, RegionSource::AwsDefaultRegion)
    } else {
        (DEFAULT_REGION.to_string(), RegionSource::Default)
    };
    if !is_valid_region(&region) {
        return Err(AwsError::invalid_config(
            "region",
            format!(
                "{} (from {source}) is not a valid AWS region name",
                snippet(&region, 64)
            ),
        ));
    }
    Ok(ResolvedRegion { region, source })
}

/// The credential profile named by `AWS_PROFILE`, if any.
///
/// # Errors
/// `InvalidConfig` when the name is longer than 128 characters or has
/// characters outside `[A-Za-z0-9._@+/-]`; the value is not echoed.
pub fn resolve_profile(env: &dyn Env) -> Result<Option<String>, AwsError> {
    let Some(profile) = non_empty(env, ENV_PROFILE) else {
        return Ok(None);
    };
    let ok = profile.len() <= 128
        && profile.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'+' | b'/' | b'-')
        });
    if ok {
        Ok(Some(profile))
    } else {
        Err(AwsError::invalid_config(
            "AWS_PROFILE",
            "profile names are at most 128 characters of letters, digits and . _ @ + / -",
        ))
    }
}

/// Name of the service-specific endpoint variable, for example
/// `AWS_ENDPOINT_URL_BEDROCK_AGENT` for `bedrock-agent`.
#[must_use]
pub fn endpoint_env_name(service: &str) -> String {
    let suffix: String = service
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{ENV_ENDPOINT_URL}_{suffix}")
}

/// Resolve an endpoint override: `explicit` > `AWS_ENDPOINT_URL_<SERVICE>` >
/// `AWS_ENDPOINT_URL`. `None` means "use the service default".
///
/// # Errors
/// Any [`validate_endpoint`] error for the selected value.
pub fn resolve_endpoint(
    service: &str,
    explicit: Option<&str>,
    env: &dyn Env,
    policy: &EndpointPolicy,
    resolver: &dyn HostResolver,
) -> Result<Option<Endpoint>, AwsError> {
    if let Some(url) = explicit.filter(|url| !url.is_empty()) {
        return validate_endpoint(url, "endpoint", policy, resolver).map(Some);
    }
    if let Some(url) = non_empty(env, &endpoint_env_name(service)) {
        return validate_endpoint(&url, "AWS_ENDPOINT_URL_<SERVICE>", policy, resolver).map(Some);
    }
    if let Some(url) = non_empty(env, ENV_ENDPOINT_URL) {
        return validate_endpoint(&url, ENV_ENDPOINT_URL, policy, resolver).map(Some);
    }
    Ok(None)
}

/// Caller-controlled inputs to [`ClientConfig::resolve`].
///
/// The strings are unvalidated, so `Debug` scrubs them (an endpoint flag may
/// carry URL userinfo).
#[derive(Clone, Copy, Default)]
pub struct ClientSettings<'a> {
    /// Region from a flag, if any.
    pub region: Option<&'a str>,
    /// Endpoint override from a flag or service variable, if any.
    pub endpoint: Option<&'a str>,
    /// Retry behavior.
    pub retry: RetryPolicy,
    /// Connect and read timeouts.
    pub timeouts: Timeouts,
}

impl fmt::Debug for ClientSettings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            f.debug_struct("ClientSettings")
                .field("region", &self.region.map(Scrubbed))
                .field("endpoint", &self.endpoint.map(Scrubbed))
                .field("retry", &self.retry)
                .field("timeouts", &self.timeouts)
                .finish()
        })
    }
}

/// Everything needed to address, authenticate and pace requests to one service.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientConfig {
    service: String,
    region: ResolvedRegion,
    endpoint: Endpoint,
    profile: Option<String>,
    retry: RetryPolicy,
    timeouts: Timeouts,
}

impl fmt::Debug for ClientConfig {
    /// The endpoint shows scheme, host and port only (its path as a length,
    /// see `Endpoint`'s `Debug`); the strings are scrubbed and the whole
    /// printed text is scrubbed again.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            f.debug_struct("ClientConfig")
                .field("service", &Scrubbed(&self.service))
                .field("region", &self.region)
                .field("endpoint", &self.endpoint)
                .field("profile", &self.profile.as_deref().map(Scrubbed))
                .field("retry", &self.retry)
                .field("timeouts", &self.timeouts)
                .finish()
        })
    }
}

impl ClientConfig {
    /// Resolve the configuration for `service` (a lowercase DNS label such as
    /// `logs` or `bedrock-agent`).
    ///
    /// # Errors
    /// `InvalidConfig` for a bad service label, region, profile or endpoint
    /// override.
    pub fn resolve(
        service: &str,
        settings: &ClientSettings<'_>,
        policy: &EndpointPolicy,
        env: &dyn Env,
        resolver: &dyn HostResolver,
    ) -> Result<Self, AwsError> {
        if !is_valid_service_label(service) {
            return Err(AwsError::invalid_config(
                "service",
                format!("{} is not a valid service label", snippet(service, 64)),
            ));
        }
        let region = resolve_region(settings.region, env)?;
        let profile = resolve_profile(env)?;
        let endpoint = if let Some(endpoint) =
            resolve_endpoint(service, settings.endpoint, env, policy, resolver)?
        {
            endpoint
        } else {
            Endpoint::for_service(service, region.name())?
        };
        Ok(Self {
            service: service.to_string(),
            region,
            endpoint,
            profile,
            retry: settings.retry,
            timeouts: settings.timeouts,
        })
    }

    /// Service label.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Resolved region.
    #[must_use]
    pub fn region(&self) -> &ResolvedRegion {
        &self.region
    }

    /// Endpoint requests are sent to.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Credential profile for the provider chain, if `AWS_PROFILE` is set.
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Retry policy.
    #[must_use]
    pub fn retry(&self) -> &RetryPolicy {
        &self.retry
    }

    /// Timeouts.
    #[must_use]
    pub fn timeouts(&self) -> &Timeouts {
        &self.timeouts
    }

    /// Scope requests are signed for.
    #[must_use]
    pub fn signing_scope(&self) -> SigningScope {
        SigningScope {
            region: self.region.name().to_string(),
            service: self.service.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::testing::FixedResolver;

    fn region_of(explicit: Option<&str>, env: &MapEnv) -> (String, RegionSource) {
        let resolved = resolve_region(explicit, env).unwrap();
        (resolved.name().to_string(), resolved.source())
    }

    #[test]
    fn region_precedence_is_flag_then_aws_region_then_default_region_then_us_east_1() {
        let both = MapEnv::new()
            .with("AWS_REGION", "eu-west-1")
            .with("AWS_DEFAULT_REGION", "ap-south-1");
        assert_eq!(
            region_of(Some("us-west-2"), &both),
            ("us-west-2".to_string(), RegionSource::Explicit)
        );
        assert_eq!(
            region_of(None, &both),
            ("eu-west-1".to_string(), RegionSource::AwsRegion)
        );
        let default_only = MapEnv::new().with("AWS_DEFAULT_REGION", "ap-south-1");
        assert_eq!(
            region_of(None, &default_only),
            ("ap-south-1".to_string(), RegionSource::AwsDefaultRegion)
        );
        assert_eq!(
            region_of(None, &MapEnv::new()),
            ("us-east-1".to_string(), RegionSource::Default)
        );
    }

    #[test]
    fn empty_values_fall_through_like_the_python_or_chain() {
        let env = MapEnv::new()
            .with("AWS_REGION", "")
            .with("AWS_DEFAULT_REGION", "eu-central-1");
        assert_eq!(region_of(Some(""), &env).0, "eu-central-1");
        let blank = MapEnv::new()
            .with("AWS_REGION", "")
            .with("AWS_DEFAULT_REGION", "");
        assert_eq!(region_of(None, &blank).1, RegionSource::Default);
    }

    #[test]
    fn the_profile_region_is_never_consulted() {
        // A profile can only be named; nothing in the resolver reads its region.
        let env = MapEnv::new()
            .with("AWS_PROFILE", "prod")
            .with("AWS_PROFILE_REGION", "eu-west-3");
        assert_eq!(region_of(None, &env).0, "us-east-1");
    }

    #[test]
    fn malformed_regions_are_rejected_without_leaking_odd_content() {
        for bad in [
            "us-east",
            "US-EAST-1",
            "us-east-1.evil.com",
            "us east 1",
            "../../x",
        ] {
            let error = resolve_region(Some(bad), &MapEnv::new()).unwrap_err();
            assert_eq!(error.code(), "aws_invalid_config", "{bad}");
            assert!(error.to_string().contains("is not a valid AWS region name"));
        }
        let env = MapEnv::new().with("AWS_REGION", "token=abc123");
        let error = resolve_region(None, &env).unwrap_err().to_string();
        assert!(error.contains("(from AWS_REGION)"));
        assert!(!error.contains("abc123"));
    }

    #[test]
    fn profile_names_are_validated_and_optional() {
        assert_eq!(resolve_profile(&MapEnv::new()).unwrap(), None);
        assert_eq!(
            resolve_profile(&MapEnv::new().with("AWS_PROFILE", "")).unwrap(),
            None
        );
        for ok in ["default", "my-profile_1", "dev.team@corp+x/y"] {
            let env = MapEnv::new().with("AWS_PROFILE", ok);
            assert_eq!(resolve_profile(&env).unwrap().as_deref(), Some(ok));
        }
        let long = "a".repeat(129);
        for bad in ["has space", "semi;colon", "new\nline", long.as_str()] {
            let env = MapEnv::new().with("AWS_PROFILE", bad);
            let error = resolve_profile(&env).unwrap_err();
            assert!(error.to_string().starts_with("invalid AWS_PROFILE:"));
            assert!(!error.to_string().contains("semi"));
        }
    }

    #[test]
    fn service_endpoint_variables_are_named_from_the_service() {
        assert_eq!(endpoint_env_name("logs"), "AWS_ENDPOINT_URL_LOGS");
        assert_eq!(
            endpoint_env_name("bedrock-agent"),
            "AWS_ENDPOINT_URL_BEDROCK_AGENT"
        );
    }

    fn endpoint_for(explicit: Option<&str>, env: &MapEnv) -> Result<Option<Endpoint>, AwsError> {
        resolve_endpoint(
            "logs",
            explicit,
            env,
            &EndpointPolicy::aws_public(),
            &FixedResolver::new().answer_any(&["52.1.2.3"]),
        )
    }

    #[test]
    fn endpoint_precedence_is_explicit_then_service_then_global() {
        let env = MapEnv::new()
            .with("AWS_ENDPOINT_URL", "https://global.us-east-1.amazonaws.com")
            .with(
                "AWS_ENDPOINT_URL_LOGS",
                "https://service.us-east-1.amazonaws.com",
            );
        let host = |explicit: Option<&str>, env: &MapEnv| {
            endpoint_for(explicit, env)
                .unwrap()
                .map(|e| e.host().to_string())
        };
        assert_eq!(
            host(Some("https://flag.us-east-1.api.aws"), &env).as_deref(),
            Some("flag.us-east-1.api.aws")
        );
        assert_eq!(
            host(None, &env).as_deref(),
            Some("service.us-east-1.amazonaws.com")
        );
        let global_only =
            MapEnv::new().with("AWS_ENDPOINT_URL", "https://global.us-east-1.amazonaws.com");
        assert_eq!(
            host(None, &global_only).as_deref(),
            Some("global.us-east-1.amazonaws.com")
        );
        assert_eq!(host(None, &MapEnv::new()), None);
        assert_eq!(host(Some(""), &MapEnv::new()), None);
    }

    #[test]
    fn every_endpoint_override_source_goes_through_validation() {
        for (explicit, env) in [
            (Some("http://flag.amazonaws.com"), MapEnv::new()),
            (
                None,
                MapEnv::new().with("AWS_ENDPOINT_URL_LOGS", "https://evil.example.com"),
            ),
            (
                None,
                MapEnv::new().with("AWS_ENDPOINT_URL", "https://u:p@x.amazonaws.com"),
            ),
        ] {
            assert!(endpoint_for(explicit, &env).is_err());
        }
        let error = endpoint_for(
            None,
            &MapEnv::new().with("AWS_ENDPOINT_URL", "http://x.amazonaws.com"),
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("invalid AWS_ENDPOINT_URL:"));
    }

    fn resolve(
        service: &str,
        settings: &ClientSettings<'_>,
        env: &MapEnv,
    ) -> Result<ClientConfig, AwsError> {
        ClientConfig::resolve(
            service,
            settings,
            &EndpointPolicy::aws_public(),
            env,
            &FixedResolver::new().answer_any(&["52.1.2.3"]),
        )
    }

    #[test]
    fn config_defaults_synthesize_the_regional_endpoint() {
        let config = resolve("logs", &ClientSettings::default(), &MapEnv::new()).unwrap();
        assert_eq!(config.service(), "logs");
        assert_eq!(config.region().name(), "us-east-1");
        assert_eq!(config.region().source(), RegionSource::Default);
        assert_eq!(
            config.endpoint().url(),
            "https://logs.us-east-1.amazonaws.com"
        );
        assert_eq!(config.profile(), None);
        assert_eq!(*config.retry(), RetryPolicy::default());
        assert_eq!(*config.timeouts(), Timeouts::default());
        let scope = config.signing_scope();
        assert_eq!(
            (scope.region.as_str(), scope.service.as_str()),
            ("us-east-1", "logs")
        );
    }

    #[test]
    fn config_combines_flags_environment_and_overrides() {
        let env = MapEnv::new()
            .with("AWS_REGION", "eu-central-1")
            .with("AWS_PROFILE", "prod")
            .with(
                "AWS_ENDPOINT_URL_LAUNCHWITHAWS",
                "https://launch.eu-central-1.api.aws/",
            );
        let settings = ClientSettings {
            region: Some("us-west-2"),
            retry: RetryPolicy::none(),
            ..ClientSettings::default()
        };
        let config = resolve("launchwithaws", &settings, &env).unwrap();
        assert_eq!(config.region().name(), "us-west-2");
        assert_eq!(config.profile(), Some("prod"));
        assert_eq!(
            config.endpoint().url(),
            "https://launch.eu-central-1.api.aws"
        );
        assert_eq!(config.retry().max_retries(), 0);
        let from_env = resolve("launchwithaws", &ClientSettings::default(), &env).unwrap();
        assert_eq!(from_env.region().name(), "eu-central-1");
    }

    #[test]
    fn invalid_inputs_fail_resolution() {
        let defaults = ClientSettings::default();
        assert!(resolve("Logs", &defaults, &MapEnv::new()).is_err());
        assert!(resolve("a.b", &defaults, &MapEnv::new()).is_err());
        assert!(resolve("logs", &defaults, &MapEnv::new().with("AWS_REGION", "nope")).is_err());
        assert!(resolve(
            "logs",
            &defaults,
            &MapEnv::new().with("AWS_PROFILE", "bad name")
        )
        .is_err());
        let bad_endpoint = ClientSettings {
            endpoint: Some("https://evil.example.com"),
            ..defaults
        };
        assert!(resolve("logs", &bad_endpoint, &MapEnv::new()).is_err());
    }

    #[test]
    fn client_settings_debug_scrubs_unvalidated_strings() {
        let settings = ClientSettings {
            region: Some("token=FAKE-REGION-TOKEN"),
            endpoint: Some("https://FAKEUSR:pa'FAKEPW@logs.amazonaws.com/"),
            ..ClientSettings::default()
        };
        for shown in [format!("{settings:?}"), format!("{settings:#?}")] {
            for leak in ["FAKE-REGION-TOKEN", "FAKEUSR", "FAKEPW"] {
                assert!(!shown.contains(leak), "{leak} leaked: {shown}");
            }
            assert!(
                shown.contains("https://[REDACTED]@logs.amazonaws.com/"),
                "{shown}"
            );
            assert!(
                shown.contains("retry") && shown.contains("timeouts"),
                "{shown}"
            );
            assert_eq!(crate::redact::redact_text(&shown), shown);
        }
        let empty = format!("{:?}", ClientSettings::default());
        assert!(
            empty.contains("region: None") && empty.contains("endpoint: None"),
            "{empty}"
        );
    }

    #[test]
    fn client_config_debug_shows_only_the_endpoint_path_length() {
        // Re-review #4467 finding 3 (#4469): a validated override path can
        // still carry a secret; the resolved configuration must not print it.
        const PATHS: &[(&str, &[&str])] = &[
            ("/password=FAKE-CFG-PW1/x", &["FAKE-CFG-PW1"]),
            ("/token_value:FAKE-CFG-TK2", &["FAKE-CFG-TK2"]),
            ("/opaque/FAKECFGSEGMENT3", &["FAKECFGSEGMENT3", "opaque"]),
        ];
        for &(path, leaks) in PATHS {
            let url = format!("https://logs.amazonaws.com{path}");
            let settings = ClientSettings {
                endpoint: Some(url.as_str()),
                ..ClientSettings::default()
            };
            let env = MapEnv::new().with("AWS_PROFILE", "dev");
            let config = resolve("logs", &settings, &env).unwrap();
            assert_eq!(config.endpoint().url(), url);
            let marker = format!("<path: {} bytes>", path.len());
            for shown in [format!("{config:?}"), format!("{config:#?}")] {
                for leak in leaks {
                    assert!(!shown.contains(leak), "{leak:?} leaked: {shown}");
                }
                for kept in [
                    marker.as_str(),
                    "\"https\"",
                    "\"logs.amazonaws.com\"",
                    "\"logs\"",
                    "\"dev\"",
                    "us-east-1",
                    "retry",
                    "timeouts",
                ] {
                    assert!(shown.contains(kept), "{kept:?} missing: {shown}");
                }
                assert_eq!(crate::redact::redact_text(&shown), shown);
            }
        }
    }
}
