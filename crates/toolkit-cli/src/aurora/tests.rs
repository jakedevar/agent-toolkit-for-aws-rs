//! Offline tests for the Aurora analyzers.
//!
//! Every remote read goes through a mock [`AwsApi`] with canned payloads, so
//! nothing here touches AWS or the network. Expected numbers were derived from
//! the Python scripts' formulas by hand (each case notes its arithmetic).

use std::cell::RefCell;
use std::ffi::OsString;

use toolkit_core::{parse_json, Json, ToolkitError};

use super::acu::{
    calculate_costs, estimate_acu, family_ratio, format_table, recommend_min_max, refresh_pricing,
    round_up_to_half, run_estimate, AcuPricing, CostInput, EstimateInput,
};
use super::api::{
    parse_bulk_csv, parse_csv_rows, ApiResult, AwsApi, ClusterInfo, Ec2Type, NoAws, NO_AWS,
};
use super::args::{parse_py_float, parse_py_int};
use super::commitment::{
    analyze_cluster_live as commitment_cluster, best_dsp, best_ri, build_comparison,
    family_from_instance, fetch_dsp_rates, fetch_ri_offerings, format_cluster, format_fleet,
    DspRate, Workload,
};
use super::io::{
    analyze_cluster_live as io_cluster, analyze_offline, compute_comparison, data_quality_tag,
    fleet_summary, lookback_window, refresh_pricing as io_refresh, IoPricing,
};
use super::pyfmt::{dumps, fmt_f, fmt_grouped, round_to, Num};
use super::{acu_regions, location, static_price, Engine, Tool};
use crate::{run, Invocation, Output};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn json(text: &str) -> Json {
    parse_json(text).expect("valid test JSON")
}

fn at<'a>(value: &'a Json, path: &str) -> &'a Json {
    path.split('.').fold(value, |current, key| {
        let found = match current {
            Json::Array(items) => key.parse::<usize>().ok().and_then(|index| items.get(index)),
            other => other.get(key),
        };
        found.unwrap_or_else(|| panic!("missing '{key}' in path '{path}'"))
    })
}

fn num(value: &Json, path: &str) -> f64 {
    match at(value, path) {
        Json::Float(number) => *number,
        Json::Int(number) => *number as f64,
        other => panic!("'{path}' is not a number: {other:?}"),
    }
}

fn text<'a>(value: &'a Json, path: &str) -> &'a str {
    at(value, path)
        .as_str()
        .unwrap_or_else(|| panic!("'{path}' is not a string"))
}

fn is_int(value: &Json, path: &str) -> bool {
    matches!(at(value, path), Json::Int(_))
}

