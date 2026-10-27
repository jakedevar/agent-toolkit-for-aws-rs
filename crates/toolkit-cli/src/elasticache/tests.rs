//! Offline tests of the ElastiCache commands: fixtures only, no network, no
//! AWS client, no wall clock.

#![allow(clippy::items_after_statements)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, validate, Json, ObjectSchema, Schema};

use super::audit::{self, Audit};
use super::calculator;
use super::classifier::{
    classify_command, estimate_ecpus_from_commandstats, CommandClass, CommandStat,
};
use super::pricing::PricingLoader;
use super::references;
use super::serverless::{self, Cluster, CommandStats};
use super::text::{
    dumps_inline, escape_controls, group_int, py_sum, quoted, round_int, shell_quote, show,
    splitext, trunc_int, RoundError,
};
use super::{preflight, validator};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-elasticache-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `contents` to `name` (parents are created); returns the path.
    fn file(&self, name: &str, contents: &str) -> String {
        let full = self.0.join(name);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&full, contents).expect("write fixture");
        full.to_str().expect("utf-8 temp path").to_string()
    }

    fn at(&self, name: &str) -> String {
        self.0.join(name).to_str().expect("utf-8 path").to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exec(args: &[&str]) -> (u8, String, String) {
    let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = crate::run(&os_args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn json(text: &str) -> Json {
    parse_json(text).expect("fixture JSON")
}

fn member<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or_else(|| panic!("missing {key}"))
}

fn items(value: &Json) -> &[Json] {
    match value {
        Json::Array(list) => list,
        other => panic!("expected an array, got {}", other.type_name()),
    }
}

fn keys(value: &Json) -> Vec<String> {
    match value {
        Json::Object(members) => members.iter().map(|(key, _)| key.clone()).collect(),
        other => panic!("expected an object, got {}", other.type_name()),
    }
}

// ---------------------------------------------------------------------------
// Pricing fixtures
// ---------------------------------------------------------------------------

const BULK_JSON: &str = r#"{
 "products": {
  "N1": {"productFamily": "Cache Instance", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "instanceType": "cache.r7g.large", "cacheEngine": "Valkey", "usagetype": "USE1-NodeUsage:cache.r7g.large", "memory": "13.07 GiB"}},
  "N2": {"productFamily": "Cache Instance", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "instanceType": "cache.r7g.large", "cacheEngine": "Redis", "usagetype": "USE1-NodeUsage:cache.r7g.large", "memory": "13.07 GiB"}},
  "N3": {"productFamily": "Cache Instance", "attributes": {"regionCode": "eu-west-1", "locationType": "AWS Region", "instanceType": "cache.r7g.large", "cacheEngine": "Valkey", "usagetype": "EUW1-NodeUsage:cache.r7g.large", "memory": "13.07 GiB"}},
  "N4": {"productFamily": "Cache Instance", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Outposts", "instanceType": "cache.r7g.large", "cacheEngine": "Valkey", "usagetype": "USE1-NodeUsage:cache.r7g.large", "memory": "99 GiB"}},
  "S1": {"productFamily": "ElastiCache Serverless", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "cacheEngine": "Valkey", "usagetype": "USE1-ElastiCacheServerlessCachedData", "operation": ""}},
  "S2": {"productFamily": "ElastiCache Serverless", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "cacheEngine": "Valkey", "usagetype": "USE1-ElastiCacheProcessingUnits", "operation": ""}},
  "S3": {"productFamily": "ElastiCache Serverless", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "cacheEngine": "Valkey", "usagetype": "USE1-BackupUsage", "operation": "Snapshot"}},
  "X1": {"productFamily": "Cache Instance", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "instanceType": "cache.r7g.large", "cacheEngine": "Redis", "usagetype": "USE1-ExtendedSupportYr1_Yr2-NodeUsage:cache.r7g.large"}},
  "X2": {"productFamily": "Cache Instance", "attributes": {"regionCode": "us-east-1", "locationType": "AWS Region", "instanceType": "cache.r7g.large", "cacheEngine": "Redis", "usagetype": "USE1-ExtendedSupportYr3-NodeUsage:cache.r7g.large"}}
 },
 "terms": {
  "OnDemand": {
   "N1": {"N1.A": {"priceDimensions": {"N1.A.1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.1000000000"}}}}},
   "N2": {"N2.A": {"priceDimensions": {"N2.A.1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.1200000000"}}}}},
   "N3": {"N3.A": {"priceDimensions": {"N3.A.1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.2000000000"}}}}},
   "N4": {"N4.A": {"priceDimensions": {"N4.A.1": {"unit": "Hrs", "pricePerUnit": {"USD": "9.9000000000"}}}}},
   "S1": {"S1.A": {"priceDimensions": {"S1.A.1": {"unit": "GB-Hours", "pricePerUnit": {"USD": "0.0840000000"}}}}},
   "S2": {"S2.A": {"priceDimensions": {"S2.A.1": {"unit": "ElastiCacheProcessingUnit", "pricePerUnit": {"USD": "0.0000000034"}}}}},
   "S3": {"S3.A": {"priceDimensions": {"S3.A.1": {"unit": "GB-Hours", "pricePerUnit": {"USD": "0.0210000000"}}}}},
   "X1": {"X1.A": {"priceDimensions": {"X1.A.1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.0500000000"}}}}},
   "X2": {"X2.A": {"priceDimensions": {"X2.A.1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.1000000000"}}}}}
  },
  "Reserved": {
   "N1": {
    "N1.R1": {"termAttributes": {"LeaseContractLength": "1yr", "PurchaseOption": "No Upfront"}, "priceDimensions": {"d1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.0700000000"}}}},
    "N1.R2": {"termAttributes": {"LeaseContractLength": "1yr", "PurchaseOption": "All Upfront"}, "priceDimensions": {"d1": {"unit": "Quantity", "pricePerUnit": {"USD": "600.0000000000"}}, "d2": {"unit": "Hrs", "pricePerUnit": {"USD": "0.0000000000"}}}},
    "N1.R3": {"termAttributes": {"LeaseContractLength": "3yr", "PurchaseOption": "Partial Upfront"}, "priceDimensions": {"d1": {"unit": "Quantity", "pricePerUnit": {"USD": "1080.0000000000"}}, "d2": {"unit": "Hrs", "pricePerUnit": {"USD": "0.0300000000"}}}},
    "N1.R4": {"termAttributes": {"LeaseContractLength": "1yr", "PurchaseOption": "Heavy Utilization"}, "priceDimensions": {"d1": {"unit": "Hrs", "pricePerUnit": {"USD": "0.0100000000"}}}}
   }
  }
 }
}"#;

const PRICING_CSV: &str =
    "Product Family,Region Code,Cache Engine,TermType,PricePerUnit,Instance Type,Unit,Operation\n\
Cache Instance,us-east-1,Valkey,OnDemand,0.1,cache.r7g.large,Hrs,\n\
Cache Instance,us-east-1,Valkey,Reserved,0.05,cache.r7g.large,Hrs,\n\
Cache Instance,us-east-1,Valkey,OnDemand,0,cache.free,Hrs,\n\
ElastiCache Serverless,us-east-1,Valkey,OnDemand,0.084,,GB-Hours,\n\
ElastiCache Serverless,us-east-1,Valkey,OnDemand,0.0000000034,,ElastiCacheProcessingUnit,\n\
ElastiCache Serverless,us-east-1,Valkey,OnDemand,0.095,,GB-months,\n\
ElastiCache Serverless,us-east-1,Valkey,OnDemand,0.021,,GB-Hours,Snapshot\n\
ElastiCache Serverless,us-east-1,Memcached,OnDemand,0.125,,GB-Hours,\n\
ElastiCache Serverless,us-east-1,Memcached,OnDemand,0.0000000034,,ElastiCacheProcessingUnit,\n";

fn bulk_loader() -> PricingLoader {
    PricingLoader::from_bulk_pricing(&json(BULK_JSON))
}

fn csv_loader() -> PricingLoader {
    PricingLoader::from_csv_text(PRICING_CSV)
}

fn rendered(fields: Vec<(String, Json)>) -> String {
    crate::aurora::pyfmt::dumps(&Json::Object(fields))
}

// ---------------------------------------------------------------------------
// text helpers
// ---------------------------------------------------------------------------

#[test]
fn rounding_is_half_to_even_like_python() {
    assert_eq!(round_int(0.5), Ok(0));
    assert_eq!(round_int(1.5), Ok(2));
    assert_eq!(round_int(2.5), Ok(2));
    assert_eq!(round_int(-0.5), Ok(0));
    assert_eq!(round_int(-1.5), Ok(-2));
    assert_eq!(round_int(31.5068), Ok(32));
    assert_eq!(round_int(f64::NAN), Err(RoundError::Nan));
    assert_eq!(round_int(f64::INFINITY), Err(RoundError::Overflow));
    assert_eq!(round_int(1e30), Err(RoundError::Overflow));
    assert_eq!(trunc_int(-2.9), Ok(-2));
    assert_eq!(trunc_int(2.9), Ok(2));
    assert_eq!(trunc_int(f64::NEG_INFINITY), Err(RoundError::Overflow));
}

#[test]
fn compensated_sum_matches_cpython_for_repeating_decimals() {
    let tenths = [0.1_f64; 10];
    assert_eq!(py_sum(&tenths), 1.0);
    assert_eq!(py_sum(&[]), 0.0);
    assert_eq!(py_sum(&[1e100, 1.0, -1e100]), 1.0);
}

#[test]
fn grouping_and_splitting_helpers() {
    assert_eq!(group_int(0), "0");
    assert_eq!(group_int(999), "999");
    assert_eq!(group_int(1000), "1,000");
    assert_eq!(group_int(-1_234_567), "-1,234,567");
    assert_eq!(splitext("out/est.csv"), ("out/est", ".csv"));
    assert_eq!(splitext("est"), ("est", ""));
    assert_eq!(splitext(".hidden"), (".hidden", ""));
    assert_eq!(splitext("a.b/c"), ("a.b/c", ""));
    assert_eq!(splitext("x.tar.gz"), ("x.tar", ".gz"));
    assert_eq!(
        dumps_inline(&json(r#"{"a":[1,2.5],"b":"x","c":{}}"#)),
        r#"{"a": [1, 2.5], "b": "x", "c": {}}"#
    );
}

#[test]
fn untrusted_text_is_escaped_and_bounded() {
    assert_eq!(escape_controls("a\u{1b}[2Jb\nc"), "a\\x1b[2Jb\\x0ac");
    assert_eq!(escape_controls("x\u{202e}y"), "x\\u202ey");
    assert_eq!(show("plain"), "plain");
    let long = "x".repeat(300);
    let shown = show(&long);
    assert_eq!(shown.chars().count(), 123);
    assert!(shown.ends_with("..."));
    assert_eq!(quoted("it's"), "'it\\'s'");
    assert_eq!(shell_quote("path/to-file.json"), "path/to-file.json");
    assert_eq!(shell_quote("with space"), "'with space'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote("a\nb"), "'a\\x0ab'");
    assert_eq!(shell_quote(""), "''");
}

// ---------------------------------------------------------------------------
// pricing (shared price tables)
// ---------------------------------------------------------------------------

#[test]
fn bulk_pricing_is_parsed_into_every_table() {
    let loader = bulk_loader();
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.r7g.large", "valkey"),
        Ok(0.1)
    );
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.r7g.large", "Redis"),
        Ok(0.12),
        "engine lookups are case-insensitive"
    );
    assert_eq!(
        loader.get_node_hourly_rate("eu-west-1", "cache.r7g.large", "valkey"),
        Ok(0.2)
    );
    assert_eq!(loader.get_node_memory_gb("cache.r7g.large"), 13.07);
    assert_eq!(loader.get_node_memory_gb("cache.unknown"), 0.0);
    assert_eq!(
        loader.get_serverless_storage_rate("us-east-1", "valkey"),
        Ok(0.084)
    );
    assert_eq!(
        loader.get_serverless_ecpu_rate("us-east-1", "valkey"),
        Ok(3.4e-9)
    );
    assert!(loader.has_extended_support_pricing());
    for (year, price) in [("1", 0.05), ("2", 0.05), ("3", 0.1)] {
        assert_eq!(
            loader.get_extended_support_rate("us-east-1", "cache.r7g.large", "redis", year),
            Ok(price),
            "year {year}"
        );
    }
}

#[test]
fn outpost_and_snapshot_rows_are_ignored() {
    let loader = bulk_loader();
    // The Outposts row (9.9) comes after the region row and must not win.
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.r7g.large", "valkey"),
        Ok(0.1)
    );
    // The snapshot row (0.021) must not replace the storage price.
    assert_eq!(
        loader.get_serverless_storage_rate("us-east-1", "valkey"),
        Ok(0.084)
    );
}

#[test]
fn reserved_options_are_sorted_by_effective_monthly_cost() {
    let loader = bulk_loader();
    let options = loader.list_reserved_options("us-east-1", "cache.r7g.large", "valkey");
    let summary: Vec<(String, String, f64)> = options
        .iter()
        .map(|o| {
            (
                o.lease_length.clone(),
                o.purchase_option.clone(),
                o.effective_monthly,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("1yr".to_string(), "All Upfront".to_string(), 50.0),
            ("1yr".to_string(), "No Upfront".to_string(), 51.1),
            ("3yr".to_string(), "Partial Upfront".to_string(), 51.9),
        ],
        "legacy Heavy Utilization is skipped"
    );
    assert_eq!(options[0].upfront, 600.0);
    assert_eq!(options[0].hourly, 0.0);
    assert_eq!(
        loader.get_reserved_effective_monthly(
            "us-east-1",
            "cache.r7g.large",
            "valkey",
            "1yr",
            "No Upfront"
        ),
        Ok(51.1)
    );
    let missing = loader
        .get_reserved_effective_monthly(
            "us-east-1",
            "cache.r7g.large",
            "valkey",
            "3yr",
            "All Upfront",
        )
        .expect_err("not published");
    assert_eq!(
        missing,
        "No reserved pricing found for region=us-east-1, instance_type=cache.r7g.large, engine=valkey, lease=3yr, purchase=All Upfront"
    );
    assert!(loader
        .list_reserved_options("us-east-1", "cache.none", "valkey")
        .is_empty());
}

#[test]
fn missing_prices_are_reported_with_the_python_messages() {
    let loader = bulk_loader();
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.none", "valkey"),
        Err(
            "No node pricing found for region=us-east-1, instance_type=cache.none, engine=valkey"
                .to_string()
        )
    );
    assert_eq!(
        loader.get_serverless_storage_rate("us-east-1", "memcached"),
        Err("No serverless storage pricing found for region=us-east-1, engine=memcached. Available: none loaded"
            .to_string())
    );
    assert_eq!(
        loader.get_serverless_ecpu_rate("ap-south-1", "valkey"),
        Err("No serverless ECPU pricing found for region=ap-south-1, engine=valkey. Available: us-east-1"
            .to_string())
    );
    assert_eq!(
        loader.get_extended_support_rate("us-east-1", "cache.none", "redis", "1"),
        Err("No Extended Support pricing found for region=us-east-1, instance_type=cache.none, engine=redis, year=1. This may mean Extended Support pricing is not published for this combination."
            .to_string())
    );
}

#[test]
fn hostile_lookup_values_are_escaped_in_messages() {
    let loader = bulk_loader();
    let message = loader
        .get_node_hourly_rate("us\u{1b}[2J-FORGED\nline", "cache.none", "valkey")
        .expect_err("unknown region");
    assert!(!message.contains('\u{1b}'));
    assert!(!message.contains('\n'));
    assert!(message.contains("\\x1b[2J-FORGED\\x0aline"));
}

#[test]
fn available_regions_are_sorted_and_capped_at_five() {
    let cache = json(
        r#"{"serverless_storage": {
            "us-east-1|valkey": 0.1, "eu-west-1|valkey": 0.1, "ap-south-1|valkey": 0.1,
            "us-west-2|valkey": 0.1, "eu-central-1|valkey": 0.1, "ca-central-1|valkey": 0.1,
            "sa-east-1|valkey": 0.1, "us-west-1|redis": 0.1}}"#,
    );
    let loader = PricingLoader::from_cache(&cache);
    let message = loader
        .get_serverless_storage_rate("mars-1", "valkey")
        .expect_err("unknown region");
    assert_eq!(
        message,
        "No serverless storage pricing found for region=mars-1, engine=valkey. Available: ap-south-1, ca-central-1, eu-central-1, eu-west-1, sa-east-1 and 2 more"
    );
}

#[test]
fn csv_pricing_reads_on_demand_node_and_serverless_rows_only() {
    let loader = csv_loader();
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.r7g.large", "valkey"),
        Ok(0.1)
    );
    assert!(
        loader
            .get_node_hourly_rate("us-east-1", "cache.free", "valkey")
            .is_err(),
        "a zero price is skipped"
    );
    assert_eq!(
        loader.get_serverless_storage_rate("us-east-1", "valkey"),
        Ok(0.084),
        "GB-months and Snapshot rows are skipped"
    );
    assert_eq!(
        loader.get_serverless_ecpu_rate("us-east-1", "memcached"),
        Ok(3.4e-9)
    );
    assert!(!loader.has_extended_support_pricing());
    assert!(loader
        .list_reserved_options("us-east-1", "cache.r7g.large", "valkey")
        .is_empty());
    assert_eq!(loader.get_node_memory_gb("cache.r7g.large"), 0.0);
}

#[test]
fn compact_cache_round_trips_and_drops_malformed_keys() {
    let cache = json(
        r#"{
        "node_pricing": {"us-east-1|cache.m|valkey": 0.5, "bad|key": 1.0, "a|b|c|d": 2.0, "us-east-1|cache.n|redis": "x"},
        "serverless_storage": {"us-east-1|valkey": 0.08},
        "serverless_ecpu": {"us-east-1|valkey": 4e-9},
        "extended_support": {"us-east-1|cache.m|redis|1": 0.02},
        "reserved_pricing": {"us-east-1|cache.m|valkey|1yr|No Upfront": {"hourly": 0.3, "upfront": 0.0}},
        "node_memory": {"cache.m": 6.38},
        "fetched_at": 1.0}"#,
    );
    let loader = PricingLoader::from_json(&cache);
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.m", "valkey"),
        Ok(0.5)
    );
    assert!(loader
        .get_node_hourly_rate("us-east-1", "cache.n", "redis")
        .is_err());
    assert_eq!(
        loader.get_serverless_ecpu_rate("us-east-1", "valkey"),
        Ok(4e-9)
    );
    assert_eq!(
        loader.get_extended_support_rate("us-east-1", "cache.m", "redis", "1"),
        Ok(0.02)
    );
    assert_eq!(loader.get_node_memory_gb("cache.m"), 6.38);
    // 0.3 * 730 = 219.0
    assert_eq!(
        loader.get_reserved_effective_monthly(
            "us-east-1",
            "cache.m",
            "valkey",
            "1yr",
            "No Upfront"
        ),
        Ok(219.0)
    );
}

#[test]
fn pricing_files_are_read_through_the_shared_path_contract() {
    let dir = TempDir::new("pricing-files");
    let bulk = dir.file("bulk.json", BULK_JSON);
    let loader = PricingLoader::load_file(Path::new(&bulk), super::pricing::PricingKind::Json)
        .expect("bulk file loads");
    assert_eq!(
        loader.get_node_hourly_rate("us-east-1", "cache.r7g.large", "valkey"),
        Ok(0.1)
    );
    let missing = PricingLoader::load_file(
        &dir.path().join("absent.csv"),
        super::pricing::PricingKind::Csv,
    )
    .expect_err("missing file");
    assert!(missing.message().contains("does not exist"));
    let broken = dir.file("broken.json", "{not json");
    let error = PricingLoader::load_file(Path::new(&broken), super::pricing::PricingKind::Json)
        .expect_err("invalid JSON");
    assert_eq!(error.code(), "parse_error");
    #[cfg(unix)]
    {
        let link = dir.at("link.json");
        std::os::unix::fs::symlink(&bulk, &link).expect("symlink");
        let error = PricingLoader::load_file(Path::new(&link), super::pricing::PricingKind::Json)
            .expect_err("links are refused");
        assert_eq!(error.code(), "path_symlink");
    }
}

// ---------------------------------------------------------------------------
// command classifier
// ---------------------------------------------------------------------------

#[test]
fn commands_are_classified_case_insensitively() {
    assert_eq!(classify_command("GET"), CommandClass::Fixed);
    assert_eq!(classify_command("  hset "), CommandClass::Fixed);
    assert_eq!(classify_command("Auth"), CommandClass::Internal);
    assert_eq!(classify_command("INFO"), CommandClass::Internal);
    assert_eq!(classify_command("zadd"), CommandClass::Nonfixed);
    assert_eq!(classify_command("mget"), CommandClass::Nonfixed);
    assert_eq!(classify_command("no-such-command"), CommandClass::Nonfixed);
    assert_eq!(classify_command(""), CommandClass::Nonfixed);
    assert_eq!(CommandClass::Fixed.label(), "fixed");
    assert_eq!(CommandClass::Nonfixed.label(), "nonfixed");
    assert_eq!(CommandClass::Internal.label(), "internal");
}

fn stats(entries: &[(&str, i64, i64)]) -> Vec<(String, CommandStat)> {
    entries
        .iter()
        .map(|(name, calls, usec)| {
            (
                (*name).to_string(),
                CommandStat {
                    calls: *calls,
                    usec: *usec,
                },
            )
        })
        .collect()
}

