//! Credential-source preflight and guarded source selection (#4413 S1,
//! design `docs/migration/mcp-proxy-design.md` 4.1.1).
//!
//! Pure decisions, no SDK and no I/O, so every row is unit-tested in the
//! default build:
//!
//! - **R-ABS** ([`classify`]): what the launch configuration says about
//!   credentials, from presence only. It reads whether listed environment
//!   variables are set and non-blank, the selected profile name and the
//!   credential-bearing **key names** of that profile ([`ProfileFacts`],
//!   produced by the SDK adapter from aws-config's own parser). It never reads
//!   a credential value, never validates a reference and never looks at any
//!   provider error.
//! - **R-SRC** ([`select_source`]): the single provider a [`Preflight`] allows
//!   to be built, in the SDK chain's own order. Only [`Preflight::Absent`]
//!   selects the default chain.
//! - **R-PREC** ([`RequestAuth`]): error precedence across the (at most two)
//!   credential attempts of one request, and which outcomes trigger the D1
//!   rebuild.
//! - **R-STICKY** ([`SessionAuth`]): a session that signed once never goes
//!   anonymous.
//!
//! Terminality comes from **which provider was built**, never from an error's
//! text: the SDK adapter reports each attempt as an [`AttemptOutcome`] by the
//! error *variant* and the preflight class it ran under.

use std::collections::BTreeSet;
use std::fmt;

use crate::env::Env;

/// Profile keys that make the selected profile a configured credential source
/// (R-ABS). Presence of any one of them is enough; values are never read.
pub const CREDENTIAL_PROFILE_KEYS: &[&str] = &[
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "role_arn",
    "source_profile",
    "credential_source",
    "credential_process",
    "web_identity_token_file",
    "sso_session",
    "sso_start_url",
    "sso_account_id",
    "sso_role_name",
    "login_session",
];

/// The environment variables the preflight looks at (presence only, except
/// the profile name and the two file-path overrides).
pub const PREFLIGHT_ENV_VARS: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_ROLE_ARN",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_PROFILE",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
];

/// Name of the implicit profile.
pub const DEFAULT_PROFILE: &str = "default";

/// Whether `name` is set to something other than blank (the SDK environment
/// provider trims values the same way).
fn present(env: &dyn Env, name: &str) -> bool {
    env.var(name).is_some_and(|value| !value.trim().is_empty())
}

/// The profile credentials are read from, and whether the user chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedProfile {
    name: String,
    explicit: bool,
}

impl SelectedProfile {
    /// `--profile` when given, else a non-blank `AWS_PROFILE`, else the
    /// implicit `default`.
    #[must_use]
    pub fn resolve(cli_profile: Option<&str>, env: &dyn Env) -> Self {
        if let Some(name) = cli_profile {
            return Self {
                name: name.to_string(),
                explicit: true,
            };
        }
        match env.var("AWS_PROFILE") {
            Some(name) if !name.trim().is_empty() => Self {
                name,
                explicit: true,
            },
            _ => Self {
                name: DEFAULT_PROFILE.to_string(),
                explicit: false,
            },
        }
    }

    /// The profile name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether `--profile` or `AWS_PROFILE` chose it.
    #[must_use]
    pub fn is_explicit(&self) -> bool {
        self.explicit
    }
}

/// What the shared `config` and `credentials` files say about the selected
/// profile: existence and credential-bearing key **names** only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileFacts {
    /// The selected profile exists in either file.
    pub profile_exists: bool,
    /// The [`CREDENTIAL_PROFILE_KEYS`] the selected profile holds.
    pub credential_keys: BTreeSet<&'static str>,
    /// Whether the selected profile's provider could run a
    /// `credential_process`: the profile has the key itself, or it has a
    /// `source_profile` and some profile in the files has the key (key names
    /// only; the `source_profile` value is never read, so a chain is assumed
    /// to reach any such profile).
    pub credential_process_reachable: bool,
}

/// The profile files as the preflight sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileView {
    /// Both files were read (a missing default-path file reads as empty) and
    /// parsed.
    Loaded(ProfileFacts),
    /// `AWS_CONFIG_FILE` or `AWS_SHARED_CREDENTIALS_FILE` names a path that
    /// does not exist.
    MissingOverride,
    /// A file exists but could not be read or parsed.
    Unusable,
}

