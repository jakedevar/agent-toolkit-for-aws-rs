//! Tests for `toolkit startup emit-plan` (migrated from
//! `tests/test_emit_plan_json.py`, plus the route, cap, accuracy and
//! fail-open rules of `emit-plan-json.py`).

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use toolkit_core::Json;

use super::{
    accuracy, billing_service_items, is_uuid, looser_accuracy, normalized, pricing_source,
    service_items, source_monthly, Stop,
};
use crate::run;
use crate::startup::artifacts::pyjson::loads;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-plan-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

fn fixture(name: &str) -> PathBuf {
    plugin().join("fixtures").join(name)
}

fn json(text: &str) -> Json {
    loads(text).expect("valid JSON")
}

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).expect("write fixture");
}

fn status(dir: &Path, skill: &str, run_id: &str) {
    write(
        dir,
        ".phase-status.json",
        &format!("{{\"owning_skill\": \"{skill}\", \"run_id\": \"{run_id}\"}}"),
    );
}

fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

/// Runs the emitter on `dir`; returns stdout (the status line).
fn emit(dir: &Path) -> String {
    emit_with(dir, &[])
}

fn emit_with(dir: &Path, extra: &[&str]) -> String {
    let dir_arg = dir.to_str().expect("utf-8 path");
    let mut args = vec!["startup", "emit-plan", "--migration-dir", dir_arg];
    args.extend_from_slice(extra);
    let (code, out, err) = exec(&args);
    assert_eq!(code, 0, "emit-plan failed: {err}");
    assert_eq!(err, "", "emit-plan wrote to stderr");
    out
}

