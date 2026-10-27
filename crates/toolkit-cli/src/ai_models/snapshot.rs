//! The short-lived catalog snapshot of `get_deployable_models.py`
//! (`build_catalog_snapshot`, `validate_catalog_snapshot`,
//! `write_catalog_snapshot`, `create_catalog_snapshot_file`,
//! `load_catalog_snapshot`).
//!
//! A snapshot is a private (mode 0600) JSON cache of the normalized
//! `ListHubContents` summaries, written once per selection session. Reads and
//! writes refuse symbolic links and non-regular files. Every message is built
//! from fixed text plus escaped, bounded quotations, so a hostile snapshot or
//! path cannot forge a diagnostic line.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, quote_untrusted, read_regular_file, Json, PathError};

use super::normalize::{list_available_values, SIZE_BUCKETS};
use crate::dynamodb::pyfmt;
use crate::manifests::read_failure;
use crate::safe_text::safe_path;

/// `SNAPSHOT_SCHEMA_VERSION`.
pub(super) const SCHEMA_VERSION: i64 = 1;

/// Largest snapshot file read, in bytes.
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

/// A missing or malformed snapshot (`CatalogSnapshotError`); the text is safe
/// to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SnapshotError(pub(super) String);

fn fail<T>(message: impl Into<String>) -> Result<T, SnapshotError> {
    Err(SnapshotError(message.into()))
}

fn objects(models: &[Json]) -> bool {
    models.iter().all(|model| matches!(model, Json::Object(_)))
}

/// A short description of a value for a message: strings are quoted and
/// escaped, everything else is named by type.
fn describe(value: Option<&Json>) -> String {
    match value {
        None => "missing".to_string(),
        Some(Json::String(text)) => quote_untrusted(text),
        Some(Json::Int(number)) => number.to_string(),
        Some(other) => other.type_name().to_string(),
    }
}

/// Builds the snapshot of `models`. `region` and `fetched_at` are required
/// (the caller supplies the clock).
///
/// # Errors
///
/// A [`SnapshotError`] for an empty hub, region or timestamp, or models that
/// are not objects.
pub(super) fn build_catalog_snapshot(
    hub_name: &str,
    region: Option<&str>,
    models: &[Json],
    fetched_at: &str,
) -> Result<Json, SnapshotError> {
    if hub_name.is_empty() {
        return fail("Snapshot hub_name must be a non-empty string");
    }
    let Some(region) = region.filter(|region| !region.is_empty()) else {
        return fail("A configured AWS region is required to create a catalog snapshot");
    };
    if !objects(models) {
        return fail("Snapshot catalog_models must be a list of objects");
    }
    if fetched_at.is_empty() {
        return fail("Snapshot fetched_at must be a non-empty timestamp");
    }

    let mut creation_times: Vec<(String, Json)> = Vec::new();
    for model in models {
        if let (Some(Json::String(name)), Some(Json::String(time))) =
            (model.get("name"), model.get("original_creation_time"))
        {
            // A repeated name keeps the first position and the last value.
            if let Some(slot) = creation_times.iter_mut().find(|(key, _)| key == name) {
                slot.1 = Json::string(time.as_str());
            } else {
                creation_times.push((name.clone(), Json::string(time.as_str())));
            }
        }
    }
    let raw_sizes_by_bucket: Vec<(String, Json)> = SIZE_BUCKETS
        .iter()
        .map(|bucket| {
            let mut raw: Vec<&str> = models
                .iter()
                .filter(|model| model.get("size").and_then(Json::as_str) == Some(*bucket))
                .filter_map(|model| model.get("size_raw").and_then(Json::as_str))
                .collect();
            raw.sort_unstable();
            raw.dedup();
            // Case-insensitive order; spellings that differ only in case keep
            // plain order so the output is deterministic.
            raw.sort_by_key(|text| text.to_lowercase());
            (
                (*bucket).to_string(),
                Json::Array(raw.into_iter().map(Json::string).collect()),
            )
        })
        .collect();
    let available_values = list_available_values(models).map_err(SnapshotError)?;
    Ok(Json::object(vec![
        ("schema_version", Json::Int(SCHEMA_VERSION)),
        ("hub_name", Json::string(hub_name)),
        ("region", Json::string(region)),
        ("fetched_at", Json::string(fetched_at)),
        (
            "model_count",
            Json::Int(i64::try_from(models.len()).unwrap_or(i64::MAX)),
        ),
        ("available_values", available_values),
        ("raw_sizes_by_bucket", Json::Object(raw_sizes_by_bucket)),
        ("original_creation_times", Json::Object(creation_times)),
        ("catalog_models", Json::Array(models.to_vec())),
    ]))
}