fn cli(args: &[&str]) -> (u8, String, String) {
    let os: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = run(&os, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn invocation(positionals: &[&str], flags: &[&str], values: &[(&str, &str)]) -> Invocation {
    Invocation {
        positionals: positionals.iter().map(|word| (*word).to_string()).collect(),
        flags: flags.iter().map(|word| (*word).to_string()).collect(),
        values: values
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
    }
}

/// Runs a handler with captured stdout and stderr.
fn capture(
    handler: impl FnOnce(&mut Output<'_>) -> Result<(), ToolkitError>,
) -> (Result<(), ToolkitError>, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = {
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        handler(&mut output).and_then(|()| output.finish())
    };
    (
        result,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn price_item(instance_type: &str, usage: &str, usd: &str) -> String {
    format!(
        r#"{{"product":{{"attributes":{{"instanceType":"{instance_type}","usagetype":"{usage}"}}}},"terms":{{"OnDemand":{{"T1":{{"priceDimensions":{{"D1":{{"pricePerUnit":{{"USD":"{usd}"}}}}}}}}}}}}}}"#
    )
}

type ProductRule = (Vec<(&'static str, &'static str)>, ApiResult<Vec<String>>);

struct Mock {
    available: bool,
    products: Vec<ProductRule>,
    csv: ApiResult<String>,
    orderable: ApiResult<Vec<String>>,
    ec2: Vec<(&'static str, i64, f64)>,
    ec2_fail_multi: bool,
    offerings: ApiResult<Vec<Json>>,
    rates: ApiResult<Vec<Json>>,
    clusters: Vec<(String, String)>,
    described: Vec<(String, ClusterInfo)>,
    classes: Vec<(String, ApiResult<Vec<String>>)>,
    metrics: Vec<(&'static str, &'static str, Vec<f64>)>,
    calls: RefCell<Vec<String>>,
}

impl Mock {
    fn new(available: bool) -> Self {
        let missing = || "not configured".to_string();
        Self {
            available,
            products: Vec::new(),
            csv: Err(missing()),
            orderable: Err(missing()),
            ec2: Vec::new(),
            ec2_fail_multi: false,
            offerings: Err(missing()),
            rates: Err(missing()),
            clusters: Vec::new(),
            described: Vec::new(),
            classes: Vec::new(),
            metrics: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn note(&self, call: String) {
        self.calls.borrow_mut().push(call);
    }
}

impl AwsApi for Mock {
    fn available(&self) -> bool {
        self.available
    }

    fn pricing_products(&self, filters: &[(&str, &str)]) -> ApiResult<Vec<String>> {
        self.note(format!("products:{filters:?}"));
        self.products
            .iter()
            .find(|(wanted, _)| wanted.iter().all(|pair| filters.contains(pair)))
            .map_or_else(
                || Err("no products configured".to_string()),
                |(_, reply)| reply.clone(),
            )
    }

    fn bulk_pricing_csv(&self, region: &str) -> ApiResult<String> {
        self.note(format!("csv:{region}"));
        self.csv.clone()
    }

    fn orderable_instance_classes(&self, region: &str, engine: &str) -> ApiResult<Vec<String>> {
        self.note(format!("orderable:{region}:{engine}"));
        self.orderable.clone()
    }

    fn ec2_instance_types(&self, region: &str, names: &[String]) -> ApiResult<Vec<Ec2Type>> {
        self.note(format!("ec2:{region}:{}", names.len()));
        if self.ec2_fail_multi && names.len() > 1 {
            return Err("batch rejected".to_string());
        }
        Ok(names
            .iter()
            .filter_map(|name| {
                self.ec2.iter().find(|entry| entry.0 == name.as_str()).map(
                    |(known, vcpus, memory)| Ec2Type {
                        name: (*known).to_string(),
                        vcpus: *vcpus,
                        memory_mib: *memory,
                    },
                )
            })
            .collect())
    }

    fn reserved_offerings(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
    ) -> ApiResult<Vec<Json>> {
        self.note(format!("offerings:{region}:{instance_type}:{engine}"));
        self.offerings.clone()
    }

    fn savings_plan_rates(&self, region: &str, product_description: &str) -> ApiResult<Vec<Json>> {
        self.note(format!("rates:{region}:{product_description}"));
        self.rates.clone()
    }

    fn list_clusters(&self, region: &str) -> ApiResult<Vec<(String, String)>> {
        self.note(format!("list:{region}"));
        Ok(self.clusters.clone())
    }

    fn describe_cluster(&self, region: &str, cluster_id: &str) -> ApiResult<Option<ClusterInfo>> {
        self.note(format!("describe:{region}:{cluster_id}"));
        if cluster_id == "boom" {
            return Err("AccessDenied".to_string());
        }
        Ok(self
            .described
            .iter()
            .find(|(id, _)| id == cluster_id)
            .map(|(_, info)| info.clone()))
    }

    fn instance_classes(&self, _region: &str, instance_id: &str) -> ApiResult<Vec<String>> {
        self.classes
            .iter()
            .find(|(id, _)| id == instance_id)
            .map_or_else(
                || Err("DBInstanceNotFound".to_string()),
                |(_, reply)| reply.clone(),
            )
    }

    fn metric_values(
        &self,
        _region: &str,
        cluster_id: &str,
        metric: &str,
        statistic: &str,
        window: (i64, i64),
    ) -> ApiResult<Vec<f64>> {
        self.note(format!(
            "metric:{cluster_id}:{metric}:{statistic}:{}:{}",
            window.0, window.1
        ));
        Ok(self
            .metrics
            .iter()
            .find(|(name, stat, _)| *name == metric && *stat == statistic)
            .map(|(_, _, values)| values.clone())
            .unwrap_or_default())
    }
}

fn cluster(storage: &str, engine: &str, members: &[&str]) -> ClusterInfo {
    ClusterInfo {
        storage_type: storage.to_string(),
        engine: engine.to_string(),
        engine_version: "8.0.mysql_aurora.3.05.2".to_string(),
        members: members.iter().map(|member| (*member).to_string()).collect(),
    }
}

// ---------------------------------------------------------------------------
// Python formatting layer
// ---------------------------------------------------------------------------

#[test]
fn round_matches_python_half_even_on_the_exact_binary_value() {
    // 0.125 is exact in binary, so the tie goes to the even digit.
    assert_eq!(round_to(0.125, 2), 0.12);
    assert_eq!(round_to(0.375, 2), 0.38);
    // 2.675 is stored as 2.67499999999999982236431605997495353221893310546875.
    assert_eq!(round_to(2.675, 2), 2.67);
    assert_eq!(round_to(-146.730_000_000_000_02, 2), -146.73);
    assert!(round_to(f64::NAN, 2).is_nan());
    assert!(round_to(f64::INFINITY, 2).is_infinite());
    assert!(
        round_to(-0.001, 2).is_sign_negative(),
        "round(-0.001, 2) is -0.0"
    );
}

#[test]
fn fixed_point_formatting_matches_python() {
    assert_eq!(fmt_f(837.5, 0), "838", "ties round to even");
    assert_eq!(fmt_f(2.5, 0), "2");
    assert_eq!(fmt_f(0.5, 0), "0");
    assert_eq!(fmt_f(-0.4, 0), "-0");
    assert_eq!(fmt_f(f64::NAN, 1), "nan");
    assert_eq!(fmt_f(f64::NEG_INFINITY, 1), "-inf");
    assert_eq!(fmt_grouped(1_234_567.4, 0), "1,234,567");
    assert_eq!(fmt_grouped(-1234.5, 0), "-1,234");
    assert_eq!(fmt_grouped(999.0, 0), "999");
    assert_eq!(fmt_grouped(1000.0, 2), "1,000.00");
    assert_eq!(fmt_grouped(f64::INFINITY, 0), "inf");
}

#[test]
fn json_dumps_matches_python_for_floats_ints_and_text() {
    let value = Json::object(vec![
        ("zero", Json::Int(0)),
        ("one", Json::Float(1.0)),
        ("big", Json::Float(1e16)),
        ("small", Json::Float(0.00001)),
        ("nan", Json::Float(f64::NAN)),
        ("inf", Json::Float(f64::INFINITY)),
        ("ninf", Json::Float(f64::NEG_INFINITY)),
        ("text", Json::string("é—\"\\\n\u{1}\u{7f}😀")),
        ("empty_list", Json::Array(vec![])),
        ("empty_map", Json::Object(vec![])),
        ("nested", Json::Array(vec![Json::Null, Json::Bool(true)])),
    ]);
    assert_eq!(
        dumps(&value),
        "{\n  \"zero\": 0,\n  \"one\": 1.0,\n  \"big\": 1e+16,\n  \"small\": 1e-05,\n  \
         \"nan\": NaN,\n  \"inf\": Infinity,\n  \"ninf\": -Infinity,\n  \
         \"text\": \"\\u00e9\\u2014\\\"\\\\\\n\\u0001\\u007f\\ud83d\\ude00\",\n  \
         \"empty_list\": [],\n  \"empty_map\": {},\n  \"nested\": [\n    null,\n    true\n  ]\n}"
    );
}

#[test]
fn numbers_keep_the_python_int_float_distinction() {
    assert_eq!(Num::Int(0).py_str(), "0");
    assert_eq!(Num::Float(0.0).py_str(), "0.0");
    assert_eq!(Num::Int(1).plus(Num::Int(2)), Num::Int(3));
    assert_eq!(Num::Int(1).plus(Num::Float(0.5)), Num::Float(1.5));
    assert_eq!(
        Num::Int(i64::MAX).plus(Num::Int(1)),
        Num::Float(i64::MAX as f64 + 1.0)
    );
    // Python min/max keep the first operand on a tie.
    assert_eq!(
        super::pyfmt::py_min(Num::Float(256.0), Num::Int(256)),
        Num::Float(256.0)
    );
    assert_eq!(
        super::pyfmt::py_min(Num::Float(300.0), Num::Int(256)),
        Num::Int(256)
    );
    assert_eq!(
        super::pyfmt::py_max(Num::Float(0.5), Num::Int(256)),
        Num::Int(256)
    );
    assert_eq!(
        super::pyfmt::title_case("partial_live".replace('_', " ").as_str()),
        "Partial Live"
    );
}

#[test]
fn argparse_numbers_follow_python() {
    assert_eq!(parse_py_float(" 1e3 "), Some(1000.0));
    assert_eq!(parse_py_float("1_000.5"), Some(1000.5));
    assert_eq!(parse_py_float("-.5"), Some(-0.5));
    assert!(parse_py_float("inf").is_some_and(f64::is_infinite));
    assert!(parse_py_float("nan").is_some_and(f64::is_nan));
    for bad in ["", "abc", "1__0", "_1", "1_", "1e", "0x10", "1,5"] {
        assert_eq!(parse_py_float(bad), None, "{bad:?}");
    }
    assert_eq!(parse_py_int(" +42 "), Some(42));
    assert_eq!(parse_py_int("1_000"), Some(1000));
    assert_eq!(parse_py_int("-0"), Some(0));
    for bad in ["", "1.5", "1e3", "9223372036854775808", "1__0", "--1"] {
        assert_eq!(parse_py_int(bad), None, "{bad:?}");
    }
}

// ---------------------------------------------------------------------------
// ACU calculator
// ---------------------------------------------------------------------------

#[test]
fn acu_rounding_boundaries() {
    assert_eq!(round_up_to_half(0.0).unwrap(), 0.0);
    assert_eq!(round_up_to_half(0.5).unwrap(), 0.5);
    assert_eq!(round_up_to_half(0.51).unwrap(), 1.0);
    assert_eq!(round_up_to_half(1.0).unwrap(), 1.0);
    assert_eq!(round_up_to_half(2.2).unwrap(), 2.5);
    assert_eq!(round_up_to_half(255.9).unwrap(), 256.0);
    let negative = round_up_to_half(-0.3).unwrap();
    assert_eq!(negative, 0.0);
    assert!(
        negative.is_sign_positive(),
        "Python's int ceiling has no -0"
    );
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        assert!(round_up_to_half(bad).is_err(), "{bad}");
    }
    assert_eq!(
        round_up_to_half(f64::INFINITY).unwrap_err().message(),
        "cannot convert float infinity to integer"
    );
    assert_eq!(
        round_up_to_half(f64::NAN).unwrap_err().message(),
        "cannot convert float NaN to integer"
    );
}

#[test]
fn family_ratio_follows_the_table_and_defaults_to_four() {
    for (instance, ratio) in [
        ("db.r6g.large", 4),
        ("db.x2g.large", 4),
        ("db.m5.large", 2),
        ("db.t3.medium", 2),
        ("db.c5.large", 1),
        ("db.z1d.large", 4),
        ("db.R6g.large", 4),
        ("r6g.large", 4),
        ("db.", 4),
        ("", 4),
    ] {
        assert_eq!(family_ratio(instance), ratio, "{instance:?}");
    }
}

fn estimate_input(instance: &str, p95: f64, max: f64) -> EstimateInput {
    EstimateInput {
        instance: instance.to_string(),
        cpu_p95: p95,
        cpu_max: max,
        cpu_avg: Num::Int(0),
        connections: 0,
        working_set: Num::Int(0),
        storage: Num::Int(10),
        num_instances: 1,
    }
}

#[test]
fn estimate_matches_the_python_formulas_for_the_documented_example() {
    // db.r6g.xlarge: 4 vCPU, ratio 4. weighted = (35*.95+72*.05)/100 = .3685,
    // raw typical = 5.896 -> 6.0; raw peak = 11.52 -> 12.0; average falls back
    // to 60% of P95 = 3.36 -> 3.5; max = ceil(15.6*2)/2 = 16.0.
    let mut input = estimate_input("db.r6g.xlarge", 35.0, 72.0);
    input.storage = Num::Float(500.0);
    let result = run_estimate(&AcuPricing::static_defaults(), &input).unwrap();

    assert_eq!(num(&result, "acu_settings.typical_acu"), 6.0);
    assert_eq!(num(&result, "acu_settings.peak_acu"), 12.0);
    assert_eq!(num(&result, "acu_settings.avg_acu"), 3.5);
    assert_eq!(num(&result, "acu_settings.recommended_min"), 3.5);
    assert_eq!(num(&result, "acu_settings.recommended_max"), 16.0);
    assert!(
        is_int(&result, "acu_settings.memory_floor_acu"),
        "int 0 when unset"
    );
    assert_eq!(
        at(&result, "acu_settings.exceeds_capacity"),
        &Json::Bool(false)
    );
    assert_eq!(at(&result, "acu_settings.memory_advisory"), &Json::Null);

    // Provisioned 0.519*730 = 378.87 + 500*0.10; serverless 6*0.12*730 = 525.6.
    assert_eq!(
        num(&result, "cost_comparison.provisioned.compute_monthly"),
        378.87
    );
    assert_eq!(
        num(&result, "cost_comparison.provisioned.storage_monthly"),
        50.0
    );
    assert_eq!(
        num(&result, "cost_comparison.provisioned.total_monthly"),
        428.87
    );
    assert_eq!(
        num(&result, "cost_comparison.serverless.compute_monthly"),
        525.6
    );
    assert_eq!(
        num(&result, "cost_comparison.serverless.total_monthly"),
        575.6
    );
    assert_eq!(
        num(&result, "cost_comparison.serverless.cost_range.low"),
        356.6
    );
    assert_eq!(
        num(&result, "cost_comparison.serverless.cost_range.high"),
        1451.6
    );
    assert_eq!(num(&result, "cost_comparison.savings_monthly"), -146.73);
    assert_eq!(num(&result, "cost_comparison.savings_pct"), -34.2);
    assert_eq!(
        text(&result, "cost_comparison.recommendation"),
        "not_recommended"
    );
    assert_eq!(
        text(&result, "cost_comparison.reason"),
        "Serverless at $576/mo is 34% more expensive than provisioned at $429/mo. Sustained \
         workloads are cheaper on provisioned instances."
    );
}

#[test]
fn estimate_json_echoes_python_int_and_float_inputs() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "acu",
        "estimate",
        "--engine",
        "mysql",
        "--instance",
        "db.r6g.xlarge",
        "--cpu-p95",
        "35",
        "--cpu-max",
        "72",
        "--storage",
        "500",
        "--offline",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(
        stdout.starts_with(
            "{\n  \"input\": {\n    \"instance_type\": \"db.r6g.xlarge\",\n    \"vcpus\": 4,\n    \
             \"memory_gib\": 32,\n    \"cpu_p95\": 35.0,\n    \"cpu_max\": 72.0,\n    \
             \"cpu_avg\": 0,\n    \"connections\": 0,\n    \"working_set_gib\": 0,\n    \
             \"storage_gib\": 500.0,\n    \"num_instances\": 1\n  },\n  \"acu_settings\": {\n    \
             \"recommended_min\": 3.5,\n    \"recommended_max\": 16.0,\n    \
             \"typical_acu\": 6.0,\n"
        ),
        "{stdout}"
    );
    assert!(stdout.ends_with("    \"details\": \"Built-in us-east-1 defaults\"\n  }\n}\n"));

    // Explicit zero values are floats, defaults are ints.
    let (status, stdout, _) = cli(&[
        "aurora",
        "acu",
        "estimate",
        "--engine",
        "postgresql",
        "--instance",
        "db.r6g.xlarge",
        "--cpu-p95",
        "35",
        "--cpu-max",
        "72",
        "--cpu-avg",
        "0",
        "--working-set",
        "0",
        "--storage",
        "10",
        "--offline",
    ]);
    assert_eq!(status, 0);
    assert!(stdout.contains("\"cpu_avg\": 0.0,"));
    assert!(stdout.contains("\"working_set_gib\": 0.0,"));
    assert!(stdout.contains("\"storage_gib\": 10.0,"));
}

#[test]
fn estimate_connection_and_memory_floors_raise_the_minimum() {
    // 1000 connections * 10 MB = 9.765625 GiB -> /2 -> ceil(9.77)/2 = 5.0 ACU;
    // 12 GiB working set -> 6.0 ACU, above the 5.0 minimum, so an advisory.
    let mut input = estimate_input("db.r6g.xlarge", 35.0, 72.0);
    input.connections = 1000;
    input.working_set = Num::Float(12.0);
    let result = run_estimate(&AcuPricing::static_defaults(), &input).unwrap();
    assert_eq!(num(&result, "acu_settings.connection_floor_acu"), 5.0);
    assert_eq!(num(&result, "acu_settings.memory_floor_acu"), 6.0);
    assert_eq!(num(&result, "acu_settings.recommended_min"), 5.0);
    assert_eq!(
        text(&result, "acu_settings.memory_advisory"),
        "Working set needs 6.0 ACU (12.0 GiB / 2.0 GiB per ACU). Your min ACU (5.0) is below \
         this. Setting min to 6.0 ACU keeps the working set cached and avoids cold-cache I/O \
         penalties on scale-up. Trade-off: higher baseline cost."
    );

    // A working set at or below the minimum gives no advisory.
    let mut input = estimate_input("db.r6g.xlarge", 35.0, 72.0);
    input.working_set = Num::Float(7.0);
    let result = run_estimate(&AcuPricing::static_defaults(), &input).unwrap();
    assert_eq!(at(&result, "acu_settings.memory_advisory"), &Json::Null);
}

#[test]
fn estimate_above_the_serverless_ceiling_keeps_integer_caps() {
    // 192 vCPU * 4 at 100% CPU = 768 ACU raw: typical and max are capped at the
    // integer 256, the baseline (461 ACU) exceeds capacity.
    let result = run_estimate(
        &AcuPricing::static_defaults(),
        &estimate_input("db.r8g.48xlarge", 100.0, 100.0),
    )
    .unwrap();
    assert!(is_int(&result, "acu_settings.typical_acu"));
    assert!(is_int(&result, "acu_settings.recommended_max"));
    assert!(is_int(&result, "acu_settings.recommended_min"));
    assert_eq!(num(&result, "acu_settings.typical_acu"), 256.0);
    assert_eq!(num(&result, "acu_settings.avg_acu"), 461.0);
    assert_eq!(
        at(&result, "acu_settings.exceeds_capacity"),
        &Json::Bool(true)
    );
    assert_eq!(
        text(&result, "cost_comparison.recommendation"),
        "not_recommended"
    );
    assert!(text(&result, "cost_comparison.reason")
        .starts_with("Workload's baseline/peak demand exceeds the 256 ACU serverless maximum."));
}

#[test]
fn estimate_exactly_at_the_ceiling_is_not_over_capacity() {
    // 64 vCPU * 4 at 100% = 256 raw: not above the ceiling, typical stays a float.
    let estimate = estimate_acu(100.0, 100.0, 64, "db.r6g.16xlarge", 0.0).unwrap();
    assert_eq!(estimate.raw_typical, 256.0);
    assert!(!estimate.exceeds_capacity);
    assert_eq!(estimate.typical, Num::Float(256.0));
    let over = estimate_acu(100.0, 100.5, 64, "db.r6g.16xlarge", 0.0).unwrap();
    assert!(over.exceeds_capacity);
}

#[test]
fn estimate_minimum_never_exceeds_maximum() {
    // A huge connection floor is capped by the maximum, and flagged.
    let estimate = estimate_acu(10.0, 20.0, 2, "db.t3.medium", 0.0).unwrap();
    let min_max = recommend_min_max(&estimate, 100_000, Num::Int(0)).unwrap();
    assert!(min_max.recommended_min.value() <= min_max.recommended_max.value());
    assert!(min_max.exceeds_capacity);
}

#[test]
fn estimate_handles_huge_connection_counts_without_overflow() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "acu",
        "estimate",
        "--engine",
        "mysql",
        "--instance",
        "db.t3.medium",
        "--cpu-p95",
        "10",
        "--cpu-max",
        "20",
        "--connections",
        "9223372036854775807",
        "--offline",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.contains("\"connections\": 9223372036854775807,"));
    assert!(stdout.contains("\"exceeds_capacity\": true"));

    let (status, _, stderr) = cli(&[
        "aurora",
        "acu",
        "estimate",
        "--engine",
        "mysql",
        "--instance",
        "db.t3.medium",
        "--cpu-p95",
        "10",
        "--cpu-max",
        "20",
        "--connections",
        "9223372036854775808",
    ]);
    assert_eq!(status, 2);
    assert!(stderr
        .starts_with("error: argument --connections: invalid int value: '9223372036854775808'"));
}

#[test]
fn estimate_non_finite_cpu_is_a_runtime_error() {
    for value in ["inf", "nan"] {
        let (status, stdout, stderr) = cli(&[
            "aurora",
            "acu",
            "estimate",
            "--engine",
            "mysql",
            "--instance",
            "db.t3.medium",
            "--cpu-p95",
            value,
            "--cpu-max",
            "20",
            "--offline",
        ]);
        assert_eq!(status, 1, "{value}");
        assert_eq!(stdout, "");
        assert!(
            stderr.starts_with("error: cannot convert float "),
            "{stderr}"
        );
        assert!(!stderr.contains("Usage:"));
    }
}

#[test]
fn costs_use_an_integer_zero_when_there_is_no_provisioned_cost() {
    // No instances and no storage: provisioned cost is 0, so the percentage is
    // the integer 0 (Python's `else 0`) and the savings are negative.
    let pricing = AcuPricing::static_defaults();
    let costs = calculate_costs(
        &pricing,
        &CostInput {
            typical_acu: Num::Float(1.0),
            min_acu: Num::Float(0.5),
            max_acu: Num::Float(2.0),
            storage_gib: Num::Int(0),
            provisioned_instance: "db.r6g.large",
            num_instances: 0,
            exceeds_capacity: false,
        },
    )
    .unwrap();
    assert!(is_int(&costs, "savings_pct"));
    assert_eq!(num(&costs, "savings_monthly"), -87.6);
    assert_eq!(text(&costs, "recommendation"), "consider");
    assert!(text(&costs, "reason").starts_with("Similar cost ($88 vs $0/mo)."));
}

#[test]
fn recommendation_thresholds_have_exact_boundaries() {
    // Serverless 0.5 ACU: 0.5*0.12*730 = 43.8/mo, no storage. Choose the
    // provisioned side so savings_pct lands on each boundary.
    let mut pricing = AcuPricing::static_defaults();
    let cost = |pricing: &AcuPricing, exceeds: bool| {
        calculate_costs(
            pricing,
            &CostInput {
                typical_acu: Num::Float(0.5),
                min_acu: Num::Float(0.5),
                max_acu: Num::Float(0.5),
                storage_gib: Num::Int(0),
                provisioned_instance: "db.t3.medium",
                num_instances: 1,
                exceeds_capacity: exceeds,
            },
        )
        .unwrap()
    };
    // t3.medium 0.082*730 = 59.86: savings 16.06 = 26.8% and sv_high <= 2x.
    let result = cost(&pricing, false);
    assert_eq!(text(&result, "recommendation"), "recommended");
    assert_eq!(
        text(&result, "reason"),
        "Serverless saves $16/mo (27%) vs provisioned. Cost range: $44–$44/mo."
    );
    // Exceeding capacity wins over everything else.
    assert_eq!(
        text(&cost(&pricing, true), "recommendation"),
        "not_recommended"
    );
    // Provisioned price equal to serverless: 0% -> "consider" (similar cost).
    pricing.instances.get_mut("db.t3.medium").unwrap().price = 43.8 / 730.0;
    assert_eq!(text(&cost(&pricing, false), "recommendation"), "consider");
    // Serverless 20% dearer: -25% -> more_expensive.
    pricing.instances.get_mut("db.t3.medium").unwrap().price = 43.8 / 1.25 / 730.0;
    assert_eq!(
        text(&cost(&pricing, false), "recommendation"),
        "more_expensive"
    );
    // Serverless twice as expensive: -100% -> not_recommended.
    pricing.instances.get_mut("db.t3.medium").unwrap().price = 43.8 / 2.0 / 730.0;
    assert_eq!(
        text(&cost(&pricing, false), "recommendation"),
        "not_recommended"
    );
    // A cheap average with a costly peak: savings > 10% but sv_high > 2x total.
    let spiky = calculate_costs(
        &AcuPricing::static_defaults(),
        &CostInput {
            typical_acu: Num::Float(0.5),
            min_acu: Num::Float(0.5),
            max_acu: Num::Float(256.0),
            storage_gib: Num::Int(0),
            provisioned_instance: "db.t3.medium",
            num_instances: 1,
            exceeds_capacity: false,
        },
    )
    .unwrap();
    assert_eq!(text(&spiky, "recommendation"), "consider");
    assert!(text(&spiky, "reason").starts_with("Typical cost is lower ($44 vs $60/mo)"));
}

#[test]
fn unknown_instance_type_lists_the_supported_classes() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "acu",
        "estimate",
        "--engine",
        "mysql",
        "--instance",
        "db.nope.large",
        "--cpu-p95",
        "10",
        "--cpu-max",
        "20",
        "--offline",
    ]);
    assert_eq!(status, 1);
    assert_eq!(stdout, "");
    assert!(stderr
        .starts_with("error: Unknown instance type: db.nope.large. Supported: db.r5.12xlarge, "));
    assert!(stderr.trim_end().ends_with("db.t4g.medium"));
}

#[test]
fn estimate_table_matches_the_python_layout() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "acu",
        "estimate",
        "--engine",
        "mysql",
        "--instance",
        "db.r6g.xlarge",
        "--cpu-p95",
        "35",
        "--cpu-max",
        "72",
        "--storage",
        "500",
        "--format",
        "table",
        "--offline",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    let rule = "=".repeat(65);
    let expected = format!(
        "{rule}\n  Aurora Serverless v2 — ACU Estimate & Cost Comparison\n  Pricing: Static \
         Fallback (us-east-1)\n{rule}\n\n  ACU Configuration\n  {dash}\n  Recommended Min ACU:  \
         3.5\n  Recommended Max ACU:  16.0\n  Typical ACU:          6.0\n  Peak ACU:             \
         12.0\n\n  Monthly Cost Comparison\n  {dash}\n  {blank:30} {prov:>14} {sv:>14}\n  \
         {compute:30} {c1:>14} {c2:>14}\n  {storage:30} {s1:>14} {s2:>14}\n  {total:30} {t1:>14} \
         {t2:>14}\n\n  Serverless cost range: $357 – $1452/mo\n  Savings: $-147/mo (-34%)\n\n  \
         Recommendation: NOT_RECOMMENDED\n  Serverless at $576/mo is 34% more expensive than \
         provisioned at $429/mo. Sustained workloads are cheaper on provisioned instances.\n\n\
         {rule}\n",
        dash = "-".repeat(45),
        blank = "",
        prov = "Provisioned",
        sv = "Serverless",
        compute = "Compute",
        c1 = "$378.87",
        c2 = "$525.6",
        storage = "Storage",
        s1 = "$50.0",
        s2 = "$50.0",
        total = "Total",
        t1 = "$428.87",
        t2 = "$575.6",
    );
    assert_eq!(stdout, expected);
}

