//! Offline normalizer of a user-run mongo-shell capture (#5327, S5 of the
//! #4886 item 2 design; requirement R-DG-08, Q1).
//!
//! `wa_review.py --uri` connected to the database with `pymongo` and ran
//! `collStats`, `$indexStats` and `listIndexes` per collection. The toolkit
//! has no database driver, shell launcher or connection: the user runs those
//! read-only commands one at a time in their own authenticated shell, pastes
//! the canonical Extended JSON replies into a versioned envelope, and
//! `toolkit documentdb normalize-capture` turns that envelope into the
//! `--analysis-data` snapshot of `wa-review` ([`super::snapshot`]).
//!
//! The normalizer accepts only the allowlisted envelope of [`MANIFEST`]
//! (mirrored by `references/collection-manifest.json`). It refuses anything
//! else, secret-bearing or malformed, with a fixed [`CaptureError`] class and
//! a fixed sentence: no value, name or key of the capture is ever echoed. A
//! capture file that is group- or world-accessible is refused before it is
//! parsed. Nothing here reads an environment variable, opens a socket or
//! starts a process.

use std::io::Read;
use std::path::Path;

use toolkit_core::{open_regular_file, parse_json_with_limits, Json, JsonLimits};

use super::snapshot::Num;

/// Fixed error classes of a refused capture. [`CaptureError::class`] is the
/// stable token printed in the diagnostic; [`CaptureError::message`] is the
/// fixed sentence. Neither carries data from the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureError {
    Unreadable,
    TooLarge,
    Permissions,
    Malformed,
    UnsupportedFormat,
    UnknownField,
    SecretField,
    InvalidNumber,
    InvalidErrorCode,
    Incomplete,
    Empty,
    OutputExists,
    OutputFailed,
}

impl CaptureError {
    pub(super) fn class(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::TooLarge => "too_large",
            Self::Permissions => "permissions",
            Self::Malformed => "malformed",
            Self::UnsupportedFormat => "unsupported_format",
            Self::UnknownField => "unknown_field",
            Self::SecretField => "secret_field",
            Self::InvalidNumber => "invalid_number",
            Self::InvalidErrorCode => "invalid_error_code",
            Self::Incomplete => "incomplete",
            Self::Empty => "empty",
            Self::OutputExists => "output_exists",
            Self::OutputFailed => "output_failed",
        }
    }

    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Unreadable => "the capture file cannot be opened as a regular file",
            Self::TooLarge => "the capture exceeds a size or count limit",
            Self::Permissions => {
                "the capture file must be private to its owner (mode 0600 or stricter)"
            }
            Self::Malformed => "the capture is not a well-formed collection capture envelope",
            Self::UnsupportedFormat => {
                "the capture format, dialect or shell is unsupported (canonical Extended JSON from mongosh 1.x or 2.x is required)"
            }
            Self::UnknownField => "the capture contains a field outside the allowlisted envelope",
            Self::SecretField => {
                "the capture contains a connection, credential or secret-shaped field or value"
            }
            Self::InvalidNumber => {
                "the capture contains a number that is not a finite, in-range canonical value"
            }
            Self::InvalidErrorCode => "an error marker is not one of the fixed status codes",
            Self::Incomplete => {
                "the capture records a connection or listing failure, so no snapshot is published"
            }
            Self::Empty => "the capture holds no collection statistics, so no snapshot is published",
            Self::OutputExists => "the output file already exists and is never overwritten",
            Self::OutputFailed => "the private output file cannot be created",
        }
    }
}

#[allow(clippy::struct_field_names)]
/// Size and count bounds. The defaults suit a large deployment; tests lower
/// them to exercise the boundary and boundary-plus-one cases.
pub(super) struct Limits {
    pub(super) max_bytes: u64,
    pub(super) max_databases: usize,
    pub(super) max_collections: usize,
    pub(super) max_indexes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_databases: 1_000,
            max_collections: 100_000,
            max_indexes: 256,
        }
    }
}

