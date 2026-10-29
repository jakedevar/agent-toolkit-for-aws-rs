//! The R-ABS profile adapter (#4413 S1): the shared `config` and
//! `credentials` files as the preflight needs them.
//!
//! [`profile_view`] resolves each file's path the way aws-config does (the
//! `AWS_CONFIG_FILE`/`AWS_SHARED_CREDENTIALS_FILE` override, else
//! `~/.aws/config`/`~/.aws/credentials` with the SDK's home-directory rule),
//! reads it, and hands both texts to [`profile_facts`], which parses them with
//! aws-config's **own** profile parser (`aws_config::profile::load`), so file
//! semantics match the provider chain. Only existence of the selected profile
//! and the **names** of its credential-bearing keys leave this module; no
//! value is read into a result, logged or compared.
//!
//! Where aws-config would quietly read a missing or unreadable file as empty,
//! the preflight is stricter: an override naming a missing path is
//! [`ProfileView::MissingOverride`], and a file that exists but cannot be read
//! or parsed is [`ProfileView::Unusable`] (R-ABS "Invalid"). A missing file at
//! a default path reads as empty, as in the SDK.

use std::borrow::Cow;
use std::future::Future;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};
use aws_runtime::fs_util::{home_dir, Os};
use aws_types::os_shim_internal::{Env as SdkEnv, Fs};

use crate::env::Env;
use crate::preflight::{ProfileFacts, ProfileView, SelectedProfile, CREDENTIAL_PROFILE_KEYS};

/// One profile file: its kind, override variable and default path.
const FILES: [(EnvConfigFileKind, &str, &str); 2] = [
    (EnvConfigFileKind::Config, "AWS_CONFIG_FILE", ".aws/config"),
    (
        EnvConfigFileKind::Credentials,
        "AWS_SHARED_CREDENTIALS_FILE",
        ".aws/credentials",
    ),
];

/// Poll a future that never waits (in-memory profile parsing) to completion.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

/// Parse the two file texts with aws-config's parser and report the selected
/// profile's existence and credential-bearing key names. `None` when the
/// parser refuses the files.
#[must_use]
pub fn profile_facts(
    config: &str,
    credentials: &str,
    selected: &SelectedProfile,
) -> Option<ProfileFacts> {
    let files = EnvConfigFiles::builder()
        .with_contents(EnvConfigFileKind::Config, config)
        .with_contents(EnvConfigFileKind::Credentials, credentials)
        .build();
    let fs = Fs::from_slice(&[]);
    let env = SdkEnv::from_slice(&[]);
    let override_name = Some(Cow::Owned(selected.name().to_string()));
    let parsed = ready(aws_config::profile::load(&fs, &env, &files, override_name))?.ok()?;
    let Some(profile) = parsed.get_profile(selected.name()) else {
        return Some(ProfileFacts::default());
    };
    let has = |name: &str| profile.get(name).is_some();
    // Key names only: a `source_profile` chain is assumed to reach any profile
    // that runs a process (the `source_profile` value is never read).
    let any_process = parsed
        .profiles()
        .filter_map(|name| parsed.get_profile(name))
        .any(|other| other.get("credential_process").is_some());
    Some(ProfileFacts {
        profile_exists: true,
        credential_keys: CREDENTIAL_PROFILE_KEYS
            .iter()
            .copied()
            .filter(|key| has(key))
            .collect(),
        credential_process_reachable: has("credential_process")
            || (has("source_profile") && any_process),
    })
}

/// Where one file is, and whether the user chose the path.
fn file_path(env: &dyn Env, variable: &str, default: &str) -> Option<(PathBuf, bool)> {
    if let Some(path) = env.var(variable) {
        return Some((PathBuf::from(path), true));
    }
    let home = home_dir_from(env)?;
    Some((PathBuf::from(home).join(default), false))
}

/// The SDK's home-directory rule over our environment view.
fn home_dir_from(env: &dyn Env) -> Option<String> {
    let names = ["HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH"];
    let pairs: Vec<(&str, String)> = names
        .iter()
        .filter_map(|name| env.var(name).map(|value| (*name, value)))
        .collect();
    let borrowed: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    home_dir(&SdkEnv::from_slice(&borrowed), Os::real())
}