#[test]
fn estimate_table_shows_floors_and_the_advisory() {
    let mut input = estimate_input("db.r6g.xlarge", 35.0, 72.0);
    input.connections = 1000;
    input.working_set = Num::Float(12.0);
    let result = run_estimate(&AcuPricing::static_defaults(), &input).unwrap();
    let table = format_table(&result);
    assert!(table.contains("  Connection floor:     5.0 ACU\n"));
    assert!(table.contains("  Memory floor:         6.0 ACU (advisory)\n"));
    assert!(table.contains("  NOTE: Working set needs 6.0 ACU"));
}

#[test]
fn list_instances_and_pricing_source_offline() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "acu",
        "list-instances",
        "--engine",
        "mysql",
        "--offline",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "Pricing source: static_fallback (us-east-1)");
    assert_eq!(
        lines[1],
        format!(
            "{:<25} {:>6} {:>8} {:>8} {:>10}",
            "Instance Type", "vCPUs", "Memory", "$/hr", "$/mo"
        )
    );
    assert_eq!(lines[2], "-".repeat(62));
    assert_eq!(
        lines[3],
        format!(
            "{:<25} {:>6} {:>6} GiB {:>8} {:>10}",
            "db.r5.12xlarge", 48, "384", "6.960", "5080.80"
        )
    );
    assert!(lines.contains(
        &format!(
            "{:<25} {:>6} {:>6} GiB {:>8} {:>10}",
            "db.r6g.xlarge", 4, "32", "0.519", "378.87"
        )
        .as_str()
    ));
    assert_eq!(lines[lines.len() - 2], "");
    assert_eq!(lines[lines.len() - 1], "Total: 35 instance types");

    let (status, stdout, stderr) = cli(&[
        "aurora",
        "acu",
        "pricing-source",
        "--engine",
        "postgresql",
        "--offline",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "{\n  \"source\": \"static_fallback\",\n  \"region\": \"us-east-1\",\n  \
         \"last_updated\": \"2026-03-28\",\n  \"details\": \"Built-in us-east-1 defaults\"\n}\n"
    );
}

#[test]
fn live_pricing_without_an_sdk_falls_back_with_the_errors_listed() {
    let pricing = refresh_pricing(&NoAws, Engine::Mysql, "us-east-1");
    assert_eq!(text(&pricing.source, "source"), "static_fallback");
    assert_eq!(text(&pricing.source, "last_updated"), "2026-03-28");
    assert_eq!(num(&pricing.source, "instance_count"), 35.0);
    assert_eq!(num(&pricing.source, "acu_price_standard"), 0.12);
    assert_eq!(num(&pricing.source, "storage_price_standard"), 0.1);
    assert_eq!(
        at(&pricing.source, "errors"),
        &json(&format!(
            "[\"Instance catalog: {NO_AWS}\",\"Instance pricing (API): {NO_AWS}\",\
         \"Instance pricing (bulk CSV): {NO_AWS}\",\"ACU/storage pricing: {NO_AWS}\"]"
        ))
    );
    assert!(text(&pricing.source, "details").starts_with(
        "Static fallback (us-east-1, 2026-03-28). Live fetch failed: Instance catalog: "
    ));

    // The command prints a status line on stderr, but not for pricing-source.
    let (status, _, stderr) = cli(&["aurora", "acu", "list-instances", "--engine", "mysql"]);
    assert_eq!(status, 0);
    assert_eq!(
        stderr,
        "[Pricing: STATIC — us-east-1, 35 instances, ACU=$0.12/hr]\n"
    );
    let (status, _, stderr) = cli(&["aurora", "acu", "pricing-source", "--engine", "mysql"]);
    assert_eq!((status, stderr.as_str()), (0, ""));
}

fn live_acu_mock() -> Mock {
    let mut mock = Mock::new(true);
    mock.products = vec![
        (
            vec![("deploymentOption", "Single-AZ")],
            Ok(vec![
                price_item("db.r6g.large", "InstanceUsage:db.r6g.large", "0.30"),
                price_item(
                    "db.r6g.xlarge",
                    "InstanceUsageIOOptimized:db.r6g.xlarge",
                    "9.9",
                ),
                price_item("cache.m5.large", "x", "1.0"),
                price_item("db.r6g.2xlarge", "InstanceUsage:db.r6g.2xlarge", "0"),
            ]),
        ),
        (
            vec![("productFamily", "Database Storage")],
            Ok(vec![
                price_item("", "USE1-Aurora:StorageUsage", "0.11"),
                price_item("", "USE1-Aurora:IOOptimized-StorageUsage", "0.25"),
                price_item("", "USE1-Aurora:StorageIOUsage", "0.0000002"),
            ]),
        ),
        (
            vec![("termType", "OnDemand")],
            Ok(vec![
                price_item("", "USE1-Aurora:ServerlessV2Usage", "0.14"),
                price_item("", "USE1-Aurora:ServerlessV2IOOptimizedUsage", "0.18"),
            ]),
        ),
    ];
    mock.orderable = Ok(vec![
        "db.r6g.large".to_string(),
        "db.r6g.xlarge".to_string(),
        "db.serverless".to_string(),
        "db.unknown.large".to_string(),
    ]);
    mock.ec2 = vec![("r6g.large", 2, 16384.0), ("r6g.xlarge", 4, 32768.0)];
    mock
}