/// The normalized snapshot and what went into it.
pub(super) struct Normalized {
    pub(super) snapshot: Json,
    /// Databases kept (not excluded, holding at least one collection).
    pub(super) databases: usize,
    /// Collections kept, failed ones included.
    pub(super) collections: usize,
    /// Collections recorded as an error marker.
    pub(super) incomplete: usize,
}

#[derive(Clone, Copy)]
/// What the platform says about who can access the capture file.
pub(super) enum Access {
    /// A Unix `st_mode`.
    UnixMode(u32),
    /// Verified ACL evidence: whether only the owner has access.
    #[cfg_attr(not(test), allow(dead_code))]
    Acl { owner_only: bool },
    /// The platform cannot tell.
    #[cfg_attr(unix, allow(dead_code))]
    Unverifiable,
}

/// Admits owner-only evidence; refuses everything else (fail closed).
pub(super) fn admit_access(access: Access) -> Result<(), CaptureError> {
    match access {
        Access::UnixMode(mode) if mode.trailing_zeros() >= 6 => Ok(()),
        Access::Acl { owner_only: true } => Ok(()),
        _ => Err(CaptureError::Permissions),
    }
}

#[cfg(unix)]
fn access_of(metadata: &std::fs::Metadata) -> Access {
    use std::os::unix::fs::PermissionsExt;
    Access::UnixMode(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn access_of(_metadata: &std::fs::Metadata) -> Access {
    Access::Unverifiable
}

#[allow(clippy::struct_field_names)]
/// One read-only manual command of the recipe.
// Read by the manifest-equality tests; the recipe pages are generated from it by hand.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct Command {
    pub(super) id: &'static str,
    /// The envelope member the output is pasted into.
    pub(super) slot: &'static str,
    pub(super) command: &'static str,
    /// Output fields the normalizer reads.
    pub(super) fields: &'static [&'static str],
    /// Output fields accepted and dropped without being read.
    pub(super) ignored: &'static [&'static str],
}

/// The allowlist and envelope schema; `collection-manifest.json` mirrors it
/// and a test compares them.
pub(super) struct Manifest {
    pub(super) format: &'static str,
    pub(super) version: i64,
    pub(super) dialect: &'static str,
    pub(super) envelope_keys: &'static [&'static str],
    pub(super) database_keys: &'static [&'static str],
    pub(super) collection_keys: &'static [&'static str],
    pub(super) error_codes: &'static [&'static str],
    pub(super) excluded_databases: &'static [&'static str],
    pub(super) excluded_collection_prefix: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) commands: &'static [Command],
}

