//! T-ABS preflight and R-SRC selector units, R-PREC and R-STICKY tables.

use super::*;
use crate::env::MapEnv;

fn facts(exists: bool, keys: &[&'static str]) -> ProfileView {
    ProfileView::Loaded(ProfileFacts {
        profile_exists: exists,
        credential_keys: keys.iter().copied().collect(),
    })
}

fn no_profile() -> ProfileView {
    facts(false, &[])
}

fn implicit() -> SelectedProfile {
    SelectedProfile::resolve(None, &MapEnv::new())
}

fn class(env: &MapEnv, files: &ProfileView) -> Preflight {
    classify(env, &SelectedProfile::resolve(None, env), files)
}

fn configured(sources: ConfiguredSources) -> Preflight {
    Preflight::Configured(sources)
}

#[test]
fn the_selected_profile_is_cli_then_non_blank_aws_profile_then_default() {
    let env = MapEnv::new().with("AWS_PROFILE", "dev");
    let cli = SelectedProfile::resolve(Some("ops"), &env);
    assert_eq!((cli.name(), cli.is_explicit()), ("ops", true));
    let from_env = SelectedProfile::resolve(None, &env);
    assert_eq!((from_env.name(), from_env.is_explicit()), ("dev", true));
    for blank in ["", "  "] {
        let env = MapEnv::new().with("AWS_PROFILE", blank);
        let selected = SelectedProfile::resolve(None, &env);
        assert_eq!(
            (selected.name(), selected.is_explicit()),
            ("default", false)
        );
    }
    let none = implicit();
    assert_eq!((none.name(), none.is_explicit()), ("default", false));
}

#[test]
fn genuine_absence_and_a_profile_without_credential_keys_are_absent() {
    assert_eq!(class(&MapEnv::new(), &no_profile()), Preflight::Absent);
    // A `default` profile that holds only `region` (its key names reach the
    // preflight already filtered to credential-bearing ones: none).
    assert_eq!(class(&MapEnv::new(), &facts(true, &[])), Preflight::Absent);
    // An explicit profile that exists without credential keys is also absent.
    let env = MapEnv::new().with("AWS_PROFILE", "regional");
    assert_eq!(class(&env, &facts(true, &[])), Preflight::Absent);
    // Non-credential variables do not configure anything.
    let env = MapEnv::new()
        .with("AWS_REGION", "us-west-2")
        .with("AWS_ACCESS_KEY_ID", " ")
        .with("AWS_SESSION_TOKEN", "");
    assert_eq!(class(&env, &no_profile()), Preflight::Absent);
}

#[test]
fn every_partial_environment_configuration_is_invalid() {
    let cases: [(&[(&str, &str)], InvalidReason); 8] = [
        (
            &[("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")],
            InvalidReason::PartialStaticKeys,
        ),
        (
            &[("AWS_SECRET_ACCESS_KEY", "s")],
            InvalidReason::PartialStaticKeys,
        ),
        (
            &[("SECRET_ACCESS_KEY", "s")],
            InvalidReason::PartialStaticKeys,
        ),
        (
            &[
                ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE"),
                ("AWS_SECRET_ACCESS_KEY", "  "),
            ],
            InvalidReason::PartialStaticKeys,
        ),
        (
            &[("AWS_SESSION_TOKEN", "t")],
            InvalidReason::OrphanSessionToken,
        ),
        (
            &[("AWS_WEB_IDENTITY_TOKEN_FILE", "/tmp/token")],
            InvalidReason::PartialWebIdentity,
        ),
        (
            &[("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/r")],
            InvalidReason::PartialWebIdentity,
        ),
        (
            &[
                ("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/r"),
                ("AWS_WEB_IDENTITY_TOKEN_FILE", ""),
            ],
            InvalidReason::PartialWebIdentity,
        ),
    ];
    for (vars, reason) in cases {
        let env = vars
            .iter()
            .fold(MapEnv::new(), |env, (name, value)| env.with(name, value));
        // A valid static `default` profile next to it changes nothing: no
        // fall-through to the profile identity.
        for files in [
            no_profile(),
            facts(true, &["aws_access_key_id", "aws_secret_access_key"]),
        ] {
            assert_eq!(class(&env, &files), Preflight::Invalid(reason), "{vars:?}");
            assert_eq!(
                select_source(&class(&env, &files)),
                Selection::Refuse(reason),
                "{vars:?}"
            );
        }
    }
}

#[test]
fn an_explicit_profile_that_exists_nowhere_is_invalid_and_the_implicit_default_is_not() {
    let env = MapEnv::new().with("AWS_PROFILE", "typo");
    assert_eq!(
        class(&env, &no_profile()),
        Preflight::Invalid(InvalidReason::MissingSelectedProfile)
    );
    let cli = SelectedProfile::resolve(Some("typo"), &MapEnv::new());
    assert_eq!(
        classify(&MapEnv::new(), &cli, &no_profile()),
        Preflight::Invalid(InvalidReason::MissingSelectedProfile)
    );
    // `--profile` wins over `AWS_PROFILE` for existence too.
    let env = MapEnv::new().with("AWS_PROFILE", "exists");
    let cli = SelectedProfile::resolve(Some("typo"), &env);
    assert_eq!(
        classify(&env, &cli, &no_profile()),
        Preflight::Invalid(InvalidReason::MissingSelectedProfile)
    );
    assert_eq!(
        classify(&MapEnv::new(), &implicit(), &no_profile()),
        Preflight::Absent
    );
}

#[test]
fn unusable_profile_files_are_invalid() {
    assert_eq!(
        class(&MapEnv::new(), &ProfileView::MissingOverride),
        Preflight::Invalid(InvalidReason::MissingProfileFile)
    );
    assert_eq!(
        class(&MapEnv::new(), &ProfileView::Unusable),
        Preflight::Invalid(InvalidReason::UnusableProfileFile)
    );
}

#[test]
fn every_credential_bearing_profile_key_configures_the_profile_source() {
    assert_eq!(CREDENTIAL_PROFILE_KEYS.len(), 13);
    for key in CREDENTIAL_PROFILE_KEYS {
        let expected = configured(ConfiguredSources {
            profile: true,
            ..ConfiguredSources::default()
        });
        // Implicit default, AWS_PROFILE and --profile all count.
        assert_eq!(
            class(&MapEnv::new(), &facts(true, &[key])),
            expected,
            "{key}"
        );
        let env = MapEnv::new().with("AWS_PROFILE", "dev");
        assert_eq!(class(&env, &facts(true, &[key])), expected, "{key}");
        let cli = SelectedProfile::resolve(Some("dev"), &MapEnv::new());
        assert_eq!(
            classify(&MapEnv::new(), &cli, &facts(true, &[key])),
            expected,
            "{key}"
        );
        assert_eq!(
            select_source(&expected),
            Selection::Build(SourceKind::Profile),
            "{key}"
        );
    }
}

#[test]
fn every_environment_source_is_configured() {
    let pair = MapEnv::new()
        .with("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
        .with("AWS_SECRET_ACCESS_KEY", "secret");
    let alt_pair = MapEnv::new()
        .with("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
        .with("SECRET_ACCESS_KEY", "secret")
        .with("AWS_SESSION_TOKEN", "token");
    for env in [pair, alt_pair] {
        assert_eq!(
            class(&env, &no_profile()),
            configured(ConfiguredSources {
                static_env: true,
                ..ConfiguredSources::default()
            })
        );
    }
    let web = MapEnv::new()
        .with("AWS_WEB_IDENTITY_TOKEN_FILE", "/tmp/token")
        .with("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/r");
    assert_eq!(
        class(&web, &no_profile()),
        configured(ConfiguredSources {
            web_identity: true,
            ..ConfiguredSources::default()
        })
    );
    for name in [
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    ] {
        let env = MapEnv::new().with(name, "/fallback-creds");
        assert_eq!(
            class(&env, &no_profile()),
            configured(ConfiguredSources {
                container: true,
                ..ConfiguredSources::default()
            }),
            "{name}"
        );
    }
}

#[test]
fn the_selector_takes_the_first_source_in_sdk_chain_order() {
    let all = ConfiguredSources {
        static_env: true,
        profile: true,
        web_identity: true,
        container: true,
    };
    let order = [
        (all, SourceKind::Env),
        (
            ConfiguredSources {
                static_env: false,
                ..all
            },
            SourceKind::Profile,
        ),
        (
            ConfiguredSources {
                web_identity: true,
                container: true,
                ..ConfiguredSources::default()
            },
            SourceKind::WebIdentity,
        ),
        (
            ConfiguredSources {
                container: true,
                ..ConfiguredSources::default()
            },
            SourceKind::Ecs,
        ),
    ];
    for (sources, expected) in order {
        assert_eq!(
            select_source(&Preflight::Configured(sources)),
            Selection::Build(expected)
        );
    }
    assert_eq!(
        select_source(&Preflight::Absent),
        Selection::Build(SourceKind::DefaultChain)
    );
    // The default chain is selected for the Absent class only.
    for sources in [
        all,
        ConfiguredSources {
            container: true,
            ..ConfiguredSources::default()
        },
    ] {
        assert_ne!(
            select_source(&Preflight::Configured(sources)),
            Selection::Build(SourceKind::DefaultChain)
        );
    }
}

#[test]
fn source_order_env_pair_beats_profile_and_profile_beats_web_identity() {
    let env = MapEnv::new()
        .with("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
        .with("AWS_SECRET_ACCESS_KEY", "secret");
    let static_profile = facts(true, &["aws_access_key_id", "aws_secret_access_key"]);
    assert_eq!(
        select_source(&class(&env, &static_profile)),
        Selection::Build(SourceKind::Env)
    );
    let env = MapEnv::new()
        .with("AWS_WEB_IDENTITY_TOKEN_FILE", "/tmp/token")
        .with("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/r");
    assert_eq!(
        select_source(&class(&env, &static_profile)),
        Selection::Build(SourceKind::Profile)
    );
}

fn decide(outcomes: &[AttemptOutcome], skip_auth: bool) -> AuthDecision {
    let mut auth = RequestAuth::new();
    for outcome in outcomes {
        auth.record(*outcome);
    }
    auth.decide(skip_auth)
}

#[test]
fn precedence_picks_the_highest_failure_of_any_attempt() {
    use AttemptOutcome::{AbsentNotLoaded, ConfiguredNotLoaded, Invalid, Loaded, ProviderFailed};
    let invalid = Invalid(InvalidReason::PartialStaticKeys);
    for skip in [false, true] {
        // A success signs, even after a failed first attempt.
        assert_eq!(
            decide(&[ConfiguredNotLoaded, Loaded], skip),
            AuthDecision::Sign
        );
        assert_eq!(decide(&[ProviderFailed, Loaded], skip), AuthDecision::Sign);
        // Configured provider error, then the rebuild finds Absent: fails closed.
        assert_eq!(
            decide(&[ProviderFailed, AbsentNotLoaded], skip),
            AuthDecision::Refuse(CredentialRefusal::ProviderFailed)
        );
        assert_eq!(
            decide(&[ConfiguredNotLoaded, AbsentNotLoaded], skip),
            AuthDecision::Refuse(CredentialRefusal::ConfiguredSourceNotLoaded)
        );
        assert_eq!(
            decide(&[ConfiguredNotLoaded, ProviderFailed], skip),
            AuthDecision::Refuse(CredentialRefusal::ProviderFailed)
        );
        assert_eq!(
            decide(&[AbsentNotLoaded, invalid], skip),
            AuthDecision::Refuse(CredentialRefusal::InvalidConfiguration(
                InvalidReason::PartialStaticKeys
            ))
        );
        assert_eq!(
            decide(&[invalid], skip),
            AuthDecision::Refuse(CredentialRefusal::InvalidConfiguration(
                InvalidReason::PartialStaticKeys
            ))
        );
    }
    // Unsigned only when every attempt was level 4 and --skip-auth is set.
    assert_eq!(decide(&[AbsentNotLoaded], true), AuthDecision::Unsigned);
    assert_eq!(
        decide(&[AbsentNotLoaded], false),
        AuthDecision::Refuse(CredentialRefusal::NoCredentials)
    );
    assert_eq!(
        decide(&[], true),
        AuthDecision::Refuse(CredentialRefusal::NoCredentials)
    );
}

#[test]
fn only_levels_two_and_three_rebuild_and_only_once() {
    use AttemptOutcome::{AbsentNotLoaded, ConfiguredNotLoaded, Invalid, Loaded, ProviderFailed};
    let levels = [
        (Loaded, None, false),
        (Invalid(InvalidReason::OrphanSessionToken), Some(1), false),
        (ProviderFailed, Some(2), true),
        (ConfiguredNotLoaded, Some(3), true),
        (AbsentNotLoaded, Some(4), false),
    ];
    for (outcome, level, rebuild) in levels {
        assert_eq!(outcome.level(), level, "{outcome:?}");
        assert_eq!(outcome.triggers_rebuild(), rebuild, "{outcome:?}");
        let mut auth = RequestAuth::new();
        auth.record(outcome);
        assert_eq!(auth.should_retry(outcome), rebuild, "{outcome:?}");
        auth.record(outcome);
        assert!(!auth.should_retry(outcome), "{outcome:?}");
    }
}

#[test]
fn a_signed_session_never_silently_becomes_anonymous() {
    let mut session = SessionAuth::new();
    // An unsigned session may start signing.
    assert_eq!(
        session.apply(AuthDecision::Unsigned),
        AuthDecision::Unsigned
    );
    assert!(!session.has_signed());
    assert_eq!(session.apply(AuthDecision::Sign), AuthDecision::Sign);
    assert!(session.has_signed());
    // Then a level-4 outcome under --skip-auth fails closed.
    assert_eq!(
        session.apply(AuthDecision::Unsigned),
        AuthDecision::Refuse(CredentialRefusal::SignedSessionLostCredentials)
    );
    // Refusals pass through unchanged.
    let refusal = AuthDecision::Refuse(CredentialRefusal::ProviderFailed);
    assert_eq!(session.apply(refusal), refusal);
    // A new session starts clean.
    session.reset();
    assert_eq!(
        session.apply(AuthDecision::Unsigned),
        AuthDecision::Unsigned
    );
}

#[test]
fn the_preflight_reads_only_presence_of_listed_variables() {
    // A value that would be a secret is never needed: only presence counts,
    // so two different values classify identically.
    let a = MapEnv::new()
        .with("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
        .with("AWS_SECRET_ACCESS_KEY", "one");
    let b = MapEnv::new()
        .with("AWS_ACCESS_KEY_ID", "OTHER")
        .with("AWS_SECRET_ACCESS_KEY", "two");
    assert_eq!(class(&a, &no_profile()), class(&b, &no_profile()));
    assert_eq!(PREFLIGHT_ENV_VARS.len(), 11);
}
