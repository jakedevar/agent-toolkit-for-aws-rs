//! Loading the runtime registry (`load_profiles`).

use std::fs;
use std::path::Path;

use toolkit_core::{parse_json, Json};

use super::shown;
use super::value::Res;
use super::ScoreError;
use crate::startup::read_text_under;

const REQUIRED_PROFILE_KEYS: [&str; 4] = ["id", "status", "affinities", "hard_constraints"];

/// The status Python's `load_profiles` selects by default.
pub(crate) const GA_ONLY: [&str; 1] = ["ga"];

/// The profile's `id` (validated as a string when the profile was loaded).
pub(crate) fn profile_id(profile: &Json) -> &str {
    profile.get("id").and_then(Json::as_str).unwrap_or("")
}

/// Loads the runtime profiles of `dir` whose status is in `statuses`, sorted
/// by id. Every `*.json` file is read and checked, whatever its status; a
/// file that is a link or not a regular file is refused.
///
/// # Errors
///
/// An unreadable directory or file, invalid JSON, a profile that is not an
/// object, misses a required key or has an `id` that is not a string.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub(crate) fn load_profiles(dir: &Path, statuses: &[&str]) -> Res<Vec<Json>> {
    let shown_dir = shown(&dir.display().to_string());
    let entries = fs::read_dir(dir).map_err(|error| {
        ScoreError::new(format!(
            "cannot read runtime profiles in {shown_dir}: {error}"
        ))
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            ScoreError::new(format!(
                "cannot list runtime profiles in {shown_dir}: {error}"
            ))
        })?;
        let os_name = entry.file_name();
        // Python's `*.json` glob is case-sensitive and also matches `.json`.
        if !os_name.to_string_lossy().ends_with(".json") {
            continue;
        }
        let Some(name) = os_name.to_str() else {
            return Err(ScoreError::new(format!(
                "{shown_dir} holds a profile file name that is not valid UTF-8"
            )));
        };
        names.push(name.to_string());
    }
    names.sort();

    let mut profiles = Vec::new();
    for name in &names {
        let label = shown(&dir.join(name).display().to_string());
        let text = read_text_under(dir, &dir.join(name), &shown(name)).map_err(ScoreError::new)?;
        let profile = parse_json(&text)
            .map_err(|error| ScoreError::new(format!("{label}: invalid JSON ({error})")))?;
        if !matches!(profile, Json::Object(_)) {
            return Err(ScoreError::new(format!(
                "{label}: a runtime profile must be a JSON object, found {}",
                profile.type_name()
            )));
        }
        let missing: Vec<String> = REQUIRED_PROFILE_KEYS
            .iter()
            .filter(|key| profile.get(key).is_none())
            .map(|key| format!("'{key}'"))
            .collect();
        if !missing.is_empty() {
            return Err(ScoreError::new(format!(
                "{label}: missing required keys [{}]",
                missing.join(", ")
            )));
        }
        if profile.get("id").and_then(Json::as_str).is_none() {
            return Err(ScoreError::new(format!("{label}: \"id\" must be a string")));
        }
        let status = profile.get("status").and_then(Json::as_str);
        if status.is_some_and(|status| statuses.contains(&status)) {
            profiles.push(profile);
        }
    }
    profiles.sort_by(|left, right| profile_id(left).cmp(profile_id(right)));
    Ok(profiles)
}