pub(super) const MANIFEST: Manifest = Manifest {
    format: "toolkit-docdb-collection-capture",
    version: 1,
    dialect: "mongosh-ejson-canonical",
    envelope_keys: &[
        "format", "version", "dialect", "shell", "engine", "error", "databases",
    ],
    database_keys: &["ordinal", "name", "error", "collections"],
    collection_keys: &[
        "ordinal",
        "name",
        "error",
        "collStats",
        "indexStats",
        "indexes",
    ],
    error_codes: &[
        "connection_failed",
        "authentication_failed",
        "unauthorized",
        "command_failed",
        "unsupported_command",
        "timeout",
    ],
    excluded_databases: &["admin", "local", "config"],
    excluded_collection_prefix: "system.",
    commands: &[
        Command {
            id: "list_databases",
            slot: "databases[].name",
            command: "db.adminCommand({ listDatabases: 1, nameOnly: true })",
            fields: &["databases[].name"],
            ignored: &[],
        },
        Command {
            id: "list_collections",
            slot: "databases[].collections[].name",
            command: "db.getSiblingDB(\"DATABASE\").runCommand({ listCollections: 1, nameOnly: true })",
            fields: &["cursor.firstBatch[].name"],
            ignored: &[],
        },
        Command {
            id: "coll_stats",
            slot: "databases[].collections[].collStats",
            command: "EJSON.stringify(db.getSiblingDB(\"DATABASE\").runCommand({ collStats: \"COLLECTION\" }), { relaxed: false })",
            fields: &[
                "count",
                "size",
                "avgObjSize",
                "storageSize",
                "totalIndexSize",
                "indexSizes",
                "compression.enabled",
                "compression.enable",
            ],
            ignored: COLL_STATS_IGNORED,
        },
        Command {
            id: "index_stats",
            slot: "databases[].collections[].indexStats",
            command: "EJSON.stringify(db.getSiblingDB(\"DATABASE\").getCollection(\"COLLECTION\").aggregate([{ $indexStats: {} }]).toArray(), { relaxed: false })",
            fields: &["name", "accesses.ops"],
            ignored: INDEX_STATS_IGNORED,
        },
        Command {
            id: "list_indexes",
            slot: "databases[].collections[].indexes",
            command: "EJSON.stringify(db.getSiblingDB(\"DATABASE\").getCollection(\"COLLECTION\").getIndexes(), { relaxed: false })",
            fields: &["name", "key", "expireAfterSeconds"],
            ignored: INDEX_IGNORED,
        },
    ],
};

const COLL_STATS_IGNORED: &[&str] = &[
    "ns",
    "ok",
    "capped",
    "nindexes",
    "scaleFactor",
    "totalSize",
    "freeStorageSize",
    "sharded",
];
const INDEX_STATS_IGNORED: &[&str] = &["key", "host", "spec", "shard"];
const INDEX_IGNORED: &[&str] = &["v", "ns", "unique", "sparse", "background", "hidden"];
const ACCESSES_IGNORED: &[&str] = &["since"];
const COMPRESSION_KEYS: &[&str] = &["enabled", "enable"];
const COLL_STATS_READ: &[&str] = &[
    "count",
    "size",
    "avgObjSize",
    "storageSize",
    "totalIndexSize",
    "indexSizes",
    "compression",
];
const INDEX_STATS_READ: &[&str] = &["name", "accesses"];
const INDEX_READ: &[&str] = &["name", "key", "expireAfterSeconds"];
const SHELL_KEYS: &[&str] = &["name", "version"];
const MAX_NAME_BYTES: usize = 255;
const MAX_IGNORED_DEPTH: usize = 8;
const JSON_LIMITS: JsonLimits = JsonLimits {
    max_depth: 32,
    max_nodes: 4_000_000,
};

type Pairs = [(String, Json)];

fn members(value: &Json) -> Option<&Pairs> {
    match value {
        Json::Object(pairs) => Some(pairs.as_slice()),
        _ => None,
    }
}

fn find<'a>(pairs: &'a Pairs, key: &str) -> Option<&'a Json> {
    pairs
        .iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

/// Key fragments that mark a connection string, credential or secret.
const SECRET_FRAGMENTS: [&str; 14] = [
    "password",
    "passwd",
    "secret",
    "token",
    "credential",
    "connectionstring",
    "connstring",
    "apikey",
    "accesskey",
    "privatekey",
    "authorization",
    "bearer",
    "cookie",
    "sessionkey",
];

/// A key that looks like a connection string, credential or secret: split on
/// non-alphanumerics and case changes, or any of the long names.
fn secret_shaped(key: &str) -> bool {
    let squashed: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if SECRET_FRAGMENTS
        .iter()
        .any(|needle| squashed.contains(needle))
    {
        return true;
    }
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in key.chars() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if ch.is_ascii_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = ch.is_ascii_lowercase();
        current.push(ch.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "uri" | "url" | "pwd" | "pass" | "auth" | "conn" | "connection" | "dsn"
        )
    })
}

