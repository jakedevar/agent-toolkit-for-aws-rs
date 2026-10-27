//! Injectable view of the process environment.
//!
//! Configuration code reads settings through [`Env`] so tests supply a fixed
//! [`MapEnv`] and never depend on (or read) the real environment. This crate
//! only ever asks for non-secret settings: region, profile name and endpoint
//! URLs. Credentials are never read here.

use std::collections::BTreeMap;
use std::fmt;

use crate::redact::{is_secret_name, write_scrubbed_debug, Scrubbed, REDACTED};

/// Source of environment variables.
pub trait Env {
    /// Value of `name`, or `None` when unset or not valid Unicode.
    fn var(&self, name: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// A fixed in-memory environment for tests and for callers that pre-resolve.
///
/// `Debug` hides values of secret-looking names and scrubs the others.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MapEnv(BTreeMap<String, String>);

impl fmt::Debug for MapEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_scrubbed_debug(f, |f| {
            let mut map = f.debug_map();
            for (name, value) in &self.0 {
                if is_secret_name(name) {
                    map.entry(&Scrubbed(name), &REDACTED);
                } else {
                    map.entry(&Scrubbed(name), &Scrubbed(value));
                }
            }
            map.finish()
        })
    }
}

impl MapEnv {
    /// An empty environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace one variable.
    #[must_use]
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.0.insert(name.to_string(), value.to_string());
        self
    }
}

impl Env for MapEnv {
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// Value of `name` treating an empty string as unset (the Python `or` rule).
#[must_use]
pub fn non_empty(env: &dyn Env, name: &str) -> Option<String> {
    env.var(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_env_returns_only_what_was_set() {
        let env = MapEnv::new().with("A", "1").with("B", "");
        assert_eq!(env.var("A").as_deref(), Some("1"));
        assert_eq!(env.var("B").as_deref(), Some(""));
        assert_eq!(env.var("C"), None);
    }

    #[test]
    fn map_env_debug_hides_secret_names_and_scrubs_values() {
        let env = MapEnv::new()
            .with("AWS_SECRET_ACCESS_KEY", "FAKE-SECRET-VALUE")
            .with("MY_TOKEN", "FAKE-TOKEN-VALUE")
            .with("NOTE", "see https://FAKE-USER:FAKE-PW@h.example/ now")
            .with("AWS_REGION", "us-east-1");
        let shown = format!("{env:?}");
        assert!(!shown.contains("FAKE-"), "{shown}");
        assert!(shown.contains("\"AWS_REGION\": \"us-east-1\""));
        assert!(shown.contains("\"MY_TOKEN\": \"[REDACTED]\""));
    }

    #[test]
    fn later_values_replace_earlier_ones() {
        let env = MapEnv::new().with("A", "1").with("A", "2");
        assert_eq!(env.var("A").as_deref(), Some("2"));
    }

    #[test]
    fn empty_values_count_as_unset() {
        let env = MapEnv::new().with("A", "").with("B", "x");
        assert_eq!(non_empty(&env, "A"), None);
        assert_eq!(non_empty(&env, "B").as_deref(), Some("x"));
        assert_eq!(non_empty(&env, "C"), None);
    }

    #[test]
    fn process_env_reports_unset_variables_as_none() {
        // A name no environment defines; asserts behavior without reading a real setting.
        assert_eq!(ProcessEnv.var("TOOLKIT_AWS_TEST_SURELY_UNSET_7F3A"), None);
    }
}