#[test]
fn live_pricing_overlays_the_catalog_prices_and_acu_rates() {
    let mock = live_acu_mock();
    let pricing = refresh_pricing(&mock, Engine::Postgresql, "eu-west-1");
    assert_eq!(text(&pricing.source, "source"), "live");
    assert_eq!(text(&pricing.source, "last_updated"), "now");
    assert_eq!(
        text(&pricing.source, "details"),
        "Live AWS APIs (eu-west-1): 2 instance types, 1 prices, ACU=$0.14/hr"
    );
    assert_eq!(pricing.acu_standard, 0.14);
    assert_eq!(pricing.acu_io_optimized, 0.182);
    assert_eq!(pricing.storage_standard, 0.11);
    assert_eq!(pricing.storage_io_opt, 0.25);
    let large = pricing.instances["db.r6g.large"];
    assert_eq!(
        (large.vcpus, large.memory_gib, large.price),
        (2, Num::Float(16.0), 0.30)
    );
    // Catalog class without a live price keeps the static price; the class the
    // EC2 API does not know is dropped.
    assert_eq!(pricing.instances["db.r6g.xlarge"].price, 0.519);
    assert!(!pricing.instances.contains_key("db.unknown.large"));
    assert!(!pricing.instances.contains_key("db.serverless"));
    assert_eq!(pricing.instances.len(), 35);

    let calls = mock.calls().join("\n");
    assert!(calls.contains("orderable:eu-west-1:aurora-postgresql"));
    assert!(calls.contains("(\"databaseEngine\", \"Aurora PostgreSQL\")"));
    assert!(calls.contains("(\"location\", \"EU (Ireland)\")"));
}

#[test]
fn live_pricing_retries_ec2_one_type_at_a_time() {
    let mut mock = live_acu_mock();
    mock.ec2_fail_multi = true;
    let pricing = refresh_pricing(&mock, Engine::Mysql, "us-east-1");
    assert_eq!(pricing.instances["db.r6g.xlarge"].vcpus, 4);
    let calls = mock.calls();
    assert!(calls.iter().any(|call| call == "ec2:us-east-1:3"));
    assert!(calls.iter().any(|call| call == "ec2:us-east-1:1"));
}

#[test]
fn live_pricing_recovers_through_the_bulk_csv_and_reports_a_partial_source() {
    let mut mock = Mock::new(true);
    mock.products = vec![(vec![], Err("boom".to_string()))];
    mock.orderable = Err("denied".to_string());
    mock.csv = Ok([
        "\"FormatVersion\",\"v1.0\"",
        "\"Disclaimer\",\"All prices, \"\"quoted\"\", in USD\"",
        "\"SKU\",\"TermType\",\"Unit\",\"PricePerUnit\",\"Database Engine\",\"Deployment Option\",\"Instance Type\",\"usageType\"",
        "\"A1\",\"OnDemand\",\"Hrs\",\"0.3000000000\",\"Aurora MySQL\",\"Single-AZ\",\"db.r6g.large\",\"InstanceUsage:db.r6g.large\"",
        "\"A2\",\"Reserved\",\"Quantity\",\"2530\",\"Aurora MySQL\",\"Single-AZ\",\"db.r6g.large\",\"InstanceUsage:db.r6g.large\"",
        "\"A3\",\"OnDemand\",\"Hrs\",\"9.9\",\"Aurora MySQL\",\"Single-AZ\",\"db.r6g.xlarge\",\"InstanceUsageIOOptimized:db.r6g.xlarge\"",
        "\"A4\",\"OnDemand\",\"Hrs\",\"0.5\",\"Aurora PostgreSQL\",\"Single-AZ\",\"db.r6g.xlarge\",\"InstanceUsage:db.r6g.xlarge\"",
        "\"A5\",\"OnDemand\",\"Hrs\",\"0.0000\",\"Aurora MySQL\",\"Single-AZ\",\"db.r6g.2xlarge\",\"u\"",
        "\"A6\",\"OnDemand\",\"Hrs\"",
        "",
    ]
    .join("\r\n"));
    let pricing = refresh_pricing(&mock, Engine::Mysql, "us-east-1");
    assert_eq!(text(&pricing.source, "source"), "partial_live");
    assert_eq!(pricing.instances["db.r6g.large"].price, 0.3);
    assert_eq!(
        pricing.instances["db.r6g.xlarge"].price, 0.519,
        "other engines never leak"
    );
    assert_eq!(
        pricing.instances["db.r6g.2xlarge"].price, 1.038,
        "zero prices are ignored"
    );
    assert_eq!(
        at(&pricing.source, "errors"),
        &json("[\"Instance pricing (API): boom [recovered via bulk pricing CSV]\",\"ACU/storage pricing: boom\"]")
    );
    assert!(text(&pricing.source, "details").starts_with(
        "Partial live data (us-east-1): 0 instances, 1 prices. Gaps filled from static defaults. Errors: "
    ));
}

#[test]
fn unknown_regions_are_reported_not_fetched() {
    let mock = live_acu_mock();
    let pricing = refresh_pricing(&mock, Engine::Mysql, "mars-1");
    assert_eq!(text(&pricing.source, "source"), "static_fallback");
    let details = text(&pricing.source, "details");
    assert!(details.contains("Unknown region 'mars-1'. Supported: ap-northeast-1, ap-northeast-2,"));
    assert!(details.contains("ACU/storage pricing: Unknown region 'mars-1'"));
    assert!(
        !mock
            .calls()
            .iter()
            .any(|call| call.starts_with("products:")),
        "no pricing call for an unknown region"
    );
}

#[test]
fn region_maps_differ_per_tool_exactly_as_the_scripts_do() {
    assert_eq!(location(Tool::Acu, "eu-west-3"), Some("EU (Paris)"));
    assert_eq!(location(Tool::Io, "eu-west-3"), None);
    assert_eq!(location(Tool::Commitment, "eu-west-3"), None);
    assert_eq!(
        location(Tool::Acu, "ap-northeast-2"),
        Some("Asia Pacific (Seoul)")
    );
    assert_eq!(location(Tool::Io, "ap-northeast-2"), None);
    assert_eq!(
        location(Tool::Io, "sa-east-1"),
        Some("South America (Sao Paulo)")
    );
    assert_eq!(location(Tool::Commitment, "sa-east-1"), None);
    assert_eq!(
        location(Tool::Commitment, "us-east-1"),
        Some("US East (N. Virginia)")
    );
    assert_eq!(location(Tool::Acu, "US-EAST-1"), None);
    assert_eq!(acu_regions().len(), 16);
    assert_eq!(static_price("db.r8g.48xlarge"), Some(26.496));
    assert_eq!(static_price("db.nope"), None);
}

#[test]
fn bulk_csv_parsing_matches_dictreader() {
    let rows = parse_csv_rows("a,b\n1,\"x,\"\"y\"\"\"\n\n2\n3,4,5\n");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["a"], "1");
    assert_eq!(rows[0]["b"], "x,\"y\"");
    assert_eq!(rows[1].get("b"), None, "short rows leave columns absent");
    assert_eq!(rows[2].len(), 2, "extra cells are dropped");
    assert!(parse_csv_rows("").is_empty());
    // Metadata lines before the SKU header are skipped, line endings normalized.
    let rows = parse_bulk_csv("meta\r\n\"x\"\r\n\"SKU\",\"P\"\r\n\"1\",\"2\"\r\n");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["P"], "2");
}

// ---------------------------------------------------------------------------
// Commitment analyzer
// ---------------------------------------------------------------------------

fn offerings_fixture() -> Vec<Json> {
    vec![
        json(
            r#"{"DBInstanceClass":"db.r7g.2xlarge","Duration":31536000,"OfferingType":"No Upfront","FixedPrice":0.0,"RecurringCharges":[{"RecurringChargeAmount":0.9}]}"#,
        ),
        json(
            r#"{"DBInstanceClass":"db.r7g.2xlarge","Duration":31536000,"OfferingType":"All Upfront","FixedPrice":7000.0,"RecurringCharges":[]}"#,
        ),
        json(
            r#"{"DBInstanceClass":"db.r7g.2xlarge","Duration":94608000,"OfferingType":"Partial Upfront","FixedPrice":9000.0,"RecurringCharges":[{"RecurringChargeAmount":0.2}]}"#,
        ),
        // Same offering listed again, and one for another class.
        json(
            r#"{"DBInstanceClass":"db.r7g.2xlarge","Duration":31536000,"OfferingType":"No Upfront","FixedPrice":0.0,"RecurringCharges":[{"RecurringChargeAmount":0.9}]}"#,
        ),
        json(
            r#"{"DBInstanceClass":"db.r7g.4xlarge","Duration":31536000,"OfferingType":"All Upfront","FixedPrice":1.0}"#,
        ),
    ]
}

fn rates_fixture() -> Vec<Json> {
    vec![
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"No Upfront"},"rate":"0.95","usageType":"InstanceUsage:db.r7g.2xl","unit":"Hrs"}"#,
        ),
        // Skipped: I/O-Optimized, zero rate, unparseable rate, no usage match.
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"No Upfront"},"rate":"0.5","usageType":"InstanceUsageIOOptimized:db.r7g.2xl","unit":"Hrs"}"#,
        ),
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"All Upfront"},"rate":"0","usageType":"InstanceUsage:db.r7g.2xl","unit":"Hrs"}"#,
        ),
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"All Upfront"},"rate":"n/a","usageType":"InstanceUsage:db.r7g.2xl","unit":"Hrs"}"#,
        ),
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"All Upfront"},"rate":"0.4","usageType":"Other:thing","unit":"Hrs"}"#,
        ),
        // Three-year rate and the serverless ACU rate.
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":94608000,"paymentOption":"No Upfront"},"rate":"0.7","usageType":"InstanceUsage:db.r7g.2xl","unit":"Hrs"}"#,
        ),
        json(
            r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"No Upfront"},"rate":"0.10","usageType":"Aurora:ServerlessV2Usage","unit":"ACU-Hr"}"#,
        ),
    ]
}

fn commitment_mock() -> Mock {
    let mut mock = Mock::new(true);
    mock.products = vec![(vec![], Err("no pricing access".to_string()))];
    mock.offerings = Ok(offerings_fixture());
    mock.rates = Ok(rates_fixture());
    mock
}

fn provisioned(instance: &str, count: i64, io_optimized: bool) -> Workload<'_> {
    Workload {
        instance_type: instance,
        num_instances: count,
        io_optimized,
        serverless: false,
        avg_acu: 0.0,
    }
}

#[test]
fn offerings_are_parsed_deduplicated_and_the_cheapest_term_wins() {
    let mock = commitment_mock();
    let offerings = fetch_ri_offerings(&mock, Engine::Mysql, "db.r7g.2xlarge", "us-east-1");
    assert_eq!(
        offerings.len(),
        3,
        "duplicate and foreign-class offerings are dropped"
    );
    // 7000 / 8760 = 0.799087..., 9000 / 26280 + 0.2 = 0.542466...
    assert_eq!(offerings[1].effective_hourly, 0.799_087);
    assert_eq!(offerings[1].upfront_cost, 7000.0);
    assert_eq!(offerings[2].effective_hourly, 0.542_466);
    assert_eq!(offerings[2].term_years, 3);
    assert_eq!(
        best_ri(&offerings, 1).map(|o| o.payment_option.as_str()),
        Some("All Upfront")
    );
    assert_eq!(
        best_ri(&offerings, 3).map(|o| o.payment_option.as_str()),
        Some("Partial Upfront")
    );
    assert!(best_ri(&offerings, 2).is_none());
    assert!(mock
        .calls()
        .contains(&"offerings:us-east-1:db.r7g.2xlarge:aurora-mysql".to_string()));

    let postgres = fetch_ri_offerings(&mock, Engine::Postgresql, "db.r7g.2xlarge", "us-east-1");
    assert_eq!(postgres.len(), 3);
    assert!(mock
        .calls()
        .contains(&"offerings:us-east-1:db.r7g.2xlarge:aurora-postgresql".to_string()));
}