fn classify(key: &str) -> CaptureError {
    if secret_shaped(key) {
        CaptureError::SecretField
    } else {
        CaptureError::UnknownField
    }
}

fn connection_like(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("://") || lower.starts_with("mongodb")
}

/// Every key must be allowed (read or ignored); the first that is not decides
/// the class.
fn check_keys(pairs: &Pairs, read: &[&str], ignored: &[&str]) -> Result<(), CaptureError> {
    for (key, _) in pairs {
        if !read.contains(&key.as_str()) && !ignored.contains(&key.as_str()) {
            return Err(classify(key));
        }
    }
    Ok(())
}

/// An ignored value is dropped unread, but still scanned for secrets.
fn scan_ignored(value: &Json, depth: usize) -> Result<(), CaptureError> {
    if depth > MAX_IGNORED_DEPTH {
        return Err(CaptureError::Malformed);
    }
    match value {
        Json::String(text) if connection_like(text) => Err(CaptureError::SecretField),
        Json::Array(items) => items
            .iter()
            .try_for_each(|item| scan_ignored(item, depth + 1)),
        Json::Object(pairs) => pairs.iter().try_for_each(|(key, item)| {
            if secret_shaped(key) {
                Err(CaptureError::SecretField)
            } else {
                scan_ignored(item, depth + 1)
            }
        }),
        _ => Ok(()),
    }
}

fn scan_ignored_members(pairs: &Pairs, ignored: &[&str]) -> Result<(), CaptureError> {
    for (key, value) in pairs {
        if ignored.contains(&key.as_str()) {
            scan_ignored(value, 0)?;
        }
    }
    Ok(())
}

/// A database, collection or index name.
fn name_of(value: &Json) -> Result<&str, CaptureError> {
    let Json::String(text) = value else {
        return Err(CaptureError::Malformed);
    };
    if connection_like(text) {
        return Err(CaptureError::SecretField);
    }
    if text.is_empty() || text.len() > MAX_NAME_BYTES || text.chars().any(char::is_control) {
        return Err(CaptureError::Malformed);
    }
    Ok(text)
}

fn ordinal_ok(pairs: &Pairs, expected: usize) -> Result<(), CaptureError> {
    match find(pairs, "ordinal") {
        Some(Json::Int(n)) if usize::try_from(*n) == Ok(expected) => Ok(()),
        _ => Err(CaptureError::Malformed),
    }
}

fn error_code(value: &Json) -> Result<&'static str, CaptureError> {
    let Json::String(text) = value else {
        return Err(CaptureError::InvalidErrorCode);
    };
    MANIFEST
        .error_codes
        .iter()
        .copied()
        .find(|code| *code == text.as_str())
        .ok_or_else(|| {
            if connection_like(text) {
                CaptureError::SecretField
            } else {
                CaptureError::InvalidErrorCode
            }
        })
}

// ---------------------------------------------------------- canonical numbers

