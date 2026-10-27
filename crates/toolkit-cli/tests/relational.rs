//! End-to-end tests of the relational helpers (#4394): real process spawns with
//! an empty environment (no credentials, no endpoint, no `ORACLE_PASSWORD`),
//! real exit codes and separated stdout/stderr. Nothing contacts AWS or a
//! database; recordings are temporary files.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

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

fn temp_root(label: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "toolkit-relational-cli-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn toolkit(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("spawn toolkit")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8 output")
}

#[test]
fn rds_offline_accepts_the_scripts_option_order_and_prints_json_on_stdout() {
    // `rds_commitment_pricing_analyzer.py --region R offline --instance-type ...`
    let out = toolkit(&[
        "rds",
        "commitment",
        "--region",
        "us-east-1",
        "offline",
        "--instance-type",
        "db.r7g.2xlarge",
        "--engine",
        "mysql",
        "--num-instances",
        "2",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("\"hourly\": 0.912"), "{stdout}");
    assert!(stdout.contains("\"monthly\": 1331.52"), "{stdout}");
    assert!(
        stdout.contains("\"best_option\": \"on_demand\""),
        "{stdout}"
    );
}

#[test]
fn rds_replays_recorded_rates_and_never_needs_credentials() {
    let dir = temp_root("rds");
    let path = dir.join("aws.json");
    fs::write(
        &path,
        r#"{"calls": [
          {"service": "rds", "region": "us-east-1", "operation": "DescribeReservedDBInstancesOfferings",
           "request": {"DBInstanceClass": "db.r7g.2xlarge", "ProductDescription": "MySQL", "MultiAZ": false},
           "response": {"ReservedDBInstancesOfferings": [
             {"DBInstanceClass": "db.r7g.2xlarge", "Duration": 31536000, "OfferingType": "All Upfront",
              "FixedPrice": 5000.0, "RecurringCharges": []}]}}]}"#,
    )
    .expect("write recording");
    let out = toolkit(&[
        "rds",
        "commitment",
        "offline",
        "--instance-type",
        "db.r7g.2xlarge",
        "--engine",
        "mysql",
        "--recorded",
        path.to_str().expect("utf8"),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("\"best_option\": \"1yr RI\""), "{stdout}");
    assert!(
        stdout.contains("\"payment_option\": \"All Upfront\""),
        "{stdout}"
    );
}

#[test]
fn oracle_connection_failure_exits_one_and_a_driver_gap_never_opens_a_socket() {
    let dir = temp_root("oracle");
    let path = dir.join("session.json");
    fs::write(
        &path,
        r#"{"connect": {"elapsed_seconds": 2, "error": {"code": 12170, "message": "TNS:connect timeout occurred"}}}"#,
    )
    .expect("write recording");
    let out = toolkit(&[
        "rds-oracle",
        "test-connection",
        "db.example.com",
        "1521",
        "ORCL",
        "admin",
        "--recorded",
        path.to_str().expect("utf8"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("FAIL: Connection failed after 2.00s"),
        "{stdout}"
    );
    assert!(stdout.contains("Hint: TNS connect timeout"), "{stdout}");
    assert!(
        !stdout.contains("db.example.com"),
        "metadata hidden: {stdout}"
    );
    // Without a recording nothing is dialed: the run ends at the driver check.
    let out = toolkit(&[
        "rds-oracle",
        "test-connection",
        "db.example.com",
        "1521",
        "ORCL",
        "admin",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).contains("no live Oracle driver"));
}

#[test]
fn dsql_tenant_queries_are_parameter_checked_and_read_only() {
    let dir = temp_root("dsql");
    let path = dir.join("rows.json");
    fs::write(
        &path,
        r#"{"statements": [{"sql": "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'", "rows": [["e1", "acme"]]}]}"#,
    )
    .expect("write recording");
    let ok = toolkit(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "acme",
        "--recorded",
        path.to_str().expect("utf8"),
    ]);
    assert_eq!(ok.status.code(), Some(0), "{}", text(&ok.stderr));
    assert_eq!(
        text(&ok.stdout),
        "[\n  [\n    \"e1\",\n    \"acme\"\n  ]\n]\n"
    );
    for hostile in ["acme' OR '1'='1", "acme;DROP TABLE entities", "ACME"] {
        let out = toolkit(&[
            "dsql",
            "tenant-query",
            "--tenant",
            hostile,
            "--recorded",
            path.to_str().expect("utf8"),
        ]);
        assert_eq!(out.status.code(), Some(1), "{hostile}");
        assert!(out.stdout.is_empty(), "no rows for a rejected tenant");
    }
    assert_eq!(
        text(&toolkit(&["dsql", "selftest"]).stdout),
        "safe_query self-test passed\n"
    );
}

#[test]
fn timestream_validation_status_follows_the_inputs() {
    let valid = toolkit(&[
        "timestream-influxdb",
        "validate-input",
        "storage_gb=20",
        "region=eu-west-1",
    ]);
    assert_eq!(valid.status.code(), Some(0));
    let invalid = toolkit(&["timestream-influxdb", "validate-input", "storage_gb=16385"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert_eq!(
        text(&invalid.stdout),
        "Validation FAILED:\n  storage_gb=16385 \u{2014} Must be 20-16384\n"
    );
}
