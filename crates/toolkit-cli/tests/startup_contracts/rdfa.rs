//! `test_azure_rdfa_discover_contracts.py` (#4806): behavioural simulations of
//! the Azure RDfA discovery rules and the prose contracts that publish them.
//!
//! The Python file defines small helper functions (metric eligibility, node
//! pool normalisation, report preparation, source identity ...) and checks them
//! against the committed `fixtures/azure-rdfa-report`. The helpers are
//! re-implemented here and every positive assertion is accompanied by a
//! negative case on the helper, so a wrong helper fails. The prose contracts
//! are `rules::check_rules` rules, which derive one broken copy per assertion.
//! The one case that runs the artifact validator is
//! `startup::artifacts::tests::rdfa` (it needs the crate-private CLI).

#![allow(
    clippy::case_sensitive_file_extension_comparisons,
    clippy::struct_field_names
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, sha256_hex, Json};

use crate::common::{check, ensure, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule};

const DISCOVER: &str = "skills/azure-to-aws/references/phases/discover/discover.md";
const DISCOVER_RDFA: &str = "skills/azure-to-aws/references/phases/discover/discover-rdfa.md";
const DISCOVER_ASSEMBLE: &str =
    "skills/azure-to-aws/references/phases/discover/discover-assemble.md";
const DISCOVER_LIVE: &str = "skills/azure-to-aws/references/phases/discover/discover-live.md";
const SCHEMA_AZURE: &str = "skills/azure-to-aws/references/shared/schema-discover-azure.md";
const SCHEMA_AI: &str = "skills/azure-to-aws/references/shared/schema-discover-ai.md";
const THRESHOLDS: &str = "skills/azure-to-aws/knowledge/estimate/rightsizing-thresholds.json";
const REPORT: &str = "fixtures/azure-rdfa-report";
const CORRUPT_DIR: &str = "fixtures/azure-rdfa-report-corrupt-zip";
const MULTI_DIR: &str = "fixtures/azure-rdfa-report-multi-zip";
const NEWEST_REPORT: &str = "AllSubscriptions_ResourcesReport_2025-06-01_12-00-00.zip";

// ---- fixture and file helpers ---------------------------------------------------------

fn plugin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .join("plugins")
        .join("aws-startup-advisor")
}

fn json_text(source: &str) -> Json {
    parse_json(source).expect("valid JSON")
}

/// A JSON file of the committed `fixtures/azure-rdfa-report`.
fn report_fixture(name: &str) -> Json {
    let path = plugin_root().join(REPORT).join(name);
    json_text(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

fn items<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    match value.get(key) {
        Some(Json::Array(items)) => items,
        other => panic!("{key} is not an array: {other:?}"),
    }
}

fn str_of<'a>(value: &'a Json, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("{key} is not a string"))
}

/// Python `int(value)` of a JSON integer or a numeric string.
fn to_int(value: Option<&Json>) -> i64 {
    match value {
        Some(Json::Int(n)) => *n,
        Some(Json::String(s)) => s.trim().parse().expect("numeric string"),
        other => panic!("not an integer: {other:?}"),
    }
}

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

/// A private temporary directory removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "startup-rdfa-{label}-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, bytes).expect("write");
}

/// Files below `dir` (recursively, sorted) whose name satisfies `keep`;
/// `Path.rglob` of the Python.
fn rglob(dir: &Path, keep: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            found.extend(rglob(&path, keep));
        } else if path.file_name().and_then(|n| n.to_str()).is_some_and(keep) {
            found.push(path);
        }
    }
    found
}

/// `Inventory_*.json`.
fn is_inventory(name: &str) -> bool {
    name.len() >= "Inventory_.json".len()
        && name.starts_with("Inventory_")
        && name.ends_with(".json")
}

/// `*ResourcesReport*.zip`.
fn is_report_zip(name: &str) -> bool {
    name.strip_suffix(".zip")
        .is_some_and(|stem| stem.contains("ResourcesReport"))
}

// ---- a minimal stored-entry zip (there is no zip crate) ---------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn le16(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&u16::try_from(value).expect("u16").to_le_bytes());
}

fn le32(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&u32::try_from(value).expect("u32").to_le_bytes());
}