fn parse_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || digits.len() > 19 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_double(text: &str) -> Option<f64> {
    if text.is_empty()
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// A canonical Extended JSON number. A bare JSON number is the relaxed dialect
/// and is refused as an unsupported format, never read as a double.
fn ejson_number(value: &Json, signed: bool) -> Result<Num, CaptureError> {
    let pairs = match value {
        Json::Object(pairs) => pairs.as_slice(),
        Json::Int(_) | Json::Float(_) => return Err(CaptureError::UnsupportedFormat),
        _ => return Err(CaptureError::Malformed),
    };
    let [(kind, Json::String(text))] = pairs else {
        return Err(CaptureError::Malformed);
    };
    let number = match kind.as_str() {
        "$numberInt" => parse_integer(text)
            .and_then(|n| i32::try_from(n).ok())
            .map(|n| Num::Int(i64::from(n))),
        "$numberLong" => parse_integer(text).map(Num::Int),
        "$numberDouble" => parse_double(text).map(Num::Float),
        "$numberDecimal" => None,
        _ => return Err(CaptureError::Malformed),
    };
    let number = number.ok_or(CaptureError::InvalidNumber)?;
    if !signed && number.value() < 0.0 {
        return Err(CaptureError::InvalidNumber);
    }
    Ok(number)
}

fn num_json(number: Num) -> Json {
    match number {
        Num::Int(n) => Json::Int(n),
        Num::Float(n) => Json::Float(n),
    }
}

fn optional_number(pairs: &Pairs, key: &str) -> Result<Num, CaptureError> {
    find(pairs, key).map_or(Ok(Num::Int(0)), |value| ejson_number(value, false))
}

// ------------------------------------------------------------ envelope checks

fn check_shell(value: &Json) -> Result<(), CaptureError> {
    let pairs = members(value).ok_or(CaptureError::Malformed)?;
    check_keys(pairs, SHELL_KEYS, &[])?;
    let (Some(Json::String(name)), Some(Json::String(version))) =
        (find(pairs, "name"), find(pairs, "version"))
    else {
        return Err(CaptureError::Malformed);
    };
    if name != "mongosh" {
        return Err(CaptureError::UnsupportedFormat);
    }
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|p| (1..=4).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(CaptureError::Malformed);
    }
    if !matches!(parts[0], "1" | "2") {
        return Err(CaptureError::UnsupportedFormat);
    }
    Ok(())
}

fn check_engine(value: &Json) -> Result<(), CaptureError> {
    let pairs = members(value).ok_or(CaptureError::Malformed)?;
    check_keys(pairs, SHELL_KEYS, &[])?;
    let (Some(Json::String(name)), Some(Json::String(version))) =
        (find(pairs, "name"), find(pairs, "version"))
    else {
        return Err(CaptureError::Malformed);
    };
    if !matches!(name.as_str(), "documentdb" | "mongodb") {
        return Err(CaptureError::UnsupportedFormat);
    }
    let parts: Vec<&str> = version.split('.').collect();
    if !(2..=3).contains(&parts.len())
        || !parts
            .iter()
            .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(CaptureError::Malformed);
    }
    Ok(())
}

fn check_header(pairs: &Pairs) -> Result<(), CaptureError> {
    check_keys(pairs, MANIFEST.envelope_keys, &[])?;
    match find(pairs, "format") {
        Some(Json::String(text)) if text == MANIFEST.format => {}
        Some(Json::String(_)) => return Err(CaptureError::UnsupportedFormat),
        _ => return Err(CaptureError::Malformed),
    }
    match find(pairs, "version") {
        Some(Json::Int(n)) if *n == MANIFEST.version => {}
        Some(Json::Int(_)) => return Err(CaptureError::UnsupportedFormat),
        _ => return Err(CaptureError::Malformed),
    }
    match find(pairs, "dialect") {
        Some(Json::String(text)) if text == MANIFEST.dialect => {}
        Some(Json::String(_)) => return Err(CaptureError::UnsupportedFormat),
        _ => return Err(CaptureError::Malformed),
    }
    if let Some(shell) = find(pairs, "shell") {
        check_shell(shell)?;
    }
    if let Some(engine) = find(pairs, "engine") {
        check_engine(engine)?;
    }
    if let Some(error) = find(pairs, "error") {
        error_code(error)?;
        return Err(CaptureError::Incomplete);
    }
    Ok(())
}

// ------------------------------------------------------------ statistics

fn index_sizes(stats: &Pairs) -> Result<Vec<(String, Num)>, CaptureError> {
    let Some(sizes) = find(stats, "indexSizes") else {
        return Ok(Vec::new());
    };
    let pairs = members(sizes).ok_or(CaptureError::Malformed)?;
    pairs
        .iter()
        .map(|(name, value)| {
            if connection_like(name) {
                return Err(CaptureError::SecretField);
            }
            Ok((name.clone(), ejson_number(value, false)?))
        })
        .collect()
}

