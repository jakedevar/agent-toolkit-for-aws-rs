//! Credentials, only ever through a provider chain.
//!
//! Policy (Python `di_session.build_client` and every `boto3` call site): the
//! toolkit never accepts, stores or prints credentials. They arrive through the
//! standard AWS provider chain (environment, shared config/profile including
//! SSO, web identity and assume-role, container role, instance role), selected
//! at most by `AWS_PROFILE`. This module is the seam: [`CredentialsProvider`] is
//! implemented by the SDK binding (and by test doubles), [`ProviderChain`]
//! composes providers with the botocore rules, [`Credentials`] and
//! [`Secret`] never reveal their values, and
//! [`reject_credential_arguments`] refuses credential command-line flags.
//!
//! This crate reads no credential environment variable or file itself.

use std::fmt;
use std::time::SystemTime;

use crate::error::AwsError;
use crate::redact::{write_scrubbed_debug, Scrubbed, Secret};

/// Provider names the SDK binding must report, in the standard chain order.
/// `profile` covers shared-config static keys, SSO, web identity, assume-role
/// and credential-process; `container` is the ECS/EKS task role.
pub const STANDARD_CHAIN_STAGES: &[&str] =
    &["environment", "profile", "container", "instance-metadata"];

/// Flags that would put credentials on a command line.
const CREDENTIAL_FLAGS: &[&str] = &[
    "--access-key-id",
    "--access-key",
    "--secret-access-key",
    "--secret-key",
    "--session-token",
    "--security-token",
    "--aws-access-key-id",
    "--aws-secret-access-key",
    "--aws-session-token",
];

/// Resolved AWS credentials. `Debug` shows nothing but a marker.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    access_key_id: Secret,
    secret_access_key: Secret,
    session_token: Option<Secret>,
    expires_at: Option<SystemTime>,
}

impl Credentials {
    /// Build credentials; an empty session token counts as none.
    ///
    /// # Errors
    /// `PartialCredentials` when the access key id or secret is empty.
    pub fn new(
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
        expires_at: Option<SystemTime>,
    ) -> Result<Self, AwsError> {
        if access_key_id.is_empty() {
            return Err(AwsError::PartialCredentials {
                missing: "access_key_id",
            });
        }
        if secret_access_key.is_empty() {
            return Err(AwsError::PartialCredentials {
                missing: "secret_access_key",
            });
        }
        Ok(Self {
            access_key_id: Secret::new(access_key_id),
            secret_access_key: Secret::new(secret_access_key),
            session_token: session_token
                .filter(|token| !token.is_empty())
                .map(Secret::new),
            expires_at,
        })
    }

    /// The access key id; hand it only to a signer.
    #[must_use]
    pub fn access_key_id(&self) -> &Secret {
        &self.access_key_id
    }

    /// The secret access key; hand it only to a signer.
    #[must_use]
    pub fn secret_access_key(&self) -> &Secret {
        &self.secret_access_key
    }

    /// The session token of temporary credentials.
    #[must_use]
    pub fn session_token(&self) -> Option<&Secret> {
        self.session_token.as_ref()
    }

    /// When the credentials stop working, if they expire.
    #[must_use]
    pub fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    /// Whether the credentials are expired at `now` (never for non-expiring).
    #[must_use]
    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        self.expires_at.is_some_and(|expiry| expiry <= now)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}

/// A source of credentials.
pub trait CredentialsProvider {
    /// Stable provider name (see [`STANDARD_CHAIN_STAGES`]).
    fn name(&self) -> &'static str;

    /// Load credentials.
    ///
    /// # Errors
    /// `NoCredentials` when this provider has none (the chain moves on); any
    /// other error stops the chain.
    fn load(&self) -> Result<Credentials, AwsError>;
}

/// Providers tried in order with botocore's rules: the first success wins,
/// `NoCredentials` moves to the next provider, and any other error (partial or
/// expired credentials, a failing provider) stops the chain and is returned.
#[derive(Default)]
pub struct ProviderChain {
    providers: Vec<Box<dyn CredentialsProvider>>,
}