/// Why a configuration is refused before any provider is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidReason {
    /// Exactly one of the access key id and the secret access key.
    PartialStaticKeys,
    /// `AWS_SESSION_TOKEN` without both keys.
    OrphanSessionToken,
    /// Exactly one of `AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN`.
    PartialWebIdentity,
    /// An explicitly selected profile exists in neither file.
    MissingSelectedProfile,
    /// A file-path override names a missing file.
    MissingProfileFile,
    /// A profile file exists but cannot be read or parsed.
    UnusableProfileFile,
    /// The selected profile could run a `credential_process` but this
    /// process cannot give children a null stdin (non-Unix, D2): the child
    /// would inherit the MCP message stream (#4413 S3).
    CredentialProcessNeedsNullStdin,
}

/// The configured sources a preflight found (R-ABS "Configured"), before
/// R-SRC picks the first.
// One flag per R-SRC row: a presence set, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConfiguredSources {
    /// A complete static environment pair.
    pub static_env: bool,
    /// The selected profile has a credential-bearing key.
    pub profile: bool,
    /// Both web-identity variables.
    pub web_identity: bool,
    /// A container credentials URI variable.
    pub container: bool,
}

impl ConfiguredSources {
    fn any(self) -> bool {
        self.static_env || self.profile || self.web_identity || self.container
    }
}

/// The R-ABS class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preflight {
    /// Fail closed; no provider is built.
    Invalid(InvalidReason),
    /// At least one source is configured; only R-SRC's first is built.
    Configured(ConfiguredSources),
    /// Nothing is configured; the default chain may run.
    Absent,
}

/// Classify the launch configuration (R-ABS).
#[must_use]
pub fn classify(env: &dyn Env, profile: &SelectedProfile, files: &ProfileView) -> Preflight {
    let access_key = present(env, "AWS_ACCESS_KEY_ID");
    let secret_key = present(env, "AWS_SECRET_ACCESS_KEY") || present(env, "SECRET_ACCESS_KEY");
    if access_key != secret_key {
        return Preflight::Invalid(InvalidReason::PartialStaticKeys);
    }
    if present(env, "AWS_SESSION_TOKEN") && !(access_key && secret_key) {
        return Preflight::Invalid(InvalidReason::OrphanSessionToken);
    }
    let token_file = present(env, "AWS_WEB_IDENTITY_TOKEN_FILE");
    let role_arn = present(env, "AWS_ROLE_ARN");
    if token_file != role_arn {
        return Preflight::Invalid(InvalidReason::PartialWebIdentity);
    }
    let facts = match files {
        ProfileView::Loaded(facts) => facts,
        ProfileView::MissingOverride => {
            return Preflight::Invalid(InvalidReason::MissingProfileFile)
        }
        ProfileView::Unusable => return Preflight::Invalid(InvalidReason::UnusableProfileFile),
    };
    if profile.is_explicit() && !facts.profile_exists {
        return Preflight::Invalid(InvalidReason::MissingSelectedProfile);
    }
    let sources = ConfiguredSources {
        static_env: access_key && secret_key,
        profile: facts.profile_exists && !facts.credential_keys.is_empty(),
        web_identity: token_file && role_arn,
        container: present(env, "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            || present(env, "AWS_CONTAINER_CREDENTIALS_FULL_URI"),
    };
    if sources.any() {
        Preflight::Configured(sources)
    } else {
        Preflight::Absent
    }
}

/// Which provider an attempt builds (and the test-visible build log entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// R-SRC row 1: the environment provider.
    Env,
    /// Row 2: the selected profile's provider.
    Profile,
    /// Row 3: the web-identity provider.
    WebIdentity,
    /// Row 4: the container (ECS/EKS) provider.
    Ecs,
    /// Absent: the SDK default chain.
    DefaultChain,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Env => "Env",
            Self::Profile => "Profile",
            Self::WebIdentity => "WebIdentity",
            Self::Ecs => "Ecs",
            Self::DefaultChain => "DefaultChain",
        })
    }
}

/// What R-SRC allows for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Build nothing; the attempt is a level-1 failure.
    Refuse(InvalidReason),
    /// Build exactly this provider and nothing else.
    Build(SourceKind),
}