/// A zip archive of uncompressed (stored) entries.
fn write_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len();
        let crc = usize::try_from(crc32(data)).expect("crc");
        // Local file header.
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        le16(&mut out, 20);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le16(&mut out, 0x21);
        le32(&mut out, crc);
        le32(&mut out, data.len());
        le32(&mut out, data.len());
        le16(&mut out, name.len());
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // Central directory header.
        central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        le16(&mut central, 20);
        le16(&mut central, 20);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0x21);
        le32(&mut central, crc);
        le32(&mut central, data.len());
        le32(&mut central, data.len());
        le16(&mut central, name.len());
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, 0);
        le32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len();
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, entries.len());
    le16(&mut out, entries.len());
    le32(&mut out, central.len());
    le32(&mut out, central_offset);
    le16(&mut out, 0);
    out
}

fn le16_at(data: &[u8], at: usize) -> Result<usize, String> {
    let bytes = data.get(at..at + 2).ok_or("truncated zip")?;
    Ok(usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
}

fn le32_at(data: &[u8], at: usize) -> Result<usize, String> {
    let bytes = data.get(at..at + 4).ok_or("truncated zip")?;
    usize::try_from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .map_err(|e| e.to_string())
}

/// Offset of the end-of-central-directory record whose directory lies inside
/// the file, searched from the end.
fn find_eocd(data: &[u8]) -> Option<usize> {
    let last = data.len().checked_sub(22)?;
    let first = last.saturating_sub(65_535);
    (first..=last).rev().find(|&at| {
        data[at..at + 4] == [0x50, 0x4b, 0x05, 0x06]
            && matches!(
                (le32_at(data, at + 12), le32_at(data, at + 16)),
                (Ok(size), Ok(offset)) if offset + size <= at
            )
    })
}

/// `zipfile.is_zipfile`: a readable end-of-central-directory record.
fn is_zipfile(data: &[u8]) -> bool {
    find_eocd(data).is_some()
}

/// `(name, bytes)` of every entry; stored entries only, CRC checked.
fn read_zip(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let eocd = find_eocd(data).ok_or("File is not a zip file")?;
    let count = le16_at(data, eocd + 10)?;
    let mut at = le32_at(data, eocd + 16)?;
    let mut out = Vec::new();
    for _ in 0..count {
        if data.get(at..at + 4) != Some(&[0x50, 0x4b, 0x01, 0x02][..]) {
            return Err("Bad central directory".into());
        }
        let method = le16_at(data, at + 10)?;
        let crc = le32_at(data, at + 16)?;
        let size = le32_at(data, at + 20)?;
        let name_len = le16_at(data, at + 28)?;
        let extra_len = le16_at(data, at + 30)?;
        let comment_len = le16_at(data, at + 32)?;
        let local = le32_at(data, at + 42)?;
        let name = data
            .get(at + 46..at + 46 + name_len)
            .ok_or("truncated zip")?;
        let name = String::from_utf8(name.to_vec()).map_err(|e| e.to_string())?;
        if method != 0 {
            return Err(format!("compression method {method} is not supported"));
        }
        let local_name = le16_at(data, local + 26)?;
        let local_extra = le16_at(data, local + 28)?;
        let start = local + 30 + local_name + local_extra;
        let bytes = data.get(start..start + size).ok_or("truncated zip")?;
        if usize::try_from(crc32(bytes)).ok() != Some(crc) {
            return Err(format!("Bad CRC-32 for file {name:?}"));
        }
        out.push((name, bytes.to_vec()));
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

// ---- the helpers of the Python file ---------------------------------------------------------

/// `_flatten_metric_batches`.
fn flatten_metric_batches(batches: &[Json]) -> Result<Vec<Json>, String> {
    let mut rows = Vec::new();
    for batch in batches {
        let Some(Json::Array(metrics)) = batch.get("Metrics") else {
            return Err(format!("not a metrics envelope: {batch:?}"));
        };
        rows.extend(metrics.iter().cloned());
    }
    Ok(rows)
}

/// `_eligible`: no metric error, samples, and a known window of at least `minimum` days.
fn eligible(row: &Json, window_days: Option<f64>, minimum: f64) -> bool {
    let sampled = match row.get("MetricCount") {
        Some(Json::Int(n)) => *n > 0,
        Some(Json::Float(f)) => *f > 0.0,
        other => panic!("MetricCount is not a number: {other:?}"),
    };
    row.get("MetricError") == Some(&Json::Bool(false))
        && sampled
        && window_days.is_some_and(|days| days >= minimum)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Iac,
    Live,
    Rdfa,
    AppAi,
}

/// `_discover_outputs`.
fn discover_outputs(sources: &[Source]) -> BTreeSet<&'static str> {
    let mut outputs = BTreeSet::new();
    if sources
        .iter()
        .any(|s| matches!(s, Source::Iac | Source::Live | Source::Rdfa))
    {
        outputs.insert("azure-resource-inventory.json");
        outputs.insert("azure-resource-clusters.json");
    }
    if sources.contains(&Source::AppAi) {
        outputs.insert("ai-workload-profile.json");
    }
    outputs
}

#[derive(Debug, PartialEq, Eq)]
struct Pool {
    desired_size: i64,
    min_size: i64,
    max_size: i64,
}

/// `_normalize_pool`.
fn normalize_pool(row: &Json) -> Pool {
    let desired = to_int(row.get("Nodes"));
    let mut minimum = to_int(row.get("AutoscaleMin"));
    let mut maximum = to_int(row.get("AutoscaleMax"));
    if str_of(row, "Autoscale").to_lowercase() != "true"
        || (desired > 0 && minimum == 0 && maximum == 0)
    {
        minimum = desired;
        maximum = desired;
    }
    // EKS requires maxSize >= 1; the real zero stays in desired/minimum.
    if maximum == 0 {
        maximum = 1;
    }
    assert!(minimum <= desired && desired <= maximum);
    assert!(maximum >= 1);
    Pool {
        desired_size: desired,
        min_size: minimum,
        max_size: maximum,
    }
}

type Disk = Vec<(&'static str, Json)>;

/// `_map_vm`: the size and the canonical nested `os_disk`.
fn map_vm(row: &Json) -> (String, Disk) {
    let mut disk: Disk = Vec::new();
    if let Some(Json::String(kind)) = row.get("OSDisk") {
        if kind != "None" && kind != "Custom VHD" {
            disk.push(("storage_account_type", Json::string(kind.as_str())));
        }
    }
    match row.get("OSDiskSizeGB") {
        None | Some(Json::Null) => {}
        Some(size) => disk.push(("disk_size_gb", size.clone())),
    }
    (str_of(row, "Size").to_owned(), disk)
}

#[derive(Debug, PartialEq, Eq)]
struct SourceManifest {
    source_path: String,
    source_type: &'static str,
    fingerprint: String,
}

/// `_fingerprint_directory`: SHA-256 over `name NUL bytes` of the sorted files.
fn fingerprint_directory(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut bytes = Vec::new();
    for (name, value) in files {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(value);
    }
    sha256_hex(&bytes)
}

fn source_manifest(
    path: &str,
    source_type: &'static str,
    files: &[(&str, &[u8])],
) -> SourceManifest {
    let files: BTreeMap<String, Vec<u8>> = files
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
        .collect();
    SourceManifest {
        source_path: path.to_owned(),
        source_type,
        fingerprint: fingerprint_directory(&files),
    }
}

/// `_discover_candidates`: report zips, directories holding an inventory and
/// the recorded source of a prior manifest while it still exists.
fn discover_candidates(workspace: &Path, prior: Option<&SourceManifest>) -> Vec<PathBuf> {
    let mut candidates = rglob(workspace, &is_report_zip);
    let parents: BTreeSet<PathBuf> = rglob(workspace, &is_inventory)
        .into_iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    candidates.extend(parents);
    if let Some(prior) = prior {
        let source = PathBuf::from(&prior.source_path);
        if source.exists() && !candidates.contains(&source) {
            candidates.push(source);
        }
    }
    candidates
}

#[derive(Debug)]
struct Warning {
    code: &'static str,
    identifier: String,
}

/// Extracts every entry of `archive` below `target`.
fn extract(archive: &Path, target: &Path) -> Result<(), String> {
    let data = fs::read(archive).map_err(|e| e.to_string())?;
    for (name, bytes) in read_zip(&data)? {
        let relative = Path::new(&name);
        if relative.is_absolute() || name.split('/').any(|part| part == "..") {
            return Err(format!("unsafe archive member {name:?}"));
        }
        if name.ends_with('/') {
            fs::create_dir_all(target.join(relative)).map_err(|e| e.to_string())?;
        } else {
            let path = target.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
            fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 file name")
        .to_owned()
}

fn prepare_archive(
    archive: &Path,
    output: &Path,
    inventories: &mut Vec<Json>,
    warnings: &mut Vec<Warning>,
) {
    let stem = archive
        .file_stem()
        .and_then(|n| n.to_str())
        .expect("utf-8 stem")
        .to_owned();
    let target = output.join(&stem);
    if extract(archive, &target).is_err() {
        warnings.push(Warning {
            code: "rdfa_report_unreadable",
            identifier: file_name(archive),
        });
        return;
    }
    for nested in rglob(&target, &|name| name.ends_with(".zip")) {
        prepare_archive(&nested, output, inventories, warnings);
    }
    for inventory in rglob(&target, &is_inventory) {
        match fs::read_to_string(&inventory)
            .map_err(|e| e.to_string())
            .and_then(|text| parse_json(&text).map_err(|e| format!("{e:?}")))
        {
            Ok(value) => inventories.push(value),
            Err(_) => warnings.push(Warning {
                code: "rdfa_report_unreadable",
                identifier: file_name(&inventory),
            }),
        }
    }
}

/// `_prepare_zip`: the inventories of a report ZIP (and the ZIPs nested in it)
/// and a warning for every archive or inventory that cannot be read.
fn prepare_zip(source: &Path, output: &Path) -> (Vec<Json>, Vec<Warning>) {
    let mut inventories = Vec::new();
    let mut warnings = Vec::new();
    prepare_archive(source, output, &mut inventories, &mut warnings);
    (inventories, warnings)
}

struct AiEntry {
    azure_id: String,
    source: &'static str,
    address: Option<&'static str>,
    config: Vec<(&'static str, &'static str)>,
}

struct Merged {
    azure_id: String,
    address: Option<&'static str>,
    sources: Vec<&'static str>,
    config: BTreeMap<&'static str, &'static str>,
}

/// `_reconcile_ai`: one record per `azure_id`, every contributing source kept,
/// later config values winning.
fn reconcile_ai(entries: &[AiEntry]) -> (Vec<Merged>, BTreeSet<&'static str>) {
    let mut merged: Vec<Merged> = Vec::new();
    let mut sources = BTreeSet::new();
    for entry in entries {
        sources.insert(entry.source);
        let slot = if let Some(slot) = merged.iter().position(|m| m.azure_id == entry.azure_id) {
            slot
        } else {
            merged.push(Merged {
                azure_id: entry.azure_id.clone(),
                address: None,
                sources: Vec::new(),
                config: BTreeMap::new(),
            });
            merged.len() - 1
        };
        merged[slot].sources.push(entry.source);
        merged[slot].config.extend(entry.config.iter().copied());
        if entry.address.is_some() {
            merged[slot].address = entry.address;
        }
    }
    (merged, sources)
}

// ---- producer-compatible execution simulations ------------------------------------------------

#[test]
fn test_metrics_batches_flatten_real_envelopes_and_accept_empty_batch() {
    let batch = report_fixture("metrics.json");
    let rows = flatten_metric_batches(&[batch.clone(), json_text(r#"{"Metrics": []}"#)])
        .expect("real envelopes flatten");
    assert_eq!(rows.len(), items(&batch, "Metrics").len());
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .all(|row| row.get("StartTime").is_none() && row.get("EndTime").is_none()));
    // Non-vacuous: a batch that is not a metrics envelope is refused.
    assert!(flatten_metric_batches(&[json_text(r#"{"Rows": []}"#)]).is_err());
    assert!(flatten_metric_batches(&[json_text(r#"{"Metrics": 3}"#)]).is_err());
    assert!(flatten_metric_batches(&[json_text("[]")]).is_err());
}

#[test]
fn test_error_empty_and_sampled_zero_have_distinct_eligibility() {
    let metrics = report_fixture("metrics.json");
    let rows = items(&metrics, "Metrics");
    let errored = rows
        .iter()
        .find(|row| row.get("MetricError") == Some(&Json::Bool(true)))
        .expect("an errored row");
    let empty = rows
        .iter()
        .find(|row| row.get("Name").and_then(Json::as_str) == Some("vm-empty"))
        .expect("vm-empty");
    let sampled_zero = rows
        .iter()
        .find(|row| row.get("Name").and_then(Json::as_str) == Some("vm-idle"))
        .expect("vm-idle");
    assert!(!eligible(errored, Some(31.0), 14.0));
    assert!(!eligible(empty, Some(31.0), 14.0));
    assert!(eligible(sampled_zero, Some(31.0), 14.0));
    assert!(!eligible(sampled_zero, None, 14.0));
    // Non-vacuous: the window floor is inclusive at the minimum and fails closed below it.
    assert!(eligible(sampled_zero, Some(14.0), 14.0));
    assert!(!eligible(sampled_zero, Some(13.0), 14.0));
    // ... and each other condition alone is enough to refuse a row.
    let mut flagged = sampled_zero.clone();
    if let Json::Object(members) = &mut flagged {
        for (key, value) in members {
            if key == "MetricError" {
                *value = Json::Bool(true);
            }
        }
    }
    assert!(!eligible(&flagged, Some(31.0), 14.0));
}

#[test]
fn test_report_only_and_app_code_plus_report_are_admitted() {
    let infra: BTreeSet<&str> = [
        "azure-resource-inventory.json",
        "azure-resource-clusters.json",
    ]
    .into_iter()
    .collect();
    assert_eq!(discover_outputs(&[Source::Rdfa]), infra);
    let mut with_ai = infra.clone();
    with_ai.insert("ai-workload-profile.json");
    assert_eq!(discover_outputs(&[Source::Rdfa, Source::AppAi]), with_ai);
    assert_eq!(discover_outputs(&[]), BTreeSet::new());
    // Non-vacuous: the other infrastructure sources admit the same outputs and
    // application code alone admits only the AI profile.
    assert_eq!(discover_outputs(&[Source::Iac]), infra);
    assert_eq!(discover_outputs(&[Source::Live]), infra);
    assert_eq!(
        discover_outputs(&[Source::AppAi]),
        ["ai-workload-profile.json"].into_iter().collect()
    );
}

#[test]
fn test_source_identity_reuses_only_exact_source_and_replacement_wins() {
    // The digest is real SHA-256 (the fingerprint contract), not a stand-in.
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let first = source_manifest("/reports/a", "directory", &[("Inventory_a.json", b"one")]);
    let repeated = source_manifest("/reports/a", "directory", &[("Inventory_a.json", b"one")]);
    let replaced = source_manifest("/reports/b", "directory", &[("Inventory_b.json", b"two")]);
    let changed_zip = source_manifest("/reports/a.zip", "zip", &[("a.zip", b"new bytes")]);
    assert_eq!(first, repeated);
    assert_ne!(first, replaced);
    assert_ne!(first, changed_zip);
    // Non-vacuous: the same path and type with changed bytes, or the same bytes
    // with another path or type, is a different source.
    let edited = source_manifest("/reports/a", "directory", &[("Inventory_a.json", b"one!")]);
    assert_ne!(first, edited);
    assert_ne!(first.fingerprint, edited.fingerprint);
    assert_ne!(
        first,
        source_manifest("/reports/a", "zip", &[("Inventory_a.json", b"one")])
    );
    assert_ne!(
        first,
        source_manifest("/reports/c", "directory", &[("Inventory_a.json", b"one")])
    );
}

#[test]
fn test_external_directory_reentry_seeds_recorded_source_without_new_input() {
    let tmp = Tmp::new("reentry");
    let workspace = tmp.path().join("workspace");
    let external = tmp.path().join("outside-workspace").join("report");
    fs::create_dir_all(&workspace).expect("workspace");
    let inventory = external.join("Inventory_external.json");
    write_file(&inventory, br#"{"VirtualMachines": []}"#);
    let prior = source_manifest(
        external.to_str().expect("utf-8"),
        "directory",
        &[(
            "Inventory_external.json",
            &fs::read(&inventory).expect("read inventory"),
        )],
    );
    assert_eq!(
        discover_candidates(&workspace, Some(&prior)),
        vec![external.clone()]
    );
    // Non-vacuous: without the recorded source there is no candidate, a recorded
    // source that is gone seeds nothing, and a source already inside the
    // workspace is not listed twice.
    assert_eq!(discover_candidates(&workspace, None), Vec::<PathBuf>::new());
    let gone = source_manifest("/no/such/report", "directory", &[]);
    assert_eq!(
        discover_candidates(&workspace, Some(&gone)),
        Vec::<PathBuf>::new()
    );
    let inside = workspace.join("here");
    write_file(&inside.join("Inventory_here.json"), b"{}");
    let recorded = source_manifest(inside.to_str().expect("utf-8"), "directory", &[]);
    assert_eq!(
        discover_candidates(&workspace, Some(&recorded)),
        vec![inside]
    );
}

#[test]
fn test_corrupt_selected_report_records_preparation_warning() {
    let tmp = Tmp::new("corrupt");
    let corrupt = tmp.path().join("Selected_ResourcesReport_2025.zip");
    write_file(&corrupt, b"not a zip");
    let (inventories, warnings) = prepare_zip(&corrupt, &tmp.path().join("prepared"));
    assert!(inventories.is_empty());
    assert_eq!(warnings[0].code, "rdfa_report_unreadable");
    assert_eq!(warnings[0].identifier, "Selected_ResourcesReport_2025.zip");
    // Non-vacuous: the same preparation of a readable report yields its
    // inventory and no warning.
    let good = tmp.path().join("Good_ResourcesReport_2025.zip");
    write_file(
        &good,
        &write_zip(&[("Inventory_good.json", br#"{"VirtualMachines": []}"#)]),
    );
    let (inventories, warnings) = prepare_zip(&good, &tmp.path().join("prepared"));
    assert_eq!(inventories.len(), 1);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn test_partial_nested_report_keeps_valid_inner_and_warns_for_bad_inner() {
    let tmp = Tmp::new("nested");
    let good_inner = write_zip(&[("Inventory_good.json", br#"{"VirtualMachines": []}"#)]);
    let outer = tmp.path().join("AllSubscriptions_ResourcesReport_2025.zip");
    write_file(
        &outer,
        &write_zip(&[("good.zip", &good_inner), ("bad.zip", b"not a zip")]),
    );
    let (inventories, warnings) = prepare_zip(&outer, &tmp.path().join("prepared"));
    assert_eq!(inventories, vec![json_text(r#"{"VirtualMachines": []}"#)]);
    assert!(warnings.iter().any(|w| w.identifier == "bad.zip"));
    // Stronger than the Python `any`: the bad inner ZIP is the only warning.
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    // Non-vacuous: an inventory that is not JSON is a warning, not a result.
    let broken = tmp.path().join("Broken_ResourcesReport_2025.zip");
    write_file(
        &broken,
        &write_zip(&[("Inventory_broken.json", b"{not json")]),
    );
    let (inventories, warnings) = prepare_zip(&broken, &tmp.path().join("prepared"));
    assert!(inventories.is_empty());
    assert_eq!(warnings[0].identifier, "Inventory_broken.json");
}

#[test]
fn test_fixed_and_autoscaled_aks_bounds_are_valid() {
    let inventory = report_fixture("inventory.json");
    let pools = items(&inventory, "AKS");
    assert_eq!(str_of(&pools[0], "Autoscale"), "true");
    let fixed = normalize_pool(&pools[0]);
    let autoscaled = normalize_pool(&pools[2]);
    assert_eq!(
        fixed,
        Pool {
            desired_size: 3,
            min_size: 3,
            max_size: 3
        }
    );
    assert_eq!(
        autoscaled,
        Pool {
            desired_size: 1,
            min_size: 0,
            max_size: 2
        }
    );
    // Non-vacuous: real autoscale bounds are kept, and autoscale off pins them.
    assert_eq!(
        normalize_pool(&pools[1]),
        Pool {
            desired_size: 4,
            min_size: 2,
            max_size: 8
        }
    );
    let off =
        json_text(r#"{"Nodes": 5, "Autoscale": "false", "AutoscaleMin": 1, "AutoscaleMax": 9}"#);
    assert_eq!(
        normalize_pool(&off),
        Pool {
            desired_size: 5,
            min_size: 5,
            max_size: 5
        }
    );
}

#[test]
fn test_zero_node_aks_pool_gets_eks_valid_max_size() {
    let zero_node = json_text(
        r#"{"Nodes": "0", "Autoscale": "false", "AutoscaleMin": "0", "AutoscaleMax": "0"}"#,
    );
    assert_eq!(
        normalize_pool(&zero_node),
        Pool {
            desired_size: 0,
            min_size: 0,
            max_size: 1
        }
    );
    // Non-vacuous: only an absent maximum is floored; a real one is kept.
    let scalable = json_text(
        r#"{"Nodes": "0", "Autoscale": "true", "AutoscaleMin": "0", "AutoscaleMax": "4"}"#,
    );
    assert_eq!(
        normalize_pool(&scalable),
        Pool {
            desired_size: 0,
            min_size: 0,
            max_size: 4
        }
    );
}

#[test]
fn test_vm_os_disk_maps_to_canonical_nested_shape() {
    let inventory = report_fixture("inventory.json");
    let (size, disk) = map_vm(&items(&inventory, "VirtualMachines")[0]);
    assert_eq!(size, "Standard_D4s_v5");
    assert_eq!(
        disk,
        vec![
            ("storage_account_type", Json::string("Premium_LRS")),
            ("disk_size_gb", Json::Int(128)),
        ]
    );
    assert!(!disk.iter().any(|(key, _)| *key == "caching"));
    // Non-vacuous: the placeholder disk types and a missing size map to nothing.
    for kind in ["None", "Custom VHD"] {
        let row = json_text(&format!(r#"{{"Size": "s", "OSDisk": "{kind}"}}"#));
        assert!(map_vm(&row).1.is_empty(), "{kind}");
    }
    let sized = json_text(r#"{"Size": "s", "OSDisk": null, "OSDiskSizeGB": 64}"#);
    assert_eq!(map_vm(&sized).1, vec![("disk_size_gb", Json::Int(64))]);
}

#[test]
fn test_rdfa_ml_workspace_contributes_and_mixed_identity_reconciles() {
    let inventory = report_fixture("inventory.json");
    let workspace = &items(&inventory, "MachineLearning")[0];
    let id = str_of(workspace, "ID").to_owned();
    let entry = |azure_id: &str, source, address, config| AiEntry {
        azure_id: azure_id.to_owned(),
        source,
        address,
        config,
    };
    let entries = vec![
        entry(
            &id,
            "terraform",
            Some("azurerm_machine_learning_workspace.main"),
            vec![],
        ),
        entry(&id, "rdfa", None, vec![("sku", "Basic")]),
        entry(&id, "live", None, vec![("sku", "Enterprise")]),
    ];
    let (merged, sources) = reconcile_ai(&entries);
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].sources.iter().copied().collect::<BTreeSet<_>>(),
        ["terraform", "rdfa", "live"].into_iter().collect()
    );
    assert_eq!(sources, ["terraform", "rdfa", "live"].into_iter().collect());
    // Non-vacuous: the later config wins, and another identity stays separate.
    assert_eq!(merged[0].config.get("sku"), Some(&"Enterprise"));
    assert_eq!(merged[0].azure_id, id);
    assert_eq!(
        merged[0].address,
        Some("azurerm_machine_learning_workspace.main")
    );
    let mut two = entries;
    two.push(entry("/other/id", "rdfa", None, vec![]));
    assert_eq!(reconcile_ai(&two).0.len(), 2);
}

// ---- published prose and artifact contracts -------------------------------------------------------

#[test]
fn test_rw_fragment_owns_no_executable_preparation() {
    check_rules(&[
        Rule::new(DISCOVER).all(&[
            "main-window pre-dispatch action",
            "never extracts ZIPs, runs Python",
        ]),
        Rule::new(DISCOVER_RDFA)
            .all(&["This `rw` fragment reads only prepared files"])
            .none(&["Write a throwaway extraction script"]),
    ]);
}

#[test]
fn test_absent_report_guidance_is_reachable_before_trigger() {
    check_rules(&[Rule::new(DISCOVER).before(
        "If no candidate exists, output",
        "Run each fragment whose `_trigger` holds",
    )]);
}

#[test]
fn test_main_window_owns_reentry_and_preparation_failure_contracts() {
    check_rules(&[
        Rule::new(DISCOVER).all(&[
            "existing manifest's `source_path`",
            "preparation-result.json",
            "rdfa_report_unreadable",
            "corrupt or password-protected ZIP",
            "continue preparing every other valid inner ZIP",
        ]),
        Rule::new(DISCOVER_ASSEMBLE).all(&["preparation-result.json"]),
    ]);
}

#[test]
fn test_discover_and_assembler_include_rdfa_everywhere() {
    check_rules(&[
        Rule::new(DISCOVER).all(&["OR a validated RDfA preparation manifest"]),
        Rule::new(DISCOVER_ASSEMBLE).all(&[
            "A report-only run is an infrastructure run",
            "rdfa (fragment contribution",
        ]),
    ]);
}

#[test]
fn test_schema_registers_rdfa_inventory_and_ai_provenance() {
    check_rules(&[
        Rule::new(SCHEMA_AZURE).all(&["\"rdfa_metadata\": {}", "## rdfa_metadata"]),
        Rule::new(SCHEMA_AI).all(&["\"rdfa\": false", "rdfa_report"]),
    ]);
}

/// `rules["confidence_rules"]["measured_requires"]` of the rightsizing thresholds.
fn confidence_requires_samples_and_window(p: &Plugin) -> Verdict {
    let rules = parse_json(&p.read(THRESHOLDS)?).map_err(|e| format!("{THRESHOLDS}: {e:?}"))?;
    let requirement = rules
        .get("confidence_rules")
        .and_then(|rules| rules.get("measured_requires"))
        .and_then(Json::as_str)
        .ok_or("confidence_rules.measured_requires is not a string")?;
    ensure!(
        requirement.contains("sample_count > 0"),
        "measured_requires no longer needs sample_count > 0"
    );
    ensure!(
        requirement.contains("Null/unknown window_days fails closed"),
        "measured_requires no longer says Null/unknown window_days fails closed"
    );
    Ok(())
}

#[test]
fn test_confidence_contract_requires_samples_and_known_window() {
    check(
        confidence_requires_samples_and_window,
        &[
            (
                edit_replace!(THRESHOLDS, "sample_count > 0", "has samples"),
                "sample_count > 0",
            ),
            (
                edit_replace!(
                    THRESHOLDS,
                    "Null/unknown window_days fails closed",
                    "unknown window_days is tolerated"
                ),
                "Null/unknown window_days fails closed",
            ),
        ],
    );
}

#[test]
fn test_live_and_rdfa_use_same_node_pool_key() {
    check_rules(&[
        Rule::new(DISCOVER_RDFA).all(&["config.node_pools[]"]),
        Rule::new(DISCOVER_LIVE).all(&["rename the array's own key `nodePools` → `node_pools`"]),
    ]);
}

/// The corrupt-ZIP fixture is not a ZIP and the newest multi-ZIP report is
/// chosen by its digits, under the fixture root `root` (the plugin tree).
fn corrupt_zip_and_tie_break(root: &Path) -> Verdict {
    let corrupt = rglob(&root.join(CORRUPT_DIR), &|name| name.ends_with(".zip"));
    let corrupt = corrupt.first().ok_or("no corrupt-zip fixture")?;
    let bytes = fs::read(corrupt).map_err(|e| e.to_string())?;
    ensure!(
        !is_zipfile(&bytes),
        "{} is a readable zip, not a corrupt one",
        file_name(corrupt)
    );
    let mut newest: Option<(String, String)> = None;
    for path in rglob(&root.join(MULTI_DIR), &|name| name.ends_with(".zip")) {
        let name = file_name(&path);
        let digits: String = name.chars().filter(char::is_ascii_digit).collect();
        if newest.as_ref().is_none_or(|(_, best)| digits > *best) {
            newest = Some((name, digits));
        }
    }
    let (newest, _) = newest.ok_or("no multi-zip fixture")?;
    ensure!(
        newest == NEWEST_REPORT,
        "the newest report is {newest}, not {NEWEST_REPORT}"
    );
    Ok(())
}

/// A scratch plugin root whose two ZIP fixture directories hold the named files.
fn scratch_zip_fixtures(corrupt: &[u8], multi: &[&str]) -> Tmp {
    let tmp = Tmp::new("zip-fixtures");
    write_file(
        &tmp.path().join(CORRUPT_DIR).join("Report_2024-03-01.zip"),
        corrupt,
    );
    for name in multi {
        write_file(&tmp.path().join(MULTI_DIR).join(name), b"");
    }
    tmp
}

#[test]
fn test_fixture_corrupt_zip_is_invalid_and_tie_break_is_deterministic() {
    let root = plugin_root();
    if let Err(reason) = corrupt_zip_and_tie_break(&root) {
        panic!("the committed fixtures break the contract: {reason}");
    }
    // The helper agrees with the committed fixture: it is a truncated archive.
    let corrupt = rglob(&root.join(CORRUPT_DIR), &|name| name.ends_with(".zip"));
    let bytes = fs::read(&corrupt[0]).expect("fixture bytes");
    assert!(
        bytes.starts_with(b"PK\x03\x04"),
        "a real archive, cut short"
    );
    assert!(read_zip(&bytes).is_err());
    // The same checks accept a scratch copy of the committed shape ...
    let names = [
        "CompanyName_ResourcesReport_20240101000000083a.zip",
        NEWEST_REPORT,
    ];
    let same = scratch_zip_fixtures(&bytes, &names);
    assert_eq!(corrupt_zip_and_tie_break(same.path()), Ok(()));
    // ... and refuse a readable archive in the corrupt slot, or a newer name.
    let valid = write_zip(&[("Inventory_a.json", b"{}")]);
    assert!(is_zipfile(&valid));
    assert_eq!(
        read_zip(&valid).expect("round trip"),
        vec![("Inventory_a.json".to_owned(), b"{}".to_vec())]
    );
    let readable = scratch_zip_fixtures(&valid, &names);
    let reason = corrupt_zip_and_tie_break(readable.path()).expect_err("a readable archive");
    assert!(reason.contains("is a readable zip"), "{reason}");
    let newer = scratch_zip_fixtures(
        &bytes,
        &[
            names[0],
            names[1],
            "AllSubscriptions_ResourcesReport_2026-01-01_00-00-00.zip",
        ],
    );
    let reason = corrupt_zip_and_tie_break(newer.path()).expect_err("a newer report");
    assert!(reason.contains("the newest report is"), "{reason}");
}