#[test]
fn the_duration_boundary_between_one_and_three_years_is_exclusive() {
    let mut mock = Mock::new(true);
    mock.offerings = Ok(vec![
        json(r#"{"DBInstanceClass":"db.t","Duration":94000000,"OfferingType":"a","FixedPrice":1}"#),
        json(r#"{"DBInstanceClass":"db.t","Duration":94000001,"OfferingType":"b","FixedPrice":1}"#),
    ]);
    let offerings = fetch_ri_offerings(&mock, Engine::Mysql, "db.t", "us-east-1");
    assert_eq!(offerings[0].term_years, 1);
    assert_eq!(offerings[1].term_years, 3);
}

#[test]
fn a_bad_offering_ends_the_list_like_the_python_exception_did() {
    let mut mock = Mock::new(true);
    mock.offerings = Ok(vec![
        json(
            r#"{"DBInstanceClass":"db.t","Duration":31536000,"OfferingType":"ok","FixedPrice":10}"#,
        ),
        json(
            r#"{"DBInstanceClass":"db.t","Duration":31536000,"OfferingType":"bad","FixedPrice":"abc"}"#,
        ),
        json(
            r#"{"DBInstanceClass":"db.t","Duration":31536000,"OfferingType":"after","FixedPrice":10}"#,
        ),
    ]);
    let offerings = fetch_ri_offerings(&mock, Engine::Mysql, "db.t", "us-east-1");
    assert_eq!(offerings.len(), 1);
    assert_eq!(offerings[0].payment_option, "ok");

    mock.offerings = Err("throttled".to_string());
    assert!(fetch_ri_offerings(&mock, Engine::Mysql, "db.t", "us-east-1").is_empty());
    assert!(fetch_ri_offerings(&NoAws, Engine::Mysql, "db.t", "us-east-1").is_empty());
}

#[test]
fn savings_plan_rates_are_keyed_by_normalized_class() {
    let mock = commitment_mock();
    let rates = fetch_dsp_rates(&mock, Engine::Postgresql, "us-east-1");
    assert!(mock
        .calls()
        .contains(&"rates:us-east-1:Aurora PostgreSQL".to_string()));
    let keys: Vec<&str> = rates.keys().map(String::as_str).collect();
    assert_eq!(keys, ["ServerlessV2", "db.r7g.2xlarge"]);
    let class_rates = &rates["db.r7g.2xlarge"];
    assert_eq!(class_rates.len(), 2, "1-year and 3-year");
    assert_eq!(
        best_dsp(class_rates).map(|rate| rate.rate_per_hour),
        Some(0.95)
    );
    assert_eq!(
        best_dsp(&[DspRate {
            term_years: 3,
            payment_option: String::new(),
            rate_per_hour: 1.0
        }]),
        None,
        "only 1-year plans are considered"
    );
    assert!(fetch_dsp_rates(&NoAws, Engine::Mysql, "us-east-1").is_empty());
    let mut failing = Mock::new(true);
    failing.rates = Err("denied".to_string());
    assert!(fetch_dsp_rates(&failing, Engine::Mysql, "us-east-1").is_empty());
}

#[test]
fn provisioned_comparison_picks_the_cheapest_commitment() {
    let mock = commitment_mock();
    let result = build_comparison(
        &mock,
        Engine::Mysql,
        "us-east-1",
        &provisioned("db.r7g.2xlarge", 2, false),
        None,
    );
    // On-demand 1.106 * 730 * 2 = 1614.76; 3yr RI 0.542466*730*2 = 792.00036.
    assert_eq!(text(&result, "workload_type"), "provisioned");
    assert_eq!(num(&result, "on_demand.hourly"), 1.106);
    assert_eq!(num(&result, "on_demand.monthly"), 1614.76);
    assert_eq!(text(&result, "ri_1yr.payment_option"), "All Upfront");
    assert_eq!(num(&result, "ri_1yr.upfront_total"), 14000.0);
    assert_eq!(num(&result, "ri_1yr.effective_hourly_per_instance"), 0.7991);
    assert_eq!(num(&result, "ri_1yr.monthly"), 1166.67);
    assert_eq!(num(&result, "ri_1yr.savings_monthly"), 448.09);
    assert_eq!(num(&result, "ri_3yr.monthly"), 792.0);
    assert_eq!(num(&result, "dsp_1yr.rate_per_hour"), 0.95);
    assert_eq!(num(&result, "dsp_1yr.monthly"), 1387.0);
    assert_eq!(num(&result, "dsp_1yr.savings_monthly"), 227.76);
    assert_eq!(num(&result, "dsp_1yr.savings_pct"), 14.1);
    assert_eq!(text(&result, "recommendation.best_option"), "3yr RI");
    assert_eq!(num(&result, "recommendation.best_monthly_cost"), 792.0);
    assert_eq!(num(&result, "recommendation.savings_vs_on_demand"), 822.76);
    assert_eq!(num(&result, "recommendation.savings_pct"), 51.0);
    assert_eq!(
        text(&result, "recommendation.reason"),
        "3yr RI is the lowest-cost option, saving $823/mo (51%) vs on-demand. Best fit for \
         steady 24/7 workloads you're confident will stay on this instance family for 3 years."
    );
    assert_eq!(at(&result, "notes"), &Json::Array(vec![]));
}

#[test]
fn io_optimized_clusters_pay_the_premium_on_demand_and_on_ri_units() {
    let mock = commitment_mock();
    let result = build_comparison(
        &mock,
        Engine::Mysql,
        "us-east-1",
        &provisioned("db.r7g.2xlarge", 2, true),
        None,
    );
    // 1.106 * 1.30 = 1.4378; on-demand 1.4378*730*2 = 2099.188.
    assert_eq!(num(&result, "on_demand.hourly"), 1.4378);
    assert_eq!(num(&result, "on_demand.monthly"), 2099.19);
    // RI draws down capacity at 1.30x: 0.542466 * 1.30 * 730 * 2 = 1029.60047.
    assert_eq!(num(&result, "ri_3yr.monthly"), 1029.6);
    // DSP covers the full rate, not scaled.
    assert_eq!(num(&result, "dsp_1yr.monthly"), 1387.0);
    let notes = at(&result, "notes");
    assert!(matches!(notes, Json::Array(items) if items.len() == 1));
    assert!(text(&result, "notes.0").starts_with("Cluster is I/O-Optimized (30% compute premium)."));
}

#[test]
fn unknown_and_ineligible_classes_have_no_price_and_a_family_note() {
    // Without an SDK: unknown class prices at 0 and nothing is on offer.
    let result = build_comparison(
        &NoAws,
        Engine::Postgresql,
        "us-east-1",
        &provisioned("db.zz9.large", 3, false),
        None,
    );
    assert_eq!(num(&result, "on_demand.hourly"), 0.0);
    assert_eq!(num(&result, "on_demand.monthly"), 0.0);
    assert_eq!(text(&result, "recommendation.best_option"), "on_demand");
    assert_eq!(
        text(&result, "recommendation.reason"),
        "No RI or DSP offerings available for this instance type/region."
    );
    assert!(text(&result, "notes.0").starts_with(
        "Database Savings Plans do not cover the zz9 family. DSP requires r7g, r8g, r7i"
    ));

    // With an RI but a zero on-demand price the percentages are integer zeros.
    let mut mock = commitment_mock();
    mock.offerings = Ok(vec![json(
        r#"{"DBInstanceClass":"db.zz9.large","Duration":31536000,"OfferingType":"No Upfront","FixedPrice":0,"RecurringCharges":[{"RecurringChargeAmount":0.5}]}"#,
    )]);
    let result = build_comparison(
        &mock,
        Engine::Mysql,
        "us-east-1",
        &provisioned("db.zz9.large", 1, false),
        None,
    );
    assert!(is_int(&result, "ri_1yr.savings_pct"));
    assert!(is_int(&result, "recommendation.savings_pct"));
    assert_eq!(num(&result, "ri_1yr.monthly"), 365.0);
    assert_eq!(num(&result, "ri_1yr.savings_monthly"), -365.0);
    assert!(text(&result, "recommendation.reason").starts_with(
        "1yr RI is the lowest-cost option, saving $-365/mo (0%) vs on-demand. Shorter commitment"
    ));
    assert!(text(&result, "recommendation.reason").ends_with(
        "DSP is not available for this instance family, so RI is the only commitment option."
    ));
    assert_eq!(family_from_instance("db.r7g.2xlarge"), "r7g");
    for bad in [
        "r7g.large",
        "db.R7g.large",
        "db..large",
        "db.r7g",
        "db.r7-g.large",
        "",
    ] {
        assert_eq!(family_from_instance(bad), "", "{bad:?}");
    }
}

#[test]
fn serverless_comparison_prices_the_average_acu() {
    let mock = commitment_mock();
    let workload = Workload {
        instance_type: "",
        num_instances: 0,
        io_optimized: false,
        serverless: true,
        avg_acu: 8.0,
    };
    let result = build_comparison(&mock, Engine::Mysql, "us-east-1", &workload, None);
    // 0.12 * 8 * 730 = 700.8 on demand; DSP 0.10 * 8 * 730 = 584.0.
    assert_eq!(num(&result, "on_demand.hourly"), 0.12);
    assert_eq!(num(&result, "on_demand.monthly"), 700.8);
    assert_eq!(num(&result, "dsp_1yr.monthly"), 584.0);
    assert_eq!(num(&result, "dsp_1yr.savings_monthly"), 116.8);
    assert_eq!(num(&result, "dsp_1yr.savings_pct"), 16.7);
    assert_eq!(at(&result, "ri_1yr"), &Json::Null);
    assert_eq!(text(&result, "recommendation.best_option"), "1yr DSP");
    assert_eq!(
        text(&result, "recommendation.reason"),
        "1yr DSP saves $117/mo (17%). Size the commitment to your steady baseline ACU — DSP \
         bills the committed $/hr continuously, even during idle periods."
    );
    // I/O-Optimized uses the 0.156 ACU rate.
    let io = Workload {
        io_optimized: true,
        ..workload.clone()
    };
    let result = build_comparison(&mock, Engine::Mysql, "us-east-1", &io, None);
    assert_eq!(num(&result, "on_demand.hourly"), 0.156);

    // No rates for the region, then a rate that would cost more.
    let result = build_comparison(&NoAws, Engine::Mysql, "us-east-1", &workload, None);
    assert_eq!(
        text(&result, "recommendation.reason"),
        "No Database Savings Plan rates available for Serverless v2 ACU in this region."
    );
    let mut dear = Mock::new(true);
    dear.rates = Ok(vec![json(
        r#"{"savingsPlanOffering":{"durationSeconds":31536000,"paymentOption":"No Upfront"},"rate":"0.2","usageType":"Aurora:ServerlessV2Usage","unit":"ACU-Hr"}"#,
    )]);
    let result = build_comparison(&dear, Engine::Mysql, "us-east-1", &workload, None);
    assert_eq!(
        text(&result, "recommendation.reason"),
        "DSP would not save money at the specified average ACU."
    );
}

#[test]
fn commitment_offline_commands() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "commitment",
        "offline",
        "--engine",
        "mysql",
        "--instance",
        "db.r6g.xlarge",
        "--format",
        "table",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    let rule = "=".repeat(72);
    assert!(stdout.starts_with(&format!(
        "{rule}\nAurora Commitment Pricing — 1× db.r6g.xlarge\n{rule}\n\n"
    )));
    assert!(stdout.contains(&format!(
        "  {:<28} ${:>11} {:>12} {:>12}  —\n",
        "On-Demand", "379", "—", "$0"
    )));
    assert!(stdout.contains("  Recommendation: on_demand\n"));
    assert!(stdout.contains("  Note: Database Savings Plans do not cover the r6g family."));

    let (status, stdout, _) = cli(&[
        "aurora",
        "commitment",
        "offline",
        "--engine",
        "postgresql",
        "--instance",
        "db.r6g.xlarge",
        "--num-instances",
        "2",
        "--io-optimized",
    ]);
    assert_eq!(status, 0);
    assert!(stdout.contains("\"num_instances\": 2,\n  \"io_optimized\": true,"));
    assert!(stdout.contains("\"hourly\": 0.6747,"), "0.519 * 1.30");
    assert!(stdout.contains("\"ri_1yr\": null,"));

    let (status, stdout, _) = cli(&[
        "aurora",
        "commitment",
        "offline",
        "--engine",
        "mysql",
        "--serverless",
        "--avg-acu",
        "8",
        "--format",
        "table",
    ]);
    assert_eq!(status, 0);
    assert!(stdout.contains(
        "Aurora Serverless v2 Commitment Pricing\n  Avg ACU: 8.0  I/O-Optimized: False\n"
    ));
}

#[test]
fn commitment_offline_input_errors_exit_two() {
    for (args, message) in [
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql", "--serverless"],
            "error: --serverless requires --avg-acu > 0",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql", "--serverless", "--avg-acu", "0"],
            "error: --serverless requires --avg-acu > 0",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql", "--serverless", "--avg-acu", "-3"],
            "error: --serverless requires --avg-acu > 0",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql"],
            "error: offline mode requires --instance (or --serverless)",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql", "--instance", ""],
            "error: offline mode requires --instance (or --serverless)",
        ),
        (
            vec!["aurora", "commitment", "offline"],
            "error: the following arguments are required: --engine",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "oracle"],
            "error: argument --engine: invalid choice: 'oracle' (choose from 'mysql', 'postgresql')",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql", "--instance", "x", "--format", "xml"],
            "error: argument --format: invalid choice: 'xml' (choose from 'json', 'table')",
        ),
        (
            vec!["aurora", "commitment", "offline", "--engine", "mysql", "--instance", "x", "--num-instances", "two"],
            "error: argument --num-instances: invalid int value: 'two'",
        ),
        (
            vec!["aurora", "commitment", "online", "--engine", "mysql"],
            "error: argument mode: invalid choice: 'online' (choose from 'offline')",
        ),
        (
            vec!["aurora", "commitment", "--engine", "mysql", "--instance", "db.r6g.large"],
            "error: unrecognized arguments: --instance",
        ),
        (vec!["aurora", "commitment", "offline", "extra", "--engine", "mysql"], "error: invalid arguments"),
    ] {
        let (status, stdout, stderr) = cli(&args);
        assert_eq!(status, 2, "{args:?}");
        assert_eq!(stdout, "", "{args:?}");
        assert!(stderr.starts_with(message), "{args:?}: {stderr}");
    }
}