fn compression_enabled(stats: &Pairs) -> Result<bool, CaptureError> {
    let Some(value) = find(stats, "compression") else {
        return Ok(false);
    };
    let pairs = members(value).ok_or(CaptureError::Malformed)?;
    check_keys(pairs, COMPRESSION_KEYS, &[])?;
    let flag = |key: &str| match find(pairs, key) {
        None => Ok(false),
        Some(Json::Bool(flag)) => Ok(*flag),
        Some(_) => Err(CaptureError::Malformed),
    };
    Ok(flag("enabled")? | flag("enable")?)
}

/// Index usage by index name; a later entry of the same name wins, as the
/// script's dict comprehension did.
fn index_usage(entries: &Json) -> Result<Vec<(String, Num)>, CaptureError> {
    let Json::Array(items) = entries else {
        return Err(CaptureError::Malformed);
    };
    let mut usage: Vec<(String, Num)> = Vec::new();
    for item in items {
        let pairs = members(item).ok_or(CaptureError::Malformed)?;
        check_keys(pairs, INDEX_STATS_READ, INDEX_STATS_IGNORED)?;
        scan_ignored_members(pairs, INDEX_STATS_IGNORED)?;
        let name = name_of(find(pairs, "name").ok_or(CaptureError::Malformed)?)?;
        let ops = match find(pairs, "accesses") {
            None => Num::Int(0),
            Some(accesses) => {
                let access = members(accesses).ok_or(CaptureError::Malformed)?;
                check_keys(access, &["ops"], ACCESSES_IGNORED)?;
                scan_ignored_members(access, ACCESSES_IGNORED)?;
                optional_number(access, "ops")?
            }
        };
        match usage.iter_mut().find(|(existing, _)| existing == name) {
            Some(slot) => slot.1 = ops,
            None => usage.push((name.to_string(), ops)),
        }
    }
    Ok(usage)
}

fn key_json(value: &Json) -> Result<Json, CaptureError> {
    match value {
        Json::String(text) => {
            if connection_like(text) {
                Err(CaptureError::SecretField)
            } else {
                Ok(Json::String(text.clone()))
            }
        }
        other => ejson_number(other, true).map(num_json),
    }
}

fn index_entries(
    indexes: &Json,
    sizes: &[(String, Num)],
    usage: &[(String, Num)],
    limits: &Limits,
) -> Result<Vec<Json>, CaptureError> {
    let Json::Array(items) = indexes else {
        return Err(CaptureError::Malformed);
    };
    if items.len() > limits.max_indexes {
        return Err(CaptureError::TooLarge);
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let pairs = members(item).ok_or(CaptureError::Malformed)?;
        check_keys(pairs, INDEX_READ, INDEX_IGNORED)?;
        scan_ignored_members(pairs, INDEX_IGNORED)?;
        let name = name_of(find(pairs, "name").ok_or(CaptureError::Malformed)?)?;
        let key_pairs = match find(pairs, "key") {
            None => &[][..],
            Some(key) => members(key).ok_or(CaptureError::Malformed)?,
        };
        let mut fields = Vec::with_capacity(key_pairs.len());
        let mut ordered = Vec::with_capacity(key_pairs.len());
        for (field, value) in key_pairs {
            if connection_like(field) {
                return Err(CaptureError::SecretField);
            }
            fields.push((field.clone(), key_json(value)?));
            ordered.push(Json::String(field.clone()));
        }
        let lookup = |table: &[(String, Num)]| {
            table
                .iter()
                .find_map(|(entry, number)| (entry == name).then_some(*number))
                .unwrap_or(Num::Int(0))
        };
        let ops = lookup(usage);
        let mut entry = vec![
            ("name", Json::string(name)),
            ("fields", Json::Object(fields)),
            ("ordered_fields", Json::Array(ordered)),
            ("size", num_json(lookup(sizes))),
            (
                "usage",
                Json::object(vec![
                    ("ops", num_json(ops)),
                    (
                        "potential_unused",
                        Json::Bool(ops.value() == 0.0 && name != "_id_"),
                    ),
                ]),
            ),
            (
                "cardinality",
                Json::object(vec![("is_low", Json::Bool(false))]),
            ),
        ];
        match find(pairs, "expireAfterSeconds") {
            None | Some(Json::Null) => {}
            Some(value) => {
                entry.push(("expireAfterSeconds", num_json(ejson_number(value, false)?)));
            }
        }
        out.push(Json::object(entry));
    }
    Ok(out)
}