fn non_empty_text<'a>(snapshot: &'a Json, key: &str, rule: &str) -> Result<&'a str, SnapshotError> {
    match snapshot.get(key) {
        Some(Json::String(text)) if !text.is_empty() => Ok(text),
        _ => fail(rule),
    }
}

/// Validates a snapshot before it is used for Search or the local fallback.
///
/// # Errors
///
/// A [`SnapshotError`] naming the first rule the snapshot breaks.
pub(super) fn validate_catalog_snapshot(
    snapshot: &Json,
    expected_hub: Option<&str>,
    expected_region: Option<&str>,
) -> Result<(), SnapshotError> {
    if !matches!(snapshot, Json::Object(_)) {
        return fail("Catalog snapshot must be a JSON object");
    }
    if snapshot.get("schema_version") != Some(&Json::Int(SCHEMA_VERSION)) {
        return fail(format!(
            "Unsupported catalog snapshot schema: {}",
            describe(snapshot.get("schema_version"))
        ));
    }
    let hub_name = non_empty_text(
        snapshot,
        "hub_name",
        "Catalog snapshot hub_name must be a non-empty string",
    )?;
    if expected_hub.is_some_and(|expected| expected != hub_name) {
        return fail(format!(
            "Catalog snapshot is for Hub {}, not {}",
            quote_untrusted(hub_name),
            quote_untrusted(expected_hub.unwrap_or(""))
        ));
    }
    let region = non_empty_text(
        snapshot,
        "region",
        "Catalog snapshot region must be a non-empty string",
    )?;
    if expected_region.is_some_and(|expected| expected != region) {
        return fail(format!(
            "Catalog snapshot is for region {}, not {}",
            quote_untrusted(region),
            quote_untrusted(expected_region.unwrap_or(""))
        ));
    }
    non_empty_text(
        snapshot,
        "fetched_at",
        "Catalog snapshot fetched_at must be a timestamp string",
    )?;
    let Some(Json::Array(models)) = snapshot.get("catalog_models") else {
        return fail("Catalog snapshot catalog_models must be a list of objects");
    };
    if !objects(models) {
        return fail("Catalog snapshot catalog_models must be a list of objects");
    }
    let Some(Json::Int(count)) = snapshot.get("model_count") else {
        return fail("Catalog snapshot model_count must be an integer");
    };
    if usize::try_from(*count).ok() != Some(models.len()) {
        return fail("Catalog snapshot model_count does not match catalog_models length");
    }
    let mut names: Vec<&str> = Vec::with_capacity(models.len());
    for model in models {
        match model.get("name") {
            Some(Json::String(name)) if !name.is_empty() => names.push(name),
            _ => return fail("Every cached model must have a non-empty name"),
        }
    }
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return fail("Catalog snapshot contains duplicate model names");
    }
    for field in [
        "available_values",
        "raw_sizes_by_bucket",
        "original_creation_times",
    ] {
        if !matches!(snapshot.get(field), Some(Json::Object(_))) {
            return fail(format!("Catalog snapshot {field} must be an object"));
        }
    }
    Ok(())
}

fn write_failure(error: &std::io::Error) -> SnapshotError {
    SnapshotError(format!(
        "Could not write catalog snapshot: {}",
        quote_untrusted(&error.to_string())
    ))
}

