//! `toolkit startup artifacts`: validates migration artifacts against the
//! contracts the skills publish for them (port of `validate-artifacts.py`).
//!
//! The published shape of each artifact is a JSON/JSONC example block in a
//! `schema-*.md` document, plus two real JSON Schemas. This module turns the
//! blocks into key-path templates ([`shape`]) and checks run directories or
//! the fixture goldens against them, with a draft-07 subset validator
//! ([`schema`]) for the real schemas. `pyjson` reproduces Python's
//! `json.loads` (duplicate keys, `NaN`, error messages), [`jsonc`] the
//! comment stripping and [`patterns`] the small regular expressions, so the
//! findings match the Python tool. Deliberate differences are listed in
//! `docs/migration/startup-validators.md`.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{Json, ToolkitError};

use self::contracts::{artifact_name, build_contracts, matches_glob, shape_for, Contract, Env};
use self::patterns::fnmatch;
use self::pyjson::{dumps, loads};
use self::schema::Context;
use self::shape::validate_shape;
use super::{is_regular_file_under, read_text, read_text_under, refuse_links_below};
use crate::{Invocation, Output};

pub(crate) mod contracts;
pub(crate) mod jsonc;
pub(crate) mod patterns;
pub(crate) mod pyjson;
pub(crate) mod schema;
pub(crate) mod shape;

#[cfg(test)]
mod tests;

/// A finding code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Code {
    UnknownKey,
    MissingRequired,
    TypeMismatch,
    EnumViolation,
    SchemaViolation,
    SchemaParse,
    StaleBaseline,
    NoContract,
}

impl Code {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UnknownKey => "UNKNOWN_KEY",
            Self::MissingRequired => "MISSING_REQUIRED",
            Self::TypeMismatch => "TYPE_MISMATCH",
            Self::EnumViolation => "ENUM_VIOLATION",
            Self::SchemaViolation => "SCHEMA_VIOLATION",
            Self::SchemaParse => "SCHEMA_PARSE",
            Self::StaleBaseline => "STALE_BASELINE",
            Self::NoContract => "NO_CONTRACT",
        }
    }

    fn is_error(self) -> bool {
        !matches!(self, Self::NoContract)
    }
}

/// One validation finding.
#[derive(Clone, Debug)]
pub(crate) struct Finding {
    pub(crate) code: Code,
    pub(crate) artifact: String,
    pub(crate) path: String,
    pub(crate) message: String,
    pub(crate) contract: String,
    pub(crate) baselined: bool,
    pub(crate) baseline_reason: String,
}

impl Finding {
    pub(crate) fn new(
        code: Code,
        artifact: &str,
        path: &str,
        message: String,
        contract: &str,
    ) -> Self {
        Self {
            code,
            artifact: artifact.to_string(),
            path: path.to_string(),
            message,
            contract: contract.to_string(),
            baselined: false,
            baseline_reason: String::new(),
        }
    }

    pub(crate) fn severity(&self) -> &'static str {
        if self.code.is_error() {
            "error"
        } else {
            "info"
        }
    }

    fn as_json(&self) -> Json {
        let mut members = vec![
            ("code", Json::string(self.code.as_str())),
            ("severity", Json::string(self.severity())),
            ("artifact", Json::string(self.artifact.as_str())),
            ("path", Json::string(self.path.as_str())),
            ("message", Json::string(self.message.as_str())),
            ("contract", Json::string(self.contract.as_str())),
        ];
        if self.baselined {
            members.push(("baselined", Json::Bool(true)));
            members.push((
                "baseline_reason",
                Json::string(self.baseline_reason.as_str()),
            ));
        }
        Json::object(members)
    }
}