#[test]
fn nonfixed_commands_take_the_higher_of_calls_and_cpu_time() {
    let estimate = estimate_ecpus_from_commandstats(&stats(&[
        ("get", 100, 200),
        ("zadd", 50, 300),
        ("eval", 500, 30),
        ("sort", 100, 300),
        ("info", 7, 70),
        ("auth", 3, 1),
    ]))
    .expect("finite");
    // fixed: 100; nonfixed: max(50,100) + max(500,10) + max(100,100) = 700.
    assert_eq!(estimate.fixed_ecpus, 100);
    assert_eq!(estimate.nonfixed_ecpus, 700);
    assert_eq!(estimate.total_ecpus, 800);
    assert_eq!(estimate.internal_calls, 10);
    let order: Vec<(&str, i64)> = estimate
        .command_breakdown
        .iter()
        .map(|entry| (entry.command.as_str(), entry.ecpus))
        .collect();
    assert_eq!(
        order,
        vec![
            ("eval", 500),
            ("get", 100),
            ("zadd", 100),
            ("sort", 100),
            ("info", 0),
            ("auth", 0)
        ],
        "most ECPUs first, ties keep input order"
    );
}

#[test]
fn nonfixed_estimates_round_half_to_even() {
    // 5 usec / 3 = 1.67 -> max(1, 1.67) -> round 2; 1.5 would round to 2, 2.5 to 2.
    let estimate = estimate_ecpus_from_commandstats(&stats(&[("zadd", 1, 5)])).expect("ok");
    assert_eq!(estimate.nonfixed_ecpus, 2);
    let half = estimate_ecpus_from_commandstats(&stats(&[("zadd", 0, 15)])).expect("ok");
    assert_eq!(half.nonfixed_ecpus, 5);
    let tie =
        estimate_ecpus_from_commandstats(&stats(&[("zadd", 2, 0), ("zrem", 0, 1)])).expect("ok");
    assert_eq!(tie.nonfixed_ecpus, 2, "2 + 0.33 = 2.33 rounds to 2");
    assert_eq!(
        estimate_ecpus_from_commandstats(&[])
            .expect("empty is fine")
            .total_ecpus,
        0
    );
}

#[test]
fn raw_info_commandstats_text_is_parsed() {
    let text = "# Commandstats\r\ncmdstat_get:calls=1000,usec=1500,usec_per_call=1.50,rejected_calls=0,failed_calls=0\r\n\r\ncmdstat_Config|get:calls=3,usec=9\r\ncmdstat_set:calls=5,usec=7\n  cmdstat_zadd:calls=2,usec=4\ngarbage\ncmdstat_bad:calls=x,usec=1\ncmdstat_get:calls=9,usec=8\n";
    let parsed = serverless::parse_commandstats_info(text);
    assert_eq!(
        parsed,
        vec![
            ("get".to_string(), CommandStat { calls: 9, usec: 8 }),
            ("set".to_string(), CommandStat { calls: 5, usec: 7 }),
            ("zadd".to_string(), CommandStat { calls: 2, usec: 4 }),
        ],
        "a later line replaces an earlier one; malformed lines are skipped"
    );
}

// ---------------------------------------------------------------------------
// price calculator
// ---------------------------------------------------------------------------

#[test]
fn serverless_estimate_matches_the_python_formulas() {
    let loader = bulk_loader();
    let fields = calculator::estimate_serverless(&loader, "valkey", "us-east-1", 5.0, 200.0, 1.0)
        .expect("prices exist");
    assert_eq!(
        rendered(fields),
        "{\n  \"model\": \"serverless\",\n  \"engine\": \"valkey\",\n  \"region\": \"us-east-1\",\n  \"data_gb\": 5.0,\n  \"ecpu_millions\": 200.0,\n  \"storage_monthly\": 306.6,\n  \"compute_monthly\": 0.68,\n  \"total_monthly\": 307.28\n}"
    );
}

#[test]
fn serverless_minimum_storage_and_payload_scaling() {
    let loader = bulk_loader();
    // valkey bills at least 0.1 GB: 0.1 * 0.084 * 730 = 6.132
    let small = calculator::estimate_serverless(&loader, "valkey", "us-east-1", 0.01, 0.0, 1.0)
        .expect("prices exist");
    assert_eq!(
        member(&Json::Object(small), "storage_monthly"),
        &Json::Float(6.13)
    );

    // Payloads above 1 KB scale the ECPUs linearly and add two fields.
    let scaled = Json::Object(
        calculator::estimate_serverless(&loader, "valkey", "us-east-1", 1.0, 100.0, 3.2)
            .expect("prices exist"),
    );
    assert_eq!(member(&scaled, "avg_payload_kb"), &Json::Float(3.2));
    assert_eq!(member(&scaled, "ecpu_millions_scaled"), &Json::Float(320.0));
    // Payloads of 1 KB or less add nothing, even below 1.
    let tiny = Json::Object(
        calculator::estimate_serverless(&loader, "valkey", "us-east-1", 1.0, 100.0, 0.5)
            .expect("prices exist"),
    );
    assert_eq!(member(&tiny, "avg_payload_kb"), &Json::Float(0.5));
    assert_eq!(member(&tiny, "ecpu_millions_scaled"), &Json::Float(100.0));
}

#[test]
fn serverless_estimate_without_a_price_is_an_error() {
    let loader = bulk_loader();
    let message = calculator::estimate_serverless(&loader, "memcached", "us-east-1", 1.0, 1.0, 1.0)
        .expect_err("no memcached prices");
    assert!(message.starts_with("No serverless storage pricing found"));
}

#[test]
fn node_estimate_with_a_reserved_comparison() {
    let loader = bulk_loader();
    let fields = calculator::estimate_node_based(
        &loader,
        "valkey",
        "us-east-1",
        "cache.r7g.large",
        2,
        Some("1yr_no_upfront"),
    )
    .expect("priced");
    assert_eq!(
        rendered(fields),
        "{\n  \"model\": \"node-based (on-demand)\",\n  \"engine\": \"valkey\",\n  \"region\": \"us-east-1\",\n  \"node_type\": \"cache.r7g.large\",\n  \"node_count\": 2,\n  \"memory_per_node_gb\": 13.07,\n  \"total_memory_gb\": 26.1,\n  \"per_node_monthly\": 73.0,\n  \"total_monthly\": 146.0,\n  \"reserved\": {\n    \"term\": \"1yr_no_upfront\",\n    \"discount_pct\": 30,\n    \"per_node_monthly\": 51.1,\n    \"total_monthly\": 102.2,\n    \"annual_savings\": 525.6,\n    \"source\": \"live\"\n  }\n}"
    );
}

#[test]
fn node_estimate_edge_cases() {
    let loader = bulk_loader();
    // Unknown memory prints "unknown" for both memory fields.
    let cache = json(r#"{"node_pricing": {"us-east-1|cache.x|valkey": 0.2}}"#);
    let bare = PricingLoader::from_cache(&cache);
    let unknown = Json::Object(
        calculator::estimate_node_based(&bare, "valkey", "us-east-1", "cache.x", 1, None)
            .expect("priced"),
    );
    assert_eq!(
        member(&unknown, "memory_per_node_gb"),
        &Json::string("unknown")
    );
    assert_eq!(
        member(&unknown, "total_memory_gb"),
        &Json::string("unknown")
    );
    assert!(unknown.get("reserved").is_none());

    // A missing on-demand price is reported inside the result.
    let missing = Json::Object(
        calculator::estimate_node_based(&loader, "valkey", "us-east-1", "cache.none", 1, None)
            .expect("reported in the result"),
    );
    assert_eq!(
        member(&missing, "error"),
        &Json::string(
            "No node pricing found for region=us-east-1, instance_type=cache.none, engine=valkey"
        )
    );

    // A reserved term without a published price is silently left out.
    let no_ri = Json::Object(
        calculator::estimate_node_based(
            &loader,
            "valkey",
            "us-east-1",
            "cache.r7g.large",
            1,
            Some("3yr_all_upfront"),
        )
        .expect("priced"),
    );
    assert!(no_ri.get("reserved").is_none());

    // Zero nodes divides by zero in the discount, as the script's ZeroDivisionError.
    let error = calculator::estimate_node_based(
        &loader,
        "valkey",
        "us-east-1",
        "cache.r7g.large",
        0,
        Some("1yr_no_upfront"),
    )
    .expect_err("zero nodes");
    assert_eq!(error, "float division by zero");
}

#[test]
fn reserved_options_report_layout() {
    let loader = bulk_loader();
    let report =
        calculator::format_ri_options(&loader, "valkey", "us-east-1", "cache.r7g.large", 2)
            .expect("priced");
    let row = |term: &str, option: &str, monthly: &str, upfront: &str, savings: i64| {
        format!("  {term:<10} {option:<18} ${monthly:>9} ${upfront:>8} {savings:>8}%")
    };
    let expected = [
        "=".repeat(68),
        "Reserved Pricing Options (live pricing)".to_string(),
        "=".repeat(68),
        String::new(),
        "  Node type: cache.r7g.large  x 2  (valkey engine)".to_string(),
        "  On-demand: $146.00/month".to_string(),
        String::new(),
        format!(
            "  {:<10} {:<18} {:>12} {:>10} {:>10}",
            "Term", "Purchase Option", "Monthly", "Upfront", "Savings"
        ),
        format!(
            "  {} {} {} {} {}",
            "-".repeat(10),
            "-".repeat(18),
            "-".repeat(12),
            "-".repeat(10),
            "-".repeat(10)
        ),
        row("1yr", "All Upfront", "100.00", "1,200", 32),
        row("1yr", "No Upfront", "102.20", "0", 30),
        row("3yr", "Partial Upfront", "103.80", "2,160", 29),
        String::new(),
        "  Estimates only. Verify current pricing at https://aws.amazon.com/elasticache/pricing/"
            .to_string(),
        String::new(),
    ]
    .join("\n");
    assert_eq!(report, expected);
}

#[test]
fn reserved_options_report_without_options_or_prices() {
    let loader = bulk_loader();
    let report = calculator::format_ri_options(&loader, "redis", "us-east-1", "cache.r7g.large", 1)
        .expect("on-demand price exists");
    assert!(report.contains("  No reserved pricing options found for this configuration."));
    assert!(report.ends_with("pricing/\n"));
    let error = calculator::format_ri_options(&loader, "redis", "us-east-1", "cache.none", 1)
        .expect_err("no on-demand price");
    assert!(error.starts_with("No node pricing found"));
    let zero = calculator::format_ri_options(&loader, "valkey", "us-east-1", "cache.r7g.large", 0)
        .expect_err("zero nodes divide by zero");
    assert_eq!(zero, "float division by zero");
}

#[test]
fn extended_support_estimate_and_report() {
    let loader = bulk_loader();
    let result =
        calculator::estimate_extended_support(&loader, "redis", "us-east-1", "cache.r7g.large", 6);
    let years: Vec<(i64, f64, f64)> = result
        .years
        .iter()
        .map(|y| (y.year, y.total_monthly, y.total_annual))
        .collect();
    assert_eq!(
        years,
        vec![(1, 219.0, 2628.0), (2, 219.0, 2628.0), (3, 438.0, 5256.0)]
    );
    assert_eq!(result.base_monthly, Some(525.6));
    assert_eq!(result.total_with_surcharge_monthly, Some(744.6));
    let report = calculator::format_extended_support_report(&result);
    assert!(report.contains(&format!(
        "  Year {:<13} ${:>13} ${:>13}",
        1, "219.00", "2,628.00"
    )));
    assert!(report.contains(&format!(
        "  Year {:<13} ${:>13} ${:>13}",
        3, "438.00", "5,256.00"
    )));
    assert!(report.contains(&format!("  Base node cost:       ${:>13}/month", "525.60")));
    assert!(report.contains(&format!("  Total (base + Yr1):   ${:>13}/month", "744.60")));
    assert!(report.contains("Migrating to Valkey eliminates Extended Support charges."));
}

#[test]
fn extended_support_without_published_prices() {
    let none = calculator::estimate_extended_support(
        &csv_loader(),
        "redis",
        "us-east-1",
        "cache.r7g.large",
        2,
    );
    assert!(none.years.is_empty());
    let report = calculator::format_extended_support_report(&none);
    assert!(report.contains(
        "  Extended Support pricing not available in bulk pricing API. Check the ElastiCache pricing page."
    ));
    let unlisted = calculator::estimate_extended_support(
        &bulk_loader(),
        "redis",
        "us-east-1",
        "cache.other",
        2,
    );
    assert!(unlisted.years.is_empty() && unlisted.note.is_none());
    assert!(calculator::format_extended_support_report(&unlisted)
        .contains("Extended Support Cost Estimate"));
}

#[test]
fn ops_per_second_convert_to_millions_of_ecpus() {
    // 100 ops/s * 3600 * 730 = 262.8M
    assert_eq!(calculator::estimate_ecpu_from_ops(100.0, 1.0), 262.8);
    assert_eq!(calculator::estimate_ecpu_from_ops(0.0, 1.0), 0.0);
    assert_eq!(calculator::estimate_ecpu_from_ops(1.0, 2.0), 5.3);
}

// ---------------------------------------------------------------------------
// serverless estimator
// ---------------------------------------------------------------------------

fn cluster(name: &str) -> Cluster {
    Cluster {
        cluster_name: name.to_string(),
        instance_type: "cache.r7g.large".to_string(),
        region: "us-east-1".to_string(),
        engine: "valkey".to_string(),
        node_count: 2,
        primary_nodes: 1,
        avg_memory_gb: 5.0,
        daily_commands: 1_000_000.0,
        current_monthly_cost: 0.0,
        avg_payload_kb: 1.0,
        peak_commands: 0.0,
        min_commands: 0.0,
        peak_to_avg_ratio: 0.0,
        peak_memory_gb: 0.0,
        min_memory_gb: 0.0,
    }
}

#[test]
fn cluster_csv_defaults_and_validation() {
    let clusters = serverless::load_clusters(
        "cluster_name,instance_type,region,engine,node_count,primary_nodes,avg_memory_gb,daily_commands,current_monthly_cost\n orders ,cache.r7g.large,us-east-1,Valkey,5,,2.5,1000,\nbare,,,,,,,,\n",
    );
    // Empty node_count is not an integer: the script's int('') raised.
    assert!(clusters.is_err());

    let clusters = serverless::load_clusters(
        "cluster_name,instance_type,region,engine,node_count,primary_nodes,avg_memory_gb,daily_commands,current_monthly_cost,avg_payload_kb\n orders ,cache.r7g.large,us-east-1,Valkey,5,0,2.5,1000,,\nsingle,,eu-west-1,redis,1,0,0,0,12.5,3.5\n",
    )
    .expect("valid");
    assert_eq!(clusters[0].cluster_name, "orders");
    assert_eq!(clusters[0].engine, "valkey");
    assert_eq!(clusters[0].primary_nodes, 2, "half of 5 nodes, floored");
    assert_eq!(clusters[0].current_monthly_cost, 0.0);
    assert_eq!(clusters[0].avg_payload_kb, 1.0, "an empty payload is 1.0");
    assert_eq!(clusters[1].primary_nodes, 1, "at least one primary");
    assert_eq!(clusters[1].current_monthly_cost, 12.5);
    assert_eq!(clusters[1].avg_payload_kb, 3.5);

    // Missing optional columns take the defaults.
    let minimal = serverless::load_clusters("cluster_name\nonly\n").expect("valid");
    assert_eq!(minimal[0].node_count, 1);
    assert_eq!(minimal[0].region, "us-east-1");
    assert_eq!(minimal[0].engine, "valkey");
    assert_eq!(minimal[0].avg_memory_gb, 0.0);

    let missing = serverless::load_clusters("instance_type\ncache.x\n").expect_err("no name");
    assert!(missing.contains("cluster_name"));
    let bad =
        serverless::load_clusters("cluster_name,avg_memory_gb\nx,lots\n").expect_err("bad float");
    assert_eq!(bad, "row 1: could not convert string to float: 'lots'");
    let hostile =
        serverless::load_clusters("cluster_name,node_count\nx,\u{1b}[2J9\n").expect_err("bad int");
    assert!(!hostile.contains('\u{1b}'));
    assert!(hostile.contains("invalid literal for int() with base 10: '\\x1b[2J9'"));
}

#[test]
fn commandstats_csv_is_normalized_by_uptime() {
    let parsed = serverless::parse_commandstats_file(
        "cluster_name,command,calls,usec,uptime_days\norders,GET,1000,2000,10\norders,zadd,500,3000,10\nplain,get,40,50,\nplain,get,41,51,\n",
    )
    .expect("valid");
    assert_eq!(parsed.len(), 2);
    let (name, orders) = &parsed[0];
    assert_eq!(name, "orders");
    assert!(orders.normalized);
    assert_eq!(
        orders.commands,
        vec![
            (
                "get".to_string(),
                CommandStat {
                    calls: 100,
                    usec: 200
                }
            ),
            (
                "zadd".to_string(),
                CommandStat {
                    calls: 50,
                    usec: 300
                }
            ),
        ]
    );
    let (_, plain) = &parsed[1];
    assert!(!plain.normalized);
    assert_eq!(
        plain.commands,
        vec![(
            "get".to_string(),
            CommandStat {
                calls: 41,
                usec: 51
            }
        )],
        "the later row replaces the command"
    );
    // A non-positive uptime leaves the numbers raw; an invalid one is an error.
    let raw = serverless::parse_commandstats_file(
        "cluster_name,command,calls,usec,uptime_days\nx,get,9,9,0\n",
    )
    .expect("valid");
    assert!(!raw[0].1.normalized);
    assert_eq!(raw[0].1.commands[0].1.calls, 9);
    assert!(serverless::parse_commandstats_file(
        "cluster_name,command,calls,usec,uptime_days\nx,get,9,9,abc\n"
    )
    .is_err());
    assert!(serverless::parse_commandstats_file(
        "cluster_name,command,calls,usec,uptime_days\nx,get,9,9,1e-320\n"
    )
    .is_err());
    // Truncation toward zero: 7 / 2 = 3.
    let truncated = serverless::parse_commandstats_file(
        "cluster_name,command,calls,usec,uptime_days\nx,get,7,7,2\n",
    )
    .expect("valid");
    assert_eq!(
        truncated[0].1.commands[0].1,
        CommandStat { calls: 3, usec: 3 }
    );
}

fn estimate(
    cluster: &Cluster,
    stats: Option<&CommandStats>,
) -> (serverless::ClusterResult, Vec<String>) {
    let mut warnings = Vec::new();
    let result =
        serverless::estimate_cluster(cluster, &csv_loader(), stats, &mut warnings).expect("priced");
    (result, warnings)
}

#[test]
fn simple_mode_estimate_uses_list_price_for_the_current_cost() {
    let (result, warnings) = estimate(&cluster("orders"), None);
    assert!(warnings.is_empty());
    let value = result.to_json();
    assert_eq!(member(&value, "current_monthly_cost"), &Json::Float(146.0));
    assert_eq!(member(&value, "cost_source"), &Json::string("list_price"));
    assert_eq!(member(&value, "billing_memory_gb"), &Json::Float(5.0));
    assert_eq!(member(&value, "monthly_gb_hours"), &Json::Float(3650.0));
    assert_eq!(
        member(&value, "storage_rate_per_gb_hour"),
        &Json::Float(0.084)
    );
    assert_eq!(member(&value, "storage_cost"), &Json::Float(306.6));
    assert_eq!(member(&value, "monthly_ecpus"), &Json::Int(30_000_000));
    assert_eq!(
        member(&value, "ecpu_rate_per_million"),
        &Json::Float(0.0034)
    );
    assert_eq!(member(&value, "ecpu_cost"), &Json::Float(0.1));
    assert_eq!(member(&value, "ecpu_mode"), &Json::string("simple"));
    assert_eq!(member(&value, "fixed_ecpus"), &Json::Null);
    assert_eq!(member(&value, "nonfixed_ecpus"), &Json::Null);
    assert_eq!(member(&value, "internal_calls_excluded"), &Json::Null);
    assert_eq!(member(&value, "serverless_total"), &Json::Float(306.7));
    assert_eq!(member(&value, "diff"), &Json::Float(160.7));
    assert_eq!(member(&value, "diff_pct"), &Json::Float(110.1));
    assert_eq!(
        member(&value, "notes"),
        &Json::string("Current cost: on-demand list price; ECPU: 1 cmd = 1 ECPU (provide avg_payload_kb for better accuracy); Serverless requires cluster-mode-enabled clients and TLS; Global Data Store and Data Tiering are not supported")
    );
    assert_eq!(
        keys(&value),
        [
            "cluster_name",
            "region",
            "engine",
            "instance_type",
            "node_count",
            "primary_nodes",
            "current_monthly_cost",
            "cost_source",
            "avg_memory_gb",
            "billing_memory_gb",
            "monthly_gb_hours",
            "storage_rate_per_gb_hour",
            "storage_cost",
            "monthly_ecpus",
            "ecpu_rate_per_million",
            "ecpu_cost",
            "ecpu_mode",
            "fixed_ecpus",
            "nonfixed_ecpus",
            "internal_calls_excluded",
            "serverless_total",
            "diff",
            "diff_pct",
            "notes"
        ]
    );
}

#[test]
fn provided_cost_minimum_storage_and_unknown_cost() {
    let mut tiny = cluster("tiny");
    tiny.avg_memory_gb = 0.01;
    tiny.daily_commands = 1000.0;
    tiny.current_monthly_cost = 500.0;
    let (result, _) = estimate(&tiny, None);
    let value = result.to_json();
    assert_eq!(member(&value, "cost_source"), &Json::string("provided"));
    assert_eq!(member(&value, "current_monthly_cost"), &Json::Float(500.0));
    assert_eq!(member(&value, "billing_memory_gb"), &Json::Float(0.1));
    assert_eq!(member(&value, "monthly_gb_hours"), &Json::Float(73.0));
    assert_eq!(member(&value, "serverless_total"), &Json::Float(6.13));
    assert_eq!(member(&value, "diff"), &Json::Float(-493.87));
    assert_eq!(member(&value, "diff_pct"), &Json::Float(-98.8));
    assert!(member(&value, "notes")
        .as_str()
        .expect("notes")
        .starts_with("Min storage applied (0.1 GB); ECPU: 1 cmd = 1 ECPU"));

    // No cost and no instance type: the integer 0, no comparison.
    let mut unknown = cluster("unknown");
    unknown.instance_type = String::new();
    let (result, _) = estimate(&unknown, None);
    let value = result.to_json();
    assert_eq!(member(&value, "current_monthly_cost"), &Json::Int(0));
    assert_eq!(member(&value, "cost_source"), &Json::string("unknown"));
    assert_eq!(member(&value, "diff"), &Json::Null);
    assert_eq!(member(&value, "diff_pct"), &Json::Null);
    assert!(member(&value, "notes")
        .as_str()
        .expect("notes")
        .starts_with("Current cost unknown - provide current_monthly_cost or instance_type"));

    // An instance type without a price is "unknown" too.
    let mut unpriced = cluster("unpriced");
    unpriced.instance_type = "cache.none".to_string();
    let (result, _) = estimate(&unpriced, None);
    assert_eq!(
        member(&result.to_json(), "cost_source"),
        &Json::string("unknown")
    );
}

#[test]
fn payload_size_scales_simple_mode_ecpus() {
    let mut big = cluster("big-payload");
    big.avg_payload_kb = 3.2;
    big.daily_commands = 1000.0;
    let (result, _) = estimate(&big, None);
    let value = result.to_json();
    // 1000 * 30 * 3.2 = 96000
    assert_eq!(member(&value, "monthly_ecpus"), &Json::Int(96_000));
    assert!(member(&value, "notes")
        .as_str()
        .expect("notes")
        .contains("ECPU: 3.2 ECPUs per request (3.2 KB avg payload)"));
    // A zero payload column means the default 1.0.
    let mut zero = cluster("zero-payload");
    zero.avg_payload_kb = 0.0;
    zero.daily_commands = 1000.0;
    let (result, _) = estimate(&zero, None);
    assert_eq!(
        member(&result.to_json(), "monthly_ecpus"),
        &Json::Int(30_000)
    );
}

fn orders_stats(normalized: bool) -> CommandStats {
    CommandStats {
        normalized,
        commands: stats(&[("get", 100, 200), ("zadd", 50, 300), ("info", 1, 10)]),
    }
}

#[test]
fn detailed_mode_bills_the_higher_of_cpu_and_network() {
    // CPU: get 100 + zadd max(50, 100) = 200 ECPUs/day; network: 100 commands/day.
    let mut cpu_bound = cluster("cpu-bound");
    cpu_bound.daily_commands = 100.0;
    let (result, warnings) = estimate(&cpu_bound, Some(&orders_stats(true)));
    assert!(warnings.is_empty());
    let value = result.to_json();
    assert_eq!(member(&value, "ecpu_mode"), &Json::string("detailed"));
    assert_eq!(member(&value, "monthly_ecpus"), &Json::Int(6000));
    assert_eq!(member(&value, "fixed_ecpus"), &Json::Int(3000));
    assert_eq!(member(&value, "nonfixed_ecpus"), &Json::Int(3000));
    assert_eq!(member(&value, "internal_calls_excluded"), &Json::Int(30));
    assert!(!member(&value, "notes")
        .as_str()
        .expect("notes")
        .contains("not normalized"));

    // Network-bound: 1M commands/day beats 200 CPU ECPUs/day.
    let (result, _) = estimate(&cluster("net-bound"), Some(&orders_stats(false)));
    let value = result.to_json();
    assert_eq!(member(&value, "monthly_ecpus"), &Json::Int(30_000_000));
    assert!(member(&value, "notes").as_str().expect("notes").contains(
        "Commandstats not normalized by uptime; provide uptime_days column for accuracy"
    ));
}

#[test]
fn memcached_ignores_commandstats_with_a_warning() {
    let mut memcached = cluster("mc\u{1b}[2J");
    memcached.engine = "memcached".to_string();
    let (result, warnings) = estimate(&memcached, Some(&orders_stats(true)));
    assert_eq!(
        member(&result.to_json(), "ecpu_mode"),
        &Json::string("simple")
    );
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].starts_with("WARNING: commandstats mode uses Redis/Valkey INFO format"));
    assert!(warnings[0].contains("'mc\\x1b[2J'"));
    assert!(!warnings[0].contains('\u{1b}'));
    // Memcached has no serverless-compatibility note and a 1 GB minimum.
    let notes = member(&result.to_json(), "notes")
        .as_str()
        .expect("notes")
        .to_string();
    assert!(!notes.contains("cluster-mode-enabled"));
}