/// Opens `path` for a private write: refuses a symbolic link and a
/// non-regular file, then restricts the file to its owner before any content
/// is written.
fn open_private(path: &Path) -> Result<fs::File, SnapshotError> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return fail("Could not write catalog snapshot: symbolic links are not followed");
        }
        Ok(metadata) if !metadata.is_file() => {
            return fail("Could not write catalog snapshot: not a regular file");
        }
        Ok(_) => options.open(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            options.create_new(true).open(path)
        }
        Err(error) => return Err(write_failure(&error)),
    }
    .map_err(|error| write_failure(&error))?;
    #[cfg(unix)]
    {
        // The open mode applies only to new files: restrict an existing
        // caller-owned path as well.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| write_failure(&error))?;
    }
    let opened = file.metadata().map_err(|error| write_failure(&error))?;
    if !opened.is_file() {
        return fail("Could not write catalog snapshot: not a regular file");
    }
    file.set_len(0).map_err(|error| write_failure(&error))?;
    Ok(file)
}

/// Writes a validated snapshot to a caller-owned path with private permissions.
///
/// # Errors
///
/// A [`SnapshotError`] for an invalid snapshot or an unwritable path.
pub(super) fn write_catalog_snapshot(path: &Path, snapshot: &Json) -> Result<(), SnapshotError> {
    validate_catalog_snapshot(snapshot, None, None)?;
    let mut file = open_private(path)?;
    file.write_all(pyfmt::dumps(snapshot, false).as_bytes())
        .map_err(|error| write_failure(&error))
}

/// Characters of a random file-name suffix.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";

/// A unique `sherpa-model-catalog.XXXXXXXX` name part from the process id, the
/// clock and the randomized hasher.
fn random_suffix(attempt: u32) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u32(attempt);
    if let Some(now) = super::timeutil::now() {
        hasher.write_i64(now.epoch_micros());
    }
    let mut value = hasher.finish();
    let mut suffix = String::new();
    for _ in 0..8 {
        let base = u64::try_from(ALPHABET.len()).unwrap_or(37);
        let index = usize::try_from(value % base).unwrap_or(0);
        suffix.push(char::from(ALPHABET[index]));
        value /= base;
    }
    suffix
}

/// Creates a unique snapshot file in the temporary directory and writes the
/// snapshot; a failed write removes the file this function created.
///
/// # Errors
///
/// A [`SnapshotError`] for an invalid snapshot or when no file can be created.
pub(super) fn create_catalog_snapshot_file(snapshot: &Json) -> Result<PathBuf, SnapshotError> {
    validate_catalog_snapshot(snapshot, None, None)?;
    for attempt in 0..16 {
        let path =
            std::env::temp_dir().join(format!("sherpa-model-catalog.{}", random_suffix(attempt)));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&path) {
            Ok(mut file) => {
                let written = file.write_all(pyfmt::dumps(snapshot, false).as_bytes());
                drop(file);
                return match written {
                    Ok(()) => Ok(path),
                    Err(error) => {
                        let _ = fs::remove_file(&path);
                        Err(write_failure(&error))
                    }
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(write_failure(&error)),
        }
    }
    fail("Could not write catalog snapshot: no unused file name")
}

/// Reads and validates a snapshot from disk (symbolic links are refused).
///
/// # Errors
///
/// A [`SnapshotError`] for a missing, linked, unreadable, malformed or
/// mismatching snapshot.
pub(super) fn load_catalog_snapshot(
    path: &Path,
    expected_hub: Option<&str>,
    expected_region: Option<&str>,
) -> Result<Json, SnapshotError> {
    let shown = safe_path(&path.display().to_string());
    let text = read_regular_file(path, MAX_SNAPSHOT_BYTES).map_err(|error| match error {
        PathError::NotFound(_) => SnapshotError(format!("Catalog snapshot not found: {shown}")),
        other => SnapshotError(format!(
            "Could not read catalog snapshot: {}",
            read_failure(&other)
        )),
    })?;
    let snapshot = parse_json(&text).map_err(|error| {
        SnapshotError(format!(
            "Catalog snapshot is not valid JSON: {}",
            crate::ai_data::parse_failure_text(&error)
        ))
    })?;
    validate_catalog_snapshot(&snapshot, expected_hub, expected_region)?;
    Ok(snapshot)
}