/// Entries that matched nothing are returned; findings they match are marked.
pub(crate) fn apply_baseline(findings: &mut [Finding], baseline: &Json) -> Vec<Json> {
    let entries: &[Json] = match baseline.get("entries") {
        Some(Json::Array(entries)) => entries,
        _ => &[],
    };
    let mut used = vec![false; entries.len()];
    let field = |entry: &Json, key: &str, default: &str| -> String {
        entry
            .get(key)
            .and_then(Json::as_str)
            .unwrap_or(default)
            .to_string()
    };
    for finding in findings.iter_mut() {
        for (index, entry) in entries.iter().enumerate() {
            if entry.get("code").and_then(Json::as_str) != Some(finding.code.as_str()) {
                continue;
            }
            if !fnmatch(&finding.artifact, &field(entry, "artifact", "*"))
                || !fnmatch(&finding.path, &field(entry, "path", "*"))
            {
                continue;
            }
            finding.baselined = true;
            finding.baseline_reason = field(entry, "reason", "");
            used[index] = true;
            break;
        }
    }
    entries
        .iter()
        .zip(used)
        .filter(|(_, matched)| !matched)
        .map(|(entry, _)| entry.clone())
        .collect()
}

/// Directories whose contents are never artifacts.
const SKIPPED_DIRS: [&str; 4] = [
    "live-capture",
    "workspace-terraform",
    ".terraform",
    "node_modules",
];

/// `find_artifacts`: every `*.json` file below `run_dir`, sorted by path
/// components, without generated or answer files.
pub(crate) fn find_artifacts(run_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found: Vec<(Vec<String>, PathBuf)> = Vec::new();
    collect(run_dir, &mut Vec::new(), &mut found)?;
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

// `rglob("*.json")` is case-sensitive on POSIX, like this comparison.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn collect(
    dir: &Path,
    parts: &mut Vec<String>,
    found: &mut Vec<(Vec<String>, PathBuf)>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let kind = fs::symlink_metadata(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?
            .file_type();
        if kind.is_symlink() {
            return Err(format!(
                "cannot read {}: symbolic links are not followed",
                path.display()
            ));
        }
        if kind.is_dir() {
            if SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            parts.push(name);
            collect(&path, parts, found)?;
            parts.pop();
        } else if kind.is_file()
            && name.ends_with(".json")
            && !name.starts_with("expected-")
            && !name.starts_with("clarify-answers")
        {
            let mut full = parts.clone();
            full.push(name);
            found.push((full, path));
        }
    }
    Ok(())
}

/// `validate_run_dir`; returns the number of artifacts checked.
pub(crate) fn validate_run_dir(
    env: &mut Env,
    run_dir: &Path,
    skill: Option<&str>,
    manifest: &Json,
    findings: &mut Vec<Finding>,
) -> Result<usize, String> {
    let contracts = build_contracts(env, manifest, skill, findings, false)?;
    let mut checked = 0;
    for path in find_artifacts(run_dir)? {
        let name = artifact_name(&path);
        let matched: Vec<&Contract> = contracts
            .iter()
            .filter(|c| matches_glob(&name, c))
            .collect();
        let shown = env.rel(&path);
        if matched.is_empty() {
            if let Some(skill) = skill {
                findings.push(Finding::new(
                    Code::NoContract,
                    &shown,
                    "",
                    format!("no contract covers '{name}' for skill {skill}"),
                    "",
                ));
            }
            continue;
        }
        let value = match loads(&read_text(&path, &shown)?) {
            Ok(value) => value,
            Err(error) => {
                findings.push(Finding::new(
                    Code::TypeMismatch,
                    &shown,
                    "$",
                    format!("artifact is not valid JSON: {}", error.msg),
                    "",
                ));
                continue;
            }
        };
        checked += 1;
        for contract in matched {
            if let Some(schema) = &contract.json_schema {
                let context = Context {
                    artifact: &shown,
                    contract: &contract.json_schema_path,
                };
                env.validator
                    .validate(schema, &value, "", &context, findings, schema)?;
            }
            if let Some(shape) = shape_for(contract, &value) {
                validate_shape(&shape.root, &value, "", &shown, &shape.label, findings);
            }
        }
    }
    Ok(checked)
}

fn skill_for_fixture(dirname: &str, manifest: &Json) -> Option<String> {
    let Some(Json::Object(prefixes)) = manifest.get("fixture_prefixes") else {
        return None;
    };
    for (prefix, skill) in prefixes {
        if dirname.starts_with(prefix.as_str()) {
            return skill.as_str().map(str::to_string);
        }
    }
    None
}

/// The three run modes.
enum Mode {
    SelfCheck,
    Fixtures,
    RunDir(PathBuf, String),
}

const SKILLS: [&str; 3] = ["gcp-to-aws", "heroku-to-aws", "azure-to-aws"];