#[test]
fn missing_serverless_prices_fail_the_estimate() {
    let mut elsewhere = cluster("elsewhere");
    elsewhere.region = "ap-south-1".to_string();
    let mut warnings = Vec::new();
    let error = serverless::estimate_cluster(&elsewhere, &csv_loader(), None, &mut warnings)
        .expect_err("no regional price");
    assert!(error.starts_with("No serverless storage pricing found for region=ap-south-1"));
}

#[test]
fn burst_multiplier_models_a_daily_sine_wave() {
    use serverless::compute_burst_multiplier as burst;
    // No burst information, flat workloads and non-positive averages: no change.
    assert_eq!(burst(100.0, None, None, None), 1.0);
    assert_eq!(burst(100.0, Some(50.0), None, None), 1.0);
    assert_eq!(burst(100.0, Some(150.0), Some(150.0), None), 1.0);
    assert_eq!(burst(100.0, Some(150.0), Some(100.0), None), 1.0);
    assert_eq!(burst(0.0, Some(0.0), Some(10.0), None), 1.0);
    assert_eq!(
        burst(100.0, None, None, Some(1.0)),
        1.0,
        "ratio must exceed 1"
    );
    // Symmetric around the average: the sine wave averages out.
    assert!((burst(100.0, Some(50.0), Some(150.0), None) - 1.0).abs() < 1e-12);
    // peak 3x average: the wave runs 0..300, whose average is 150.
    assert!((burst(100.0, None, None, Some(3.0)) - 1.5).abs() < 1e-12);
    // Asymmetric explicit bounds: (min+max)/2 = 100 for an average of 80.
    assert!((burst(80.0, Some(20.0), Some(180.0), None) - 1.25).abs() < 1e-12);
}

#[test]
fn bursty_clusters_get_a_note_and_a_higher_cost() {
    let mut bursty = cluster("bursty");
    bursty.daily_commands = 1000.0;
    bursty.peak_to_avg_ratio = 3.0;
    bursty.avg_memory_gb = 2.0;
    bursty.min_memory_gb = 1.0;
    bursty.peak_memory_gb = 5.0;
    let (result, _) = estimate(&bursty, None);
    let value = result.to_json();
    let notes = member(&value, "notes").as_str().expect("notes").to_string();
    assert!(notes.contains("Burst adjustment applied: 1.50x (peak/avg commands)"));
    // Memory 1..5 averages 3 against an average of 2.
    assert!(notes.contains("Burst adjustment applied: 1.50x (peak/avg memory)"));
    assert_eq!(member(&value, "monthly_ecpus"), &Json::Int(45_000));
    assert_eq!(member(&value, "monthly_gb_hours"), &Json::Float(2190.0));
}

#[test]
fn cluster_report_csv_and_json_use_python_number_formats() {
    let mut tiny = cluster("tiny, \"quoted\"");
    tiny.avg_memory_gb = 0.01;
    tiny.daily_commands = 1000.0;
    tiny.current_monthly_cost = 500.0;
    let mut unknown = cluster("unknown");
    unknown.instance_type = String::new();
    let (first, _) = estimate(&tiny, None);
    let (second, _) = estimate(&unknown, None);
    let csv = serverless::csv_report(&[first, second]);
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines.len(), 4, "header, two rows and the final terminator");
    assert_eq!(lines[0], serverless::CSV_FIELDS.join(","));
    assert!(lines[1].starts_with(
        "\"tiny, \"\"quoted\"\"\",us-east-1,valkey,cache.r7g.large,2,1,500.0,provided,0.01,0.1,73.0,6.13,30000,0.0,simple,6.13,-493.87,-98.8,"
    ));
    // The unknown cost is the integer 0 and the empty diff cells stay empty.
    assert!(lines[2].starts_with(
        "unknown,us-east-1,valkey,,2,1,0,unknown,5.0,5.0,3650.0,306.6,30000000,0.1,simple,306.7,,,"
    ));
    assert_eq!(lines[3], "");
}

#[test]
fn summary_lists_totals_savings_and_sorted_clusters() {
    let mut cheap = cluster("cheap");
    cheap.avg_memory_gb = 0.01;
    cheap.daily_commands = 1000.0;
    cheap.current_monthly_cost = 500.0;
    let pricey = cluster("pricey");
    let (first, _) = estimate(&cheap, None);
    let (second, _) = estimate(&pricey, None);
    let lines = serverless::summary_lines(&[second, first]);
    let text = lines.join("\n");
    assert!(text.contains("  Clusters analyzed:        2"));
    assert!(text.contains("  Total nodes:              4"));
    assert!(text.contains(&format!(
        "  Current provisioned cost: ${:>12} /month",
        "646.00"
    )));
    assert!(text.contains(&format!(
        "  Estimated serverless:     ${:>12} /month",
        "312.83"
    )));
    assert!(text.contains(&format!("    Data storage:           ${:>12}", "312.73")));
    assert!(text.contains(&format!("    ECPUs:                  ${:>12}", "0.10")));
    assert!(text.contains(&format!(
        "  Estimated savings:        ${:>12} (51.6%)",
        "333.17"
    )));
    assert!(text.contains("  Clusters cheaper on SL:   1"));
    assert!(text.contains("  Clusters more expensive:  1"));
    let cheap_row = format!(
        "  {:<30} {:>10} {:>10} {:>8}",
        "cheap", "$500", "$6", "-99%"
    );
    let pricey_row = format!(
        "  {:<30} {:>10} {:>10} {:>8}",
        "pricey", "$146", "$307", "+110%"
    );
    let cheap_at = text.find(&cheap_row).expect("cheap row");
    let pricey_at = text.find(&pricey_row).expect("pricey row");
    assert!(cheap_at < pricey_at, "the biggest saving comes first");
    assert!(text.contains("  IMPORTANT: These are approximate estimates."));

    // No results: zero totals and an empty table.
    let empty = serverless::summary_lines(&[]).join("\n");
    assert!(empty.contains("  Clusters analyzed:        0"));
    assert!(!empty.contains("Estimated savings"));
}

#[test]
fn cluster_names_in_the_summary_are_escaped_and_cut_to_30_characters() {
    let mut hostile = cluster(&format!("x\u{1b}[2J{}", "y".repeat(60)));
    hostile.current_monthly_cost = 10.0;
    let (result, _) = estimate(&hostile, None);
    let text = serverless::summary_lines(&[result]).join("\n");
    assert!(!text.contains('\u{1b}'));
    let expected_name = format!("x\\x1b[2J{}", "y".repeat(22));
    assert!(text.contains(&format!("  {expected_name:<30} ")));
}

// ---------------------------------------------------------------------------
// input validator
// ---------------------------------------------------------------------------

#[test]
fn cache_names_follow_the_aws_rules() {
    assert_eq!(validator::validate_cache_name("my-cache1"), Ok(()));
    assert_eq!(validator::validate_cache_name(&"a".repeat(40)), Ok(()));
    assert_eq!(
        validator::validate_cache_name(&"a".repeat(41)),
        Err("Cache name must be 1-40 characters (got 41)".to_string())
    );
    assert_eq!(
        validator::validate_cache_name(""),
        Err("Cache name must not be empty".to_string())
    );
    for (name, message) in [
        ("1abc", "Cache name must start with a lowercase letter (serverless requires lowercase; node-based is case-insensitive but lowercased by AWS)"),
        ("Abc", "Cache name must start with a lowercase letter (serverless requires lowercase; node-based is case-insensitive but lowercased by AWS)"),
        ("aBc", "Cache name may only contain lowercase alphanumeric characters and hyphens"),
        ("a_b", "Cache name may only contain lowercase alphanumeric characters and hyphens"),
        ("a b", "Cache name may only contain lowercase alphanumeric characters and hyphens"),
        ("a--b", "Cache name must not contain consecutive hyphens"),
        ("ab-", "Cache name must not end with a hyphen"),
    ] {
        assert_eq!(validator::validate_cache_name(name), Err(message.to_string()), "{name}");
    }
    // The Python `$` also accepted a trailing newline; this validator does not.
    assert!(validator::validate_cache_name("my-cache\n").is_err());
    assert!(validator::validate_cache_name("my-cache\r").is_err());
    assert!(validator::validate_cache_name("my-cache\0").is_err());
    assert!(validator::validate_cache_name("my-cache;rm -rf /").is_err());
}

#[test]
fn replication_group_ids_allow_capital_letters() {
    assert_eq!(
        validator::validate_replication_group_id("My-Group2"),
        Ok(())
    );
    assert_eq!(
        validator::validate_replication_group_id(&"G".repeat(40)),
        Ok(())
    );
    assert_eq!(
        validator::validate_replication_group_id(&"G".repeat(41)),
        Err("Replication group ID must be 1-40 characters (got 41)".to_string())
    );
    assert_eq!(
        validator::validate_replication_group_id("9g"),
        Err("Replication group ID must start with a letter".to_string())
    );
    assert_eq!(
        validator::validate_replication_group_id("g_1"),
        Err(
            "Replication group ID may only contain alphanumeric characters and hyphens".to_string()
        )
    );
    assert_eq!(
        validator::validate_replication_group_id("g--1"),
        Err("Replication group ID must not contain consecutive hyphens".to_string())
    );
    assert_eq!(
        validator::validate_replication_group_id("g-"),
        Err("Replication group ID must not end with a hyphen".to_string())
    );
    assert_eq!(
        validator::validate_replication_group_id(""),
        Err("Replication group ID must not be empty".to_string())
    );
}

#[test]
fn ec2_identifiers_need_eight_to_seventeen_hex_digits() {
    assert_eq!(
        validator::validate_subnet_id("subnet-0123abcd"),
        Ok(()),
        "8 digits"
    );
    assert_eq!(
        validator::validate_subnet_id("subnet-0abc1234def56789a"),
        Ok(()),
        "17 digits"
    );
    assert_eq!(
        validator::validate_subnet_id("subnet-xyz"),
        Err("Subnet ID must match pattern subnet-<8-17 hex characters> (e.g. subnet-0abc1234def56789a)".to_string())
    );
    assert!(
        validator::validate_subnet_id("subnet-0abc123").is_err(),
        "7 digits"
    );
    assert!(
        validator::validate_subnet_id("subnet-0abc1234def567890a").is_err(),
        "18 digits"
    );
    assert!(
        validator::validate_subnet_id("subnet-0ABC1234").is_err(),
        "upper case hex"
    );
    assert!(validator::validate_subnet_id("subnet-0abc123g").is_err());
    assert!(validator::validate_subnet_id("subnet-0abc1234\n").is_err());
    assert!(validator::validate_subnet_id("vpc-0abc1234").is_err());
    assert_eq!(
        validator::validate_subnet_id(""),
        Err("Subnet ID must not be empty".to_string())
    );

    assert_eq!(
        validator::validate_security_group_id("sg-0abc1234def56789"),
        Ok(())
    );
    assert!(validator::validate_security_group_id("sg-0abc123").is_err());
    assert_eq!(
        validator::validate_security_group_id(""),
        Err("Security group ID must not be empty".to_string())
    );
    assert_eq!(validator::validate_vpc_id("vpc-0abc1234"), Ok(()));
    assert!(validator::validate_vpc_id("vpc-0abc1234def567890a").is_err());
    assert_eq!(
        validator::validate_vpc_id(""),
        Err("VPC ID must not be empty".to_string())
    );
    assert_eq!(
        validator::validate_vpc_id("vpc-zz"),
        Err(
            "VPC ID must match pattern vpc-<8-17 hex characters> (e.g. vpc-0abc1234def56789a)"
                .to_string()
        )
    );
}

#[test]
fn regions_and_engine_versions() {
    for region in ["us-east-1", "eu-west-2", "ap-southeast-1", "sa-east-12"] {
        assert_eq!(validator::validate_region(region), Ok(()), "{region}");
    }
    for region in [
        "us-east",
        "us-east-",
        "useast1",
        "US-east-1",
        "us-gov-west-1",
        "u-east-1",
        "usa-east-1",
        "us-east-1\n",
        "us-east-\u{661}",
        "us-e4st-1",
        "",
    ] {
        assert!(validator::validate_region(region).is_err(), "{region:?}");
    }
    assert_eq!(
        validator::validate_region("nope"),
        Err("Region must match AWS region pattern (e.g. us-east-1, eu-west-2)".to_string())
    );
    for version in ["7", "7.1", "7.0.4", "10.20.30"] {
        assert_eq!(
            validator::validate_engine_version(version),
            Ok(()),
            "{version}"
        );
    }
    for version in [
        "7.", ".7", "7.1.2.3", "7.x", "v7", "7..1", " 7", "7\n", "\u{667}", "",
    ] {
        assert!(
            validator::validate_engine_version(version).is_err(),
            "{version:?}"
        );
    }
    assert_eq!(
        validator::validate_engine_version("7.x"),
        Err(
            "Engine version must match pattern <major>[.<minor>[.<patch>]] (e.g. 7, 7.1, 7.0.4)"
                .to_string()
        )
    );
}

#[test]
fn kms_keys_are_uuids_or_arns() {
    let uuid = "12345678-1234-1234-1234-123456789abc";
    assert_eq!(validator::validate_kms_key_id(uuid), Ok(()));
    assert!(validator::validate_kms_key_id("12345678-1234-1234-1234-123456789ABC").is_err());
    assert!(validator::validate_kms_key_id("12345678-1234-1234-1234-123456789ab").is_err());
    for arn in [
        "arn:aws:kms:us-east-1:123456789012:key/12345678-1234-1234-1234-123456789abc",
        "arn:aws:kms:us-east-1:123456789012:alias/my/alias_name-1",
    ] {
        assert_eq!(validator::validate_kms_key_id(arn), Ok(()), "{arn}");
    }
    for arn in [
        "arn:aws:kms:us-east-1:12345678901:key/abc",
        "arn:aws:kms:us-east-1:123456789012:key/",
        "arn:aws:kms:us-east-1:123456789012:secret/abc",
        "arn:aws:kms::123456789012:key/abc",
        "arn:aws:kms:us_east:123456789012:key/abc",
        "arn:aws:kms:us-east-1:123456789012:key/a b",
        "arn:aws:kms:us-east-1:123456789012:key/abc\n",
        "arn:aws-cn:kms:cn-north-1:123456789012:key/abc",
        "alias/foo",
    ] {
        assert!(validator::validate_kms_key_id(arn).is_err(), "{arn}");
    }
    assert_eq!(
        validator::validate_kms_key_id(""),
        Err("KMS key ID must not be empty".to_string())
    );
    assert!(validator::validate_kms_key_id("nope")
        .expect_err("invalid")
        .starts_with("KMS key ID must be a UUID"));
}

#[test]
fn snapshot_names_differ_for_serverless_and_node_based() {
    assert_eq!(
        validator::validate_snapshot_name(&"s".repeat(40), false),
        Ok(())
    );
    assert_eq!(
        validator::validate_snapshot_name(&"s".repeat(41), false),
        Err("Snapshot name must be 1-40 characters for node-based (got 41)".to_string())
    );
    assert_eq!(
        validator::validate_snapshot_name(&"s".repeat(255), true),
        Ok(())
    );
    assert_eq!(
        validator::validate_snapshot_name(&"s".repeat(256), true),
        Err("Snapshot name must be 1-255 characters for serverless (got 256)".to_string())
    );
    assert_eq!(validator::validate_snapshot_name("Snap-1", false), Ok(()));
    assert_eq!(
        validator::validate_snapshot_name("", true),
        Err("Snapshot name must not be empty".to_string())
    );
    assert_eq!(
        validator::validate_snapshot_name("1snap", true),
        Err("Snapshot name must start with a letter".to_string())
    );
    assert_eq!(
        validator::validate_snapshot_name("s_1", true),
        Err("Snapshot name may only contain alphanumeric characters and hyphens".to_string())
    );
    assert_eq!(
        validator::validate_snapshot_name("s--1", true),
        Err("Snapshot name must not contain consecutive hyphens".to_string())
    );
    assert_eq!(
        validator::validate_snapshot_name("s-", true),
        Err("Snapshot name must not end with a hyphen".to_string())
    );
}

#[test]
fn bulk_validation_skips_unknown_keys_and_reports_type_errors() {
    let params = vec![
        ("cache_name".to_string(), Json::string("Bad_Name")),
        ("unrelated".to_string(), Json::Int(1)),
        ("region".to_string(), Json::string("us-east-1")),
        ("subnet_id".to_string(), Json::Int(5)),
        ("vpc_id".to_string(), Json::Null),
        ("kms_key_id".to_string(), Json::Bool(true)),
        ("engine_version".to_string(), Json::Float(7.1)),
        ("snapshot_name".to_string(), Json::Array(Vec::new())),
        ("security_group_id".to_string(), Json::Object(Vec::new())),
        ("replication_group_id".to_string(), Json::string("rg-1")),
    ];
    assert_eq!(
        validator::validate_all(&params),
        vec![
            "cache_name: Cache name must start with a lowercase letter (serverless requires lowercase; node-based is case-insensitive but lowercased by AWS)".to_string(),
            "subnet_id: expected a string, got int".to_string(),
            "vpc_id: expected a string, got NoneType".to_string(),
            "kms_key_id: expected a string, got bool".to_string(),
            "engine_version: expected a string, got float".to_string(),
            "snapshot_name: expected a string, got list".to_string(),
            "security_group_id: expected a string, got dict".to_string(),
        ]
    );
    assert!(validator::validate_all(&[]).is_empty());
}