fn plan(dir: &Path) -> Json {
    json(&fs::read_to_string(dir.join("plan.json")).expect("plan.json written"))
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy target");
    for entry in fs::read_dir(from).expect("read copy source") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

fn seeded_gcp(label: &str, run_id: &str) -> TempDir {
    let dir = TempDir::new(label);
    copy_dir(&fixture("gcp-decision-gate/after-decide-complete"), &dir);
    status(&dir, "GCP_TO_AWS", run_id);
    dir
}

const GOLDEN_RUN_ID: &str = "8c1e4f2a-3b6d-4a7e-9f10-5d2c8b7a6e41";

#[test]
fn uppercase_uuid_run_id_is_lowercased() {
    let upper = GOLDEN_RUN_ID.to_uppercase();
    let dir = seeded_gcp("upper", &upper);
    let line = emit(&dir);
    assert!(line.starts_with("PLAN_OK | path="), "{line}");
    assert_eq!(plan(&dir).get("runId"), Some(&Json::string(GOLDEN_RUN_ID)));
}

#[test]
fn lowercase_uuid_run_id_is_unchanged() {
    let dir = seeded_gcp("lower", GOLDEN_RUN_ID);
    emit(&dir);
    assert_eq!(plan(&dir).get("runId"), Some(&Json::string(GOLDEN_RUN_ID)));
}

#[test]
fn non_uuid_run_id_passes_through_stripped() {
    let dir = seeded_gcp("plain", "  Run-ABC  ");
    emit(&dir);
    assert_eq!(plan(&dir).get("runId"), Some(&Json::string("Run-ABC")));
    assert!(is_uuid("8C1E4F2A-3B6D-4A7E-9F10-5D2C8B7A6E41"));
    assert!(!is_uuid("8c1e4f2a-3b6d-4a7e-9f10-5d2c8b7a6e4"));
    assert!(!is_uuid("8c1e4f2a-3b6d-4a7e-9f10-5d2c8b7a6e4g"));
    assert!(!is_uuid("8c1e4f2a3b6d4a7e9f105d2c8b7a6e41"));
}

#[test]
fn golden_gcp_run_emits_the_exact_plan_document() {
    let dir = seeded_gcp("golden", GOLDEN_RUN_ID);
    let line = emit_with(&dir, &["--plugin-root", plugin().to_str().expect("utf-8")]);
    let path = dir.join("plan.json");
    assert_eq!(
        line,
        format!(
            "PLAN_OK | path={} | platform=GCP | scope=INFRA_ONLY\n",
            path.display()
        )
    );
    let version = json(
        &fs::read_to_string(plugin().join(".claude-plugin/plugin.json")).expect("plugin.json"),
    );
    let version = version
        .get("version")
        .and_then(Json::as_str)
        .expect("version");
    let expected = format!(
        "{{\n  \"schemaVersion\": 1,\n  \"sourcePlatform\": \"GCP\",\n  \"scope\": \"INFRA_ONLY\",\n  \"runId\": \"{GOLDEN_RUN_ID}\",\n  \"cost\": {{\n    \"awsMonthly\": 155,\n    \"awsMonthlyBasis\": \"BALANCED\"\n  }},\n  \"producerVersion\": \"{version}\"\n}}\n"
    );
    assert_eq!(fs::read_to_string(&path).expect("plan.json"), expected);
}

#[cfg(unix)]
#[test]
fn plan_json_is_private_and_no_temp_is_left() {
    use std::os::unix::fs::PermissionsExt;
    let dir = seeded_gcp("mode", GOLDEN_RUN_ID);
    emit(&dir);
    let mode = fs::metadata(dir.join("plan.json"))
        .expect("plan.json")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let leftovers: Vec<_> = fs::read_dir(&*dir)
        .expect("read dir")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json.tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

#[test]
fn azure_infra_run_emits_plan_skip_and_no_file() {
    let dir = TempDir::new("azure");
    copy_dir(&fixture("azure-iac-terraform/after-estimate"), &dir);
    status(&dir, "AZURE_TO_AWS", GOLDEN_RUN_ID);
    let line = emit(&dir);
    assert!(
        line.starts_with("PLAN_SKIP | reason=owning_skill 'AZURE_TO_AWS' has no web handoff yet"),
        "{line}"
    );
    assert!(!dir.join("plan.json").exists());
}

#[test]
fn azure_composition_share_is_not_exported_as_accuracy() {
    let infra = json(
        &fs::read_to_string(fixture(
            "azure-iac-terraform/after-estimate/estimation-infra.json",
        ))
        .expect("fixture"),
    );
    assert!(matches!(accuracy(&infra), Ok(None)));
}

fn band(text: &str) -> Option<(i64, i64)> {
    let data = Json::object(vec![("accuracy_confidence", Json::string(text))]);
    let Ok(result) = accuracy(&data) else {
        panic!("unexpected failure for {text:?}");
    };
    result.map(|value| {
        let get = |key| match value.get(key) {
            Some(Json::Int(number)) => *number,
            other => panic!("unexpected {key}: {other:?}"),
        };
        (get("minPercent"), get("maxPercent"))
    })
}

#[test]
fn accuracy_parses_declared_overall_bands() {
    assert_eq!(band("+/-5-10%"), Some((5, 10)));
    assert_eq!(band("±30-40%"), Some((30, 40)));
    assert_eq!(band("accuracy ± 30 %"), Some((30, 30)));
    assert_eq!(band("+/- 5 - 10 %"), Some((5, 10)));
    assert_eq!(band("±10-5%"), Some((5, 10)), "a reversed band is swapped");
    assert_eq!(band("±2.5%"), Some((2, 2)), "Python rounds half to even");
    assert_eq!(band("±3.5%"), Some((4, 4)));
    assert_eq!(band("±0.4-100.4%"), Some((0, 100)));
}

#[test]
fn accuracy_rejects_unsigned_scoped_and_out_of_range_figures() {
    assert_eq!(band("the RDS line — 59% of the priced total"), None);
    assert_eq!(band("+/-35% on the observability heuristic"), None);
    assert_eq!(band("±20% of the total"), None);
    assert_eq!(band("±20% of  the  bill"), None);
    assert_eq!(band("±20% for the bill"), None);
    assert_eq!(band("±20%on the bill"), None);
    assert_eq!(
        band("±20% on theory"),
        Some((20, 20)),
        "`the` needs a word boundary"
    );
    assert_eq!(band("±20% on thé"), Some((20, 20)));
    assert_eq!(band("±101%"), None);
    assert_eq!(band("±5-101%"), None);
    assert_eq!(band("±5."), None);
    assert_eq!(band("±5-%"), None);
    assert_eq!(band("5%"), None);
    // A later unscoped band is still found after a scoped one.
    assert_eq!(
        band("+/-35% on the heuristic, otherwise ±10-20%"),
        Some((10, 20))
    );
}

#[test]
fn accuracy_huge_digit_strings_fail_open_instead_of_wrapping() {
    let digits = "9".repeat(400);
    let data = Json::object(vec![(
        "accuracy_confidence",
        Json::string(format!("±{digits}%")),
    )]);
    assert!(matches!(
        accuracy(&data),
        Err(Stop::Fail(message)) if message == "cannot convert float infinity to integer"
    ));
}

#[test]
fn accuracy_pricing_source_comes_from_the_route_shape() {
    let infra = json(
        r#"{"accuracy_confidence": "±5-10%", "pricing_source": {"status": "cached", "fallback_staleness": {"is_stale": true}}}"#,
    );
    let stale = accuracy(&infra).ok().flatten().expect("band");
    assert_eq!(
        stale.get("pricingSource"),
        Some(&Json::string("CACHED_STALE"))
    );

    let billing = json(r#"{"accuracy_confidence": "±5%", "metadata": {"pricing_source": "live"}}"#);
    let live = accuracy(&billing).ok().flatten().expect("band");
    assert_eq!(live.get("pricingSource"), Some(&Json::string("LIVE")));

    let unknown = json(r#"{"accuracy_confidence": "±5%", "pricing_source": "bogus"}"#);
    let none = accuracy(&unknown).ok().flatten().expect("band");
    assert_eq!(none.get("pricingSource"), None);

    for (raw, expected) in [
        (r#"{"status": "cached"}"#, Some("CACHED")),
        (r#"{"status": "cached_fallback"}"#, Some("CACHED_FALLBACK")),
        (r#"{"status": "cached_stale"}"#, Some("CACHED_STALE")),
        (r#"{"status": "unavailable"}"#, Some("UNAVAILABLE")),
        (
            r#"{"status": "cached", "fallback_staleness": {"is_stale": false}}"#,
            Some("CACHED"),
        ),
        (r#"{"status": 5}"#, None),
        (r"[]", None),
        (r"5", None),
    ] {
        let value = json(raw);
        assert!(
            matches!(pricing_source(Some(&value)), Ok(found) if found == expected),
            "{raw}"
        );
    }
    assert!(matches!(pricing_source(None), Ok(None)));
    let unhashable = json(r#"{"status": ["cached"]}"#);
    assert!(matches!(
        pricing_source(Some(&unhashable)),
        Err(Stop::Fail(message)) if message == "unhashable type: 'list'"
    ));
}

#[test]
fn looser_band_is_carried_whole() {
    let a = json(r#"{"minPercent": 5, "maxPercent": 10, "pricingSource": "LIVE"}"#);
    let b = json(r#"{"minPercent": 20, "maxPercent": 30, "pricingSource": "LIVE"}"#);
    let c = json(r#"{"minPercent": 25, "maxPercent": 30}"#);
    assert_eq!(
        looser_accuracy(&a, &b),
        json(r#"{"minPercent": 20, "maxPercent": 30, "pricingSource": "LIVE"}"#)
    );
    assert_eq!(
        looser_accuracy(&b, &a),
        json(r#"{"minPercent": 20, "maxPercent": 30, "pricingSource": "LIVE"}"#)
    );
    assert_eq!(
        looser_accuracy(&b, &c),
        json(r#"{"minPercent": 25, "maxPercent": 30}"#),
        "a tie on max picks the larger min, and differing sources are dropped"
    );
}

#[test]
fn azure_service_items_use_azure_field_names_and_exclude_floors() {
    let infra = json(
        &fs::read_to_string(fixture(
            "azure-iac-terraform/after-estimate/estimation-infra.json",
        ))
        .expect("fixture"),
    );
    let items = service_items(infra.get("projected_costs").expect("projected_costs"));
    assert_eq!(items.len(), 12, "{items:?}");
    let mut sum = 0.0_f64;
    let mut names = Vec::new();
    for item in &items {
        match item.get("monthlyCost") {
            Some(Json::Float(value)) => sum += value,
            Some(Json::Int(value)) => sum += f64::from(i32::try_from(*value).expect("small")),
            other => panic!("unexpected cost {other:?}"),
        }
        names.push(
            item.get("serviceName")
                .and_then(Json::as_str)
                .expect("name")
                .to_string(),
        );
        assert_eq!(
            item.get("classification"),
            Some(&Json::string("INFRASTRUCTURE"))
        );
    }
    assert_eq!(format!("{sum:.2}"), "465.09");
    for excluded in ["FSx for Windows File Server", "DocumentDB", "Lambda"] {
        assert!(
            !names.iter().any(|name| name == excluded),
            "{excluded} leaked"
        );
    }
    assert!(!items.iter().any(|item| {
        matches!(item.get("monthlyCost"), Some(Json::Float(v)) if (*v - 70.72).abs() < 1e-9 || (*v - 140.16).abs() < 1e-9)
    }));
}

#[test]
fn breakdown_rules_cover_forms_labels_rollups_and_bounds() {
    let long_name = "x".repeat(129);
    let wide_name = "\u{1F600}".repeat(65);
    let projected = json(&format!(
        r#"{{"breakdown": {{
            "compute": 75,
            "Total": 999,
            " TOTAL ": 999,
            "labelled_total": {{"monthly": 5, "service": "Total"}},
            "bool": true,
            "negative": -1,
            "over_cap": 100000001,
            "at_cap": 100000000,
            "gcp": {{"monthly": 10.5, "service": "Cloud Run"}},
            "heroku": {{"mid": 20}},
            "azure": {{"right_sized_monthly": 30, "aws_service": "AKS"}},
            "blank_service": {{"monthly": 4, "service": "  ", "aws_service": "ignored"}},
            "falsy_service": {{"monthly": 6, "service": "", "aws_service": "Fallback"}},
            "numeric_service": {{"monthly": 7, "service": 5}},
            "first_key_wins": {{"monthly": -1, "mid": 5}},
            "present_null": {{"monthly": null, "mid": 5}},
            "excluded": {{"monthly": 5, "excluded_from_total": true}},
            "reasoned": {{"monthly": 5, "exclusion_reason": "no_rate"}},
            "empty_reason": {{"monthly": 8, "exclusion_reason": ""}},
            "nested": {{"alternative": {{"monthly": 5}}}},
            "string_entry": "5",
            "{long_name}": 3,
            "{wide_name}": 3,
            "  ": 3
        }}}}"#
    ));
    let names: Vec<String> = service_items(&projected)
        .iter()
        .map(|item| {
            let name = item
                .get("serviceName")
                .and_then(Json::as_str)
                .expect("name");
            format!(
                "{name}={}",
                item.get("monthlyCost").expect("cost").to_compact_string()
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            "compute=75",
            "at_cap=100000000",
            "Cloud Run=10.5",
            "heroku=20",
            "AKS=30",
            "blank_service=4",
            "Fallback=6",
            "numeric_service=7",
            "empty_reason=8",
        ],
        "items keep breakdown order"
    );
    assert!(service_items(&json(r#"{"breakdown": []}"#)).is_empty());
    assert!(service_items(&json("{}")).is_empty());
}

#[test]
fn billing_items_follow_the_same_bounds() {
    let projection = json(
        r#"{"services": [
            {"aws_target": " Lambda ", "aws_mid": 12},
            {"aws_target": "Total", "aws_mid": 12},
            {"aws_target": "", "aws_mid": 12},
            {"aws_target": 5, "aws_mid": 12},
            {"aws_target": "Over", "aws_mid": 100000001},
            {"aws_target": "Bool", "aws_mid": true},
            "not-an-object",
            {"aws_target": "S3", "aws_mid": 1.5}
        ]}"#,
    );
    let items = billing_service_items(Some(&projection));
    let names: Vec<_> = items
        .iter()
        .map(|item| {
            item.get("serviceName")
                .and_then(Json::as_str)
                .expect("name")
        })
        .collect();
    assert_eq!(names, ["Lambda", "S3"]);
    assert!(billing_service_items(None).is_empty());
    assert!(billing_service_items(Some(&json(r#"{"services": {}}"#))).is_empty());
}

#[test]
fn non_usd_baseline_is_omitted_from_source_monthly() {
    let eur = json(r#"{"current_costs": {"currency": "EUR", "azure_monthly": 4200}}"#);
    assert_eq!(source_monthly(&eur, "AZURE"), None);
    let usd = json(r#"{"current_costs": {"currency": " usd ", "azure_monthly": 4200}}"#);
    assert_eq!(source_monthly(&usd, "AZURE"), Some(Json::Int(4200)));
    let blank = json(r#"{"current_costs": {"currency": "  ", "azure_monthly": 42.5}}"#);
    assert_eq!(source_monthly(&blank, "AZURE"), Some(Json::Float(42.5)));
    let gcp = json(r#"{"current_costs": {"gcp_monthly": 300}}"#);
    assert_eq!(source_monthly(&gcp, "GCP"), Some(Json::Int(300)));
    assert_eq!(source_monthly(&gcp, "HEROKU"), None);
    let heroku = json(r#"{"cost_comparison": {"heroku_monthly_baseline": 80}}"#);
    assert_eq!(source_monthly(&heroku, "HEROKU"), Some(Json::Int(80)));
    let over = json(r#"{"current_costs": {"gcp_monthly": 100000001}}"#);
    assert_eq!(source_monthly(&over, "GCP"), None);
    assert_eq!(source_monthly(&gcp, "OPENAI"), None);
    assert_eq!(
        source_monthly(&json(r#"{"current_costs": 5}"#), "GCP"),
        None
    );
}

#[test]
fn heroku_infra_run_copies_its_baseline_and_items() {
    let dir = TempDir::new("heroku");
    status(&dir, "HEROKU_TO_AWS", GOLDEN_RUN_ID);
    write(
        &dir,
        "estimation-infra.json",
        r#"{
          "accuracy_confidence": "±15-25% overall",
          "pricing_source": {"status": "live"},
          "projected_costs": {"aws_monthly_balanced": 210.5, "breakdown": {"web": {"mid": 100.25}, "total": 210.5}},
          "cost_comparison": {"heroku_monthly_baseline": 300}
        }"#,
    );
    let line = emit(&dir);
    assert!(
        line.ends_with("| platform=HEROKU | scope=INFRA_ONLY\n"),
        "{line}"
    );
    let document = plan(&dir);
    let cost = document.get("cost").expect("cost");
    assert_eq!(cost.get("awsMonthly"), Some(&Json::Float(210.5)));
    assert_eq!(cost.get("sourceMonthly"), Some(&Json::Int(300)));
    assert_eq!(
        cost.get("accuracy"),
        Some(&json(
            r#"{"minPercent": 15, "maxPercent": 25, "pricingSource": "LIVE"}"#
        ))
    );
    assert_eq!(
        cost.get("awsServiceItems"),
        Some(&json(
            r#"[{"serviceName": "web", "monthlyCost": 100.25, "classification": "INFRASTRUCTURE"}]"#
        ))
    );
    assert_eq!(
        document.get("producerVersion"),
        None,
        "no plugin.json under `.`"
    );
}

#[test]
fn billing_route_is_billing_mid() {
    let dir = TempDir::new("billing");
    status(&dir, "GCP_TO_AWS", GOLDEN_RUN_ID);
    write(
        &dir,
        "estimation-billing.json",
        r#"{
          "accuracy_confidence": "+/-30%",
          "metadata": {"pricing_source": "cached"},
          "cost_comparison": {"aws_monthly_mid": 80, "gcp_monthly": 120},
          "aws_projection": {"services": [{"aws_target": "Fargate", "aws_mid": 80}]}
        }"#,
    );
    emit(&dir);
    let document = plan(&dir);
    let cost = document.get("cost").expect("cost");
    assert_eq!(
        cost.get("awsMonthlyBasis"),
        Some(&Json::string("BILLING_MID"))
    );
    assert_eq!(cost.get("sourceMonthly"), Some(&Json::Int(120)));
    assert_eq!(
        cost.get("accuracy"),
        Some(&json(
            r#"{"minPercent": 30, "maxPercent": 30, "pricingSource": "CACHED"}"#
        ))
    );
}

#[test]
fn infra_takes_precedence_over_a_stale_billing_file() {
    let dir = seeded_gcp("precedence", GOLDEN_RUN_ID);
    write(
        &dir,
        "estimation-billing.json",
        r#"{"cost_comparison": {"aws_monthly_mid": 1}}"#,
    );
    emit(&dir);
    let document = plan(&dir);
    assert_eq!(
        document.get("cost").and_then(|cost| cost.get("awsMonthly")),
        Some(&Json::Int(155))
    );
}

fn full_run(label: &str, infra_band: &str, ai_band: &str) -> TempDir {
    let dir = TempDir::new(label);
    status(&dir, "GCP_TO_AWS", GOLDEN_RUN_ID);
    write(
        &dir,
        "estimation-infra.json",
        &format!(
            r#"{{"accuracy_confidence": "{infra_band}", "pricing_source": {{"status": "live"}},
              "projected_costs": {{"aws_monthly_balanced": 100.5, "breakdown": {{"compute": 100.5}}}},
              "current_costs": {{"gcp_monthly": 400}}}}"#
        ),
    );
    write(
        &dir,
        "estimation-ai.json",
        &format!(
            r#"{{"accuracy_confidence": "{ai_band}", "pricing_source": "live",
              "cost_comparison": {{"projected_bedrock_monthly": 50, "gcp_monthly_ai_spend": 70}}}}"#
        ),
    );
    dir
}

#[test]
fn infra_plus_ai_sums_and_keeps_the_looser_band() {
    let dir = full_run("full", "±5-10%", "±20-30%");
    let line = emit(&dir);
    assert!(line.ends_with("| platform=GCP | scope=FULL\n"), "{line}");
    let document = plan(&dir);
    let cost = document.get("cost").expect("cost");
    assert_eq!(cost.get("awsMonthly"), Some(&Json::Float(150.5)));
    assert_eq!(
        cost.get("awsMonthlyBasis"),
        Some(&Json::string("INFRA_PLUS_AI_SUM"))
    );
    assert_eq!(cost.get("sourceMonthly"), None, "baselines are not summed");
    assert_eq!(
        cost.get("accuracy"),
        Some(&json(
            r#"{"minPercent": 20, "maxPercent": 30, "pricingSource": "LIVE"}"#
        ))
    );
    assert_eq!(
        cost.get("awsServiceItems"),
        Some(&json(
            r#"[{"serviceName": "compute", "monthlyCost": 100.5, "classification": "INFRASTRUCTURE"},
                {"serviceName": "Amazon Bedrock", "monthlyCost": 50, "classification": "AI_ML"}]"#
        ))
    );
}

#[test]
fn billing_plus_ai_uses_the_billing_sum_basis() {
    let dir = full_run("billing-full", "±5%", "±5%");
    fs::remove_file(dir.join("estimation-infra.json")).expect("remove infra");
    write(
        &dir,
        "estimation-billing.json",
        r#"{"accuracy_confidence": "±8%", "cost_comparison": {"aws_monthly_mid": 10}}"#,
    );
    emit(&dir);
    let document = plan(&dir);
    let cost = document.get("cost").expect("cost");
    assert_eq!(
        cost.get("awsMonthlyBasis"),
        Some(&Json::string("BILLING_MID_PLUS_AI_SUM"))
    );
    assert_eq!(cost.get("awsMonthly"), Some(&Json::Int(60)));
}

#[test]
fn full_run_without_a_band_from_both_routes_is_skipped() {
    let dir = full_run("noband", "no declared band", "±5%");
    write(&dir, "plan.json", "{\"stale\": true}\n");
    let line = emit(&dir);
    assert_eq!(
        line,
        "PLAN_SKIP | reason=FULL run needs a usable accuracy band from both the base and AI estimates\n"
    );
    assert!(
        !dir.join("plan.json").exists(),
        "a deterministic skip drops the stale plan"
    );
    let dir = full_run("noband-ai", "±5%", "none");
    assert!(emit(&dir).starts_with("PLAN_SKIP | reason=FULL run needs"));
}

fn skip_reason(dir: &Path) -> String {
    let line = emit(dir);
    line.strip_prefix("PLAN_SKIP | reason=")
        .unwrap_or_else(|| panic!("expected a skip, got {line}"))
        .trim_end()
        .to_string()
}

#[test]
fn deterministic_skips_name_their_reason_and_drop_a_stale_plan() {
    let dir = TempDir::new("skips");
    assert_eq!(skip_reason(&dir), "no .phase-status.json in migration dir");

    write(&dir, ".phase-status.json", "[]");
    assert_eq!(skip_reason(&dir), ".phase-status.json is not a JSON object");

    write(&dir, ".phase-status.json", "{}");
    assert_eq!(
        skip_reason(&dir),
        "owning_skill None has no web handoff yet"
    );
    write(&dir, ".phase-status.json", r#"{"owning_skill": ["x"]}"#);
    assert_eq!(
        skip_reason(&dir),
        "owning_skill ['x'] has no web handoff yet"
    );
    write(
        &dir,
        ".phase-status.json",
        r#"{"owning_skill": "LLM_TO_BEDROCK"}"#,
    );
    assert_eq!(
        skip_reason(&dir),
        "owning_skill 'LLM_TO_BEDROCK' has no web handoff yet"
    );

    for run_id in ["null", "5", r#""   ""#, "[]"] {
        write(
            &dir,
            ".phase-status.json",
            &format!(r#"{{"owning_skill": "GCP_TO_AWS", "run_id": {run_id}}}"#),
        );
        assert_eq!(skip_reason(&dir), "no usable run_id in .phase-status.json");
    }

    status(&dir, "GCP_TO_AWS", "run-1");
    assert_eq!(skip_reason(&dir), "no cost estimate to hand off");

    write(&dir, "plan.json", "{\"stale\": true}\n");
    write(&dir, "estimation-infra.json", "[1]");
    assert_eq!(
        skip_reason(&dir),
        "estimation-infra.json is not a JSON object"
    );
    assert!(!dir.join("plan.json").exists());

    write(
        &dir,
        "estimation-infra.json",
        r#"{"projected_costs": {"aws_monthly_balanced": true}}"#,
    );
    assert_eq!(
        skip_reason(&dir),
        "no usable projected_costs.aws_monthly_balanced"
    );
    fs::remove_file(dir.join("estimation-infra.json")).expect("remove");

    write(
        &dir,
        "estimation-billing.json",
        r#"{"cost_comparison": {"aws_monthly_mid": -1}}"#,
    );
    assert_eq!(
        skip_reason(&dir),
        "no usable cost_comparison.aws_monthly_mid"
    );
    fs::remove_file(dir.join("estimation-billing.json")).expect("remove");

    write(&dir, "estimation-ai.json", r#"{"cost_comparison": {}}"#);
    assert_eq!(
        skip_reason(&dir),
        "no usable cost_comparison.projected_bedrock_monthly"
    );

    write(
        &dir,
        "estimation-ai.json",
        r#"{"cost_comparison": {"projected_bedrock_monthly": 5}}"#,
    );
    assert_eq!(
        skip_reason(&dir),
        "AI-only run has no valid web mapping yet (deferred)"
    );
}

#[test]
fn totals_over_the_usd_cap_are_skipped_at_the_cap_they_are_kept() {
    let dir = TempDir::new("cap");
    status(&dir, "GCP_TO_AWS", "run-1");
    write(
        &dir,
        "estimation-infra.json",
        r#"{"projected_costs": {"aws_monthly_balanced": 100000001}}"#,
    );
    assert_eq!(
        skip_reason(&dir),
        "total AWS monthly exceeds the supported maximum"
    );
    write(
        &dir,
        "estimation-infra.json",
        r#"{"projected_costs": {"aws_monthly_balanced": 100000000}}"#,
    );
    assert!(emit(&dir).starts_with("PLAN_OK"));
    write(
        &dir,
        "estimation-infra.json",
        r#"{"projected_costs": {"aws_monthly_balanced": 100000000.5}}"#,
    );
    assert_eq!(
        skip_reason(&dir),
        "total AWS monthly exceeds the supported maximum"
    );
    for text in ["NaN", "Infinity", "-1"] {
        write(
            &dir,
            "estimation-infra.json",
            &format!(r#"{{"projected_costs": {{"aws_monthly_balanced": {text}}}}}"#),
        );
        assert_eq!(
            skip_reason(&dir),
            "no usable projected_costs.aws_monthly_balanced"
        );
    }
}

#[test]
fn unreadable_input_fails_open_and_keeps_the_previous_plan() {
    let dir = seeded_gcp("unreadable", GOLDEN_RUN_ID);
    emit(&dir);
    let before = fs::read_to_string(dir.join("plan.json")).expect("plan");
    write(&dir, "estimation-infra.json", "{\"projected_costs\": ");
    let line = emit(&dir);
    assert_eq!(
        line,
        "PLAN_SKIP | reason=unreadable input: Expecting value: line 1 column 21 (char 20)\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("plan.json")).expect("plan"),
        before
    );

    write(&dir, ".phase-status.json", "not json");
    assert!(emit(&dir).starts_with("PLAN_SKIP | reason=unreadable input: Expecting value"));
    assert!(dir.join("plan.json").exists());
}

#[test]
fn a_directory_in_place_of_the_plan_is_a_write_skip() {
    let dir = seeded_gcp("blocked", GOLDEN_RUN_ID);
    fs::create_dir(dir.join("plan.json")).expect("block plan.json");
    let line = emit(&dir);
    assert!(
        line.starts_with("PLAN_SKIP | reason=could not write plan.json: "),
        "{line}"
    );
    let temps = fs::read_dir(&*dir)
        .expect("read dir")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json.tmp"))
        .count();
    assert_eq!(temps, 0, "the failed write removed its own temp");
}

#[test]
fn a_missing_directory_is_a_write_skip_not_a_failure() {
    let dir = TempDir::new("missing");
    let gone = dir.join("absent");
    let line = emit(&gone);
    assert_eq!(
        line,
        "PLAN_SKIP | reason=no .phase-status.json in migration dir\n"
    );
}

#[test]
fn producer_version_is_optional_and_never_fatal() {
    let dir = seeded_gcp("version", GOLDEN_RUN_ID);
    let manifest = dir.join("manifest.json");
    let arg = manifest.to_str().expect("utf-8");

    for (content, expected) in [
        (Some(r#"{"version": "1.2.3"}"#), Some("1.2.3")),
        (Some(r#"{"version": ""}"#), None),
        (Some(r#"{"version": 5}"#), None),
        (Some("[1]"), None),
        (Some("{broken"), None),
        (None, None),
    ] {
        if let Some(text) = content {
            fs::write(&manifest, text).expect("write manifest");
        } else {
            let _ = fs::remove_file(&manifest);
        }
        assert!(emit_with(&dir, &["--plugin-json", arg]).starts_with("PLAN_OK"));
        assert_eq!(
            plan(&dir).get("producerVersion").and_then(Json::as_str),
            expected,
            "{content:?}"
        );
    }
}

#[test]
fn orphan_temps_older_than_an_hour_are_swept_on_every_run() {
    let dir = TempDir::new("sweep");
    let old = dir.join(".plan-old.json.tmp");
    let fresh = dir.join(".plan-fresh.json.tmp");
    let other = dir.join(".plan-old.json");
    for path in [&old, &fresh, &other] {
        fs::write(path, "x").expect("write temp");
    }
    let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
    for path in [&old, &other] {
        fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(two_hours_ago)
            .expect("set mtime");
    }
    // The run itself fails open (no status file); the sweep still happens.
    assert_eq!(skip_reason(&dir), "no .phase-status.json in migration dir");
    assert!(!old.exists(), "an old orphan temp is reclaimed");
    assert!(fresh.exists(), "a recent temp may belong to a live writer");
    assert!(other.exists(), "only the temp naming is swept");
}

#[test]
fn missing_migration_dir_argument_is_a_usage_error() {
    let (code, out, err) = exec(&["startup", "emit-plan"]);
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.contains("--migration-dir"), "{err}");
}

#[test]
fn displayed_paths_are_normalised_like_pathlib() {
    assert_eq!(normalized("a/./b//c/"), PathBuf::from("a/b/c"));
    assert_eq!(normalized("./"), PathBuf::from("."));
    assert_eq!(normalized("/x/./y"), PathBuf::from("/x/y"));
    let dir = seeded_gcp("normal", GOLDEN_RUN_ID);
    let spelled = format!("{}/./", dir.display());
    let line = emit(Path::new(&spelled));
    assert_eq!(
        line,
        format!(
            "PLAN_OK | path={}/plan.json | platform=GCP | scope=INFRA_ONLY\n",
            dir.display()
        )
    );
}

#[test]
fn integer_sums_overflow_into_floats_and_are_capped() {
    let big = i64::MAX;
    let dir = TempDir::new("overflow");
    status(&dir, "GCP_TO_AWS", "run-1");
    write(
        &dir,
        "estimation-infra.json",
        &format!(
            r#"{{"accuracy_confidence": "±5%", "projected_costs": {{"aws_monthly_balanced": {big}}}}}"#
        ),
    );
    write(
        &dir,
        "estimation-ai.json",
        &format!(
            r#"{{"accuracy_confidence": "±5%", "cost_comparison": {{"projected_bedrock_monthly": {big}}}}}"#
        ),
    );
    assert_eq!(
        skip_reason(&dir),
        "total AWS monthly exceeds the supported maximum"
    );
}