/// The selected profile's `region` value, for the proxy's `AWS_REGION`
/// metadata default (#4413 S3; upstream uses the boto3 session region). `None`
/// when the files cannot be read or parsed, the profile is missing or has no
/// valid region. A region is not a credential; no other value is read.
#[must_use]
pub fn profile_region(env: &dyn Env, selected: &SelectedProfile) -> Option<String> {
    let texts = read_texts(env).ok()?;
    let files = EnvConfigFiles::builder()
        .with_contents(EnvConfigFileKind::Config, texts[0].as_str())
        .with_contents(EnvConfigFileKind::Credentials, texts[1].as_str())
        .build();
    let fs = Fs::from_slice(&[]);
    let sdk_env = SdkEnv::from_slice(&[]);
    let name = Some(Cow::Owned(selected.name().to_string()));
    let parsed = ready(aws_config::profile::load(&fs, &sdk_env, &files, name))?.ok()?;
    let region = parsed.get_profile(selected.name())?.get("region")?.trim();
    crate::endpoint::is_valid_region(region).then(|| region.to_string())
}

/// Both file texts (config, credentials), or the view that refuses them.
fn read_texts(env: &dyn Env) -> Result<Vec<String>, ProfileView> {
    let mut texts = Vec::with_capacity(2);
    for (_, variable, default) in FILES {
        let Some((path, explicit)) = file_path(env, variable, default) else {
            // No home directory: the SDK cannot expand `~` and reads nothing.
            texts.push(String::new());
            continue;
        };
        match std::fs::read(&path) {
            Ok(bytes) => texts.push(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if explicit {
                    return Err(ProfileView::MissingOverride);
                }
                texts.push(String::new());
            }
            Err(_) => return Err(ProfileView::Unusable),
        }
    }
    Ok(texts)
}