fn mode_of(invocation: &Invocation) -> Result<Mode, ToolkitError> {
    let usage = |message: &str| ToolkitError::usage("artifacts_usage", message.to_string());
    // argparse validates `choices` while parsing, in every mode and before the
    // mode group is checked (#4586).
    let skill = invocation.value("--skill");
    if let Some(skill) = skill.filter(|skill| !SKILLS.contains(skill)) {
        return Err(usage(&format!(
            "argument --skill: invalid choice: '{skill}' (choose from 'gcp-to-aws', 'heroku-to-aws', 'azure-to-aws')"
        )));
    }
    let self_check = invocation.has_flag("--self-check");
    let fixtures = invocation.has_flag("--fixtures");
    let run_dir = invocation.value("--run-dir");
    let chosen = usize::from(self_check) + usize::from(fixtures) + usize::from(run_dir.is_some());
    if chosen != 1 {
        return Err(usage(
            "exactly one of --self-check, --fixtures or --run-dir is required",
        ));
    }
    if self_check {
        return Ok(Mode::SelfCheck);
    }
    if fixtures {
        return Ok(Mode::Fixtures);
    }
    let Some(run_dir) = run_dir else {
        return Err(usage("--run-dir needs a directory"));
    };
    let Some(skill) = skill else {
        return Err(usage("--run-dir requires --skill"));
    };
    if !Path::new(run_dir).is_dir() {
        return Err(usage(&format!("{run_dir} is not a directory")));
    }
    Ok(Mode::RunDir(PathBuf::from(run_dir), skill.to_string()))
}

/// Handler for `startup artifacts`.
///
/// # Errors
///
/// A usage error (exit status 2) for a missing or conflicting mode, a runtime
/// error (exit status 1) when an input cannot be read or any non-baselined
/// error finding remains.
#[allow(clippy::too_many_lines)]
pub fn artifacts_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let mode = mode_of(invocation)?;
    let root = PathBuf::from(invocation.value("--plugin-root").unwrap_or("."));
    let manifest_path = invocation.value("--manifest").map_or_else(
        || root.join("scripts/artifact-contracts.json"),
        PathBuf::from,
    );
    let baseline_arg = invocation.value("--baseline").map(PathBuf::from);
    let use_default_baseline = matches!(mode, Mode::Fixtures);
    let baseline_path = baseline_arg.or_else(|| {
        use_default_baseline.then(|| root.join("scripts/artifact-contracts-baseline.json"))
    });
    let failed = |message: String| ToolkitError::runtime("artifacts_input_invalid", message);

    let mut env = Env::new(&root);
    let manifest_text =
        read_text_under(&root, &manifest_path, &manifest_path.display().to_string())
            .map_err(&failed)?;
    let manifest =
        loads(&manifest_text).map_err(|e| failed(format!("{}: {e}", manifest_path.display())))?;
    let mut findings: Vec<Finding> = Vec::new();
    let mut checked = 0;
    let self_check = matches!(mode, Mode::SelfCheck);
    match &mode {
        Mode::SelfCheck => {
            if let Some(Json::Object(skills)) = manifest.get("skills") {
                for (skill, _) in skills {
                    build_contracts(
                        &mut env,
                        &manifest,
                        Some(skill.as_str()),
                        &mut findings,
                        true,
                    )
                    .map_err(&failed)?;
                }
            }
        }
        Mode::Fixtures => {
            let fixtures_root = env.root.join("fixtures");
            refuse_links_below(&env.root, &fixtures_root).map_err(&failed)?;
            let mut dirs: Vec<(String, PathBuf)> = Vec::new();
            let entries = fs::read_dir(&fixtures_root)
                .map_err(|e| failed(format!("cannot read {}: {e}", fixtures_root.display())))?;
            for entry in entries {
                let entry = entry.map_err(|e| failed(e.to_string()))?;
                let meta = fs::symlink_metadata(entry.path()).map_err(|e| failed(e.to_string()))?;
                if meta.file_type().is_symlink() {
                    return Err(failed(format!(
                        "cannot read {}: symbolic links are not followed",
                        entry.path().display()
                    )));
                }
                if meta.is_dir() {
                    dirs.push((
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.path(),
                    ));
                }
            }
            dirs.sort();
            for (name, dir) in dirs {
                let skill = skill_for_fixture(&name, &manifest);
                checked +=
                    validate_run_dir(&mut env, &dir, skill.as_deref(), &manifest, &mut findings)
                        .map_err(&failed)?;
            }
        }
        Mode::RunDir(dir, skill) => {
            checked += validate_run_dir(
                &mut env,
                dir,
                Some(skill.as_str()),
                &manifest,
                &mut findings,
            )
            .map_err(&failed)?;
        }
    }

    let mut stale: Vec<Json> = Vec::new();
    if !invocation.has_flag("--no-baseline") && !self_check {
        if let Some(path) = &baseline_path {
            if is_regular_file_under(&root, path).map_err(&failed)? {
                let shown = path.display().to_string();
                let text = read_text_under(&root, path, &shown).map_err(&failed)?;
                let baseline = loads(&text).map_err(|e| failed(format!("{shown}: {e}")))?;
                stale = apply_baseline(&mut findings, &baseline);
                let shown_rel = env.rel(path);
                for entry in &stale {
                    // A baseline entry that matches nothing is fixed (remove
                    // it) or mistyped; fail either way.
                    let at = entry.get("path").and_then(Json::as_str).unwrap_or("*");
                    findings.push(Finding::new(
                        Code::StaleBaseline,
                        &shown_rel,
                        at,
                        format!(
                            "entry matched no finding — remove it, or fix its code/artifact/path: {}",
                            dumps(entry, None, false)
                        ),
                        "",
                    ));
                }
            }
        }
    }

    // Several contracts can cover one artifact: de-duplicate identical findings.
    let mut seen: HashSet<(Code, String, String, String)> = HashSet::new();
    let mut unique: Vec<Finding> = Vec::new();
    for finding in findings {
        let key = (
            finding.code,
            finding.artifact.clone(),
            finding.path.clone(),
            finding.message.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        unique.push(finding);
    }
    report(
        invocation.has_flag("--json"),
        checked,
        &unique,
        &stale,
        output,
    )
}