#[test]
fn rejected_values_never_appear_in_messages() {
    let sentinel = "SENTINEL-SECRET\u{1b}[2J";
    for message in [
        validator::validate_cache_name(sentinel),
        validator::validate_replication_group_id(sentinel),
        validator::validate_subnet_id(sentinel),
        validator::validate_security_group_id(sentinel),
        validator::validate_vpc_id(sentinel),
        validator::validate_region(sentinel),
        validator::validate_engine_version(sentinel),
        validator::validate_kms_key_id(sentinel),
        validator::validate_snapshot_name(sentinel, true),
    ] {
        let text = message.expect_err("invalid");
        assert!(!text.contains("SENTINEL"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
    }
    let long = format!("SENTINEL{}", "a".repeat(300));
    assert!(!validator::validate_cache_name(&long)
        .expect_err("too long")
        .contains("SENTINEL"));
}

#[test]
fn validate_input_command_prints_usage_without_arguments() {
    let (status, stdout, stderr) = exec(&["elasticache", "validate-input"]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "Usage: toolkit elasticache validate-input key=value [key=value ...]\nExample: toolkit elasticache validate-input cache_name=my-cache region=us-east-1\n\nRecognized keys: cache_name, engine_version, kms_key_id, region, replication_group_id, security_group_id, snapshot_name, subnet_id, vpc_id\n"
    );
}

#[test]
fn validate_input_command_accepts_valid_values() {
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "validate-input",
        "cache_name=my-cache",
        "region=us-east-1",
        "subnet_id=subnet-0abc1234def56789a",
        "unknown_key=whatever goes",
    ]);
    assert_eq!(
        (status, stdout.as_str(), stderr.as_str()),
        (0, "All inputs valid.\n", "")
    );
}

#[test]
fn validate_input_command_reports_failures_without_echoing_values() {
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "validate-input",
        "cache_name=SENTINEL;rm -rf /\u{1b}[2J",
        "region=us-east-1",
        "vpc_id=nope",
    ]);
    assert_eq!((status, stderr.as_str()), (1, ""));
    assert!(stdout.starts_with("Validation FAILED:\n  - cache_name: "));
    assert!(stdout.contains("\n  - vpc_id: VPC ID must match pattern vpc-<8-17 hex characters>"));
    assert!(!stdout.contains("SENTINEL"));
    assert!(!stdout.contains('\u{1b}'));
}

#[test]
fn validate_input_command_last_value_wins_and_missing_equals_is_a_usage_error() {
    let (status, stdout, _) = exec(&[
        "elasticache",
        "validate-input",
        "cache_name=Bad",
        "cache_name=good-name",
    ]);
    assert_eq!((status, stdout.as_str()), (0, "All inputs valid.\n"));

    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "validate-input",
        "region=us-east-1",
        "SENTINEL\u{1b}[2Jnoequals",
    ]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(stderr.starts_with("error: argument 2 is not in key=value format\n"));
    assert!(!stderr.contains("SENTINEL"));
    assert!(!stderr.contains('\u{1b}'));
}

// ---------------------------------------------------------------------------
// reference validator
// ---------------------------------------------------------------------------

#[test]
fn skill_md_reference_extraction() {
    let content = "See `references/setup/engine-selection.md` and `references/x.md`, not `references/Bad name.md`, nor references/plain.md, nor `references/no-close.md and `references/dir/`. Again `references/x.md` and `references/Up_Per-1.md`.";
    let found: Vec<String> = references::skill_md_references(content)
        .into_iter()
        .collect();
    assert_eq!(
        found,
        vec![
            "references/Up_Per-1.md".to_string(),
            "references/setup/engine-selection.md".to_string(),
            "references/x.md".to_string(),
        ]
    );
    assert!(references::skill_md_references("").is_empty());
    assert!(references::skill_md_references("`references/`").is_empty());
}

#[test]
fn kebab_case_markdown_names() {
    for name in ["a.md", "engine-selection.md", "v2-notes-3.md", "0.md"] {
        assert!(references::is_kebab_markdown(name), "{name}");
    }
    for name in [
        "A.md", "a_b.md", "a--b.md", "-a.md", "a-.md", ".md", "a.txt", "a b.md", "a.md.md", "a.MD",
    ] {
        assert!(!references::is_kebab_markdown(name), "{name}");
    }
}

fn body_of(chars: usize) -> String {
    format!("# Title\n\n{}\n", "x".repeat(chars))
}

fn skill_fixture(dir: &TempDir) {
    dir.file(
        "SKILL.md",
        "Use `references/setup/good-file.md` and `references/stub.md`.\n",
    );
    dir.file("references/setup/good-file.md", &body_of(60));
    dir.file("references/Bad_Name.md", &body_of(60));
    dir.file("references/empty.md", "\n\n   \n");
    dir.file("references/stub.md", &body_of(49));
    dir.file(
        "references/notitle.md",
        &format!("plain text\n{}\n", "y".repeat(80)),
    );
    dir.file("references/ignored.txt", "not markdown");
    dir.file(
        "scripts/ok.py",
        "#!/usr/bin/env python3\n\"\"\"Docstring.\"\"\"\nif __name__ == \"__main__\":\n    pass\n",
    );
    dir.file(
        "scripts/nodoc.py",
        "import sys\nif __name__ == '__main__':\n    pass\n",
    );
    dir.file("scripts/lib.py", "'''Library.'''\nx = 1\n");
    dir.file("scripts/other.sh", "echo not python");
}

fn find<'a>(
    results: &'a [references::CheckResult],
    path: &str,
    level: references::Level,
) -> Vec<&'a str> {
    results
        .iter()
        .filter(|r| r.path == path && r.level == level)
        .map(|r| r.message.as_str())
        .collect()
}

#[test]
fn reference_files_are_checked_one_by_one() {
    let dir = TempDir::new("refs");
    skill_fixture(&dir);
    let (results, count) = references::validate_all_references(dir.path());
    assert_eq!(count, 5, "every .md file under references/, none else");
    use references::Level::{Fail, Pass, Warn};

    let good = "references/setup/good-file.md";
    assert_eq!(find(&results, good, Pass).len(), 4);
    assert!(find(&results, good, Pass).contains(&"Has title: # Title"));
    assert!(find(&results, good, Pass).contains(&"File has meaningful content (60 body chars)"));
    assert!(find(&results, good, Pass).contains(&"Referenced in SKILL.md"));

    let bad = "references/Bad_Name.md";
    assert_eq!(
        find(&results, bad, Fail),
        vec!["Filename 'Bad_Name.md' does not follow kebab-case convention (expected lowercase letters, digits, and hyphens only)"]
    );
    assert_eq!(
        find(&results, bad, Warn),
        vec!["Not referenced in SKILL.md"]
    );

    assert_eq!(
        find(&results, "references/empty.md", Fail),
        vec!["File is completely empty"]
    );
    assert_eq!(
        find(&results, "references/stub.md", Fail),
        vec!["File body has only 49 non-whitespace chars (minimum 50) -- appears empty or stub"]
    );
    assert_eq!(
        find(&results, "references/notitle.md", Fail),
        vec!["Missing title -- first non-blank line should start with '#'"]
    );
    // Exactly 50 body characters is enough.
    let edge = TempDir::new("refs-edge");
    edge.file("references/edge.md", &body_of(50));
    let (results, _) = references::validate_all_references(edge.path());
    assert!(find(&results, "references/edge.md", Fail).is_empty());
}

#[test]
fn script_files_are_checked_for_docstring_and_entrypoint() {
    let dir = TempDir::new("scripts");
    skill_fixture(&dir);
    let (results, count) = references::validate_all_scripts(dir.path());
    assert_eq!(count, 3);
    use references::Level::{Fail, Pass, Warn};
    assert_eq!(
        find(&results, "scripts/ok.py", Pass),
        vec!["Has module docstring", "Has CLI entrypoint"]
    );
    assert_eq!(
        find(&results, "scripts/nodoc.py", Fail),
        vec!["Missing module docstring at top of file"]
    );
    assert_eq!(
        find(&results, "scripts/nodoc.py", Pass),
        vec!["Has CLI entrypoint"]
    );
    assert_eq!(
        find(&results, "scripts/lib.py", Warn),
        vec!["No 'if __name__ == \"__main__\"' block -- OK if this is a library, not a CLI tool"]
    );
}

#[test]
fn missing_directories_and_empty_trees_fail() {
    let dir = TempDir::new("refs-missing");
    let (results, count) = references::validate_all_references(dir.path());
    assert_eq!(count, 0);
    assert_eq!(
        find(&results, "references/", references::Level::Fail),
        vec!["references/ directory not found"]
    );
    let (results, count) = references::validate_all_scripts(dir.path());
    assert_eq!(count, 0);
    assert_eq!(
        find(&results, "scripts/", references::Level::Fail),
        vec!["scripts/ directory not found"]
    );

    dir.file("references/readme.txt", "x");
    let (results, count) = references::validate_all_references(dir.path());
    assert_eq!(count, 0);
    assert_eq!(
        find(&results, "references/", references::Level::Fail),
        vec!["No .md files found under references/"]
    );
    fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
    let (results, count) = references::validate_all_scripts(dir.path());
    assert_eq!(count, 0);
    assert_eq!(
        find(&results, "scripts/", references::Level::Warn),
        vec!["No .py files found under scripts/"]
    );
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_never_followed() {
    let dir = TempDir::new("refs-links");
    dir.file("SKILL.md", "x");
    dir.file("references/real.md", &body_of(60));
    let outside = TempDir::new("refs-outside");
    outside.file("secret.md", &body_of(90));
    std::os::unix::fs::symlink(outside.at("secret.md"), dir.at("references/leak.md"))
        .expect("link");
    std::os::unix::fs::symlink(outside.path(), dir.at("references/dirlink")).expect("dir link");
    let (results, count) = references::validate_all_references(dir.path());
    assert_eq!(
        count, 2,
        "the linked file is counted, the linked directory is not entered"
    );
    let failures = find(&results, "references/leak.md", references::Level::Fail);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].starts_with("Could not read file: refusing to follow symbolic link"));
    assert!(results.iter().all(|r| !r.path.contains("dirlink")));
    assert!(results.iter().all(|r| !r.path.contains("secret")));
}

#[test]
fn validate_references_command_output_and_exit_status() {
    let dir = TempDir::new("refs-cmd");
    skill_fixture(&dir);
    let root = dir.path().to_str().expect("utf-8");
    let (status, stdout, stderr) =
        exec(&["elasticache", "validate-references", "--skill-dir", root]);
    assert_eq!((status, stderr.as_str()), (1, ""));
    assert!(stdout.starts_with("Validating reference files...\n  Scanned 5 reference .md files\nValidating scripts...\n  Scanned 3 script .py files\n\n--- FAILURES ---\n"));
    assert!(stdout.contains("\n--- WARNINGS ---\n"));
    assert!(!stdout.contains("--- PASSED ---"));
    assert!(stdout.contains("  [FAIL] references/empty.md: File is completely empty\n"));
    assert!(
        stdout.ends_with("\nSummary: 16 passed, 3 warnings, 5 failures\n"),
        "{stdout}"
    );

    let (status, quiet, _) = exec(&[
        "elasticache",
        "validate-references",
        "--skill-dir",
        root,
        "--quiet",
    ]);
    assert_eq!(status, 1);
    assert!(!quiet.contains("--- FAILURES ---"));
    assert!(quiet.ends_with("\nSummary: 16 passed, 3 warnings, 5 failures\n"));

    let (_, verbose, _) = exec(&[
        "elasticache",
        "validate-references",
        "--skill-dir",
        root,
        "--verbose",
    ]);
    assert!(verbose.contains("\n--- PASSED ---\n"));
    assert!(verbose.contains("  [PASS] scripts/ok.py: Has module docstring\n"));
}

#[test]
fn validate_references_command_passes_a_clean_skill() {
    let dir = TempDir::new("refs-clean");
    dir.file("SKILL.md", "`references/a-b.md`");
    dir.file("references/a-b.md", &body_of(70));
    dir.file(
        "scripts/tool.py",
        "\"\"\"Doc.\"\"\"\nif __name__ == \"__main__\":\n    pass\n",
    );
    let root = dir.path().to_str().expect("utf-8");
    let (status, stdout, stderr) =
        exec(&["elasticache", "validate-references", "--skill-dir", root]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(
        stdout.ends_with("\nSummary: 6 passed, 0 warnings, 0 failures\n"),
        "{stdout}"
    );
}

#[test]
fn hostile_file_names_are_escaped_in_the_report() {
    let dir = TempDir::new("refs-hostile");
    dir.file("SKILL.md", "x");
    dir.file("references/evil\u{1b}[2J.md", &body_of(60));
    let (results, _) = references::validate_all_references(dir.path());
    let text: String = results
        .iter()
        .map(references::CheckResult::render)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("evil\\x1b[2J.md"));
}

// ---------------------------------------------------------------------------
// migration preflight
// ---------------------------------------------------------------------------

const HEALTHY_SNAPSHOT: &str = r#"{
  "info": {"redis_version": "7.2.4", "os": "Linux 5.10", "uptime_in_seconds": 172800,
           "cluster_enabled": 0, "connected_slaves": 1, "used_memory": 1073741824,
           "used_memory_peak": 2147483648, "db0": {"keys": 1234567, "expires": 3},
           "rdb_last_save_time": 1700000000, "aof_enabled": 0},
  "modules": [],
  "commandstats": {"cmdstat_get": {"calls": 5, "usec": 9}},
  "config": {"maxmemory-policy": "allkeys-lru"}
}"#;

fn run_preflight(host: &str, snapshot: &str) -> preflight::Preflight {
    preflight::run_preflight(host, 6379, Some(&json(snapshot))).expect("valid snapshot")
}

fn statuses(result: &preflight::Preflight) -> Vec<(String, &'static str)> {
    result
        .findings
        .iter()
        .map(|f| (f.check.clone(), f.status))
        .collect()
}

fn detail<'a>(result: &'a preflight::Preflight, check: &str) -> &'a str {
    &result
        .findings
        .iter()
        .find(|f| f.check == check)
        .unwrap_or_else(|| panic!("no finding {check}"))
        .detail
}

#[test]
fn a_healthy_source_is_ready_to_migrate() {
    let result = run_preflight("redis.internal", HEALTHY_SNAPSHOT);
    assert!(!result.failed());
    assert_eq!(
        statuses(&result),
        [
            ("Connectivity", "PASS"),
            ("Engine version", "PASS"),
            ("Cluster mode", "INFO"),
            ("Replication", "INFO"),
            ("Memory usage", "INFO"),
            ("Key count", "INFO"),
            ("Persistence", "INFO"),
            ("Modules", "PASS"),
            ("Eviction policy", "INFO"),
            ("Data tiering advisory", "INFO"),
            ("Sizing recommendation", "INFO"),
        ]
        .map(|(c, s)| (c.to_string(), s))
    );
    assert_eq!(
        detail(&result, "Connectivity"),
        "Connected to redis.internal:6379"
    );
    assert_eq!(detail(&result, "Replication"), "1 connected replica(s)");
    assert_eq!(
        detail(&result, "Memory usage"),
        "Current: 1024.0 MB (1.0 GB), Peak: 2048.0 MB"
    );
    assert_eq!(
        detail(&result, "Key count"),
        "1,234,567 keys across 1 database(s)"
    );
    assert_eq!(detail(&result, "Persistence"), "RDB snapshots active");
    assert_eq!(detail(&result, "Eviction policy"), "Policy: allkeys-lru");
    assert!(detail(&result, "Sizing recommendation").starts_with("Source dataset: 1.0 GB."));
    let keys_in_order: Vec<&str> = result.info.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys_in_order,
        [
            "host",
            "port",
            "redis_version",
            "os",
            "uptime_days",
            "cluster_mode",
            "connected_replicas",
            "used_memory_mb",
            "used_memory_gb",
            "peak_memory_mb",
            "total_keys",
            "databases_in_use",
            "rdb_enabled",
            "aof_enabled",
            "modules",
            "lua_scripts_detected",
            "maxmemory_policy"
        ]
    );
    let value = result.to_json();
    let info = member(&value, "info");
    assert_eq!(member(info, "uptime_days"), &Json::Float(2.0));
    assert_eq!(member(info, "total_keys"), &Json::Int(1_234_567));
    assert_eq!(member(info, "cluster_mode"), &Json::Bool(false));
    assert_eq!(member(info, "rdb_enabled"), &Json::Bool(true));
    assert_eq!(member(info, "aof_enabled"), &Json::Bool(false));
    assert_eq!(member(info, "lua_scripts_detected"), &Json::Bool(false));
    assert!(value.get("error").is_none());
}

#[test]
fn engine_versions_map_to_pass_warn_and_fail() {
    for (version, expected) in [
        ("7.2.4", "PASS"),
        ("7.0", "PASS"),
        ("7", "PASS"),
        ("8.1.0", "PASS"),
        ("6.2.6", "WARN"),
        ("6.0.0", "WARN"),
        ("5.9", "FAIL"),
        ("5.0.14", "FAIL"),
        ("4.0.1", "FAIL"),
        ("unknown", "FAIL"),
        ("7-rc1", "FAIL"),
        ("", "FAIL"),
    ] {
        let snapshot = format!(r#"{{"info": {{"redis_version": "{version}"}}}}"#);
        let result = run_preflight("h", &snapshot);
        let engine = result
            .findings
            .iter()
            .find(|f| f.check == "Engine version")
            .expect("engine finding");
        assert_eq!(engine.status, expected, "{version:?}");
        let has_extended = result
            .findings
            .iter()
            .any(|f| f.check == "Extended Support");
        assert_eq!(has_extended, expected == "FAIL", "{version:?}");
    }
    let old = run_preflight("h", r#"{"info": {"redis_version": "4.0.1"}}"#);
    assert!(detail(&old, "Engine version").contains(
        "standard support ended January 31, 2026. Extended Support charges are now active. Upgrade"
    ));
    assert!(detail(&old, "Extended Support").contains("Year 1-2 premium: 80% surcharge. Year 3 premium: 160% surcharge. Run `toolkit elasticache price --extended-support`"));
    let five = run_preflight("h", r#"{"info": {"redis_version": "5.0.14"}}"#);
    assert!(detail(&five, "Engine version").contains("(since Feb 1, 2026)"));
    // A numeric version is read as text.
    let numeric = run_preflight("h", r#"{"info": {"redis_version": 7.2}}"#);
    assert!(detail(&numeric, "Engine version").starts_with("Redis 7.2 can migrate"));
}

#[test]
fn an_old_minimal_source_has_blockers() {
    let result = run_preflight("h", r#"{"info": {"redis_version": "5.0.14"}}"#);
    assert!(result.failed());
    assert_eq!(result.count("FAIL"), 2);
    assert_eq!(detail(&result, "Replication"), "0 connected replica(s)");
    assert_eq!(
        detail(&result, "Memory usage"),
        "Current: 0.0 MB (0.0 GB), Peak: 0.0 MB"
    );
    assert_eq!(detail(&result, "Key count"), "0 keys across 0 database(s)");
    assert_eq!(detail(&result, "Persistence"), "No persistence");
    assert_eq!(detail(&result, "Modules"), "No modules loaded");
    assert!(result.findings.iter().all(|f| f.check != "Eviction policy"));
    let report = preflight::format_report(&result);
    assert!(report.contains("  Verdict: BLOCKERS FOUND (2 failures, 0 warnings)"));
    assert!(report.contains("MIGRATION BLOCKERS\n"));
    assert!(report.contains("  Resolve blockers before starting migration."));
    // Failures are listed first, informational findings last.
    let fail_at = report.find("[FAIL] Engine version").expect("fail");
    let info_at = report.find("[INFO] Cluster mode").expect("info");
    assert!(fail_at < info_at);
}

#[test]
fn healthy_report_text() {
    let report = preflight::format_report(&run_preflight("redis.internal", HEALTHY_SNAPSHOT));
    assert!(report.starts_with(&format!(
        "{}\nElastiCache Migration Preflight Report\n",
        "=".repeat(72)
    )));
    assert!(report.contains("  Source:          redis.internal:6379\n"));
    assert!(report.contains("  Redis version:   7.2.4\n"));
    assert!(report.contains("  OS:              Linux 5.10\n"));
    assert!(report.contains("  Uptime:          2.0 days\n"));
    assert!(report.contains("  Cluster mode:    disabled\n"));
    assert!(report.contains("  Memory:          1024.0 MB (1.0 GB)\n"));
    assert!(report.contains("  Keys:            1234567\n"));
    assert!(report.contains("  Replicas:        1\n"));
    assert!(report.contains("  Modules:         none\n"));
    assert!(report.contains("  Verdict: READY TO MIGRATE (3 passed, 0 warnings)\n"));
    assert!(!report.contains("MIGRATION BLOCKERS"));
    assert!(report.contains("  [OK]   Connectivity\n         Connected to redis.internal:6379\n"));
    assert!(report.ends_with(
        "  4. See references/migration/instructions.md for the full migration workflow\n"
    ));
    assert!(report.contains("  2. Run: toolkit elasticache price to estimate target cost\n"));
}

#[test]
fn cluster_mode_databases_and_persistence() {
    let result = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0", "cluster_enabled": 1,
            "db0": {"keys": 1}, "db1": {"keys": 2}, "dbsize": 9,
            "aof_enabled": 1, "rdb_last_save_time": 5}}"#,
    );
    assert_eq!(
        detail(&result, "Cluster mode"),
        "Cluster mode enabled — target must also be cluster mode enabled"
    );
    assert_eq!(detail(&result, "Key count"), "3 keys across 3 database(s)");
    assert_eq!(
        detail(&result, "Multiple databases"),
        "3 databases in use — ElastiCache cluster mode uses only db0. Plan key migration."
    );
    assert_eq!(
        detail(&result, "Persistence"),
        "RDB snapshots active, AOF enabled"
    );
    let value = result.to_json();
    assert_eq!(
        member(member(&value, "info"), "databases_in_use"),
        &Json::Int(3)
    );
    assert_eq!(
        member(member(&value, "info"), "cluster_mode"),
        &Json::Bool(true)
    );

    // A key count that is not an integer is "unknown" and adds no findings.
    let odd = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0", "db0": {"keys": "many"}}}"#,
    );
    assert!(odd.findings.iter().all(|f| f.check != "Key count"));
    let info = member(&odd.to_json(), "info").clone();
    assert_eq!(member(&info, "total_keys"), &Json::string("unknown"));
    assert!(info.get("databases_in_use").is_none());
}

#[test]
fn modules_are_matched_against_the_known_incompatible_list() {
    let result = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"},
            "modules": [["name", "ReJSON", "ver", 20000], {"name": "search"}, "bf", ["name", "my\u001b[2Jmod"], ["only"]]}"#,
    );
    assert!(detail(&result, "Module: RedisJSON")
        .starts_with("The ReJSON module cannot be loaded on ElastiCache."));
    let by_check: Vec<(&str, &str)> = result
        .findings
        .iter()
        .filter(|f| f.check.starts_with("Module"))
        .map(|f| (f.check.as_str(), f.status))
        .collect();
    assert_eq!(
        by_check,
        [
            ("Module: RedisJSON", "FAIL"),
            ("Module: RediSearch", "FAIL"),
            ("Module: RedisBloom", "FAIL"),
            ("Module: my\\x1b[2Jmod", "WARN"),
            ("Module: ['only']", "WARN"),
        ]
    );
    assert_eq!(
        detail(&result, "Module: my\\x1b[2Jmod"),
        "Unknown module 'my\\x1b[2Jmod' — verify compatibility with ElastiCache"
    );
    assert!(result.failed());
    let value = result.to_json();
    assert_eq!(
        member(member(&value, "info"), "modules"),
        &json(r#"["ReJSON", "search", "bf", "my\u001b[2Jmod", "['only']"]"#)
    );
    // A refused MODULE LIST is informational.
    let refused = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"}, "modules": null}"#,
    );
    assert_eq!(
        detail(&refused, "Modules"),
        "MODULE LIST not available (may be restricted or old version)"
    );
    assert!(!refused.failed());
}