#[test]
fn live_commands_without_an_sdk_exit_two_and_print_nothing() {
    for args in [
        vec!["aurora", "commitment", "--engine", "mysql", "--all"],
        vec![
            "aurora",
            "commitment",
            "--engine",
            "postgresql",
            "--cluster",
            "c1",
        ],
        vec!["aurora", "io", "--engine", "mysql", "--all"],
        vec![
            "aurora",
            "io",
            "--engine",
            "postgresql",
            "--cluster",
            "c1",
            "--days",
            "7",
        ],
    ] {
        let (status, stdout, stderr) = cli(&args);
        assert_eq!(status, 2, "{args:?}");
        assert_eq!(stdout, "");
        assert!(
            stderr.starts_with(
                "error: AWS SDK access is not available in this build; use the 'offline' subcommand."
            ),
            "{stderr}"
        );
    }
}

#[test]
fn bare_commands_print_the_help() {
    let (status, stdout, stderr) = cli(&["aurora", "acu"]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(stdout, format!("{}\n", crate::USAGE));
    let (status, stdout, _) = cli(&["aurora", "acu", "bogus"]);
    assert_eq!((status, stdout.as_str()), (2, ""));
}

// ---------------------------------------------------------------------------
// Commitment cluster analysis
// ---------------------------------------------------------------------------

#[test]
fn cluster_analysis_prices_each_class_and_notes_serverless() {
    let mut mock = commitment_mock();
    mock.described = vec![(
        "c1".to_string(),
        cluster("aurora-iopt1", "aurora-mysql", &["w", "r1", "r2", "sv"]),
    )];
    mock.classes = vec![
        ("w".to_string(), Ok(vec!["db.r7g.2xlarge".to_string()])),
        ("r1".to_string(), Ok(vec!["db.r7g.2xlarge".to_string()])),
        ("r2".to_string(), Err("DBInstanceNotFound".to_string())),
        ("sv".to_string(), Ok(vec!["db.serverless".to_string()])),
    ];
    let result = commitment_cluster(&mock, Engine::Mysql, "c1", "us-east-1");
    assert_eq!(text(&result, "engine"), "aurora-mysql");
    assert_eq!(at(&result, "io_optimized"), &Json::Bool(true));
    assert_eq!(
        at(&result, "instance_mix"),
        &json(r#"{"db.r7g.2xlarge":2,"serverless":1}"#)
    );
    assert_eq!(num(&result, "workloads.0.num_instances"), 2.0);
    assert_eq!(at(&result, "workloads.0.io_optimized"), &Json::Bool(true));
    assert_eq!(text(&result, "workloads.1.workload_type"), "serverless_v2");
    assert_eq!(num(&result, "workloads.1.instance_count"), 1.0);
    assert!(text(&result, "workloads.1.note").starts_with("Serverless v2 instances detected."));
    // Savings Plan rates are fetched once for the whole cluster.
    let rate_calls = mock
        .calls()
        .iter()
        .filter(|call| call.starts_with("rates:"))
        .count();
    assert_eq!(rate_calls, 1);
}

#[test]
fn cluster_analysis_failures_are_reported_per_cluster() {
    let mut mock = commitment_mock();
    mock.described = vec![
        ("empty".to_string(), cluster("aurora", "aurora-mysql", &[])),
        (
            "empty-pg".to_string(),
            cluster("aurora", "aurora-postgresql", &[]),
        ),
    ];
    let result = commitment_cluster(&mock, Engine::Mysql, "empty", "us-east-1");
    assert_eq!(at(&result, "skipped"), &Json::Bool(true));
    assert_eq!(text(&result, "engine_version"), "8.0.mysql_aurora.3.05.2");
    assert!(text(&result, "reason").ends_with(
        "This typically indicates the last instance was deleted, a paused cluster, or a cluster mid-migration."
    ));
    let result = commitment_cluster(&mock, Engine::Postgresql, "empty-pg", "us-east-1");
    assert!(text(&result, "reason").ends_with(
        "This typically indicates Aurora Limitless, a paused cluster, or a cluster mid-migration."
    ));

    let result = commitment_cluster(&mock, Engine::Mysql, "missing", "us-east-1");
    assert_eq!(
        result,
        json(r#"{"cluster_id":"missing","error":"cluster not found"}"#)
    );
    let result = commitment_cluster(&mock, Engine::Mysql, "boom", "us-east-1");
    assert_eq!(
        result,
        json(r#"{"cluster_id":"boom","error":"AccessDenied"}"#)
    );
}

#[test]
fn fleet_table_totals_on_demand_against_best_commitments() {
    let mut mock = commitment_mock();
    mock.described = vec![
        (
            "c1".to_string(),
            cluster("aurora", "aurora-mysql", &["w", "r1"]),
        ),
        ("c2".to_string(), cluster("aurora", "aurora-mysql", &[])),
    ];
    mock.classes = vec![
        ("w".to_string(), Ok(vec!["db.r7g.2xlarge".to_string()])),
        ("r1".to_string(), Ok(vec!["db.r7g.2xlarge".to_string()])),
    ];
    let clusters = vec![
        commitment_cluster(&mock, Engine::Mysql, "c1", "us-east-1"),
        commitment_cluster(&mock, Engine::Mysql, "c2", "us-east-1"),
        commitment_cluster(&mock, Engine::Mysql, "missing", "us-east-1"),
    ];
    let output = Json::object(vec![
        ("region", Json::string("us-east-1")),
        ("cluster_count", Json::Int(3)),
        ("clusters", Json::Array(clusters.clone())),
    ]);
    let table = format_fleet(&output);
    // 1614.76 on demand; best 792.0; savings 822.76 = 50.95%.
    assert!(table.starts_with(
        "Region: us-east-1\n\nmissing: ERROR cluster not found\n  Fleet monthly on-demand:    $1,615\n"
    ));
    assert!(table.contains("  With best commitments:      $792\n"));
    assert!(table.contains("  Fleet savings opportunity:  $823/mo (51%)\n"));
    assert!(table.contains("missing: ERROR cluster not found\n"));
    assert!(table.contains("Cluster: c1  (aurora-mysql)  storage_type=aurora\n"));
    assert!(table.contains("Cluster: c2  (aurora-mysql)  storage_type=aurora"));
    // An errored cluster shows only its header in the per-cluster table.
    assert_eq!(
        format_cluster(&clusters[2]),
        "Cluster: missing  (?)  storage_type=?"
    );
}

// ---------------------------------------------------------------------------
// I/O-Optimized analyzer
// ---------------------------------------------------------------------------

#[test]
fn comparison_recommends_standard_when_the_premium_outweighs_io() {
    let pricing = IoPricing::static_defaults();
    // 1000 compute + 800 GiB + 1200M I/O: standard 1000+80+240 = 1320;
    // I/O-Optimized 1300+180 = 1480.
    let result = compute_comparison(&pricing, 1000.0, 800.0, 1200.0);
    assert_eq!(num(&result, "standard.storage_monthly"), 80.0);
    assert_eq!(num(&result, "standard.io_monthly"), 240.0);
    assert_eq!(num(&result, "standard.total_monthly"), 1320.0);
    assert_eq!(num(&result, "io_optimized.compute_monthly"), 1300.0);
    assert_eq!(num(&result, "io_optimized.storage_monthly"), 180.0);
    assert_eq!(num(&result, "io_optimized.total_monthly"), 1480.0);
    assert_eq!(at(&result, "io_optimized.io_monthly"), &Json::Float(0.0));
    assert_eq!(num(&result, "io_cost_pct_of_total"), 18.2);
    assert_eq!(num(&result, "savings_with_io_opt"), -160.0);
    assert_eq!(num(&result, "monthly_io_millions"), 1200.0);
    assert_eq!(num(&result, "storage_gib"), 800.0);
    assert_eq!(text(&result, "recommendation"), "standard");
    assert_eq!(
        text(&result, "reason"),
        "I/O is 18% of total cost (below 25% rule-of-thumb). I/O-Optimized would cost $160/mo more."
    );
}

#[test]
fn comparison_follows_dollar_savings_not_the_twenty_five_percent_rule() {
    let pricing = IoPricing::static_defaults();
    // Break-even: 1000 compute + 1500M I/O -> 1300 standard = 1300 I/O-Optimized.
    let result = compute_comparison(&pricing, 1000.0, 0.0, 1500.0);
    assert_eq!(num(&result, "savings_with_io_opt"), 0.0);
    assert_eq!(text(&result, "recommendation"), "standard");
    assert_eq!(
        text(&result, "reason"),
        "I/O is 23% of total cost (below 25% rule-of-thumb). I/O-Optimized would cost $0/mo more."
    );
    // One more million requests tips it: I/O-Optimized below the 25% rule.
    let result = compute_comparison(&pricing, 1000.0, 0.0, 1501.0);
    assert_eq!(text(&result, "recommendation"), "io_optimized");
    assert_eq!(
        text(&result, "reason"),
        "I/O is 23% of total cost (below 25% rule-of-thumb). I/O-Optimized saves $0/mo."
    );
    // Above the rule of thumb yet more expensive: storage dominates.
    let result = compute_comparison(&pricing, 0.0, 1000.0, 500.0);
    assert_eq!(num(&result, "io_cost_pct_of_total"), 50.0);
    assert_eq!(text(&result, "recommendation"), "standard");
    assert_eq!(
        text(&result, "reason"),
        "I/O is 50% of total cost (≥25% rule-of-thumb). I/O-Optimized would cost $25/mo more."
    );
    // I/O heavy: 500 compute + 100 GiB + 5000M I/O.
    let result = compute_comparison(&pricing, 500.0, 100.0, 5000.0);
    assert_eq!(num(&result, "savings_with_io_opt"), 837.5);
    assert_eq!(
        text(&result, "reason"),
        "I/O is 66% of total cost (≥25% rule-of-thumb). I/O-Optimized saves $838/mo."
    );
}

#[test]
fn comparison_with_nothing_to_price_has_an_integer_zero_percentage() {
    let result = compute_comparison(&IoPricing::static_defaults(), 0.0, 0.0, 0.0);
    assert!(is_int(&result, "io_cost_pct_of_total"));
    assert_eq!(num(&result, "savings_with_io_opt"), 0.0);
    assert_eq!(text(&result, "recommendation"), "standard");
    assert_eq!(
        text(&result, "reason"),
        "I/O is 0% of total cost (below 25% rule-of-thumb). I/O-Optimized would cost $0/mo more."
    );
}

#[test]
fn data_quality_tags_have_exact_day_boundaries() {
    for (days, tag) in [
        (-1, "insufficient"),
        (0, "insufficient"),
        (2, "insufficient"),
        (3, "short"),
        (6, "short"),
        (7, "adequate"),
        (13, "adequate"),
        (14, "good"),
        (365, "good"),
    ] {
        assert_eq!(data_quality_tag(days), tag, "{days}");
    }
}

#[test]
fn offline_analysis_matches_the_python_numbers() {
    let pricing = IoPricing::static_defaults();
    let result = analyze_offline(&pricing, "db.r6g.2xlarge", 2, 800.0, 1200.0);
    // 1.038 * 730 * 2 = 1515.48 compute; io-opt compute 1970.124.
    let members: Vec<&str> = match &result {
        Json::Object(pairs) => pairs.iter().map(|(key, _)| key.as_str()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        members[..5],
        [
            "cluster_id",
            "instance_type",
            "num_instances",
            "data_quality",
            "standard"
        ]
    );
    assert_eq!(text(&result, "cluster_id"), "offline-input");
    assert_eq!(text(&result, "data_quality"), "user_supplied");
    assert_eq!(num(&result, "standard.compute_monthly"), 1515.48);
    assert_eq!(num(&result, "standard.total_monthly"), 1835.48);
    assert_eq!(num(&result, "io_optimized.compute_monthly"), 1970.12);
    assert_eq!(num(&result, "io_optimized.total_monthly"), 2150.12);
    assert_eq!(num(&result, "savings_with_io_opt"), -314.64);
    assert_eq!(num(&result, "io_cost_pct_of_total"), 13.1);

    // An unknown class is an error object, not a failure.
    let result = analyze_offline(&pricing, "db.nope", 1, 1.0, 1.0);
    assert!(text(&result, "error")
        .starts_with("Unknown instance type: db.nope. Supported: db.r5.12xlarge, "));
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "io",
        "offline",
        "--engine",
        "mysql",
        "--instance",
        "db.nope",
        "--storage-gib",
        "1",
        "--monthly-io-millions",
        "1",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.starts_with("{\n  \"error\": \"Unknown instance type: db.nope."));
    assert!(stdout.contains("\"pricing_source\": {"));
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "io",
        "offline",
        "--engine",
        "mysql",
        "--instance",
        "db.nope",
        "--storage-gib",
        "1",
        "--monthly-io-millions",
        "1",
        "--format",
        "table",
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.starts_with("error: Unknown instance type: db.nope."));
}

#[test]
fn io_offline_command_output_and_pricing_source() {
    let (status, stdout, stderr) = cli(&[
        "aurora",
        "io",
        "offline",
        "--engine",
        "postgresql",
        "--instance",
        "db.r6g.2xlarge",
        "--num-instances",
        "2",
        "--storage-gib",
        "800",
        "--monthly-io-millions",
        "1200",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.starts_with(
        "{\n  \"cluster_id\": \"offline-input\",\n  \"instance_type\": \"db.r6g.2xlarge\",\n  \
         \"num_instances\": 2,\n  \"data_quality\": \"user_supplied\",\n  \"standard\": {\n"
    ));
    assert!(stdout.ends_with(
        "  \"pricing_source\": {\n    \"source\": \"static_fallback\",\n    \"region\": \
         \"us-east-1\",\n    \"note\": \"AWS SDK not available\"\n  }\n}\n"
    ));

    let (status, stdout, _) = cli(&[
        "aurora",
        "io",
        "offline",
        "--engine",
        "mysql",
        "--region",
        "eu-west-3",
        "--instance",
        "db.r6g.large",
        "--storage-gib",
        "10",
        "--monthly-io-millions",
        "5",
    ]);
    assert_eq!(status, 0);
    assert!(stdout.contains("\"note\": \"Region eu-west-3 not mapped; using us-east-1 defaults\""));

    let (status, stdout, stderr) = cli(&[
        "aurora",
        "io",
        "offline",
        "--engine",
        "mysql",
        "--instance",
        "db.r6g.2xlarge",
        "--num-instances",
        "2",
        "--storage-gib",
        "800",
        "--monthly-io-millions",
        "1200",
        "--format",
        "table",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        format!(
            "Cluster: offline-input  (user_supplied data)\n  Current storage type: ?\n  \
             Monthly I/O: 1200M requests\n  Storage: 800 GiB\n\n  {:<12} {:>12} {:>15}\n  \
             {:<12} {:>12} {:>15}\n  {:<12} {:>12} {:>15}\n  {:<12} {:>12} {:>15}\n  \
             {:<12} {:>12} {:>15}\n\n  I/O cost: 13% of total\n  Recommendation: STANDARD\n  \
             I/O is 13% of total cost (below 25% rule-of-thumb). I/O-Optimized would cost \
             $315/mo more.\n",
            "Component",
            "Standard",
            "I/O-Optimized",
            "Compute",
            "1515",
            "1970",
            "Storage",
            "80",
            "180",
            "I/O",
            "240",
            "0",
            "Total",
            "1835",
            "2150",
        )
    );
}

#[test]
fn io_offline_input_errors_exit_two() {
    for (args, message) in [
        (
            vec!["aurora", "io", "offline", "--engine", "mysql"],
            "error: the following arguments are required: --instance, --storage-gib, --monthly-io-millions",
        ),
        (
            vec!["aurora", "io", "offline", "--engine", "mysql", "--instance", "x", "--storage-gib", "1"],
            "error: the following arguments are required: --monthly-io-millions",
        ),
        (
            vec!["aurora", "io", "offline", "--engine", "mysql", "--instance", "x", "--storage-gib", "lots", "--monthly-io-millions", "1"],
            "error: argument --storage-gib: invalid float value: 'lots'",
        ),
        (
            vec!["aurora", "io", "--engine", "mysql", "--days", "x"],
            "error: argument --days: invalid int value: 'x'",
        ),
        (
            vec!["aurora", "io", "--engine", "mysql", "--storage-gib", "5"],
            "error: unrecognized arguments: --storage-gib",
        ),
    ] {
        let (status, stdout, stderr) = cli(&args);
        assert_eq!(status, 2, "{args:?}");
        assert_eq!(stdout, "");
        assert!(stderr.starts_with(message), "{args:?}: {stderr}");
    }
}

#[test]
fn live_io_pricing_reads_instances_storage_and_io_rates() {
    let mut mock = Mock::new(true);
    mock.products = vec![
        (
            vec![("deploymentOption", "Single-AZ")],
            Ok(vec![
                price_item("db.r6g.large", "InstanceUsage:db.r6g.large", "0.30"),
                price_item("db.r6g.large", "InstanceUsage:db.r6g.large", "0.31"),
                price_item(
                    "db.r6g.xlarge",
                    "InstanceUsageIOOptimized:db.r6g.xlarge",
                    "9",
                ),
            ]),
        ),
        (
            vec![("productFamily", "Database Storage")],
            Ok(vec![
                price_item("", "USE1-Aurora:StorageUsage", "0.11"),
                price_item("", "USE1-Aurora:IOOptimized-StorageUsage", "0.25"),
                price_item("", "USE1-Aurora:StorageIOUsage", "3e-7"),
                price_item("", "USE1-Aurora:Other", "7"),
                price_item("", "USE1-Aurora:StorageUsage", "0"),
            ]),
        ),
    ];
    let pricing = io_refresh(&mock, Engine::Postgresql, "sa-east-1");
    assert_eq!(
        pricing.instance_prices["db.r6g.large"], 0.31,
        "last price wins"
    );
    assert_eq!(pricing.instance_prices["db.r6g.xlarge"], 0.519);
    assert_eq!(pricing.storage_standard, 0.11);
    assert_eq!(pricing.storage_io_opt, 0.25);
    assert_eq!(pricing.io_per_million, 3e-7 * 1_000_000.0);
    assert_eq!(text(&pricing.source, "source"), "live");
    assert_eq!(text(&pricing.source, "region"), "sa-east-1");
    assert_eq!(num(&pricing.source, "live_instances"), 2.0);
    assert_eq!(num(&pricing.source, "storage_standard"), 0.11);
    assert_eq!(num(&pricing.source, "storage_io_opt"), 0.25);
    assert_eq!(
        num(&pricing.source, "io_per_million"),
        pricing.io_per_million
    );
    assert!(mock.calls()[0].contains("(\"location\", \"South America (Sao Paulo)\")"));

    // A failure after the instance prices keeps them but reports the fallback.
    mock.products.truncate(1);
    let pricing = io_refresh(&mock, Engine::Mysql, "us-east-1");
    assert_eq!(pricing.instance_prices["db.r6g.large"], 0.31);
    assert_eq!(text(&pricing.source, "source"), "static_fallback");
    assert_eq!(text(&pricing.source, "error"), "no products configured");
}

fn io_mock() -> Mock {
    let mut mock = Mock::new(true);
    mock.products = vec![(vec![], Err("no pricing".to_string()))];
    mock.described = vec![
        (
            "c1".to_string(),
            cluster("aurora", "aurora-mysql", &["w", "sv", "odd", "gone"]),
        ),
        (
            "c2".to_string(),
            cluster("aurora-iopt1", "aurora-mysql", &[]),
        ),
    ];
    mock.classes = vec![
        ("w".to_string(), Ok(vec!["db.r6g.2xlarge".to_string()])),
        ("sv".to_string(), Ok(vec!["db.serverless".to_string()])),
        ("odd".to_string(), Ok(vec!["db.zz9.large".to_string()])),
    ];
    mock.metrics = vec![
        ("VolumeReadIOPs", "Sum", vec![100_000_000.0, 200_000_000.0]),
        ("VolumeWriteIOPs", "Sum", vec![50_000_000.0]),
        (
            "VolumeBytesUsed",
            "Average",
            vec![107_374_182_400.0, 107_374_182_400.0],
        ),
    ];
    mock.clusters = vec![
        ("c1".to_string(), "aurora-mysql".to_string()),
        ("c2".to_string(), "aurora-mysql".to_string()),
        ("docdb".to_string(), "docdb".to_string()),
    ];
    mock
}

const NOW: i64 = 1_700_000_123;

#[test]
fn live_io_analysis_extrapolates_cloudwatch_totals() {
    let mock = io_mock();
    let pricing = IoPricing::static_defaults();
    let result = io_cluster(&mock, Engine::Mysql, &pricing, "c1", "us-east-1", 14, NOW).unwrap();
    // The window ends on the hour (1_699_999_200) and spans 14 days.
    let calls = mock.calls();
    assert!(
        calls.contains(&"metric:c1:VolumeReadIOPs:Sum:1698789600:1699999200".to_string()),
        "{calls:?}"
    );
    assert!(calls.contains(&"metric:c1:VolumeBytesUsed:Average:1698789600:1699999200".to_string()));

    // 350M requests / 336 h * 730 h = 760.4M per month; 100 GiB average.
    assert_eq!(num(&result, "observed_io_total"), 350_000_000.0);
    assert!(is_int(&result, "observed_io_total"));
    assert_eq!(num(&result, "lookback_days"), 14.0);
    assert_eq!(text(&result, "data_quality"), "good");
    assert_eq!(num(&result, "monthly_io_millions"), 760.4);
    assert_eq!(num(&result, "storage_gib"), 100.0);
    // Only the priced provisioned instance counts: 1.038 * 730.
    assert_eq!(num(&result, "standard.compute_monthly"), 757.74);
    assert_eq!(num(&result, "standard.storage_monthly"), 10.0);
    assert_eq!(num(&result, "standard.io_monthly"), 152.08);
    assert_eq!(text(&result, "recommendation"), "standard");
    assert_eq!(at(&result, "compute_partial"), &Json::Bool(true));
    assert_eq!(num(&result, "instances.0.price_hr"), 1.038);
    assert_eq!(text(&result, "instances.1.note"), "serverless_excluded");
    assert_eq!(text(&result, "instances.2.note"), "unknown_price");
    assert_eq!(text(&result, "instances.3.error"), "DBInstanceNotFound");
    assert_eq!(text(&result, "compute_warnings.1"), "odd (db.zz9.large) has no known hourly price in the static/live table — excluded from the compute estimate; results are partial.");
    assert!(text(&result, "compute_warnings.0").starts_with(
        "sv is Aurora Serverless v2 (db.serverless) — its ACU-based compute cost is not included"
    ));
}

#[test]
fn live_io_analysis_skips_empty_clusters_and_reports_missing_ones() {
    let mock = io_mock();
    let pricing = IoPricing::static_defaults();
    let result = io_cluster(
        &mock,
        Engine::Postgresql,
        &pricing,
        "c2",
        "us-east-1",
        14,
        NOW,
    )
    .unwrap();
    assert_eq!(at(&result, "skipped"), &Json::Bool(true));
    assert_eq!(text(&result, "current_storage_type"), "aurora-iopt1");
    assert!(text(&result, "reason").contains("or it is an Aurora Limitless cluster"));
    let result = io_cluster(&mock, Engine::Mysql, &pricing, "c2", "us-east-1", 14, NOW).unwrap();
    assert!(text(&result, "reason")
        .ends_with("does not apply until the cluster has a running instance."));
    let result = io_cluster(&mock, Engine::Mysql, &pricing, "nope", "us-east-1", 14, NOW).unwrap();
    assert_eq!(
        result,
        json(r#"{"cluster_id":"nope","error":"cluster not found"}"#)
    );
    // An API failure on the cluster itself ends the run, like the Python raise.
    let error =
        io_cluster(&mock, Engine::Mysql, &pricing, "boom", "us-east-1", 14, NOW).unwrap_err();
    assert_eq!(error.message(), "AccessDenied");
    assert_eq!(error.exit_code(), 1);
}

#[test]
fn lookback_window_checks_every_magnitude() {
    assert_eq!(
        lookback_window(NOW, 0).unwrap(),
        (1_699_999_200, 1_699_999_200)
    );
    assert_eq!(
        lookback_window(NOW, 1).unwrap(),
        (1_699_999_200 - 86_400, 1_699_999_200)
    );
    assert_eq!(lookback_window(3_599, 0).unwrap(), (0, 0));
    assert_eq!(lookback_window(-1, 0).unwrap(), (-3600, -3600));
    for days in [1_000_000_000, i64::MAX, i64::MIN, -1_000_000_000] {
        let error = lookback_window(NOW, days).unwrap_err();
        assert_eq!(
            error.message(),
            format!("days={days}; must have magnitude <= 999999999"),
            "{days}"
        );
    }
    // Within timedelta's range but outside datetime's.
    for days in [999_999_999, 100_000_000, -100_000_000] {
        assert_eq!(
            lookback_window(NOW, days).unwrap_err().message(),
            "date value out of range",
            "{days}"
        );
    }
    // Zero or negative lookbacks produce no I/O extrapolation instead of dividing.
    let mock = io_mock();
    let result = io_cluster(
        &mock,
        Engine::Mysql,
        &IoPricing::static_defaults(),
        "c1",
        "us-east-1",
        0,
        NOW,
    )
    .unwrap();
    assert_eq!(num(&result, "monthly_io_millions"), 0.0);
    assert_eq!(text(&result, "data_quality"), "insufficient");
}

#[test]
fn fleet_summary_excludes_errors_and_skips_from_totals() {
    let mock = io_mock();
    let pricing = IoPricing::static_defaults();
    let analyzable =
        io_cluster(&mock, Engine::Mysql, &pricing, "c1", "us-east-1", 14, NOW).unwrap();
    let skipped = io_cluster(&mock, Engine::Mysql, &pricing, "c2", "us-east-1", 14, NOW).unwrap();
    let missing = io_cluster(&mock, Engine::Mysql, &pricing, "nope", "us-east-1", 14, NOW).unwrap();
    let summary = fleet_summary(&[analyzable, skipped.clone(), missing.clone()]);
    assert_eq!(num(&summary, "cluster_count"), 3.0);
    assert_eq!(num(&summary, "analyzable_count"), 1.0);
    assert_eq!(num(&summary, "skipped_count"), 1.0);
    assert_eq!(text(&summary, "skipped_clusters.0.cluster_id"), "c2");
    assert_eq!(
        at(&summary, "clusters_that_should_switch"),
        &Json::Array(vec![])
    );
    assert!(!is_int(&summary, "current_monthly_total_standard"));
    assert!(
        is_int(&summary, "optimal_savings_monthly"),
        "no winners: integer 0"
    );
    assert_eq!(num(&summary, "optimal_savings_monthly"), 0.0);

    // Nothing analyzable: every total is the integer zero.
    let summary = fleet_summary(&[skipped, missing]);
    for key in [
        "current_monthly_total_standard",
        "if_all_on_io_optimized_monthly",
        "optimal_savings_monthly",
    ] {
        assert!(is_int(&summary, key), "{key}");
    }
    assert_eq!(num(&summary, "analyzable_count"), 0.0);
}

fn run_io(
    values: &[(&str, &str)],
    flags: &[&str],
    mock: &Mock,
) -> (Result<(), ToolkitError>, String, String) {
    let invocation = invocation(&[], flags, values);
    capture(|output| super::io::command(&invocation, output, mock, NOW))
}

#[test]
fn io_fleet_table_labels_skipped_clusters_per_engine() {
    let mock = io_mock();
    let (result, stdout, _) = run_io(
        &[("--engine", "mysql"), ("--format", "table")],
        &["--all"],
        &mock,
    );
    result.unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines[0],
        "Region: us-east-1  Clusters: 2 (analyzable: 1, skipped: 1)"
    );
    assert_eq!(lines[1], "  Current (Standard):      $920/mo");
    assert_eq!(lines[2], "  All on I/O-Optimized:    $1008/mo");
    assert_eq!(lines[3], "  Optimal (switch winners): saves $0/mo");
    assert_eq!(lines[4], "  Clusters to switch: (none)");
    assert_eq!(lines[5], "  Skipped (not applicable):");
    assert!(lines[6].starts_with("    - c2: Cluster has no DB instances — no compute to analyze."));
    assert_eq!(lines[6].chars().count(), "    - c2: ".chars().count() + 80);
    assert_eq!(lines[7], "");
    assert_eq!(
        lines[8],
        format!(
            "{:<30} {:>6} {:>10} {:>12} {:>14}",
            "Cluster", "I/O %", "Std $/mo", "IOOpt $/mo", "Rec"
        )
    );
    assert_eq!(lines[9], "-".repeat(76));
    assert_eq!(
        lines[10],
        format!(
            "{:<30} {:>5}% {:>10} {:>12} {:>14}",
            "c1", "16", "920", "1008", "standard"
        )
    );
    assert_eq!(
        lines[11],
        format!(
            "{:<30} {:>6} {:>10} {:>12} {:>22}",
            "c2", "—", "—", "—", "skipped (no instances)"
        )
    );
    assert_eq!(lines.len(), 12);

    let (result, stdout, _) = run_io(
        &[("--engine", "postgresql"), ("--format", "table")],
        &["--all"],
        &mock,
    );
    result.unwrap();
    assert!(stdout.contains(&format!(
        "{:<30} {:>6} {:>10} {:>12} {:>20}\n",
        "c2", "—", "—", "—", "skipped (limitless)"
    )));
}

#[test]
fn io_fleet_json_and_empty_regions() {
    let mock = io_mock();
    let (result, stdout, stderr) = run_io(&[("--engine", "mysql")], &["--all"], &mock);
    result.unwrap();
    assert_eq!(stderr, "");
    let parsed = parse_json(&stdout).expect("fleet output is JSON");
    assert_eq!(text(&parsed, "region"), "us-east-1");
    assert_eq!(
        num(&parsed, "summary.cluster_count"),
        2.0,
        "the non-Aurora cluster is not listed"
    );
    assert_eq!(text(&parsed, "clusters.1.cluster_id"), "c2");
    assert_eq!(text(&parsed, "pricing_source.source"), "static_fallback");

    let mut empty = Mock::new(true);
    empty.products = vec![(vec![], Err("no pricing".to_string()))];
    let (result, stdout, _) = run_io(
        &[("--engine", "mysql"), ("--format", "table")],
        &["--all"],
        &empty,
    );
    result.unwrap();
    assert_eq!(
        stdout,
        "{\n  \"status\": \"ok\",\n  \"region\": \"us-east-1\",\n  \"clusters\": []\n}\n"
    );
}

#[test]
fn io_single_cluster_table_and_errors() {
    let mock = io_mock();
    let (result, stdout, _) = run_io(
        &[
            ("--engine", "mysql"),
            ("--cluster", "c1"),
            ("--format", "table"),
        ],
        &[],
        &mock,
    );
    result.unwrap();
    assert!(stdout.starts_with("Cluster: c1  (good data)\n  Current storage type: aurora\n  Monthly I/O: 760M requests\n  Storage: 100 GiB\n\n"));
    assert!(stdout.contains("  I/O cost: 16% of total\n  Recommendation: STANDARD\n"));

    let (result, stdout, _) = run_io(
        &[
            ("--engine", "mysql"),
            ("--cluster", "c2"),
            ("--format", "table"),
        ],
        &[],
        &mock,
    );
    result.unwrap();
    assert!(stdout.starts_with("Cluster: c2\n  Engine: aurora-mysql 8.0.mysql_aurora.3.05.2\n  Status: SKIPPED — not applicable\n  Cluster has no DB instances"));

    // A missing cluster is an error in table form, a JSON object otherwise.
    let (result, stdout, _) = run_io(
        &[
            ("--engine", "mysql"),
            ("--cluster", "nope"),
            ("--format", "table"),
        ],
        &[],
        &mock,
    );
    assert_eq!(result.unwrap_err().message(), "cluster not found");
    assert_eq!(stdout, "");
    let (result, stdout, _) = run_io(&[("--engine", "mysql"), ("--cluster", "nope")], &[], &mock);
    result.unwrap();
    assert!(stdout.starts_with("{\n  \"cluster_id\": \"nope\",\n  \"error\": \"cluster not found\",\n  \"pricing_source\": {"));

    // Neither --cluster nor --all prints the help.
    let (result, stdout, _) = run_io(&[("--engine", "mysql")], &[], &mock);
    result.unwrap();
    assert_eq!(stdout, format!("{}\n", crate::USAGE));
    // An API failure for the cluster is a runtime error (exit 1).
    let (result, _, _) = run_io(&[("--engine", "mysql"), ("--cluster", "boom")], &[], &mock);
    assert_eq!(result.unwrap_err().exit_code(), 1);
}

#[test]
fn commitment_live_commands_with_a_mock() {
    let mut mock = commitment_mock();
    mock.clusters = vec![
        ("c1".to_string(), "aurora-postgresql".to_string()),
        ("other".to_string(), "mysql".to_string()),
    ];
    mock.described = vec![(
        "c1".to_string(),
        cluster("aurora", "aurora-postgresql", &["w"]),
    )];
    mock.classes = vec![("w".to_string(), Ok(vec!["db.r7g.2xlarge".to_string()]))];

    let all = invocation(
        &[],
        &["--all"],
        &[("--engine", "postgresql"), ("--region", "eu-west-1")],
    );
    let (result, stdout, _) = capture(|output| super::commitment::command(&all, output, &mock));
    result.unwrap();
    let parsed = parse_json(&stdout).expect("JSON");
    assert_eq!(text(&parsed, "region"), "eu-west-1");
    assert_eq!(num(&parsed, "cluster_count"), 1.0);
    assert_eq!(
        text(&parsed, "clusters.0.workloads.0.recommendation.best_option"),
        "3yr RI"
    );
    assert!(mock
        .calls()
        .contains(&"offerings:eu-west-1:db.r7g.2xlarge:aurora-postgresql".to_string()));

    let one = invocation(
        &[],
        &[],
        &[
            ("--engine", "postgresql"),
            ("--cluster", "c1"),
            ("--format", "table"),
        ],
    );
    let (result, stdout, _) = capture(|output| super::commitment::command(&one, output, &mock));
    result.unwrap();
    assert!(stdout.starts_with("Cluster: c1  (aurora-postgresql)  storage_type=aurora\n\n"));

    // --all wins over --cluster, and a listing failure is a runtime error.
    let both = invocation(
        &[],
        &["--all"],
        &[("--engine", "mysql"), ("--cluster", "c1")],
    );
    let (result, stdout, _) = capture(|output| super::commitment::command(&both, output, &mock));
    result.unwrap();
    assert!(stdout.contains("\"cluster_count\": 1"));
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

#[test]
fn aurora_commands_route_to_the_longest_matching_path() {
    use crate::{route, Routed, COMMANDS};
    let words =
        |values: &[&str]| -> Vec<String> { values.iter().map(|v| (*v).to_string()).collect() };
    for (args, path) in [
        (
            vec!["aurora", "acu", "estimate", "--engine", "mysql"],
            vec!["aurora", "acu", "estimate"],
        ),
        (
            vec!["aurora", "acu", "list-instances", "--offline"],
            vec!["aurora", "acu", "list-instances"],
        ),
        (
            vec!["aurora", "acu", "pricing-source"],
            vec!["aurora", "acu", "pricing-source"],
        ),
        (
            vec!["aurora", "acu", "--engine", "mysql"],
            vec!["aurora", "acu"],
        ),
        (
            vec!["aurora", "commitment", "offline"],
            vec!["aurora", "commitment"],
        ),
        (
            vec!["aurora", "io", "--cluster=c1", "--days=3"],
            vec!["aurora", "io"],
        ),
    ] {
        let Ok(Routed::Run(spec, _)) = route(COMMANDS, &words(&args)) else {
            panic!("{args:?} must route");
        };
        assert_eq!(spec.path, path.as_slice(), "{args:?}");
    }
    let Ok(Routed::Run(_, invocation)) = route(
        COMMANDS,
        &words(&[
            "aurora",
            "acu",
            "estimate",
            "--cpu-p95=-5",
            "--cpu-max",
            "-7",
            "--offline",
        ]),
    ) else {
        panic!("negative numbers are values");
    };
    assert_eq!(invocation.value("--cpu-p95"), Some("-5"));
    assert_eq!(invocation.value("--cpu-max"), Some("-7"));
    assert!(invocation.has_flag("--offline"));

    for args in [
        vec!["aurora"],
        vec!["aurora", "pricing"],
        vec!["aurora", "acu", "estimate", "--all"],
        vec!["aurora", "acu", "list-instances", "--format", "table"],
        vec!["aurora", "io", "--serverless"],
        vec!["aurora", "commitment", "offline", "extra"],
    ] {
        assert!(route(COMMANDS, &words(&args)).is_err(), "{args:?}");
    }
}
