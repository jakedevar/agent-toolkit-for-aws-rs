//! The pre-collected database analysis (`--analysis-data FILE`).
//!
//! The Python script collected `collStats`/`$indexStats` itself (`--uri`, with
//! `pymongo`) or read a JSON file written by an earlier collection. This build
//! has no database driver: the file is the only source. It is a snapshot the
//! user supplies, not an AWS response, but it follows the same rule as a
//! recorded response: a member that is absent takes the script's default, a
//! member that is present with another type (`null` included) rejects the
//! snapshot. The reason names the position by ordinal (`database 2,
//! collection 1`) and the member, never a database, collection or index name
//! or any value of the file.
//!
//! Layout: `{database: {collection: {"count", "size", "avgObjSize",
//! "indexes": [{"name", "size", "ordered_fields", "expireAfterSeconds",
//! "usage": {"potential_unused"}, "cardinality": {"is_low"}}],
//! "compression": {"enabled"}, "unusedStorageSize": {"unusedBytes",
//! "unusedPercent"}}}}`. A database that is not an object, and a collection
//! that is not an object or has an `error` member, are skipped, as the
//! script did.

use toolkit_aws::recorded::{optional_member, optional_object, optional_text, MemberError};
use toolkit_core::Json;

use super::access::{optional_bool, optional_number};

/// The items of a list value of a file the user supplied.
// recorded-shape: a snapshot file value, not a response member; a non-list is reported as None.
pub(super) fn list(value: &Json) -> Option<&[Json]> {
    match value {
        Json::Array(items) => Some(items.as_slice()),
        _ => None,
    }
}

/// The members of an object value of a file the user supplied.
// recorded-shape: a snapshot file value, not a response member; a non-object is reported as None.
fn members(value: &Json) -> Option<&[(String, Json)]> {
    match value {
        Json::Object(pairs) => Some(pairs.as_slice()),
        _ => None,
    }
}

/// A number that keeps its JSON kind: `{count:,}` prints an integer and a
/// float differently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    pub(super) fn value(self) -> f64 {
        match self {
            Self::Int(n) => n as f64,
            Self::Float(n) => n,
        }
    }

    /// Python `format(n, ",")`: thousands separators, a float keeps its fraction.
    pub(super) fn commas(self) -> String {
        match self {
            Self::Int(n) => group(&n.unsigned_abs().to_string(), n < 0),
            Self::Float(n) => {
                let text = format!("{:?}", n.abs());
                let (whole, rest) = text
                    .split_once('.')
                    .map_or((text.as_str(), ""), |(w, f)| (w, f));
                let mut out = group(whole, n < 0.0);
                if !rest.is_empty() {
                    out.push('.');
                    out.push_str(rest);
                }
                out
            }
        }
    }
}

/// Python `format(n, ",.0f")` of a non-negative whole number of digits.
pub(super) fn group(digits: &str, negative: bool) -> String {
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    let first = digits.len() % 3;
    for (index, ch) in digits.chars().enumerate() {
        if index != 0 && (index + 3 - first).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One index of a collection.
#[derive(Debug, Clone)]
pub(super) struct IndexStats {
    pub(super) name: String,
    pub(super) size: f64,
    pub(super) ordered_fields: Vec<Json>,
    pub(super) has_ttl: bool,
    pub(super) potential_unused: bool,
    pub(super) low_cardinality: bool,
}

/// One collection with the statistics the checks read.
#[derive(Debug, Clone)]
pub(super) struct Collection {
    pub(super) database: String,
    pub(super) name: String,
    pub(super) count: Num,
    pub(super) size: f64,
    pub(super) avg_obj_size: Num,
    pub(super) indexes: Vec<IndexStats>,
    pub(super) unused_percent: f64,
    pub(super) compression_enabled: bool,
}

fn num(object: &Json, key: &str) -> Result<Num, MemberError> {
    optional_member(object, key, "a number", |member| match member {
        Json::Int(n) => Some(Num::Int(*n)),
        Json::Float(n) => Some(Num::Float(*n)),
        _ => None,
    })
    .map(|found| found.unwrap_or(Num::Int(0)))
}

fn truthy_flag(object: &Json, key: &str) -> Result<bool, MemberError> {
    Ok(optional_bool(object, key)?.unwrap_or(false))
}

fn index(entry: &Json) -> Result<IndexStats, MemberError> {
    let ordered_fields = optional_member(entry, "ordered_fields", "a list", |member| {
        list(member).map(<[Json]>::to_vec)
    })?
    .unwrap_or_default();
    let usage = optional_object(entry, "usage")?;
    let cardinality = optional_object(entry, "cardinality")?;
    Ok(IndexStats {
        name: optional_text(entry, "name")?
            .unwrap_or_default()
            .to_string(),
        size: optional_number(entry, "size")?.unwrap_or(0.0),
        ordered_fields,
        has_ttl: entry
            .get("expireAfterSeconds")
            .is_some_and(|value| *value != Json::Null),
        potential_unused: match usage {
            Some(usage) => truthy_flag(usage, "potential_unused")?,
            None => false,
        },
        low_cardinality: match cardinality {
            Some(cardinality) => truthy_flag(cardinality, "is_low")?,
            None => false,
        },
    })
}

fn collection(database: &str, name: &str, stats: &Json) -> Result<Collection, MemberError> {
    let indexes = optional_member(stats, "indexes", "a list of objects", |member| {
        list(member).filter(|items| items.iter().all(|item| members(item).is_some()))
    })?
    .unwrap_or_default()
    .iter()
    .map(index)
    .collect::<Result<Vec<_>, _>>()?;
    let unused = optional_object(stats, "unusedStorageSize")?;
    if let Some(unused) = unused {
        // Read for its type only: the script summed the bytes and never reported them.
        optional_number(unused, "unusedBytes")?;
    }
    let compression = optional_object(stats, "compression")?;
    Ok(Collection {
        database: database.to_string(),
        name: name.to_string(),
        count: num(stats, "count")?,
        size: num(stats, "size")?.value(),
        avg_obj_size: num(stats, "avgObjSize")?,
        indexes,
        unused_percent: match unused {
            Some(unused) => optional_number(unused, "unusedPercent")?.unwrap_or(0.0),
            None => 0.0,
        },
        compression_enabled: match compression {
            Some(compression) => truthy_flag(compression, "enabled")?,
            None => false,
        },
    })
}

/// The collections of an analysis document, in file order.
///
/// `Ok(None)` is an empty document (`{}`): the script treated it as no data.
///
/// # Errors
///
/// The document is not an object, or a collection has a member of the wrong
/// type; the reason carries ordinals and member names only.
pub(super) fn read_analysis(document: &Json) -> Result<Option<Vec<Collection>>, String> {
    let databases = members(document)
        .ok_or_else(|| "the analysis data must be a JSON object of databases".to_string())?;
    if databases.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for (db_index, (db_name, colls)) in databases.iter().enumerate() {
        let Some(colls) = members(colls) else {
            continue;
        };
        for (coll_index, (coll_name, stats)) in colls.iter().enumerate() {
            if members(stats).is_none() || stats.get("error").is_some() {
                continue;
            }
            out.push(collection(db_name, coll_name, stats).map_err(|error| {
                format!(
                    "analysis data, database {}, collection {}: {error}",
                    db_index + 1,
                    coll_index + 1
                )
            })?);
        }
    }
    Ok(Some(out))
}