/// The R-SRC selector: the first configured source in the SDK chain's order
/// (environment, profile, web identity, container), or the default chain only
/// for [`Preflight::Absent`].
#[must_use]
pub fn select_source(preflight: &Preflight) -> Selection {
    match preflight {
        Preflight::Invalid(reason) => Selection::Refuse(*reason),
        Preflight::Absent => Selection::Build(SourceKind::DefaultChain),
        Preflight::Configured(sources) => Selection::Build(if sources.static_env {
            SourceKind::Env
        } else if sources.profile {
            SourceKind::Profile
        } else if sources.web_identity {
            SourceKind::WebIdentity
        } else {
            // `Configured` holds at least one source, so this is the container.
            SourceKind::Ecs
        }),
    }
}

/// How one credential attempt ended, classified by the preflight class and the
/// SDK error **variant** only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// Credentials were loaded.
    Loaded,
    /// Level 1: an Invalid preflight; nothing was built.
    Invalid(InvalidReason),
    /// Level 2: a non-`NotLoaded` provider error, or expired/partial
    /// credentials.
    ProviderFailed,
    /// Level 3: `NotLoaded` from a configured source's own provider.
    ConfiguredNotLoaded,
    /// Level 4: `NotLoaded` from the default chain under an Absent preflight.
    AbsentNotLoaded,
}

impl AttemptOutcome {
    /// R-PREC level, 1 (highest) to 4; `None` for a success.
    #[must_use]
    pub fn level(self) -> Option<u8> {
        match self {
            Self::Loaded => None,
            Self::Invalid(_) => Some(1),
            Self::ProviderFailed => Some(2),
            Self::ConfiguredNotLoaded => Some(3),
            Self::AbsentNotLoaded => Some(4),
        }
    }

    /// Whether this outcome triggers the D1 rebuild (levels 2 and 3 only).
    #[must_use]
    pub fn triggers_rebuild(self) -> bool {
        matches!(self, Self::ProviderFailed | Self::ConfiguredNotLoaded)
    }
}

/// The credential failure a request ends with (never a provider's text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRefusal {
    /// Level 1.
    InvalidConfiguration(InvalidReason),
    /// Level 2.
    ProviderFailed,
    /// Level 3.
    ConfiguredSourceNotLoaded,
    /// Level 4 without `--skip-auth`.
    NoCredentials,
    /// Level 4 in a session that already signed (R-STICKY).
    SignedSessionLostCredentials,
}

/// What one request does about authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthDecision {
    /// Sign with the loaded credentials.
    Sign,
    /// Send unsigned (only `--skip-auth` and every attempt at level 4).
    Unsigned,
    /// Fail closed.
    Refuse(CredentialRefusal),
}

/// R-PREC across the attempts of one request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestAuth {
    worst: Option<AttemptOutcome>,
    loaded: bool,
    attempts: u8,
}

impl RequestAuth {
    /// Most attempts one request makes: the load and D1's single rebuild.
    pub const MAX_ATTEMPTS: u8 = 2;

    /// No attempt yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one attempt.
    pub fn record(&mut self, outcome: AttemptOutcome) {
        self.attempts = self.attempts.saturating_add(1);
        match outcome.level() {
            None => self.loaded = true,
            Some(level) => {
                let worse = self
                    .worst
                    .and_then(AttemptOutcome::level)
                    .is_none_or(|current| level < current);
                if worse {
                    self.worst = Some(outcome);
                }
            }
        }
    }

    /// Whether another attempt (the D1 rebuild) should run after `last`.
    #[must_use]
    pub fn should_retry(&self, last: AttemptOutcome) -> bool {
        last.triggers_rebuild() && self.attempts < Self::MAX_ATTEMPTS
    }

    /// The request's decision. A success signs; otherwise the
    /// highest-precedence failure seen on **any** attempt wins, and unsigned
    /// is possible only when every attempt ended at level 4 and `skip_auth`
    /// is set.
    #[must_use]
    pub fn decide(&self, skip_auth: bool) -> AuthDecision {
        if self.loaded {
            return AuthDecision::Sign;
        }
        match self.worst {
            Some(AttemptOutcome::Invalid(reason)) => {
                AuthDecision::Refuse(CredentialRefusal::InvalidConfiguration(reason))
            }
            Some(AttemptOutcome::ProviderFailed) => {
                AuthDecision::Refuse(CredentialRefusal::ProviderFailed)
            }
            Some(AttemptOutcome::ConfiguredNotLoaded) => {
                AuthDecision::Refuse(CredentialRefusal::ConfiguredSourceNotLoaded)
            }
            Some(AttemptOutcome::AbsentNotLoaded) if skip_auth => AuthDecision::Unsigned,
            Some(AttemptOutcome::AbsentNotLoaded | AttemptOutcome::Loaded) | None => {
                AuthDecision::Refuse(CredentialRefusal::NoCredentials)
            }
        }
    }
}