/// `max(0, storageSize - size)` and its percentage of `storageSize`.
fn unused_storage(size: Num, storage: Num) -> Json {
    let bytes = match (size, storage) {
        (Num::Int(data), Num::Int(total)) => Num::Int(total.saturating_sub(data).max(0)),
        _ => Num::Float((storage.value() - size.value()).max(0.0)),
    };
    let percent = if storage.value() > 0.0 {
        Json::Float(bytes.value() / storage.value() * 100.0)
    } else {
        Json::Int(0)
    };
    Json::object(vec![
        ("unusedBytes", num_json(bytes)),
        ("unusedPercent", percent),
    ])
}

fn collection_snapshot(
    stats: &Json,
    index_stats: &Json,
    indexes: &Json,
    limits: &Limits,
) -> Result<Json, CaptureError> {
    let stats = members(stats).ok_or(CaptureError::Malformed)?;
    check_keys(stats, COLL_STATS_READ, COLL_STATS_IGNORED)?;
    scan_ignored_members(stats, COLL_STATS_IGNORED)?;
    let size = optional_number(stats, "size")?;
    let storage = optional_number(stats, "storageSize")?;
    let sizes = index_sizes(stats)?;
    let usage = index_usage(index_stats)?;
    let entries = index_entries(indexes, &sizes, &usage, limits)?;
    Ok(Json::object(vec![
        ("count", num_json(optional_number(stats, "count")?)),
        ("size", num_json(size)),
        ("storageSize", num_json(storage)),
        (
            "avgObjSize",
            num_json(optional_number(stats, "avgObjSize")?),
        ),
        (
            "totalIndexSize",
            num_json(optional_number(stats, "totalIndexSize")?),
        ),
        ("indexes", Json::Array(entries)),
        (
            "compression",
            Json::object(vec![("enabled", Json::Bool(compression_enabled(stats)?))]),
        ),
        ("unusedStorageSize", unused_storage(size, storage)),
    ]))
}