impl ProviderChain {
    /// An empty chain (loads fail with `NoCredentials`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a provider.
    #[must_use]
    pub fn with(mut self, provider: Box<dyn CredentialsProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Provider names in order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.providers
            .iter()
            .map(|provider| provider.name())
            .collect()
    }
}

impl fmt::Debug for ProviderChain {
    /// Provider names only, each scrubbed (a custom provider chooses its name).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            let names: Vec<Scrubbed<'_>> = self.names().into_iter().map(Scrubbed).collect();
            f.debug_struct("ProviderChain")
                .field("providers", &names)
                .finish()
        })
    }
}

impl CredentialsProvider for ProviderChain {
    fn name(&self) -> &'static str {
        "chain"
    }

    fn load(&self) -> Result<Credentials, AwsError> {
        for provider in &self.providers {
            match provider.load() {
                Ok(credentials) => return Ok(credentials),
                Err(AwsError::NoCredentials) => {}
                Err(other) => return Err(other),
            }
        }
        Err(AwsError::NoCredentials)
    }
}

/// Refuse credential flags (`--secret-access-key`, `--session-token=...`) in a
/// command line. Only the flag *name* is reported, never a value; arguments
/// after a bare `--` are positional and not inspected.
///
/// # Errors
/// `CredentialArgument` naming the first offending flag.
pub fn reject_credential_arguments(args: &[String]) -> Result<(), AwsError> {
    for arg in args {
        if arg == "--" {
            break;
        }
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        let lower = name.to_ascii_lowercase();
        if CREDENTIAL_FLAGS.contains(&lower.as_str()) {
            return Err(AwsError::CredentialArgument { flag: lower });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeProvider;
    use std::time::Duration;

    fn creds(key: &str) -> Credentials {
        Credentials::new(key, "fake-secret-value", None, None).unwrap()
    }

    #[test]
    fn credentials_validate_and_hide_their_values() {
        let credentials =
            Credentials::new("FAKEKEYID", "fake-secret-value", Some("fake-session"), None).unwrap();
        assert_eq!(credentials.access_key_id().expose(), "FAKEKEYID");
        assert_eq!(
            credentials.secret_access_key().expose(),
            "fake-secret-value"
        );
        assert_eq!(
            credentials.session_token().unwrap().expose(),
            "fake-session"
        );
        let shown = format!("{credentials:?}");
        assert_eq!(shown, "Credentials([REDACTED])");
        assert!(!shown.contains("FAKEKEYID") && !shown.contains("fake-secret"));
        assert_eq!(
            Credentials::new("", "s", None, None).unwrap_err(),
            AwsError::PartialCredentials {
                missing: "access_key_id"
            }
        );
        assert_eq!(
            Credentials::new("k", "", None, None).unwrap_err(),
            AwsError::PartialCredentials {
                missing: "secret_access_key"
            }
        );
        let none = Credentials::new("k", "s", Some(""), None).unwrap();
        assert!(none.session_token().is_none());
    }

    #[test]
    fn expiry_is_inclusive_and_absent_means_never() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let expiring = Credentials::new("k", "s", None, Some(now)).unwrap();
        assert!(expiring.is_expired_at(now));
        assert!(expiring.is_expired_at(now + Duration::from_secs(1)));
        assert!(!expiring.is_expired_at(now - Duration::from_secs(1)));
        assert_eq!(expiring.expires_at(), Some(now));
        assert!(!creds("k").is_expired_at(now + Duration::from_secs(10_000_000)));
    }

    #[test]
    fn the_first_provider_with_credentials_wins_and_later_ones_are_not_asked() {
        let first = FakeProvider::missing("environment");
        let second = FakeProvider::with_keys("profile", "KEY2", "secret2");
        let third = FakeProvider::with_keys("container", "KEY3", "secret3");
        let (first_calls, second_calls, third_calls) =
            (first.calls(), second.calls(), third.calls());
        let chain = ProviderChain::new()
            .with(Box::new(first))
            .with(Box::new(second))
            .with(Box::new(third));
        assert_eq!(chain.names(), vec!["environment", "profile", "container"]);
        let loaded = chain.load().unwrap();
        assert_eq!(loaded.access_key_id().expose(), "KEY2");
        assert_eq!(
            (first_calls.get(), second_calls.get(), third_calls.get()),
            (1, 1, 0)
        );
    }

    #[test]
    fn an_empty_or_fully_missing_chain_reports_no_credentials() {
        assert_eq!(
            ProviderChain::new().load().unwrap_err(),
            AwsError::NoCredentials
        );
        let chain = ProviderChain::new()
            .with(Box::new(FakeProvider::missing("a")))
            .with(Box::new(FakeProvider::missing("b")));
        assert_eq!(chain.load().unwrap_err(), AwsError::NoCredentials);
    }

    #[test]
    fn partial_expired_and_failing_providers_stop_the_chain() {
        for error in [
            AwsError::PartialCredentials {
                missing: "secret_access_key",
            },
            AwsError::CredentialsExpired,
            AwsError::provider("profile", "sso session expired"),
        ] {
            let later = FakeProvider::with_keys("later", "KEY", "secret");
            let later_calls = later.calls();
            let chain = ProviderChain::new()
                .with(Box::new(FakeProvider::failing("first", error.clone())))
                .with(Box::new(later));
            assert_eq!(chain.load().unwrap_err(), error);
            assert_eq!(later_calls.get(), 0, "{error}");
        }
    }

    #[test]
    fn chains_nest_and_debug_shows_only_names() {
        let inner = ProviderChain::new().with(Box::new(FakeProvider::with_keys("p", "K", "s")));
        let outer = ProviderChain::new()
            .with(Box::new(FakeProvider::missing("e")))
            .with(Box::new(inner));
        assert_eq!(outer.names(), vec!["e", "chain"]);
        assert!(outer.load().is_ok());
        assert_eq!(
            format!("{outer:?}"),
            "ProviderChain { providers: [\"e\", \"chain\"] }"
        );
    }

    #[test]
    fn the_standard_chain_order_is_pinned() {
        assert_eq!(
            STANDARD_CHAIN_STAGES,
            &["environment", "profile", "container", "instance-metadata"]
        );
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn credential_flags_are_refused_with_only_the_flag_name() {
        for flag in [
            "--access-key-id",
            "--secret-access-key",
            "--session-token",
            "--aws-secret-access-key",
        ] {
            let with_value = format!("{flag}=AKIAIOSFODNN7EXAMPLE");
            for form in [args(&["list", flag, "value"]), args(&["list", &with_value])] {
                let error = reject_credential_arguments(&form).unwrap_err();
                assert_eq!(
                    error,
                    AwsError::CredentialArgument {
                        flag: flag.to_string()
                    }
                );
                assert!(!error.to_string().contains("AKIA"), "{flag}");
                assert!(!error.to_string().contains("value"), "{flag}");
            }
        }
        let upper = reject_credential_arguments(&args(&["--SECRET-ACCESS-KEY=x"])).unwrap_err();
        assert_eq!(
            upper,
            AwsError::CredentialArgument {
                flag: "--secret-access-key".to_string()
            }
        );
    }

    #[test]
    fn ordinary_flags_and_positionals_after_double_dash_pass() {
        assert!(reject_credential_arguments(&args(&[
            "list",
            "--region",
            "us-east-1",
            "--profile=dev"
        ]))
        .is_ok());
        assert!(reject_credential_arguments(&args(&[
            "--region",
            "x",
            "--",
            "--secret-access-key"
        ]))
        .is_ok());
        assert!(reject_credential_arguments(&[]).is_ok());
        assert!(reject_credential_arguments(&args(&["--session-tokens"])).is_ok());
    }

    #[test]
    fn chain_debug_scrubs_provider_names() {
        let chain = ProviderChain::new()
            .with(Box::new(FakeProvider::missing("password=FAKE-CHAIN-NAME")))
            .with(Box::new(FakeProvider::missing("env")));
        for shown in [format!("{chain:?}"), format!("{chain:#?}")] {
            assert!(!shown.contains("FAKE-CHAIN-NAME"), "{shown}");
            assert!(shown.contains("\"env\""), "{shown}");
        }
        // The accessor still reports the names a caller chose.
        assert_eq!(chain.names(), vec!["password=FAKE-CHAIN-NAME", "env"]);
    }
}