#[test]
fn restricted_commands_and_lua_scripts_are_warnings() {
    let result = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"},
            "commandstats": {"cmdstat_config": {"calls": 3, "usec": 9}, "cmdstat_bgsave": {"calls": 0},
                             "cmdstat_sync": {"calls": 1}, "cmdstat_eval": {"calls": 5}, "cmdstat_evalsha": {"calls": 2}}}"#,
    );
    let restricted: Vec<&str> = result
        .findings
        .iter()
        .filter(|f| f.check == "Restricted command usage")
        .map(|f| f.detail.as_str())
        .collect();
    assert_eq!(
        restricted,
        [
            "Command CONFIG called 3 times. This command is restricted on ElastiCache. Review usage before migration.",
            "Command SYNC called 1 times. This command is restricted on ElastiCache. Review usage before migration.",
        ],
        "sorted by command name; zero calls are not reported"
    );
    assert!(detail(&result, "Lua scripts").starts_with("Lua scripts detected (7 eval calls)."));
    assert_eq!(result.count("WARN"), 3);
    assert_eq!(
        member(member(&result.to_json(), "info"), "lua_scripts_detected"),
        &Json::Bool(true)
    );
    let refused = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"}, "commandstats": null}"#,
    );
    assert_eq!(
        detail(&refused, "Restricted command usage"),
        "INFO COMMANDSTATS not available (may be restricted by ACL or unsupported on this engine version)"
    );
}

#[test]
fn large_keys_are_sampled_without_printing_key_names() {
    let result = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"},
            "sample_keys": [
              {"key": "SENTINEL-KEY-1", "type": "hash", "memory_usage": 2097152},
              {"key": "SENTINEL-KEY-2", "type": "string", "memory_usage": 1048576},
              {"key": "SENTINEL-KEY-3", "type": "list", "memory_usage": null},
              {"key": "", "type": "set", "memory_usage": 5242880}]}"#,
    );
    let large: Vec<&str> = result
        .findings
        .iter()
        .filter(|f| f.check == "Large key sampling")
        .map(|f| f.detail.as_str())
        .collect();
    assert_eq!(
        large,
        [
            "Large key detected (hash, 2.0MB). Large keys can cause replication timeouts and event loop blocking during migration.",
            "Large key detected (unknown, 5.0MB). Large keys can cause replication timeouts and event loop blocking during migration.",
        ],
        "exactly 1 MiB is not large"
    );
    let text = format!(
        "{}{}",
        preflight::format_report(&result),
        result.to_json().to_compact_string()
    );
    assert!(!text.contains("SENTINEL"));

    let unsupported = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"}, "memory_usage_supported": false,
            "sample_keys": [{"key": "a", "type": "hash", "memory_usage": 9999999}]}"#,
    );
    assert_eq!(
        detail(&unsupported, "Large key sampling"),
        "MEMORY USAGE command not available, skipping large key check"
    );
    assert_eq!(unsupported.count("WARN"), 0);

    // Only the first 20 samples are looked at.
    let samples: Vec<String> = (0..25)
        .map(|_| r#"{"key": "k", "type": "hash", "memory_usage": 3145728}"#.to_string())
        .collect();
    let many = run_preflight(
        "h",
        &format!(
            r#"{{"info": {{"redis_version": "7.2.0"}}, "sample_keys": [{}]}}"#,
            samples.join(",")
        ),
    );
    assert_eq!(many.count("WARN"), 20);
}

#[test]
fn eviction_policy_checks() {
    let noeviction = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"}, "config": {"maxmemory-policy": "noeviction"}}"#,
    );
    assert_eq!(
        detail(&noeviction, "Eviction policy"),
        "Policy is 'noeviction' -- writes will fail when memory is full. Consider volatile-lru or allkeys-lru."
    );
    // A managed ElastiCache endpoint is never asked for CONFIG.
    let managed = run_preflight(
        "my-cache.abc.use1.cache.amazonaws.com",
        r#"{"info": {"redis_version": "7.2.0"}, "config": {"maxmemory-policy": "noeviction"}}"#,
    );
    assert!(detail(&managed, "Eviction policy")
        .starts_with("CONFIG command is restricted on ElastiCache."));
    assert_eq!(
        member(member(&managed.to_json(), "info"), "maxmemory_policy"),
        &Json::string("unknown (ElastiCache source)")
    );
    // Without a config answer the policy is unknown and nothing is reported.
    let unknown = run_preflight(
        "h",
        r#"{"info": {"redis_version": "7.2.0"}, "config": null}"#,
    );
    assert_eq!(
        member(member(&unknown.to_json(), "info"), "maxmemory_policy"),
        &Json::string("unknown")
    );
    assert!(unknown
        .findings
        .iter()
        .all(|f| f.check != "Eviction policy"));
}