fn report(
    as_json: bool,
    checked: usize,
    findings: &[Finding],
    stale: &[Json],
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let errors: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.code.is_error() && !f.baselined)
        .collect();
    let baselined: Vec<&Finding> = findings.iter().filter(|f| f.baselined).collect();
    let infos: Vec<&Finding> = findings.iter().filter(|f| !f.code.is_error()).collect();
    if as_json {
        let list =
            |items: &[&Finding]| Json::Array(items.iter().map(|f| Finding::as_json(f)).collect());
        let report = Json::object(vec![
            (
                "checked_artifacts",
                Json::Int(i64::try_from(checked).unwrap_or(i64::MAX)),
            ),
            ("errors", list(&errors)),
            ("baselined", list(&baselined)),
            ("info", list(&infos)),
            ("stale_baseline_entries", Json::Array(stale.to_vec())),
        ]);
        output.line(&dumps(&report, Some(2), false))?;
    } else {
        for f in &errors {
            let mut text = format!(
                "ERROR  {:<17} {} :: {}\n       {}",
                f.code.as_str(),
                f.artifact,
                f.path,
                f.message
            );
            if !f.contract.is_empty() {
                let _ = write!(text, "\n       contract: {}", f.contract);
            }
            output.line(&text)?;
        }
        for f in &infos {
            output.line(&format!(
                "info   {:<17} {} :: {}",
                f.code.as_str(),
                f.artifact,
                f.message
            ))?;
        }
        if !baselined.is_empty() {
            let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
            for f in &baselined {
                *by_code.entry(f.code.as_str()).or_default() += 1;
            }
            let listed: Vec<String> = by_code.iter().map(|(k, v)| format!("{k}={v}")).collect();
            output.line(&format!(
                "baselined (known, explained, not failing): {}",
                listed.join(", ")
            ))?;
        }
        output.line(&format!(
            "\n{} — {checked} artifact(s) checked, {} error(s), {} baselined, {} info",
            if errors.is_empty() { "PASS" } else { "FAIL" },
            errors.len(),
            baselined.len(),
            infos.len()
        ))?;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ToolkitError::runtime(
            "artifact_contracts_failed",
            format!("{} artifact contract problem(s)", errors.len()),
        ))
    }
}