/// R-STICKY: once any request in the current MCP session was signed, a later
/// unsigned decision fails closed until the session ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionAuth {
    signed: bool,
}

impl SessionAuth {
    /// A new session that has not signed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the session rule to a request's decision and remember a signing.
    pub fn apply(&mut self, decision: AuthDecision) -> AuthDecision {
        match decision {
            AuthDecision::Sign => {
                self.signed = true;
                decision
            }
            AuthDecision::Unsigned if self.signed => {
                AuthDecision::Refuse(CredentialRefusal::SignedSessionLostCredentials)
            }
            other => other,
        }
    }

    /// The session ended (a new `Mcp-Session-Id`): forget the signing.
    pub fn reset(&mut self) {
        self.signed = false;
    }

    /// Whether this session has signed a request.
    #[must_use]
    pub fn has_signed(&self) -> bool {
        self.signed
    }
}

#[cfg(test)]
mod tests;

/// D2 on platforms without stdin isolation (#4413 S3): when `null_stdin` is
/// false (the proxy could not give child processes a null stdin, as on
/// non-Unix targets), a selection of the profile provider whose profile could
/// run a `credential_process` is refused (level 1: fail closed, no rebuild,
/// no fallback to another source). Every other selection is unchanged.
#[must_use]
pub fn guard_credential_process(
    selection: Selection,
    files: &ProfileView,
    null_stdin: bool,
) -> Selection {
    match (selection, files) {
        (Selection::Build(SourceKind::Profile), ProfileView::Loaded(facts))
            if !null_stdin && facts.credential_process_reachable =>
        {
            Selection::Refuse(InvalidReason::CredentialProcessNeedsNullStdin)
        }
        (other, _) => other,
    }
}

/// Whether this target can give `credential_process` children a null stdin
/// (the D2 mechanism exists on Unix only).
#[must_use]
pub const fn platform_isolates_stdin() -> bool {
    cfg!(unix)
}

#[cfg(test)]
mod credential_process_tests {
    use super::*;

    fn loaded(reachable: bool) -> ProfileView {
        ProfileView::Loaded(ProfileFacts {
            profile_exists: true,
            credential_keys: ["credential_process"].into_iter().collect(),
            credential_process_reachable: reachable,
        })
    }

    #[test]
    fn without_a_null_stdin_a_reachable_credential_process_is_refused() {
        let profile = Selection::Build(SourceKind::Profile);
        assert_eq!(
            guard_credential_process(profile, &loaded(true), false),
            Selection::Refuse(InvalidReason::CredentialProcessNeedsNullStdin)
        );
        // With isolation, or without a reachable process, nothing changes.
        assert_eq!(
            guard_credential_process(profile, &loaded(true), true),
            profile
        );
        assert_eq!(
            guard_credential_process(profile, &loaded(false), false),
            profile
        );
        // Other sources never run a credential process and are not refused:
        // the refusal never falls through to them either (the profile row is
        // refused, not skipped).
        for kind in [
            SourceKind::Env,
            SourceKind::WebIdentity,
            SourceKind::Ecs,
            SourceKind::DefaultChain,
        ] {
            let other = Selection::Build(kind);
            assert_eq!(guard_credential_process(other, &loaded(true), false), other);
        }
        let refused = Selection::Refuse(InvalidReason::PartialStaticKeys);
        assert_eq!(
            guard_credential_process(refused, &loaded(true), false),
            refused
        );
    }

    #[test]
    fn a_refusal_is_level_one_and_triggers_no_rebuild() {
        let outcome = AttemptOutcome::Invalid(InvalidReason::CredentialProcessNeedsNullStdin);
        assert_eq!(outcome.level(), Some(1));
        assert!(!outcome.triggers_rebuild());
        let mut auth = RequestAuth::new();
        auth.record(outcome);
        assert!(!auth.should_retry(outcome));
        assert_eq!(
            auth.decide(true),
            AuthDecision::Refuse(CredentialRefusal::InvalidConfiguration(
                InvalidReason::CredentialProcessNeedsNullStdin
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_isolates_stdin() {
        assert!(platform_isolates_stdin());
    }

    #[cfg(not(unix))]
    #[test]
    fn other_targets_do_not_isolate_stdin() {
        assert!(!platform_isolates_stdin());
    }
}