#[test]
fn connection_failures_are_error_results() {
    let live = preflight::run_preflight("redis.example.com", 6380, None).expect("error result");
    assert!(live.failed());
    assert_eq!(
        live.error.as_deref(),
        Some("Cannot connect to redis.example.com:6380: live Redis/Valkey connections are not available in this build; pass --snapshot FILE")
    );
    assert_eq!(statuses(&live), [("Connectivity".to_string(), "FAIL")]);
    assert_eq!(
        preflight::format_report(&live),
        "ERROR: Cannot connect to redis.example.com:6380: live Redis/Valkey connections are not available in this build; pass --snapshot FILE\n  Host: redis.example.com:6380"
    );
    let value = live.to_json();
    assert_eq!(keys(&value), ["error", "info", "findings"]);

    let refused = run_preflight("h", r#"{"connection_error": "timed out"}"#);
    assert_eq!(
        refused.error.as_deref(),
        Some("Cannot connect to h:6379: timed out")
    );
    assert_eq!(detail(&refused, "Connectivity"), "timed out");
    let auth = run_preflight("h", r#"{"auth_error": "WRONGPASS"}"#);
    assert_eq!(
        auth.error.as_deref(),
        Some("Authentication failed: WRONGPASS")
    );
    assert_eq!(detail(&auth, "Connectivity"), "Auth failed: WRONGPASS");

    // A failed INFO is an ERROR finding, not a FAIL: the exit status stays 0.
    let info_error = run_preflight("h", r#"{"info_error": "NOPERM"}"#);
    assert_eq!(
        detail(&info_error, "Server info"),
        "Could not retrieve INFO: NOPERM"
    );
    assert!(!info_error.failed());
    assert!(preflight::format_report(&info_error).contains("[ERR]  Server info"));
}

#[test]
fn snapshot_without_info_is_rejected() {
    let error = preflight::run_preflight("h", 6379, Some(&json(r#"{"modules": []}"#)))
        .expect_err("info is required");
    assert_eq!(error.code(), "snapshot_invalid");
    assert!(preflight::run_preflight("h", 6379, Some(&json("[]"))).is_err());
    assert!(preflight::run_preflight("h", 6379, Some(&json(r#"{"info": 3}"#))).is_err());
}

#[test]
fn snapshot_text_is_escaped_in_every_report() {
    let hostile = "evil\u{1b}[2J\nFORGED";
    let result = run_preflight(
        hostile,
        r#"{"info": {"redis_version": "7.2.0\u001b[2J", "os": "L\u001b[2Jinux"},
            "modules": [["name", "m\u001b[2J"]], "config": {"maxmemory-policy": "p\u001b[2J"}}"#,
    );
    let text = format!(
        "{}{}",
        preflight::format_report(&result),
        crate::aurora::pyfmt::dumps(&result.to_json())
    );
    assert!(!text.contains('\u{1b}'), "no raw escape character");
    assert!(!text.contains("evil\nFORGED"));
    assert!(text.contains("evil\\x1b[2J\\x0aFORGED"));
    let live = preflight::run_preflight(hostile, 1, None).expect("error result");
    assert!(!preflight::format_report(&live).contains('\u{1b}'));
}

#[test]
fn preflight_command_end_to_end() {
    let dir = TempDir::new("preflight");
    let snapshot = dir.file("snapshot.json", HEALTHY_SNAPSHOT);
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "preflight",
        "--host",
        "redis.internal",
        "--snapshot",
        &snapshot,
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.contains("Verdict: READY TO MIGRATE"));

    let (status, stdout, _) = exec(&[
        "elasticache",
        "preflight",
        "--host",
        "redis.internal",
        "--port",
        "7000",
        "--snapshot",
        &snapshot,
        "--json",
    ]);
    assert_eq!(status, 0);
    let value = json(&stdout);
    assert_eq!(member(member(&value, "info"), "port"), &Json::Int(7000));
    assert_eq!(items(member(&value, "findings")).len(), 11);

    // Live mode: the connection-failure result, exit status 1, text on stdout.
    let (status, stdout, stderr) = exec(&["elasticache", "preflight", "--host", "redis.internal"]);
    assert_eq!((status, stderr.as_str()), (1, ""));
    assert!(stdout.starts_with("ERROR: Cannot connect to redis.internal:6379: live"));

    let blocked = dir.file("old.json", r#"{"info": {"redis_version": "5.0.14"}}"#);
    let (status, stdout, _) = exec(&[
        "elasticache",
        "preflight",
        "--host",
        "h",
        "--snapshot",
        &blocked,
    ]);
    assert_eq!(status, 1);
    assert!(stdout.contains("BLOCKERS FOUND"));

    let (status, stdout, stderr) = exec(&["elasticache", "preflight"]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(stderr.starts_with("error: the following arguments are required: --host\n"));
    let (status, _, stderr) = exec(&["elasticache", "preflight", "--host", "h", "--port", "http"]);
    assert_eq!(status, 2);
    assert!(stderr.contains("argument --port: invalid int value: 'http'"));
    let bad = dir.file("bad.json", "{oops");
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "preflight",
        "--host",
        "h",
        "--snapshot",
        &bad,
    ]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.starts_with("error: JSON parse error"));
    let (status, _, stderr) = exec(&[
        "elasticache",
        "preflight",
        "--host",
        "h",
        "--snapshot",
        &dir.at("missing.json"),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("does not exist"));
}

// ---------------------------------------------------------------------------
// security audit
// ---------------------------------------------------------------------------

const SERVERLESS_DESCRIBE: &str = r#"{
  "serverless_caches": [{
    "ServerlessCacheName": "cache1", "Engine": "valkey", "MajorEngineVersion": "8",
    "Status": "available", "ARN": "arn:aws:elasticache:us-east-1:123456789012:serverlesscache:cache1",
    "UserGroupId": "ug1", "KmsKeyId": "kms-1", "SnapshotRetentionLimit": 7, "DailySnapshotTime": "03:00",
    "CacheUsageLimits": {"DataStorage": {"Maximum": 10, "Unit": "GB"}, "ECPUPerSecond": {"Maximum": 5000}},
    "SecurityGroupIds": ["sg-1"]}],
  "security_groups": [{"GroupId": "sg-1", "IpPermissions": [
    {"FromPort": 6379, "ToPort": 6379,
     "IpRanges": [{"CidrIp": "0.0.0.0/0"}, {"CidrIp": "10.0.0.0/8"}, {"CidrIp": "10.1.0.0/16"}, {"CidrIp": "192.168.1.0/28"}],
     "Ipv6Ranges": [{"CidrIpv6": "::/0"}, {"CidrIpv6": "fd00::/8"}]}]}],
  "tags": [{"Key": "Owner", "Value": "me"}, {"Key": "Environment", "Value": "prod"}]
}"#;

const REPLICATION_DESCRIBE: &str = r#"{
  "replication_groups": [{
    "ReplicationGroupId": "rg1", "Status": "available", "ARN": "arn:rg1", "ClusterEnabled": false,
    "MemberClusters": ["rg1-001", "rg1-002"],
    "NodeGroups": [{"NodeGroupMembers": [{"CacheClusterId": "rg1-001"}, {"CacheClusterId": "rg1-002"}, {"CacheClusterId": "rg1-gone"}, {}]}],
    "TransitEncryptionEnabled": false, "AtRestEncryptionEnabled": false, "AuthTokenEnabled": false,
    "MultiAZ": "disabled", "AutomaticFailover": "disabled", "SnapshotRetentionLimit": 0}],
  "cache_clusters": {
    "rg1-001": {"Engine": "redis", "EngineVersion": "6.2", "CacheNodeType": "cache.r6g.large",
                "SecurityGroups": [{"SecurityGroupId": "sg-b"}, {"SecurityGroupId": "sg-a"}]},
    "rg1-002": {"SecurityGroups": [{"SecurityGroupId": "sg-a"}]},
    "rg1-gone": {"error": "CacheClusterNotFound"}},
  "security_groups": [{"GroupId": "sg-a", "IpPermissions": []}, {"GroupId": "sg-b", "IpPermissions": []}],
  "tags": []
}"#;

fn audit_findings(result: &Audit) -> Vec<(String, &'static str, String)> {
    match result {
        Audit::Report { findings, .. } => findings
            .iter()
            .map(|f| (f.check.clone(), f.status, f.detail.clone()))
            .collect(),
        Audit::NotFound(message) => panic!("not found: {message}"),
    }
}

fn audit_info(result: &Audit) -> Json {
    match result {
        Audit::Report { info, .. } => Json::Object(info.clone()),
        Audit::NotFound(message) => panic!("not found: {message}"),
    }
}

#[test]
fn serverless_audit_findings() {
    let result = audit::audit_serverless(&json(SERVERLESS_DESCRIBE), "cache1");
    assert!(!result.failed());
    let findings = audit_findings(&result);
    let f = |check: &str, status: &'static str, detail: &str| {
        (check.to_string(), status, detail.to_string())
    };
    assert_eq!(
        findings,
        vec![
            f("TLS in-transit encryption", "PASS", "Always enabled on serverless"),
            f("Authentication", "PASS", "RBAC user group: ug1"),
            f("Encryption at rest", "PASS", "KMS key: kms-1"),
            f("Multi-AZ", "PASS", "Always enabled on serverless"),
            f("Automatic backups", "PASS", "Retention: 7 days, daily snapshot time: 03:00"),
            f("Data storage limit", "PASS", "10 GB max"),
            f("ECPU limit", "PASS", "5000 ECPUs/sec max"),
            f("Security group sg-1", "WARN", "Inbound rule allows 0.0.0.0/0 on ports 6379-6379 -- no public endpoint exists, but 0.0.0.0/0 violates least-privilege"),
            f("Security group sg-1", "WARN", "Inbound rule allows 10.0.0.0/8 on ports 6379-6379 -- very broad CIDR"),
            f("Security group sg-1", "WARN", "Inbound rule allows ::/0 (IPv6) on ports 6379-6379 -- no public endpoint exists, but ::/0 violates least-privilege"),
            f("Resource tags", "WARN", "Missing recommended tags: managed_by, Application"),
        ],
        "/16, /28 and fd00::/8 are not flagged"
    );
    let info = audit_info(&result);
    assert_eq!(
        keys(&info),
        [
            "cache_type",
            "cache_name",
            "engine",
            "engine_version",
            "status",
            "arn",
            "tls_enabled",
            "auth_mode",
            "user_group",
            "encryption_at_rest",
            "kms_key",
            "multi_az",
            "snapshot_retention_days",
            "daily_backups",
            "max_data_gb",
            "max_ecpu_per_sec",
            "security_groups",
            "tags"
        ]
    );
    assert_eq!(member(&info, "auth_mode"), &Json::string("RBAC"));
    assert_eq!(member(&info, "snapshot_retention_days"), &Json::Int(7));
    assert_eq!(member(&info, "security_groups"), &json(r#"["sg-1"]"#));
    assert_eq!(
        member(&info, "tags"),
        &json(r#"{"Owner": "me", "Environment": "prod"}"#)
    );
    let report = audit::format_report(&result);
    assert!(report.contains("  Summary: 7 passed, 4 warnings, 0 failures\n"));
    assert!(report.contains("  Cache name:      cache1\n"));
    assert!(report.contains("  Engine:          valkey 8\n"));
    assert!(report.contains("  Auth mode:       RBAC\n"));
    assert!(!report.contains("RECOMMENDED ACTIONS"));
    assert!(report.ends_with("  Consult your security team before production deployment.\n"));
}

#[test]
fn serverless_audit_without_protections() {
    let describe = json(
        r#"{"serverless_caches": [{"Status": "creating", "SnapshotRetentionLimit": 0}], "tags": []}"#,
    );
    let result = audit::audit_serverless(&describe, "bare");
    assert!(result.failed());
    let findings = audit_findings(&result);
    let by_check = |check: &str| {
        findings
            .iter()
            .find(|(name, _, _)| name == check)
            .unwrap_or_else(|| panic!("missing {check}"))
            .clone()
    };
    assert_eq!(
        by_check("Authentication"),
        (
            "Authentication".to_string(),
            "WARN",
            "No user group attached — using default user only".to_string()
        )
    );
    assert_eq!(
        by_check("Encryption at rest").2,
        "Always enabled on serverless (using AWS-managed key)"
    );
    assert_eq!(
        by_check("Automatic backups"),
        (
            "Automatic backups".to_string(),
            "FAIL",
            "No automatic backups configured, SnapshotRetentionLimit is 0 or not set".to_string()
        )
    );
    assert_eq!(
        by_check("Data storage limit").2,
        "No data storage limit set — costs could grow unbounded"
    );
    assert_eq!(
        by_check("ECPU limit").2,
        "No ECPU limit set — costs could grow unbounded"
    );
    assert_eq!(by_check("Security groups").2, "No security groups found");
    assert_eq!(
        by_check("Resource tags").2,
        "Missing recommended tags: Environment, managed_by, Application, Owner"
    );
    let info = audit_info(&result);
    assert_eq!(member(&info, "engine"), &Json::string("unknown"));
    assert_eq!(
        member(&info, "auth_mode"),
        &Json::string("default-user-only")
    );
    assert_eq!(member(&info, "daily_backups"), &Json::Bool(false));
    let report = audit::format_report(&result);
    assert!(report.contains("RECOMMENDED ACTIONS"));
    assert!(report.contains("  - Enable automatic backups: modify the replication group with\n    SnapshotRetentionLimit > 0 (recommended: 7 days).\n"));
}

#[test]
fn missing_resources_are_errors() {
    let empty = json(r#"{"serverless_caches": []}"#);
    let result = audit::audit_serverless(&empty, "gone\u{1b}[2J");
    assert!(result.failed());
    assert_eq!(
        audit::format_report(&result),
        "ERROR: Serverless cache 'gone\\x1b[2J' not found"
    );
    assert_eq!(
        result.to_json(),
        json(r#"{"error": "Serverless cache 'gone\\x1b[2J' not found"}"#)
    );
    // The recorded answer must be for the audited cache.
    let other = audit::audit_serverless(&json(SERVERLESS_DESCRIBE), "other-cache");
    assert_eq!(
        other,
        Audit::NotFound("Serverless cache 'other-cache' not found".to_string())
    );
    let rg = audit::audit_replication_group(&json("{}"), "rg9");
    assert_eq!(
        rg,
        Audit::NotFound("Replication group 'rg9' not found".to_string())
    );
    let mismatch = audit::audit_replication_group(&json(REPLICATION_DESCRIBE), "rg2");
    assert_eq!(
        mismatch,
        Audit::NotFound("Replication group 'rg2' not found".to_string())
    );
}

#[test]
fn replication_group_audit_findings() {
    let result = audit::audit_replication_group(&json(REPLICATION_DESCRIBE), "rg1");
    assert!(result.failed());
    let findings = audit_findings(&result);
    let statuses: Vec<(&str, &str)> = findings.iter().map(|(c, s, _)| (c.as_str(), *s)).collect();
    assert_eq!(
        statuses,
        [
            ("TLS in-transit encryption", "FAIL"),
            ("Encryption at rest", "FAIL"),
            ("Authentication", "FAIL"),
            ("Multi-AZ", "WARN"),
            ("Automatic failover", "WARN"),
            ("Automatic backups", "FAIL"),
            ("Security groups", "PASS"),
            ("Resource tags", "WARN"),
        ]
    );
    assert_eq!(
        findings[6].2, "No overly permissive rules in sg-a, sg-b",
        "ids are sorted"
    );
    assert_eq!(
        findings[7].2,
        "Missing recommended tags: Environment, managed_by, Application, Owner"
    );
    let info = audit_info(&result);
    assert_eq!(
        keys(&info),
        [
            "cache_type",
            "replication_group_id",
            "engine",
            "status",
            "arn",
            "cluster_enabled",
            "num_node_groups",
            "engine_version",
            "node_type",
            "tls_enabled",
            "encryption_at_rest",
            "auth_mode",
            "multi_az",
            "automatic_failover",
            "snapshot_retention_days",
            "security_groups",
            "tags"
        ]
    );
    assert_eq!(member(&info, "engine"), &Json::string("redis"));
    assert_eq!(member(&info, "engine_version"), &Json::string("6.2"));
    assert_eq!(member(&info, "node_type"), &Json::string("cache.r6g.large"));
    assert_eq!(member(&info, "num_node_groups"), &Json::Int(1));
    assert_eq!(member(&info, "auth_mode"), &Json::string("none"));
    assert_eq!(member(&info, "tags"), &Json::Object(Vec::new()));

    let report = audit::format_report(&result);
    assert!(report.contains("  Replication group: rg1\n"));
    assert!(report.contains("  Node type:       cache.r6g.large\n"));
    assert!(report.contains("  Cluster mode:    disabled\n"));
    assert!(report.contains("  Node groups:     1\n"));
    assert!(report.contains("  Summary: 1 passed, 3 warnings, 4 failures\n"));
    assert!(report.contains("  - Enable in-transit encryption. For existing node-based clusters, use a\n    two-step process: first set transit-encryption-mode to 'preferred',\n    then after migrating all clients to TLS, set it to 'required'.\n"));
    assert!(report.contains("  - Enable at-rest encryption. For existing clusters, this requires\n    creating a new cluster with AtRestEncryptionEnabled=true and migrating.\n"));
    assert!(report.contains("  - Configure RBAC: create users, a user group, and attach it to the cache.\n    See references/setup/iam-policies.md for IAM auth patterns.\n"));
    assert!(report.contains("  - Enable automatic backups: modify the replication group with\n"));
    let fail_at = report
        .find("[FAIL] TLS in-transit encryption")
        .expect("fail");
    let warn_at = report.find("[WARN] Multi-AZ").expect("warn");
    let ok_at = report.find("[OK]   Security groups").expect("ok");
    assert!(fail_at < warn_at && warn_at < ok_at);
}

#[test]
fn replication_group_authentication_modes() {
    let with = |extra: &str| {
        let text = format!(
            r#"{{"replication_groups": [{{"ReplicationGroupId": "g", {extra}}}], "tags": []}}"#
        );
        audit::audit_replication_group(&json(&text), "g")
    };
    let rbac = with(
        r#""UserGroupIds": ["ug-1", "ug-2"], "TransitEncryptionEnabled": true, "AtRestEncryptionEnabled": true, "KmsKeyId": "key-1", "MultiAZ": "enabled", "AutomaticFailover": "enabled", "SnapshotRetentionLimit": 5"#,
    );
    assert!(!rbac.failed());
    let findings = audit_findings(&rbac);
    assert_eq!(findings[0].2, "Enabled");
    assert_eq!(findings[1].2, "Enabled (key: key-1)");
    assert_eq!(findings[2].2, "RBAC user groups: ug-1, ug-2");
    assert_eq!(
        findings[3],
        ("Multi-AZ".to_string(), "PASS", "Enabled".to_string())
    );
    assert_eq!(findings[5].2, "Retention: 5 days");
    assert_eq!(
        member(&audit_info(&rbac), "auth_mode"),
        &Json::string("RBAC")
    );
    assert_eq!(
        member(&audit_info(&rbac), "user_groups"),
        &json(r#"["ug-1", "ug-2"]"#)
    );
    assert_eq!(
        member(&audit_info(&rbac), "kms_key"),
        &Json::string("key-1")
    );

    let token = with(r#""AuthTokenEnabled": true, "AtRestEncryptionEnabled": true"#);
    let findings = audit_findings(&token);
    assert_eq!(
        findings[2],
        (
            "Authentication".to_string(),
            "WARN",
            "Using legacy AUTH token — consider migrating to RBAC".to_string()
        )
    );
    assert_eq!(findings[1].2, "Enabled (key: AWS-managed)");

    // No member cluster answer: unknown engine details.
    let unknown = with(r#""MemberClusters": ["missing-001"]"#);
    let info = audit_info(&unknown);
    assert_eq!(member(&info, "engine"), &Json::string("unknown"));
    assert_eq!(member(&info, "engine_version"), &Json::string("unknown"));
    assert_eq!(member(&info, "node_type"), &Json::string("unknown"));
    let none = with(r#""Status": "available""#);
    assert!(audit_info(&none).get("engine_version").is_none());
}

#[test]
fn security_group_inspection_failures_are_errors_not_failures() {
    let mut text = SERVERLESS_DESCRIBE.to_string();
    text = text.replace(
        "\"tags\": [",
        "\"security_groups_error\": \"AccessDenied \\u001b[2J\", \"tags\": [",
    );
    let result = audit::audit_serverless(&json(&text), "cache1");
    let findings = audit_findings(&result);
    let sg = findings
        .iter()
        .find(|(check, _, _)| check == "Security groups")
        .expect("security groups finding");
    assert_eq!(sg.1, "ERROR");
    assert_eq!(sg.2, "Could not inspect: AccessDenied \\x1b[2J");
    assert!(!result.failed(), "ERROR is not FAIL");
    assert!(audit::format_report(&result).contains("[ERR]  Security groups"));
}

#[test]
fn audit_text_is_escaped() {
    let describe = json(
        r#"{"serverless_caches": [{"Engine": "v\u001b[2J", "UserGroupId": "ug\u001b[2J", "KmsKeyId": "k\u001b[2J",
            "SecurityGroupIds": ["sg\u001b[2J"], "SnapshotRetentionLimit": 3, "DailySnapshotTime": "t\u001b[2J"}],
            "security_groups": [{"GroupId": "sg\u001b[2J", "IpPermissions": [{"IpRanges": [{"CidrIp": "1.1.1.1/0\u001b[2J"}]}]}]}"#,
    );
    let result = audit::audit_serverless(&describe, "x");
    let text = format!(
        "{}{}",
        audit::format_report(&result),
        crate::aurora::pyfmt::dumps(&result.to_json())
    );
    // Values are data in the JSON (escaped by JSON), text in the report.
    assert!(!audit::format_report(&result).contains('\u{1b}'));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn audit_command_end_to_end() {
    let dir = TempDir::new("audit");
    let describe = dir.file("serverless.json", SERVERLESS_DESCRIBE);
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "audit",
        "--serverless",
        "cache1",
        "--describe",
        &describe,
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(stdout.contains("ElastiCache Security Audit Report"));

    let (status, stdout, _) = exec(&[
        "elasticache",
        "audit",
        "--serverless",
        "cache1",
        "--describe",
        &describe,
        "--json",
    ]);
    assert_eq!(status, 0);
    let value = json(&stdout);
    assert_eq!(
        member(member(&value, "info"), "cache_name"),
        &Json::string("cache1")
    );
    assert_eq!(items(member(&value, "findings")).len(), 11);

    let rg = dir.file("rg.json", REPLICATION_DESCRIBE);
    let (status, stdout, _) = exec(&[
        "elasticache",
        "audit",
        "--replication-group",
        "rg1",
        "--describe",
        &rg,
    ]);
    assert_eq!(status, 1);
    assert!(stdout.contains("RECOMMENDED ACTIONS"));

    let (status, stdout, _) = exec(&[
        "elasticache",
        "audit",
        "--replication-group",
        "other",
        "--describe",
        &rg,
    ]);
    assert_eq!(
        (status, stdout.as_str()),
        (1, "ERROR: Replication group 'other' not found\n")
    );

    // Live mode: no AWS client in this build.
    let (status, stdout, stderr) = exec(&["elasticache", "audit", "--serverless", "cache1"]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.starts_with(
        "error: Failed to create AWS session: AWS SDK access is not available in this build"
    ));

    let (status, _, stderr) = exec(&["elasticache", "audit", "--describe", &describe]);
    assert_eq!(status, 2);
    assert!(stderr
        .starts_with("error: one of the arguments --serverless --replication-group is required"));
    let (status, _, stderr) = exec(&[
        "elasticache",
        "audit",
        "--serverless",
        "a",
        "--replication-group",
        "b",
        "--describe",
        &describe,
    ]);
    assert_eq!(status, 2);
    assert!(stderr.contains("argument --replication-group: not allowed with argument --serverless"));
}

// ---------------------------------------------------------------------------
// dashboards
// ---------------------------------------------------------------------------

fn template(args: &[&str]) -> (u8, String, Json) {
    let mut full = vec!["elasticache", "dashboard"];
    full.extend_from_slice(args);
    let (status, stdout, stderr) = exec(&full);
    assert_eq!(stderr, "", "{args:?}");
    let value = if stdout.trim_start().starts_with('{') {
        json(&stdout)
    } else {
        Json::Null
    };
    (status, stdout, value)
}

fn resources(template: &Json) -> &Json {
    member(template, "Resources")
}

fn properties<'a>(template: &'a Json, key: &str) -> &'a Json {
    member(member(resources(template), key), "Properties")
}

fn widget_schema() -> Schema {
    ObjectSchema::new()
        .required("type", Schema::String)
        .required(
            "properties",
            ObjectSchema::new()
                .required("title", Schema::String)
                .required("region", Schema::String)
                .required(
                    "metrics",
                    Schema::Array(Box::new(Schema::Array(Box::new(Schema::String)))),
                )
                .required("period", Schema::Integer)
                .required("stat", Schema::String)
                .required("view", Schema::String)
                .into(),
        )
        .required("width", Schema::Integer)
        .required("height", Schema::Integer)
        .into()
}

fn alarm_schema() -> Schema {
    let dimension: Schema = ObjectSchema::new()
        .required("Name", Schema::String)
        .required("Value", Schema::String)
        .into();
    let reference: Schema = ObjectSchema::new().required("Ref", Schema::String).into();
    ObjectSchema::new()
        .required("Type", Schema::String)
        .required(
            "Properties",
            ObjectSchema::new()
                .required("AlarmName", Schema::String)
                .required("AlarmDescription", Schema::String)
                .required("Namespace", Schema::String)
                .required("MetricName", Schema::String)
                .required("Dimensions", Schema::Array(Box::new(dimension)))
                .required("Threshold", Schema::Number)
                .required("ComparisonOperator", Schema::String)
                .required("Period", Schema::Integer)
                .required("EvaluationPeriods", Schema::Integer)
                .required("DatapointsToAlarm", Schema::Integer)
                .required("Statistic", Schema::String)
                .required("TreatMissingData", Schema::String)
                .optional("AlarmActions", Schema::Array(Box::new(reference.clone())))
                .optional("OKActions", Schema::Array(Box::new(reference)))
                .into(),
        )
        .into()
}

fn dashboard_schema() -> Schema {
    ObjectSchema::new()
        .required("Type", Schema::String)
        .required(
            "Properties",
            ObjectSchema::new()
                .required("DashboardName", Schema::String)
                .required("DashboardBody", Schema::String)
                .into(),
        )
        .into()
}

/// The structural contract of every generated template (AC2).
fn assert_schema_valid(template: &Json) {
    let top: Schema = ObjectSchema::new()
        .required("AWSTemplateFormatVersion", Schema::String)
        .required("Description", Schema::String)
        .required("Resources", Schema::Any)
        .optional("Parameters", Schema::Any)
        .into();
    validate(&top, template).expect("template shape");
    assert_eq!(
        member(template, "AWSTemplateFormatVersion"),
        &Json::string("2010-09-09")
    );
    let Json::Object(entries) = resources(template) else {
        panic!("Resources must be an object");
    };
    let mut seen = Vec::new();
    for (logical_id, resource) in entries {
        assert!(
            !logical_id.is_empty() && logical_id.chars().all(|c| c.is_ascii_alphanumeric()),
            "logical id {logical_id:?} must be alphanumeric"
        );
        assert!(
            !seen.contains(logical_id),
            "duplicate logical id {logical_id}"
        );
        seen.push(logical_id.clone());
        match member(resource, "Type").as_str() {
            Some("AWS::CloudWatch::Dashboard") => {
                validate(&dashboard_schema(), resource).expect("dashboard shape");
                let body = json(
                    member(member(resource, "Properties"), "DashboardBody")
                        .as_str()
                        .expect("body text"),
                );
                validate(
                    &ObjectSchema::new()
                        .required("widgets", Schema::Array(Box::new(widget_schema())))
                        .into(),
                    &body,
                )
                .expect("dashboard body shape");
                for widget in items(member(&body, "widgets")) {
                    let metrics = items(member(member(widget, "properties"), "metrics"));
                    assert!(!metrics.is_empty());
                    for metric in metrics {
                        let parts = items(metric);
                        assert_eq!(parts.len(), 4, "namespace, metric, dimension name, value");
                        assert_eq!(parts[0], Json::string("AWS/ElastiCache"));
                    }
                    let stat = member(member(widget, "properties"), "stat")
                        .as_str()
                        .expect("stat");
                    assert!(["Average", "Sum", "Maximum", "Minimum"].contains(&stat));
                }
            }
            Some("AWS::CloudWatch::Alarm") => {
                validate(&alarm_schema(), resource).expect("alarm shape");
                let props = member(resource, "Properties");
                assert!(["LessThanThreshold", "GreaterThanThreshold"].contains(
                    &member(props, "ComparisonOperator")
                        .as_str()
                        .expect("operator")
                ));
                assert!(["Average", "Sum", "Maximum"]
                    .contains(&member(props, "Statistic").as_str().expect("statistic")));
                let Json::Int(datapoints) = member(props, "DatapointsToAlarm") else {
                    panic!("integer datapoints");
                };
                let Json::Int(periods) = member(props, "EvaluationPeriods") else {
                    panic!("integer periods");
                };
                assert!(datapoints <= periods && *datapoints >= 1);
            }
            other => panic!("unexpected resource type {other:?}"),
        }
    }
}

#[test]
fn serverless_template_defaults() {
    let (status, _, value) = template(&["--serverless", "my-cache"]);
    assert_eq!(status, 0);
    assert_schema_valid(&value);
    assert_eq!(
        keys(&value),
        ["AWSTemplateFormatVersion", "Description", "Resources"]
    );
    assert_eq!(
        member(&value, "Description"),
        &Json::string("ElastiCache observability stack for my-cache")
    );
    assert_eq!(
        keys(resources(&value)),
        ["mycacheDashboard", "CacheHitRateAlarm"]
    );
    let dashboard = properties(&value, "mycacheDashboard");
    assert_eq!(
        member(dashboard, "DashboardName"),
        &Json::string("ElastiCache-my-cache")
    );
    let body = json(member(dashboard, "DashboardBody").as_str().expect("body"));
    let titles: Vec<&str> = items(member(&body, "widgets"))
        .iter()
        .map(|w| {
            member(member(w, "properties"), "title")
                .as_str()
                .expect("title")
        })
        .collect();
    assert_eq!(
        titles,
        [
            "Cache Hit Rate",
            "ECPU Consumption",
            "Throttled Commands",
            "Read / Write Latency",
            "Current Connections",
            "New Connections",
            "Bytes Used For Cache",
            "Command-Family Breakdown",
            "Total Commands Count",
            "Item Counts"
        ]
    );
    let first = &items(member(&body, "widgets"))[0];
    assert_eq!(
        member(member(first, "properties"), "metrics"),
        &json(r#"[["AWS/ElastiCache", "CacheHitRate", "ServerlessCacheName", "my-cache"]]"#)
    );
    assert_eq!(
        member(member(first, "properties"), "region"),
        &Json::string("us-east-1")
    );
    assert_eq!(
        member(member(first, "properties"), "period"),
        &Json::Int(300)
    );
    assert_eq!(member(first, "width"), &Json::Int(12));
    assert_eq!(member(first, "height"), &Json::Int(6));

    let alarm = properties(&value, "CacheHitRateAlarm");
    assert_eq!(
        member(alarm, "AlarmName"),
        &Json::string("my-cache-low-hit-rate")
    );
    assert_eq!(
        member(alarm, "AlarmDescription"),
        &Json::string("Cache hit rate below 80% for my-cache")
    );
    assert_eq!(
        member(alarm, "Threshold"),
        &Json::Int(80),
        "the default is an integer"
    );
    assert_eq!(
        member(alarm, "ComparisonOperator"),
        &Json::string("LessThanThreshold")
    );
    assert_eq!(member(alarm, "Period"), &Json::Int(300));
    assert_eq!(member(alarm, "EvaluationPeriods"), &Json::Int(6));
    assert_eq!(member(alarm, "DatapointsToAlarm"), &Json::Int(4));
    assert_eq!(member(alarm, "Statistic"), &Json::string("Average"));
    assert_eq!(
        member(alarm, "TreatMissingData"),
        &Json::string("notBreaching")
    );
    assert!(alarm.get("AlarmActions").is_none());
    assert!(value.get("Parameters").is_none());
}

#[test]
fn explicit_thresholds_are_floats() {
    let (_, stdout, value) = template(&["--serverless", "c", "--hit-rate-threshold", "75"]);
    assert_schema_valid(&value);
    assert!(stdout.contains("\"Threshold\": 75.0,"));
    assert_eq!(
        member(properties(&value, "CacheHitRateAlarm"), "AlarmDescription"),
        &Json::string("Cache hit rate below 75.0% for c")
    );
}

#[test]
fn every_serverless_alarm_in_order() {
    let (status, stdout, value) = template(&[
        "--serverless",
        "my-cache",
        "--region",
        "eu-west-1",
        "--sns-topic",
        "arn:aws:sns:us-east-1:123456789012:alerts",
        "--max-storage-gb",
        "10",
        "--throttle-threshold",
        "5",
        "--read-latency-threshold",
        "2000",
        "--write-latency-threshold",
        "3000",
        "--ecpu-threshold",
        "100000",
        "--evictions-threshold",
        "1",
    ]);
    assert_eq!(status, 0);
    assert_schema_valid(&value);
    assert_eq!(
        keys(resources(&value)),
        [
            "mycacheDashboard",
            "CacheHitRateAlarm",
            "ThrottledCmdsAlarm",
            "StorageLimitAlarm",
            "ReadLatencyAlarm",
            "WriteLatencyAlarm",
            "ECPUSpikeAlarm",
            "EvictionsAlarm"
        ]
    );
    let storage = properties(&value, "StorageLimitAlarm");
    assert_eq!(member(storage, "Threshold"), &Json::Int(8_589_934_592));
    assert_eq!(
        member(storage, "AlarmDescription"),
        &Json::string("Storage approaching 80% of configured limit (10.0 GB) on my-cache")
    );
    assert_eq!(member(storage, "Statistic"), &Json::string("Maximum"));
    assert_eq!(
        member(properties(&value, "ThrottledCmdsAlarm"), "AlarmDescription"),
        &Json::string("Throttled commands above 5.0 on my-cache")
    );
    assert_eq!(
        member(properties(&value, "ReadLatencyAlarm"), "AlarmDescription"),
        &Json::string("Read latency above 2000.0us on my-cache")
    );
    assert_eq!(
        member(properties(&value, "ECPUSpikeAlarm"), "Period"),
        &Json::Int(300)
    );
    assert_eq!(
        member(properties(&value, "EvictionsAlarm"), "MetricName"),
        &Json::string("Evictions")
    );
    // The SNS topic becomes a parameter and every alarm references it.
    assert_eq!(
        member(
            member(member(&value, "Parameters"), "SNSTopicARN"),
            "Default"
        ),
        &Json::string("arn:aws:sns:us-east-1:123456789012:alerts")
    );
    for key in ["CacheHitRateAlarm", "StorageLimitAlarm", "EvictionsAlarm"] {
        let alarm = properties(&value, key);
        assert_eq!(
            member(alarm, "AlarmActions"),
            &json(r#"[{"Ref": "SNSTopicARN"}]"#)
        );
        assert_eq!(
            member(alarm, "OKActions"),
            &json(r#"[{"Ref": "SNSTopicARN"}]"#)
        );
    }
    assert!(stdout.contains("eu-west-1"));
}

#[test]
fn storage_alarm_percentage_and_truncation() {
    let (_, _, value) = template(&[
        "--serverless",
        "c",
        "--max-storage-gb",
        "1.5",
        "--storage-alarm-pct",
        "50",
    ]);
    let storage = properties(&value, "StorageLimitAlarm");
    // int(1.5 * 0.5 * 2**30) = 805306368
    assert_eq!(member(storage, "Threshold"), &Json::Int(805_306_368));
    assert_eq!(
        member(storage, "AlarmDescription"),
        &Json::string("Storage approaching 50.0% of configured limit (1.5 GB) on c")
    );
    let (status, _, stderr) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "c",
        "--max-storage-gb",
        "inf",
    ]);
    assert_eq!(status, 2);
    assert!(stderr.contains("argument --max-storage-gb: must be a finite number"));
}

#[test]
fn node_based_template_with_node_ids() {
    let (status, _, value) = template(&[
        "--replication-group",
        "my-rg",
        "--node-ids",
        "my-rg-001, my-rg-002,,",
        "--replication-lag-threshold",
        "30",
        "--new-connections-threshold",
        "1000",
    ]);
    assert_eq!(status, 0);
    assert_schema_valid(&value);
    assert_eq!(
        keys(resources(&value)),
        [
            "myrgDashboard",
            "CacheHitRateAlarmmyrg001",
            "EngineCPUAlarmmyrg001",
            "MemoryAlarmmyrg001",
            "ReplicationLagAlarmmyrg001",
            "NewConnectionsAlarmmyrg001",
            "CacheHitRateAlarmmyrg002",
            "EngineCPUAlarmmyrg002",
            "MemoryAlarmmyrg002",
            "ReplicationLagAlarmmyrg002",
            "NewConnectionsAlarmmyrg002",
        ]
    );
    let body = json(
        member(properties(&value, "myrgDashboard"), "DashboardBody")
            .as_str()
            .expect("body"),
    );
    let widgets = items(member(&body, "widgets"));
    assert_eq!(widgets.len(), 9);
    let cpu = member(member(&widgets[1], "properties"), "metrics");
    assert_eq!(
        cpu,
        &json(
            r#"[["AWS/ElastiCache", "EngineCPUUtilization", "CacheClusterId", "my-rg-001"],
                ["AWS/ElastiCache", "EngineCPUUtilization", "CacheClusterId", "my-rg-002"],
                ["AWS/ElastiCache", "CPUUtilization", "CacheClusterId", "my-rg-001"],
                ["AWS/ElastiCache", "CPUUtilization", "CacheClusterId", "my-rg-002"]]"#
        )
    );
    let cpu_alarm = properties(&value, "EngineCPUAlarmmyrg001");
    assert_eq!(member(cpu_alarm, "Threshold"), &Json::Int(90));
    assert_eq!(
        member(cpu_alarm, "AlarmDescription"),
        &Json::string("Engine CPU above 90% on my-rg-001")
    );
    assert_eq!(member(cpu_alarm, "Statistic"), &Json::string("Maximum"));
    assert_eq!(
        member(cpu_alarm, "Dimensions"),
        &json(r#"[{"Name": "CacheClusterId", "Value": "my-rg-001"}]"#)
    );
    assert_eq!(
        member(
            properties(&value, "ReplicationLagAlarmmyrg002"),
            "AlarmDescription"
        ),
        &Json::string("Replication lag above 30.0s on my-rg-002")
    );
    assert_eq!(
        member(
            properties(&value, "NewConnectionsAlarmmyrg001"),
            "AlarmDescription"
        ),
        &Json::string("New connections above 1000.0/min on my-rg-001")
    );
    assert_eq!(
        member(properties(&value, "MemoryAlarmmyrg001"), "Threshold"),
        &Json::Int(80)
    );
}

#[test]
fn node_based_template_falls_back_to_the_replication_group_dimension() {
    let (_, _, value) = template(&["--replication-group", "rg-1"]);
    assert_schema_valid(&value);
    assert_eq!(
        keys(resources(&value)),
        [
            "rg1Dashboard",
            "CacheHitRateAlarmrg1",
            "EngineCPUAlarmrg1",
            "MemoryAlarmrg1"
        ]
    );
    let body = json(
        member(properties(&value, "rg1Dashboard"), "DashboardBody")
            .as_str()
            .expect("body"),
    );
    let widgets = items(member(&body, "widgets"));
    assert_eq!(widgets.len(), 7);
    assert_eq!(
        member(member(&widgets[0], "properties"), "metrics"),
        &json(r#"[["AWS/ElastiCache", "CacheHitRate", "ReplicationGroupId", "rg-1"]]"#)
    );
    assert_eq!(
        member(properties(&value, "CacheHitRateAlarmrg1"), "Dimensions"),
        &json(r#"[{"Name": "ReplicationGroupId", "Value": "rg-1"}]"#)
    );
    // Blank node ids mean no node ids.
    let (_, _, blank) = template(&["--replication-group", "rg-1", "--node-ids", " , ,"]);
    assert_eq!(blank, value);
}

#[test]
fn colliding_logical_ids_overwrite_in_place() {
    // "a-b" and "ab" both become "ab": the later alarm replaces the earlier one.
    let (_, _, value) = template(&["--replication-group", "rg", "--node-ids", "a-b,ab"]);
    assert_schema_valid(&value);
    assert_eq!(
        keys(resources(&value)),
        [
            "rgDashboard",
            "CacheHitRateAlarmab",
            "EngineCPUAlarmab",
            "MemoryAlarmab"
        ]
    );
    assert_eq!(
        member(properties(&value, "CacheHitRateAlarmab"), "AlarmName"),
        &Json::string("ab-low-hit-rate")
    );
}

#[test]
fn dashboard_and_alarm_switches() {
    let (_, _, no_dashboard) = template(&["--serverless", "c", "--no-dashboard"]);
    assert_eq!(keys(resources(&no_dashboard)), ["CacheHitRateAlarm"]);
    let (_, _, no_alarms) = template(&["--serverless", "c", "--no-alarms"]);
    assert_eq!(keys(resources(&no_alarms)), ["cDashboard"]);
    let (_, stdout, neither) = template(&["--serverless", "c", "--no-alarms", "--no-dashboard"]);
    assert_eq!(member(&neither, "Resources"), &Json::Object(Vec::new()));
    assert!(stdout.contains("\"Resources\": {}"));
    assert_schema_valid(&neither);
    // An identifier without any alphanumeric character still gets a valid id.
    let (_, _, odd) = template(&["--serverless", "---", "--no-alarms"]);
    assert_eq!(keys(resources(&odd)), ["Dashboard"]);
    assert_schema_valid(&odd);
}

#[test]
fn dashboard_body_uses_python_default_separators() {
    let (_, _, value) = template(&["--serverless", "c", "--no-alarms"]);
    let body = member(properties(&value, "cDashboard"), "DashboardBody")
        .as_str()
        .expect("body");
    assert!(body.starts_with("{\"widgets\": [{\"type\": \"metric\", \"properties\": {\"title\": \"Cache Hit Rate\", \"region\": \"us-east-1\", \"metrics\": [[\"AWS/ElastiCache\", \"CacheHitRate\", \"ServerlessCacheName\", \"c\"]], \"period\": 300, \"stat\": \"Average\", \"view\": \"timeSeries\"}, \"width\": 12, \"height\": 6}"));
}

#[test]
fn hostile_identifiers_stay_inside_json_strings() {
    let (status, stdout, value) = template(&["--serverless", "x\"},\u{1b}[2J\n\"evil"]);
    assert_eq!(status, 0);
    assert_schema_valid(&value);
    assert!(
        !stdout.contains('\u{1b}'),
        "JSON escapes control characters"
    );
    assert_eq!(
        keys(resources(&value)),
        ["x2JevilDashboard", "CacheHitRateAlarm"]
    );
}

#[test]
fn dashboard_output_file_and_deploy_hint() {
    let dir = TempDir::new("dashboard");
    let target = dir.at("stack.json");
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "my-cache",
        "--output",
        &target,
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        format!(
            "Written to {target}\n\nDeploy with:\n  aws cloudformation deploy --template-file {target} --stack-name my-cache-observability\n"
        )
    );
    let written = fs::read_to_string(&target).expect("file written");
    assert!(
        !written.ends_with('\n'),
        "the file has no trailing newline, like the script"
    );
    let (_, printed, _) = template(&["--serverless", "my-cache"]);
    assert_eq!(format!("{written}\n"), printed);
    assert_schema_valid(&json(&written));

    // Awkward names are shell-quoted in the hint.
    let spaced = dir.at("my stack.json");
    let (_, stdout, _) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "a b;c",
        "--output",
        &spaced,
    ]);
    assert!(stdout.contains(&format!(
        "--template-file '{spaced}' --stack-name 'a b;c-observability'"
    )));

    #[cfg(unix)]
    {
        let link = dir.at("link.json");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let (status, _, stderr) = exec(&[
            "elasticache",
            "dashboard",
            "--serverless",
            "c",
            "--output",
            &link,
        ]);
        assert_eq!(status, 1);
        assert!(stderr.contains("refusing to follow symbolic link"));
    }
}

#[test]
fn dashboard_arguments_are_checked() {
    let (status, _, stderr) = exec(&["elasticache", "dashboard"]);
    assert_eq!(status, 2);
    assert!(stderr
        .starts_with("error: one of the arguments --serverless --replication-group is required"));
    let (status, _, stderr) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "a",
        "--replication-group",
        "b",
    ]);
    assert_eq!(status, 2);
    assert!(stderr.contains("not allowed with argument --serverless"));
    let (status, _, stderr) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "a",
        "--memory-threshold",
        "lots",
    ]);
    assert_eq!(status, 2);
    assert!(stderr.contains("argument --memory-threshold: invalid float value: 'lots'"));
    // Negative thresholds are values, not flags.
    let (status, _, _) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "a",
        "--throttle-threshold",
        "-1",
    ]);
    assert_eq!(status, 0);
}

// ---------------------------------------------------------------------------
// price command
// ---------------------------------------------------------------------------

fn price(dir: &TempDir, extra: &[&str]) -> (u8, String, String) {
    let bulk = dir.file("bulk.json", BULK_JSON);
    let mut args = vec![
        "elasticache",
        "price",
        "--region",
        "us-east-1",
        "--pricing-json",
        bulk.as_str(),
    ];
    args.extend_from_slice(extra);
    exec(&args)
}

#[test]
fn price_command_serverless_json() {
    let dir = TempDir::new("price-serverless");
    let (status, stdout, stderr) = price(
        &dir,
        &[
            "--engine",
            "valkey",
            "--mode",
            "serverless",
            "--data-gb",
            "5",
            "--ecpu-millions",
            "200",
        ],
    );
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "{\n  \"model\": \"serverless\",\n  \"engine\": \"valkey\",\n  \"region\": \"us-east-1\",\n  \"data_gb\": 5.0,\n  \"ecpu_millions\": 200.0,\n  \"storage_monthly\": 306.6,\n  \"compute_monthly\": 0.68,\n  \"total_monthly\": 307.28,\n  \"disclaimer\": \"Estimates only. Verify at https://aws.amazon.com/elasticache/pricing/\"\n}\n"
    );
}

#[test]
fn price_command_defaults_and_ops_per_second() {
    let dir = TempDir::new("price-defaults");
    let (status, stdout, _) = price(&dir, &[]);
    assert_eq!(status, 0);
    let value = json(&stdout);
    // 1 GB * 0.084 * 730 = 61.32; 100M ECPUs * 3.4e-9 = 0.34
    assert_eq!(member(&value, "data_gb"), &Json::Float(1.0));
    assert_eq!(member(&value, "ecpu_millions"), &Json::Float(100.0));
    assert_eq!(member(&value, "storage_monthly"), &Json::Float(61.32));
    assert_eq!(member(&value, "compute_monthly"), &Json::Float(0.34));

    let (_, stdout, _) = price(&dir, &["--ops-per-sec", "100"]);
    assert_eq!(member(&json(&stdout), "ecpu_millions"), &Json::Float(262.8));
    // An explicit ECPU count wins over ops/sec; zero ops/sec is "not given".
    let (_, stdout, _) = price(&dir, &["--ops-per-sec", "100", "--ecpu-millions", "5"]);
    assert_eq!(member(&json(&stdout), "ecpu_millions"), &Json::Float(5.0));
    let (_, stdout, _) = price(&dir, &["--ops-per-sec", "0"]);
    assert_eq!(member(&json(&stdout), "ecpu_millions"), &Json::Float(100.0));
    let (_, stdout, _) = price(&dir, &["--avg-payload-kb", "2"]);
    let value = json(&stdout);
    assert_eq!(member(&value, "avg_payload_kb"), &Json::Float(2.0));
    assert_eq!(member(&value, "ecpu_millions_scaled"), &Json::Float(200.0));
}

#[test]
fn price_command_node_and_reserved_reports() {
    let dir = TempDir::new("price-node");
    let (status, stdout, _) = price(
        &dir,
        &[
            "--mode",
            "node",
            "--nodes",
            "2",
            "--ri-term",
            "1yr_no_upfront",
        ],
    );
    assert_eq!(status, 0);
    let value = json(&stdout);
    assert_eq!(member(&value, "total_monthly"), &Json::Float(146.0));
    assert_eq!(
        member(member(&value, "reserved"), "discount_pct"),
        &Json::Int(30)
    );

    let (status, stdout, _) = price(&dir, &["--mode", "node", "--node-type", "cache.nope"]);
    assert_eq!(status, 0, "a missing price is a result, not a failure");
    let value = json(&stdout);
    assert!(member(&value, "error")
        .as_str()
        .expect("error")
        .starts_with("No node pricing found"));
    assert!(value.get("disclaimer").is_some());

    let (status, stdout, _) = price(&dir, &["--nodes", "2", "--show-ri-options"]);
    assert_eq!(status, 0);
    assert!(stdout.contains("Reserved Pricing Options (live pricing)"));
    assert!(stdout.contains("  On-demand: $146.00/month"));

    let (status, stdout, _) = price(
        &dir,
        &["--engine", "redis", "--nodes", "6", "--extended-support"],
    );
    assert_eq!(status, 0);
    assert!(stdout.contains("Extended Support Cost Estimate (live pricing)"));
    assert!(stdout.contains("  Engine: redis\n"));

    // Extended Support wins over the reserved options, as in the script.
    let (_, stdout, _) = price(&dir, &["--show-ri-options", "--extended-support"]);
    assert!(stdout.contains("Extended Support Cost Estimate"));
}

#[test]
fn price_command_csv_source_notes_missing_reserved_data() {
    let dir = TempDir::new("price-csv");
    let csv = dir.file("prices.csv", PRICING_CSV);
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "price",
        "--region",
        "us-east-1",
        "--pricing-csv",
        &csv,
        "--mode",
        "node",
        "--nodes",
        "3",
    ]);
    assert_eq!(status, 0);
    assert!(stderr.starts_with(
        "Note: CSV pricing does not include Reserved Instance or Extended Support data."
    ));
    assert_eq!(member(&json(&stdout), "total_monthly"), &Json::Float(219.0));
    assert_eq!(
        member(&json(&stdout), "memory_per_node_gb"),
        &Json::string("unknown")
    );
}

#[test]
fn price_command_failures() {
    let dir = TempDir::new("price-fail");
    let (status, stdout, stderr) = exec(&["elasticache", "price"]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(stderr.starts_with("error: a pricing source is required: pass --pricing-csv FILE"));

    let bulk = dir.file("bulk.json", BULK_JSON);
    let csv = dir.file("prices.csv", PRICING_CSV);
    let (status, _, stderr) = exec(&[
        "elasticache",
        "price",
        "--pricing-csv",
        &csv,
        "--pricing-json",
        &bulk,
    ]);
    assert_eq!(status, 2);
    assert!(stderr.contains("argument --pricing-json: not allowed with argument --pricing-csv"));

    let (status, _, stderr) = price(&dir, &["--interactive"]);
    assert_eq!(status, 2);
    assert!(stderr.contains("--interactive is not available in this build"));

    for (args, message) in [
        (vec!["--engine", "bogus"], "argument --engine: invalid choice: 'bogus' (choose from 'valkey', 'redis', 'memcached')"),
        (vec!["--mode", "cluster"], "argument --mode: invalid choice: 'cluster' (choose from 'serverless', 'node')"),
        (vec!["--ri-term", "5yr"], "argument --ri-term: invalid choice: '5yr' (choose from '1yr_no_upfront'"),
        (vec!["--data-gb", "much"], "argument --data-gb: invalid float value: 'much'"),
        (vec!["--nodes", "2.5"], "argument --nodes: invalid int value: '2.5'"),
    ] {
        let (status, stdout, stderr) = price(&dir, &args);
        assert_eq!((status, stdout.as_str()), (2, ""), "{args:?}");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }
    // Hostile choice values are escaped in the diagnostic.
    let (status, _, stderr) = price(&dir, &["--engine", "x\u{1b}[2J\nFORGED"]);
    assert_eq!(status, 2);
    assert!(!stderr.contains('\u{1b}'));
    assert!(stderr.contains("'x\\x1b[2J\\x0aFORGED'"));

    let (status, _, stderr) = exec(&[
        "elasticache",
        "price",
        "--pricing-csv",
        &dir.at("absent.csv"),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("does not exist"));

    // No serverless price for the engine: a runtime error with the lookup message.
    let (status, stdout, stderr) = price(&dir, &["--engine", "memcached"]);
    assert_eq!((status, stdout.as_str()), (1, ""));
    assert!(stderr.starts_with(
        "error: No serverless storage pricing found for region=us-east-1, engine=memcached"
    ));
    // An unpriced region with control characters is escaped in the message.
    let (status, _, stderr) = exec(&[
        "elasticache",
        "price",
        "--pricing-json",
        &bulk,
        "--region",
        "mars\u{1b}[2J",
    ]);
    assert_eq!(status, 1);
    assert!(!stderr.contains('\u{1b}'));
    assert!(stderr.contains("mars\\x1b[2J"));
    // Zero nodes: the script's ZeroDivisionError.
    let (status, _, stderr) = price(
        &dir,
        &[
            "--mode",
            "node",
            "--nodes",
            "0",
            "--ri-term",
            "1yr_no_upfront",
        ],
    );
    assert_eq!(status, 1);
    assert!(stderr.starts_with("error: float division by zero"));
}

// ---------------------------------------------------------------------------
// estimate-serverless command
// ---------------------------------------------------------------------------

const CLUSTERS_CSV: &str = "cluster_name,instance_type,region,engine,node_count,primary_nodes,avg_memory_gb,daily_commands,current_monthly_cost\n\
orders,cache.r7g.large,us-east-1,valkey,2,1,5,1000000,\n\
tiny,cache.r7g.large,us-east-1,valkey,2,0,0.01,1000,500\n";

#[test]
fn estimate_command_writes_csv_json_and_summary() {
    let dir = TempDir::new("estimate");
    let input = dir.file("clusters.csv", CLUSTERS_CSV);
    let pricing = dir.file("prices.csv", PRICING_CSV);
    let output = dir.at("out/estimate.csv");
    fs::create_dir_all(dir.path().join("out")).expect("out dir");
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--input",
        &input,
        "--pricing",
        &pricing,
        "--output",
        &output,
        "--json",
    ]);
    assert_eq!(status, 0, "{stderr}");
    assert!(stderr.starts_with("Note: CSV pricing does not include Reserved Instance"));
    assert!(stdout.starts_with(&format!(
        "Loading clusters from: {input}\n  2 clusters loaded\nOutput written to: {output}\n\n{}\n  ElastiCache Serverless Cost Estimate\n",
        "=".repeat(65)
    )));
    assert!(stdout.contains("  Clusters analyzed:        2\n"));
    assert!(stdout.ends_with(&format!(
        "  See https://aws.amazon.com/elasticache/pricing/\n\nJSON written to: {}\n",
        dir.at("out/estimate.json")
    )));

    let csv = fs::read_to_string(&output).expect("csv written");
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines[0], serverless::CSV_FIELDS.join(","));
    assert!(lines[1].starts_with("orders,us-east-1,valkey,cache.r7g.large,2,1,146.0,list_price,5.0,5.0,3650.0,306.6,30000000,0.1,simple,306.7,160.7,110.1,"));
    assert!(lines[2].starts_with("tiny,us-east-1,valkey,cache.r7g.large,2,1,500.0,provided,0.01,0.1,73.0,6.13,30000,0.0,simple,6.13,-493.87,-98.8,"));

    let document = fs::read_to_string(dir.at("out/estimate.json")).expect("json written");
    assert!(document.starts_with("[\n  {\n    \"cluster_name\": \"orders\","));
    let parsed = json(&document);
    assert_eq!(items(&parsed).len(), 2);
    assert_eq!(member(&items(&parsed)[1], "diff_pct"), &Json::Float(-98.8));
}

#[test]
fn estimate_command_detailed_mode_with_commandstats() {
    let dir = TempDir::new("estimate-detailed");
    let input = dir.file(
        "clusters.csv",
        "cluster_name,instance_type,region,engine,node_count,primary_nodes,avg_memory_gb,daily_commands\ncpu,cache.r7g.large,us-east-1,valkey,2,1,1,100\nmc,,us-east-1,memcached,1,1,2,100\nplain,,us-east-1,valkey,1,1,1,100\n",
    );
    let stats = dir.file(
        "stats.csv",
        "cluster_name,command,calls,usec,uptime_days\ncpu,get,1000,2000,10\ncpu,zadd,500,3000,10\ncpu,info,10,100,10\nmc,get,5,5,\n",
    );
    let pricing = dir.file("prices.csv", PRICING_CSV);
    let output = dir.at("est.csv");
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--input",
        &input,
        "--commandstats",
        &stats,
        "--pricing",
        &pricing,
        "--output",
        &output,
        "--json",
    ]);
    assert_eq!(status, 0, "{stderr}");
    assert!(stdout.contains("Loading commandstats from: "));
    assert!(stdout.contains("  Stats for 2 clusters\n"));
    assert!(stderr.contains("WARNING: commandstats mode uses Redis/Valkey INFO format, which is incompatible with Memcached. ECPU estimates for Memcached cluster 'mc' will be inaccurate."));
    let parsed = json(&fs::read_to_string(dir.at("est.json")).expect("json"));
    let rows = items(&parsed);
    assert_eq!(member(&rows[0], "ecpu_mode"), &Json::string("detailed"));
    assert_eq!(member(&rows[0], "monthly_ecpus"), &Json::Int(6000));
    assert_eq!(member(&rows[0], "fixed_ecpus"), &Json::Int(3000));
    assert_eq!(member(&rows[1], "ecpu_mode"), &Json::string("simple"));
    assert_eq!(member(&rows[2], "ecpu_mode"), &Json::string("simple"));
}

