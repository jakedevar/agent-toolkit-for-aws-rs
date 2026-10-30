//! Deployed-state discovery (R-RES-ADM, legacy `discover_deployed`).
//!
//! A convenience only: nothing security-relevant is decided from this file,
//! and any read or shape error gives nulls. Three layouts: 0.20.x
//! `targets.<t>.resources.payments[]`, 0.26.x `payments` and `connectors`
//! keyed by name, and the legacy top-level `payments[]`. D27:
//! `setup-payment-user` shares [`parse`] (so it accepts the 0.26.x layout
//! too), but keeps its single candidate.

use std::path::{Component, Path, PathBuf};

use toolkit_core::Json;

use super::AdminDeps;
use crate::agents_pay::pyval::truthy;

/// What the deploy record names (the legacy dict of possibly-`None`
/// values).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    /// `managerArn`.
    pub manager_arn: Option<String>,
    /// The first connector's `connectorId`.
    pub connector_id: Option<String>,
    /// `processPaymentRoleArn`.
    pub role_arn: Option<String>,
}

/// The working-directory candidates when no project directory is named.
pub const RELATIVE_CANDIDATES: [&str; 3] = [
    "agentcore/.cli/deployed-state.json",
    ".cli/deployed-state.json",
    "../.cli/deployed-state.json",
];

/// The single candidate of `setup-payment-user`.
pub const SETUP_PAYMENT_USER_CANDIDATE: &str = "agentcore/.cli/deployed-state.json";

/// `Path.expanduser().resolve()`, lexically (symlinks are not resolved).
#[must_use]
pub fn resolve_dir(text: &str, deps: &AdminDeps<'_>) -> PathBuf {
    let expanded = if text == "~" {
        super::home(deps)
    } else if let Some(rest) = text.strip_prefix("~/") {
        super::home(deps).join(rest)
    } else {
        PathBuf::from(text)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        deps.fs.cwd().join(expanded)
    };
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where the deploy record may be: under `project_dir` or
/// `AGENTCORE_PROJECT_DIR` when one is set, else the working-directory
/// candidates.
#[must_use]
pub fn candidates(project_dir: Option<&str>, deps: &AdminDeps<'_>) -> Vec<PathBuf> {
    let explicit = project_dir
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            deps.env
                .var("AGENTCORE_PROJECT_DIR")
                .filter(|p| !p.is_empty())
        });
    match explicit {
        Some(dir) => {
            let root = resolve_dir(&dir, deps);
            vec![
                root.join("agentcore/.cli/deployed-state.json"),
                root.join(".cli/deployed-state.json"),
            ]
        }
        None => RELATIVE_CANDIDATES.iter().map(PathBuf::from).collect(),
    }
}

/// The candidates as the legacy messages list them.
#[must_use]
pub fn checked(candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|p| crate::safe_text::shown_path(p))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first existing candidate.
#[must_use]
pub fn find(candidates: &[PathBuf], deps: &AdminDeps<'_>) -> Option<PathBuf> {
    candidates.iter().find(|p| deps.fs.exists(p)).cloned()
}

/// Read and parse the first existing candidate.
#[must_use]
pub fn discover(deps: &AdminDeps<'_>, project_dir: Option<&str>) -> Discovered {
    discover_in(&candidates(project_dir, deps), deps)
}

/// [`discover`] over an explicit candidate list.
#[must_use]
pub fn discover_in(candidates: &[PathBuf], deps: &AdminDeps<'_>) -> Discovered {
    find(candidates, deps)
        .and_then(|path| deps.fs.read(&path))
        .map_or_else(Discovered::default, |text| parse(&text))
}

/// A file as text, at most 1 MiB.
#[must_use]
pub fn read_bounded(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 1_048_576 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Python `dict.get(key)`: `Err` when `value` is not a dict (an
/// `AttributeError`), `Ok(None)` when absent.
// recorded-shape: the AgentCore CLI deploy record on disk, never an AWS response.
fn dict_get<'j>(value: &'j Json, key: &str) -> Result<Option<&'j Json>, ()> {
    match value {
        Json::Object(fields) => Ok(fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)),
        _ => Err(()),
    }
}

/// `next(iter(d.values()), {})` for a dict, `l[0] if l else {}` for a list;
/// `None` for any other type.
// recorded-shape: the AgentCore CLI deploy record on disk, never an AWS response.
fn first(value: &Json) -> Option<Json> {
    match value {
        Json::Object(fields) => Some(
            fields
                .first()
                .map_or_else(|| Json::Object(vec![]), |(_, v)| v.clone()),
        ),
        Json::Array(items) => Some(
            items
                .first()
                .cloned()
                .unwrap_or_else(|| Json::Object(vec![])),
        ),
        _ => None,
    }
}

fn text(value: Option<&Json>) -> Option<String> {
    value.and_then(Json::as_str).map(str::to_owned)
}

/// Parse a deploy record with the legacy semantics, including its partial
/// result when a later step raises.
#[must_use]
pub fn parse(text_in: &str) -> Discovered {
    let mut out = Discovered::default();
    let Ok(data) = toolkit_core::parse_json(text_in) else {
        return out;
    };
    let _ = parse_into(&data, &mut out);
    out
}

// recorded-shape: the AgentCore CLI deploy record on disk, never an AWS response.
fn parse_into(data: &Json, out: &mut Discovered) -> Result<(), ()> {
    let empty = Json::Object(vec![]);
    let targets = dict_get(data, "targets")?
        .filter(|t| truthy(t))
        .unwrap_or(&empty);
    let default = dict_get(targets, "default")?.filter(|t| truthy(t)).cloned();
    let target = match default {
        Some(target) => target,
        None if truthy(targets) => first(targets).unwrap_or_else(|| empty.clone()),
        None => empty.clone(),
    };
    let mut payments = None;
    if matches!(target, Json::Object(_)) {
        let resources = dict_get(&target, "resources")?
            .filter(|r| truthy(r))
            .unwrap_or(&empty);
        payments = dict_get(resources, "payments")?.cloned();
    }
    if !payments.as_ref().is_some_and(truthy) {
        payments = dict_get(data, "payments")?.cloned();
    }
    let Some(payments) = payments.filter(truthy) else {
        return Ok(());
    };
    let Some(pay) = first(&payments) else {
        return Ok(());
    };
    let connectors = dict_get(&pay, "connectors")?
        .filter(|c| truthy(c))
        .cloned()
        .unwrap_or_else(|| Json::Array(vec![]));
    out.manager_arn = text(dict_get(&pay, "managerArn")?);
    let connector = first(&connectors).unwrap_or_else(|| empty.clone());
    out.connector_id = if truthy(&connector) {
        text(dict_get(&connector, "connectorId")?)
    } else {
        None
    };
    out.role_arn = text(dict_get(&pay, "processPaymentRoleArn")?);
    Ok(())
}