/// Normalizes a parsed capture into the `--analysis-data` snapshot.
///
/// # Errors
///
/// A fixed [`CaptureError`] class; nothing of the capture is carried.
pub(super) fn normalize(document: &Json, limits: &Limits) -> Result<Normalized, CaptureError> {
    let header = members(document).ok_or(CaptureError::Malformed)?;
    check_header(header)?;
    let Some(Json::Array(databases)) = find(header, "databases") else {
        return Err(CaptureError::Malformed);
    };
    if databases.len() > limits.max_databases {
        return Err(CaptureError::TooLarge);
    }
    let mut snapshot: Vec<(String, Json)> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let (mut kept, mut incomplete, mut usable) = (0_usize, 0_usize, 0_usize);
    for (db_index, database) in databases.iter().enumerate() {
        let pairs = members(database).ok_or(CaptureError::Malformed)?;
        check_keys(pairs, MANIFEST.database_keys, &[])?;
        ordinal_ok(pairs, db_index + 1)?;
        let name = name_of(find(pairs, "name").ok_or(CaptureError::Malformed)?)?;
        if seen.contains(&name) {
            return Err(CaptureError::Malformed);
        }
        seen.push(name);
        if MANIFEST.excluded_databases.contains(&name) {
            continue;
        }
        if let Some(error) = find(pairs, "error") {
            error_code(error)?;
            return Err(CaptureError::Incomplete);
        }
        let collections = match find(pairs, "collections") {
            None => &[][..],
            Some(Json::Array(items)) => items.as_slice(),
            Some(_) => return Err(CaptureError::Malformed),
        };
        let mut out: Vec<(String, Json)> = Vec::new();
        let mut names: Vec<&str> = Vec::new();
        for (index, collection) in collections.iter().enumerate() {
            let pairs = members(collection).ok_or(CaptureError::Malformed)?;
            check_keys(pairs, MANIFEST.collection_keys, &[])?;
            ordinal_ok(pairs, index + 1)?;
            let name = name_of(find(pairs, "name").ok_or(CaptureError::Malformed)?)?;
            if names.contains(&name) {
                return Err(CaptureError::Malformed);
            }
            names.push(name);
            if name.starts_with(MANIFEST.excluded_collection_prefix) {
                continue;
            }
            if kept + out.len() >= limits.max_collections {
                return Err(CaptureError::TooLarge);
            }
            let replies = (
                find(pairs, "collStats"),
                find(pairs, "indexStats"),
                find(pairs, "indexes"),
            );
            if let Some(error) = find(pairs, "error") {
                if replies != (None, None, None) {
                    return Err(CaptureError::Malformed);
                }
                let code = error_code(error)?;
                incomplete += 1;
                out.push((
                    name.to_string(),
                    Json::object(vec![("error", Json::string(code))]),
                ));
                continue;
            }
            let (Some(stats), Some(index_stats), Some(indexes)) = replies else {
                return Err(CaptureError::Malformed);
            };
            usable += 1;
            out.push((
                name.to_string(),
                collection_snapshot(stats, index_stats, indexes, limits)?,
            ));
        }
        if !out.is_empty() {
            kept += out.len();
            snapshot.push((name.to_string(), Json::Object(out)));
        }
    }
    if usable == 0 {
        return Err(CaptureError::Empty);
    }
    Ok(Normalized {
        databases: snapshot.len(),
        snapshot: Json::Object(snapshot),
        collections: kept,
        incomplete,
    })
}

/// Reads a capture file: a regular, non-link file whose permissions are
/// admitted before any byte is parsed.
///
/// # Errors
///
/// A fixed [`CaptureError`] class.
pub(super) fn read_capture(path: &Path, limits: &Limits) -> Result<Json, CaptureError> {
    let (file, metadata) = open_regular_file(path).map_err(|_| CaptureError::Unreadable)?;
    admit_access(access_of(&metadata))?;
    if metadata.len() > limits.max_bytes {
        return Err(CaptureError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(limits.max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| CaptureError::Unreadable)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limits.max_bytes {
        return Err(CaptureError::TooLarge);
    }
    let text = String::from_utf8(bytes).map_err(|_| CaptureError::Malformed)?;
    parse_json_with_limits(&text, &JSON_LIMITS).map_err(|_| CaptureError::Malformed)
}

/// Creates the private snapshot file (0600 on Unix), never replacing a file
/// or following a link.
///
/// # Errors
///
/// [`CaptureError::OutputExists`] or [`CaptureError::OutputFailed`].
#[cfg(unix)]
pub(super) fn write_snapshot(path: &Path, snapshot: &Json) -> Result<(), CaptureError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                CaptureError::OutputExists
            } else {
                CaptureError::OutputFailed
            }
        })?;
    let mut text = snapshot.to_pretty_string();
    text.push('\n');
    file.write_all(text.as_bytes())
        .map_err(|_| CaptureError::OutputFailed)
}

/// Creating a private file cannot be verified on this platform: fail closed.
#[cfg(not(unix))]
pub(super) fn write_snapshot(_path: &Path, _snapshot: &Json) -> Result<(), CaptureError> {
    Err(CaptureError::OutputFailed)
}