#[test]
fn estimate_command_without_clusters_writes_no_csv() {
    let dir = TempDir::new("estimate-empty");
    let input = dir.file("clusters.csv", "cluster_name,node_count\n");
    let pricing = dir.file("prices.csv", PRICING_CSV);
    let output = dir.at("est.csv");
    let (status, stdout, _) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--input",
        &input,
        "--pricing",
        &pricing,
        "--output",
        &output,
        "--json",
    ]);
    assert_eq!(status, 0);
    assert!(!Path::new(&output).exists(), "nothing to write");
    assert!(!stdout.contains("Output written to"));
    assert!(stdout.contains("  Clusters analyzed:        0\n"));
    assert_eq!(fs::read_to_string(dir.at("est.json")).expect("json"), "[]");
}

#[test]
fn estimate_command_failures() {
    let dir = TempDir::new("estimate-fail");
    let input = dir.file("clusters.csv", CLUSTERS_CSV);
    let pricing = dir.file("prices.csv", PRICING_CSV);
    let (status, stdout, stderr) =
        exec(&["elasticache", "estimate-serverless", "--pricing", &pricing]);
    assert_eq!((status, stdout.as_str()), (2, ""));
    assert!(stderr.starts_with("error: --input (CSV) is required"));
    let (status, _, stderr) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--endpoint",
        "cache.example.com:6379",
        "--pricing",
        &pricing,
    ]);
    assert_eq!(status, 2);
    assert!(stderr.contains("--endpoint is not available in this build"));
    let (status, _, stderr) = exec(&["elasticache", "estimate-serverless", "--input", &input]);
    assert_eq!(status, 2);
    assert!(stderr.contains("a pricing source is required: pass --pricing FILE"));
    let (status, _, stderr) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--input",
        &dir.at("absent.csv"),
        "--pricing",
        &pricing,
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("does not exist"));
    let broken = dir.file("broken.csv", "cluster_name,node_count\nx,two\n");
    let (status, _, stderr) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--input",
        &broken,
        "--pricing",
        &pricing,
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("row 1: invalid literal for int() with base 10: 'two'"));
    // A region without serverless prices fails the whole run.
    let remote = dir.file("remote.csv", "cluster_name,region\nx,ap-south-1\n");
    let (status, _, stderr) = exec(&[
        "elasticache",
        "estimate-serverless",
        "--input",
        &remote,
        "--pricing",
        &pricing,
        "--output",
        &dir.at("e.csv"),
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("No serverless storage pricing found for region=ap-south-1"));
    #[cfg(unix)]
    {
        let target = dir.file("target.csv", "keep");
        let link = dir.at("link.csv");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let (status, _, stderr) = exec(&[
            "elasticache",
            "estimate-serverless",
            "--input",
            &input,
            "--pricing",
            &pricing,
            "--output",
            &link,
        ]);
        assert_eq!(status, 1);
        assert!(stderr.contains("refusing to follow symbolic link"));
        assert_eq!(fs::read_to_string(&target).expect("untouched"), "keep");
    }
}