/// Read both profile files for the preflight.
#[must_use]
pub fn profile_view(env: &dyn Env, selected: &SelectedProfile) -> ProfileView {
    let texts = match read_texts(env) {
        Ok(texts) => texts,
        Err(view) => return view,
    };
    match profile_facts(&texts[0], &texts[1], selected) {
        Some(facts) => ProfileView::Loaded(facts),
        None => ProfileView::Unusable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::sdk::test_support::TempDir;

    fn implicit() -> SelectedProfile {
        SelectedProfile::resolve(None, &MapEnv::new())
    }

    fn named(name: &str) -> SelectedProfile {
        SelectedProfile::resolve(Some(name), &MapEnv::new())
    }

    #[test]
    fn every_credential_key_is_found_by_name_in_either_file() {
        for key in CREDENTIAL_PROFILE_KEYS {
            let config = format!("[profile dev]\n{key} = value\nregion = us-east-1\n");
            let facts = profile_facts(&config, "", &named("dev")).expect("parsed");
            assert!(facts.profile_exists, "{key}");
            assert_eq!(
                facts.credential_keys.iter().copied().collect::<Vec<_>>(),
                vec![*key]
            );
            let credentials = format!("[dev]\n{key} = value\n");
            let facts = profile_facts("", &credentials, &named("dev")).expect("parsed");
            assert_eq!(facts.credential_keys.len(), 1, "{key}");
        }
    }

    #[test]
    fn the_default_profile_and_non_credential_keys() {
        let config = "[default]\nregion = us-west-2\noutput = json\n";
        let facts = profile_facts(config, "", &implicit()).expect("parsed");
        assert!(facts.profile_exists);
        assert!(facts.credential_keys.is_empty());
        // `[profile default]` is the default profile too.
        let facts = profile_facts(
            "[profile default]\ncredential_process = /bin/true\n",
            "",
            &implicit(),
        )
        .expect("parsed");
        assert_eq!(
            facts.credential_keys.iter().copied().collect::<Vec<_>>(),
            vec!["credential_process"]
        );
        // A missing profile does not exist; another profile's keys are not
        // the selected one's.
        let facts =
            profile_facts("[profile other]\nrole_arn = x\n", "", &named("dev")).expect("parsed");
        assert_eq!(facts, ProfileFacts::default());
        // An `[sso-session dev]` section is not a profile.
        let facts = profile_facts(
            "[sso-session dev]\nsso_region = us-east-1\n",
            "",
            &named("dev"),
        )
        .expect("parsed");
        assert!(!facts.profile_exists);
    }

    #[test]
    fn files_the_sdk_parser_refuses_are_unusable() {
        assert!(profile_facts("[default\nregion = x\n", "", &implicit()).is_none());
        assert!(profile_facts("no section = x\n", "", &implicit()).is_none());
    }

    #[test]
    fn paths_follow_overrides_then_the_home_directory() {
        let tmp = TempDir::new("profile-view");
        let config = tmp.write("cfg", "[default]\naws_access_key_id = AKIDEXAMPLE\n");
        let credentials = tmp.write("creds", "");
        let env = MapEnv::new()
            .with("AWS_CONFIG_FILE", &config.display().to_string())
            .with(
                "AWS_SHARED_CREDENTIALS_FILE",
                &credentials.display().to_string(),
            );
        let ProfileView::Loaded(facts) = profile_view(&env, &implicit()) else {
            panic!("loaded");
        };
        assert!(facts.credential_keys.contains("aws_access_key_id"));

        // An override naming a missing file.
        let missing = tmp.path().join("nope").display().to_string();
        let env = MapEnv::new()
            .with("AWS_CONFIG_FILE", &missing)
            .with("HOME", &tmp.path().display().to_string());
        assert_eq!(
            profile_view(&env, &implicit()),
            ProfileView::MissingOverride
        );
        let env = MapEnv::new()
            .with("AWS_SHARED_CREDENTIALS_FILE", "")
            .with("HOME", &tmp.path().display().to_string());
        assert_eq!(
            profile_view(&env, &implicit()),
            ProfileView::MissingOverride
        );

        // Default paths under HOME: missing files read as empty.
        let home = TempDir::new("profile-home");
        let env = MapEnv::new().with("HOME", &home.path().display().to_string());
        assert_eq!(
            profile_view(&env, &implicit()),
            ProfileView::Loaded(ProfileFacts::default())
        );
        home.write(".aws/credentials", "[default]\naws_secret_access_key = s\n");
        let ProfileView::Loaded(facts) = profile_view(&env, &implicit()) else {
            panic!("loaded");
        };
        assert!(facts.credential_keys.contains("aws_secret_access_key"));

        // A path that exists but is not a readable file, and a broken file.
        let env = MapEnv::new()
            .with("AWS_CONFIG_FILE", &tmp.path().display().to_string())
            .with("HOME", &home.path().display().to_string());
        assert_eq!(profile_view(&env, &implicit()), ProfileView::Unusable);
        let broken = tmp.write("broken", "[default\n");
        let env = MapEnv::new()
            .with("AWS_CONFIG_FILE", &broken.display().to_string())
            .with("HOME", &home.path().display().to_string());
        assert_eq!(profile_view(&env, &implicit()), ProfileView::Unusable);
    }
}

#[cfg(test)]
mod s3_tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::sdk::test_support::TempDir;

    fn named(name: &str) -> SelectedProfile {
        SelectedProfile::resolve(Some(name), &MapEnv::new())
    }

    #[test]
    fn a_credential_process_is_reachable_directly_or_through_a_source_profile() {
        let direct = "[profile dev]\ncredential_process = /bin/false\n";
        let facts = profile_facts(direct, "", &named("dev")).expect("parsed");
        assert!(facts.credential_process_reachable);
        let chained = "[profile dev]\nrole_arn = arn:aws:iam::1:role/r\nsource_profile = base\n\
                       [profile base]\ncredential_process = /bin/false\n";
        let facts = profile_facts(chained, "", &named("dev")).expect("parsed");
        assert!(facts.credential_process_reachable);
        // No source_profile: another profile's process is not reachable.
        let other = "[profile dev]\naws_access_key_id = AKID\naws_secret_access_key = s\n\
                     [profile base]\ncredential_process = /bin/false\n";
        let facts = profile_facts(other, "", &named("dev")).expect("parsed");
        assert!(!facts.credential_process_reachable);
        // A chain with no process anywhere.
        let static_chain =
            "[profile dev]\nrole_arn = arn:aws:iam::1:role/r\nsource_profile = base\n\
                            [profile base]\naws_access_key_id = AKID\naws_secret_access_key = s\n";
        let facts = profile_facts(static_chain, "", &named("dev")).expect("parsed");
        assert!(!facts.credential_process_reachable);
    }

    #[test]
    fn the_profile_region_is_read_and_validated() {
        let tmp = TempDir::new("profile-region");
        let config = tmp.write(
            "cfg",
            "[profile dev]\nregion = eu-west-1\n[profile bad]\nregion = not a region\n",
        );
        let env = MapEnv::new()
            .with("AWS_CONFIG_FILE", &config.display().to_string())
            .with("HOME", &tmp.path().display().to_string());
        assert_eq!(
            profile_region(&env, &named("dev")).as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(profile_region(&env, &named("bad")), None);
        assert_eq!(profile_region(&env, &named("missing")), None);
    }
}