// ---------------------------------------------------------------------------
// routing
// ---------------------------------------------------------------------------

#[test]
fn elasticache_commands_are_routed_with_the_shared_router() {
    for words in [
        vec!["elasticache"],
        vec!["elasticache", "unknown"],
        vec!["elasticache", "price", "--bogus"],
        vec!["elasticache", "price", "extra"],
        vec!["elasticache", "price", "--engine"],
        vec![
            "elasticache",
            "validate-references",
            "--skill-dir",
            "--quiet",
        ],
        vec!["elasticache", "validate-references", "--skill-dir", ""],
        vec!["elasticache", "preflight", "--password", "S3CR3T-VALUE"],
        vec!["elasticache", "audit", "--profile", "x"],
    ] {
        let (status, stdout, stderr) = exec(&words);
        assert_eq!((status, stdout.as_str()), (2, ""), "{words:?}");
        assert!(stderr.starts_with("error: invalid arguments"), "{words:?}");
        assert!(!stderr.contains("S3CR3T-VALUE"));
    }
    let (status, stdout, _) = exec(&["elasticache", "price", "--help"]);
    assert_eq!(status, 0);
    assert!(stdout.contains("toolkit elasticache price"));
    for command in [
        "price",
        "estimate-serverless",
        "validate-input",
        "validate-references",
        "preflight",
        "audit",
        "dashboard",
    ] {
        let paths: Vec<&[&str]> = crate::COMMANDS.iter().map(|spec| spec.path).collect();
        assert!(
            paths.contains(&["elasticache", command].as_slice()),
            "{command}"
        );
    }
}

// ---------------------------------------------------------------------------
// repair round: #4636 non-finite dashboard numbers, #4637 uninspected groups
// ---------------------------------------------------------------------------

const DASHBOARD_NUMBER_FLAGS: [&str; 10] = [
    "--max-storage-gb",
    "--storage-alarm-pct",
    "--memory-threshold",
    "--engine-cpu-threshold",
    "--replication-lag-threshold",
    "--hit-rate-threshold",
    "--throttle-threshold",
    "--read-latency-threshold",
    "--write-latency-threshold",
    "--ecpu-threshold",
];

#[test]
fn dashboard_rejects_non_finite_numbers_before_any_output() {
    let dir = TempDir::new("dashboard-nonfinite");
    let target = dir.at("stack.json");
    for &flag in DASHBOARD_NUMBER_FLAGS
        .iter()
        .chain(["--evictions-threshold", "--new-connections-threshold"].iter())
    {
        for value in ["NaN", "nan", "inf", "Infinity", "-inf", "1e999", "-1e999"] {
            for mode in [["--serverless", "c"], ["--replication-group", "rg"]] {
                let (status, stdout, stderr) =
                    exec(&["elasticache", "dashboard", mode[0], mode[1], flag, value]);
                assert_eq!(status, 2, "{flag} {value}: {stderr}");
                assert_eq!(stdout, "", "{flag} {value} wrote to stdout");

                let (status, stdout, _) = exec(&[
                    "elasticache",
                    "dashboard",
                    mode[0],
                    mode[1],
                    flag,
                    value,
                    "--output",
                    &target,
                ]);
                assert_eq!(status, 2, "{flag} {value} with --output");
                assert_eq!(stdout, "");
                assert!(
                    !std::path::Path::new(&target).exists(),
                    "{flag} {value} created the output file"
                );
            }
        }
    }
    // The message is actionable and does not echo the rejected text.
    for value in ["NaN", "inf", "1e999"] {
        let (status, _, stderr) = exec(&[
            "elasticache",
            "dashboard",
            "--serverless",
            "c",
            "--hit-rate-threshold",
            value,
        ]);
        assert_eq!(status, 2);
        assert!(
            stderr.contains("argument --hit-rate-threshold: must be a finite number"),
            "{stderr}"
        );
    }
}

#[test]
fn dashboard_finite_numbers_keep_working_and_stay_schema_valid() {
    let dir = TempDir::new("dashboard-finite");
    let target = dir.at("stack.json");
    let (status, _, stderr) = exec(&[
        "elasticache",
        "dashboard",
        "--serverless",
        "c",
        "--hit-rate-threshold",
        "1e300",
        "--throttle-threshold",
        "-1",
        "--max-storage-gb",
        "2.5",
        "--output",
        &target,
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    let written = fs::read_to_string(&target).expect("file written");
    assert!(!written.contains("NaN") && !written.contains("Infinity"));
    assert_schema_valid(&json(&written));
}

/// `base` with its `"security_groups"` member replaced by `fragment` (which
/// must be empty or end with a comma).
fn with_security_groups(base: &str, fragment: &str) -> Json {
    let start = base.find("\"security_groups\":").expect("security_groups");
    let end = base.find("\"tags\":").expect("tags");
    json(&format!("{}{}{}", &base[..start], fragment, &base[end..]))
}

fn assert_not_inspected(result: &Audit, what: &str) {
    let findings = audit_findings(result);
    assert!(
        !findings
            .iter()
            .any(|(check, status, _)| check == "Security groups" && *status == "PASS"),
        "{what}: reported as passing: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|(check, status, _)| check.starts_with("Security group") && *status == "ERROR"),
        "{what}: no not-inspected finding: {findings:?}"
    );
}

#[test]
fn uninspected_security_groups_are_never_passing_in_both_modes() {
    let serverless = |fragment: &str| {
        audit::audit_serverless(
            &with_security_groups(SERVERLESS_DESCRIBE, fragment),
            "cache1",
        )
    };
    let replication = |fragment: &str| {
        audit::audit_replication_group(&with_security_groups(REPLICATION_DESCRIBE, fragment), "rg1")
    };
    let cases: [(&str, &str); 10] = [
        ("missing list", ""),
        ("null list", r#""security_groups": null,"#),
        ("string list", r#""security_groups": "none","#),
        ("object list", r#""security_groups": {},"#),
        ("empty list", r#""security_groups": [],"#),
        (
            "unrelated groups",
            r#""security_groups": [{"GroupId": "sg-other", "IpPermissions": []}],"#,
        ),
        (
            "missing IpPermissions",
            r#""security_groups": [{"GroupId": "sg-1"}, {"GroupId": "sg-a"}, {"GroupId": "sg-b"}],"#,
        ),
        (
            "wrong-type IpPermissions",
            r#""security_groups": [{"GroupId": "sg-1", "IpPermissions": "x"}, {"GroupId": "sg-a", "IpPermissions": {}}, {"GroupId": "sg-b", "IpPermissions": null}],"#,
        ),
        (
            "malformed rule",
            r#""security_groups": [{"GroupId": "sg-1", "IpPermissions": [7]}, {"GroupId": "sg-a", "IpPermissions": [7]}, {"GroupId": "sg-b", "IpPermissions": [7]}],"#,
        ),
        (
            "wrong-type IpRanges",
            r#""security_groups": [{"GroupId": "sg-1", "IpPermissions": [{"IpRanges": "x"}]}, {"GroupId": "sg-a", "IpPermissions": [{"Ipv6Ranges": 1}]}, {"GroupId": "sg-b", "IpPermissions": []}],"#,
        ),
    ];
    for (what, fragment) in cases {
        assert_not_inspected(&serverless(fragment), &format!("serverless, {what}"));
        assert_not_inspected(&replication(fragment), &format!("replication, {what}"));
    }

    // A partial list: sg-b was requested but only sg-a was described.
    let partial = replication(r#""security_groups": [{"GroupId": "sg-a", "IpPermissions": []}],"#);
    assert_not_inspected(&partial, "replication, partial list");
    let findings = audit_findings(&partial);
    let missing = findings
        .iter()
        .find(|(check, _, _)| check == "Security group sg-b")
        .expect("sg-b finding");
    assert_eq!(missing.1, "ERROR");
    assert!(missing.2.starts_with("Not inspected"));
    assert!(!findings.iter().any(|(check, _, _)| check.ends_with("sg-a")));

    // A malformed record for one group does not hide a broad rule in another.
    let mixed = replication(
        r#""security_groups": [{"GroupId": "sg-a", "IpPermissions": 3}, {"GroupId": "sg-b", "IpPermissions": [{"FromPort": 1, "ToPort": 2, "IpRanges": [{"CidrIp": "0.0.0.0/0"}]}]}],"#,
    );
    assert_not_inspected(&mixed, "replication, mixed");
    assert!(audit_findings(&mixed)
        .iter()
        .any(|(check, status, _)| check == "Security group sg-b" && *status == "WARN"));
}

#[test]
fn inspected_security_groups_keep_pass_and_warn() {
    let empty = audit::audit_serverless(
        &with_security_groups(
            SERVERLESS_DESCRIBE,
            r#""security_groups": [{"GroupId": "sg-1", "IpPermissions": []}, {"GroupId": "sg-extra"}],"#,
        ),
        "cache1",
    );
    let findings = audit_findings(&empty);
    let sg = findings
        .iter()
        .find(|(check, _, _)| check == "Security groups")
        .expect("security groups finding");
    assert_eq!(
        (sg.1, sg.2.as_str()),
        ("PASS", "No overly permissive rules in sg-1")
    );
    let broad = audit::audit_serverless(&json(SERVERLESS_DESCRIBE), "cache1");
    assert!(audit_findings(&broad)
        .iter()
        .any(|(check, status, _)| check == "Security group sg-1" && *status == "WARN"));
}

/// The `IpPermissions` JSON of a one-rule group holding `ranges` under `family`.
fn rule_with(family: &str, ranges: &str) -> String {
    format!(r#"[{{"FromPort": 1, "ToPort": 2, "{family}": {ranges}}}]"#)
}

#[test]
fn malformed_cidr_entries_are_never_passing_in_both_modes() {
    let serverless = |fragment: &str| {
        audit::audit_serverless(
            &with_security_groups(SERVERLESS_DESCRIBE, fragment),
            "cache1",
        )
    };
    let replication = |fragment: &str| {
        audit::audit_replication_group(&with_security_groups(REPLICATION_DESCRIBE, fragment), "rg1")
    };
    let v4: [&str; 15] = [
        "[null]",
        "[7]",
        r#"["10.0.0.0/16"]"#,
        "[[]]",
        "[{}]",
        r#"[{"CidrIp": null}]"#,
        r#"[{"CidrIp": 7}]"#,
        r#"[{"CidrIp": ["10.0.0.0/16"]}]"#,
        r#"[{"CidrIpv6": "::/0"}]"#,
        r#"[{"CidrIp": ""}]"#,
        r#"[{"CidrIp": "garbage"}]"#,
        r#"[{"CidrIp": "10.0.0.0"}]"#,
        r#"[{"CidrIp": "10.0.0.0/33"}]"#,
        r#"[{"CidrIp": "10.0.0.0/008"}]"#,
        r#"[{"CidrIp": "::/0"}]"#,
    ];
    let v6: [&str; 12] = [
        "[null]",
        "[7]",
        r#"["::/0"]"#,
        "[{}]",
        r#"[{"CidrIpv6": 7}]"#,
        r#"[{"CidrIpv6": null}]"#,
        r#"[{"CidrIp": "10.0.0.0/16"}]"#,
        r#"[{"CidrIpv6": "garbage"}]"#,
        r#"[{"CidrIpv6": "2001:db8::"}]"#,
        r#"[{"CidrIpv6": "2001:db8::/129"}]"#,
        r#"[{"CidrIpv6": "10.0.0.0/16"}]"#,
        r#"[{"CidrIpv6": " ::/0"}]"#,
    ];
    let families: [(&str, &[&str]); 2] = [("IpRanges", &v4), ("Ipv6Ranges", &v6)];
    for (family, bad_entries) in families {
        for bad in bad_entries {
            // The bad entry may come first, last, or beside valid entries.
            let valid = if family == "IpRanges" {
                r#"{"CidrIp": "10.0.0.0/16"}"#
            } else {
                r#"{"CidrIpv6": "2001:db8::/32"}"#
            };
            let inner = &bad[1..bad.len() - 1];
            for ranges in [
                format!("[{inner}]"),
                format!("[{valid}, {inner}]"),
                format!("[{inner}, {valid}]"),
            ] {
                let rules = rule_with(family, &ranges);
                let what = format!("{family} {ranges}");
                let one = format!(
                    r#""security_groups": [{{"GroupId": "sg-1", "IpPermissions": {rules}}}],"#
                );
                assert_not_inspected(&serverless(&one), &format!("serverless, {what}"));
                let two = format!(
                    r#""security_groups": [{{"GroupId": "sg-a", "IpPermissions": []}}, {{"GroupId": "sg-b", "IpPermissions": {rules}}}],"#
                );
                let result = replication(&two);
                assert_not_inspected(&result, &format!("replication, {what}"));
                let findings = audit_findings(&result);
                assert!(
                    findings
                        .iter()
                        .any(|(check, status, text)| check == "Security group sg-b"
                            && *status == "ERROR"
                            && text.starts_with("Not inspected")),
                    "replication, {what}: {findings:?}"
                );
                assert!(!findings.iter().any(|(check, _, _)| check.ends_with("sg-a")));
            }
        }
    }

    // The review's exact fixture: two malformed families in one rule.
    let review = serverless(
        r#""security_groups": [{"GroupId": "sg-1", "IpPermissions": [{"IpRanges": [null], "Ipv6Ranges": [{"CidrIpv6": 7}]}]}],"#,
    );
    assert_not_inspected(&review, "review fixture");

    // A malformed entry in one group does not hide a broad rule in another.
    let mixed = replication(
        r#""security_groups": [{"GroupId": "sg-a", "IpPermissions": [{"IpRanges": [null]}]}, {"GroupId": "sg-b", "IpPermissions": [{"FromPort": 1, "ToPort": 2, "IpRanges": [{"CidrIp": "0.0.0.0/0"}]}]}],"#,
    );
    assert_not_inspected(&mixed, "replication, mixed");
    let findings = audit_findings(&mixed);
    assert!(findings
        .iter()
        .any(|(check, status, _)| check == "Security group sg-b" && *status == "WARN"));
    assert!(findings
        .iter()
        .any(|(check, status, _)| check == "Security group sg-a" && *status == "ERROR"));
}

#[test]
fn well_formed_cidr_entries_still_pass_and_warn() {
    let serverless = |rules: &str| {
        audit::audit_serverless(
            &with_security_groups(
                SERVERLESS_DESCRIBE,
                &format!(
                    r#""security_groups": [{{"GroupId": "sg-1", "IpPermissions": {rules}}}],"#
                ),
            ),
            "cache1",
        )
    };
    let status_of = |result: &Audit, check: &str| -> Vec<&'static str> {
        audit_findings(result)
            .iter()
            .filter(|(name, _, _)| name == check)
            .map(|(_, status, _)| *status)
            .collect()
    };

    // Empty range lists, missing ranges, no ports, IPv4 and IPv6 narrow CIDRs.
    for rules in [
        "[]",
        "[{}]",
        r#"[{"IpRanges": [], "Ipv6Ranges": []}]"#,
        r#"[{"IpProtocol": "-1", "IpRanges": [{"CidrIp": "10.0.0.0/16", "Description": "x"}]}]"#,
        r#"[{"FromPort": 1, "ToPort": 2, "IpRanges": [{"CidrIp": "10.1.2.3/32"}, {"CidrIp": "192.168.0.0/24"}], "Ipv6Ranges": [{"CidrIpv6": "2001:db8::/32"}, {"CidrIpv6": "fd00::/8"}]}]"#,
    ] {
        let result = serverless(rules);
        assert_eq!(status_of(&result, "Security groups"), ["PASS"], "{rules}");
        assert!(
            status_of(&result, "Security group sg-1").is_empty(),
            "{rules}"
        );
    }

    // Broad rules keep their WARN, including other spellings of ::/0.
    for rules in [
        r#"[{"IpRanges": [{"CidrIp": "0.0.0.0/0"}]}]"#,
        r#"[{"IpRanges": [{"CidrIp": "10.0.0.0/8"}]}]"#,
        r#"[{"Ipv6Ranges": [{"CidrIpv6": "::/0"}]}]"#,
        r#"[{"Ipv6Ranges": [{"CidrIpv6": "0::/0"}]}]"#,
        r#"[{"Ipv6Ranges": [{"CidrIpv6": "2001:db8::/0"}]}]"#,
    ] {
        let result = serverless(rules);
        assert_eq!(
            status_of(&result, "Security group sg-1"),
            ["WARN"],
            "{rules}"
        );
        assert!(status_of(&result, "Security groups").is_empty(), "{rules}");
    }
}

#[test]
fn audit_command_does_not_pass_malformed_cidr_entries() {
    let dir = TempDir::new("audit-malformed-cidr");
    let describe = dir.file(
        "describe.json",
        r#"{"serverless_caches":[{"ServerlessCacheName":"cache","SnapshotRetentionLimit":1,"UserGroupId":"users","SecurityGroupIds":["sg-1"]}],"security_groups":[{"GroupId":"sg-1","IpPermissions":[{"IpRanges":[null],"Ipv6Ranges":[{"CidrIpv6":7}]}]}]}"#,
    );
    for flags in [&["--json"][..], &[][..]] {
        let mut args = vec![
            "elasticache",
            "audit",
            "--serverless",
            "cache",
            "--describe",
            &describe,
        ];
        args.extend_from_slice(flags);
        let (status, stdout, stderr) = exec(&args);
        assert_eq!((status, stderr.as_str()), (0, ""), "{flags:?}");
        assert!(!stdout.contains("No overly permissive rules"), "{stdout}");
        assert!(stdout.contains("Not inspected"), "{stdout}");
    }
}

#[test]
fn audit_command_does_not_pass_uninspected_security_groups() {
    let dir = TempDir::new("audit-uninspected");
    let describe = dir.file(
        "describe.json",
        r#"{"serverless_caches":[{"ServerlessCacheName":"cache","SnapshotRetentionLimit":1,"UserGroupId":"users","SecurityGroupIds":["sg-1"]}]}"#,
    );
    let (status, stdout, stderr) = exec(&[
        "elasticache",
        "audit",
        "--serverless",
        "cache",
        "--describe",
        &describe,
        "--json",
    ]);
    assert_eq!((status, stderr.as_str()), (0, ""));
    let value = json(&stdout);
    let sg: Vec<&Json> = items(member(&value, "findings"))
        .iter()
        .filter(|finding| member(finding, "check") == &Json::string("Security groups"))
        .collect();
    assert_eq!(sg.len(), 1);
    assert_eq!(member(sg[0], "status"), &Json::string("ERROR"));
    assert!(!stdout.contains("No overly permissive rules"));
    let (_, text, _) = exec(&[
        "elasticache",
        "audit",
        "--serverless",
        "cache",
        "--describe",
        &describe,
    ]);
    assert!(text.contains("[ERR]  Security groups"));
    assert!(!text.contains("[OK]   Security groups"));
}
