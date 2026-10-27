//! The registered commands, end to end through the in-process router.

use super::{assert_single_line_safe, json, run, TempDir};
use toolkit_core::Json;

fn parse(text: &str) -> Json {
    json(text)
}

#[test]
fn every_command_of_the_slice_is_registered_strict_and_not_passthrough() {
    let mut seen = 0;
    for spec in crate::COMMANDS {
        let first = spec.path[0];
        if !["dsql", "rds", "rds-oracle", "timestream-influxdb"].contains(&first) {
            continue;
        }
        seen += 1;
        assert!(spec.strict_values, "{:?}", spec.path);
        assert!(!spec.passthrough, "{:?}", spec.path);
        assert!(spec.repeatable_flags.is_empty(), "{:?}", spec.path);
    }
    assert_eq!(seen, 7);
}

#[test]
fn dsql_selftest_and_tenant_sql() {
    let ran = run(&["dsql", "selftest"]);
    assert_eq!(
        (ran.status, ran.stdout.as_str()),
        (0, "safe_query self-test passed\n")
    );
    let ran = run(&["dsql", "tenant-sql", "--tenant", "acme-1"]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert_eq!(
        ran.stdout,
        "SELECT * FROM \"entities\" WHERE tenant_id = 'acme-1'\n"
    );
    let ran = run(&["dsql", "tenant-sql"]);
    assert_eq!(ran.status, 2);
    assert!(ran.stderr.contains("--tenant"));
}

#[test]
fn a_hostile_tenant_is_refused_with_an_escaped_one_line_error() {
    for hostile in [
        "x' OR '1'='1",
        "A",
        "a\nerror: injected",
        "a\u{1b}[31m\u{202e}",
        "",
    ] {
        let ran = run(&["dsql", "tenant-sql", "--tenant", hostile]);
        assert_eq!(
            ran.status,
            if hostile.is_empty() { 2 } else { 1 },
            "{hostile:?}: {}",
            ran.stderr
        );
        assert!(ran.stdout.is_empty());
        assert_single_line_safe(&ran.stderr);
        assert!(!ran.stderr.contains("\nerror: injected"), "{}", ran.stderr);
    }
}

fn rows_recording(dir: &TempDir) -> String {
    dir.write(
        "rows.json",
        r#"{"statements": [
            {"sql": "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'", "rows": [["e1", "acme", null, 3]]},
            {"sql": "SELECT * FROM \"entities\" WHERE tenant_id = 'down'", "error": {"message": "connection reset"}}
        ]}"#,
    )
}

#[test]
fn dsql_tenant_query_replays_rows_and_fails_closed_without_a_recording() {
    let dir = TempDir::new("dsql");
    let path = rows_recording(&dir);
    let ran = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "acme",
        "--recorded",
        &path,
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert_eq!(parse(&ran.stdout), parse(r#"[["e1", "acme", null, 3]]"#));
    let down = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "down",
        "--recorded",
        &path,
    ]);
    assert_eq!(down.status, 1);
    assert!(down.stderr.contains("connection reset"), "{}", down.stderr);
    let unmatched = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "other",
        "--recorded",
        &path,
    ]);
    assert_eq!(unmatched.status, 1);
    assert!(
        unmatched.stderr.contains("no recorded statement"),
        "{}",
        unmatched.stderr
    );
    let bad = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "ACME",
        "--recorded",
        &path,
    ]);
    assert_eq!(bad.status, 1);
    assert!(bad.stderr.contains("failed pattern"), "{}", bad.stderr);
    // Without a recording this build has no connection: nothing is sent.
    let live = run(&["dsql", "tenant-query", "--tenant", "acme"]);
    assert_eq!(live.status, 1);
    assert!(
        live.stderr.contains("no live Aurora DSQL connection"),
        "{}",
        live.stderr
    );
    assert!(live.stdout.is_empty());
}

#[test]
fn a_malformed_or_missing_recording_is_a_runtime_error_that_names_no_value() {
    let dir = TempDir::new("dsql-bad");
    let wrong = dir.write(
        "wrong.json",
        r#"{"statements": [{"sql": "SELECT 1", "rows": null}]}"#,
    );
    let ran = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "acme",
        "--recorded",
        &wrong,
    ]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stderr
            .contains("\"rows\" must be an array when present (it is null)"),
        "{}",
        ran.stderr
    );
    let invalid = dir.write("invalid.json", "{\"a\": \u{1b}[31m FAKE-SECRET-4639");
    let ran = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "acme",
        "--recorded",
        &invalid,
    ]);
    assert_eq!(ran.status, 1);
    assert!(!ran.stderr.contains("FAKE-SECRET"), "{}", ran.stderr);
    assert_single_line_safe(&ran.stderr);
    let missing = dir.join("absent.json");
    let ran = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "acme",
        "--recorded",
        missing.to_str().expect("utf8"),
    ]);
    assert_eq!(ran.status, 1);
}

#[cfg(unix)]
#[test]
fn a_recording_behind_a_symbolic_link_is_refused() {
    let dir = TempDir::new("dsql-link");
    let real = rows_recording(&dir);
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let ran = run(&[
        "dsql",
        "tenant-query",
        "--tenant",
        "acme",
        "--recorded",
        link.to_str().expect("utf8"),
    ]);
    assert_eq!(ran.status, 1, "{}", ran.stdout);
    assert!(ran.stdout.is_empty());
}

const OFFLINE: &[&str] = &[
    "rds",
    "commitment",
    "offline",
    "--instance-type",
    "db.r7g.2xlarge",
    "--engine",
    "mysql",
];

fn with(extra: &[&'static str]) -> Vec<&'static str> {
    let mut words: Vec<&'static str> = OFFLINE.to_vec();
    words.extend_from_slice(extra);
    words
}

#[test]
fn rds_offline_prints_the_comparison_json() {
    let ran = run(&with(&["--num-instances", "2", "--multi-az"]));
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let doc = parse(&ran.stdout);
    assert_eq!(doc.get("num_instances"), Some(&Json::Int(2)));
    assert_eq!(doc.get("multi_az"), Some(&Json::Bool(true)));
    assert_eq!(doc.get("engine"), Some(&Json::string("mysql")));
    assert!(
        ran.stdout.starts_with("{\n  \"engine\": \"mysql\",\n"),
        "{}",
        ran.stdout
    );
    assert!(ran.stderr.is_empty(), "{}", ran.stderr);
}

#[test]
fn rds_prints_the_region_warning_on_stderr_once() {
    let ran = run(&with(&["--region", "eu-west-1"]));
    assert_eq!(ran.status, 0);
    assert_eq!(
        ran.stderr,
        "warning: On-demand price for db.r7g.2xlarge is based on us-east-1. Actual price in eu-west-1 may differ.\n"
    );
}

#[test]
fn rds_argument_errors_exit_two_and_never_echo_raw_text() {
    for (words, needle) in [
        (
            vec!["rds", "commitment", "online"],
            "argument mode: invalid choice",
        ),
        (
            vec!["rds", "commitment", "offline"],
            "the following arguments are required: --instance-type, --engine",
        ),
        (
            vec![
                "rds",
                "commitment",
                "offline",
                "--instance-type",
                "db.m5.large",
            ],
            "required: --engine",
        ),
        (
            with(&["--region", "mars"]),
            "argument --region: invalid region",
        ),
        (
            with(&["--format", "table"]),
            "argument --format: invalid choice",
        ),
        (
            with(&["--num-instances", "two"]),
            "argument --num-instances: invalid int value",
        ),
        (
            vec![
                "rds",
                "commitment",
                "offline",
                "--instance-type",
                "db.m5.large",
                "--engine",
                "oracle-ee",
            ],
            "argument --engine: invalid choice",
        ),
        (
            vec!["rds", "commitment", "--engine", "mysql", "--instance", "x"],
            "unrecognized arguments: --engine",
        ),
        (
            vec!["rds", "commitment", "--multi-az", "--instance", "x"],
            "unrecognized arguments: --multi-az",
        ),
    ] {
        let ran = run(&words);
        assert_eq!(ran.status, 2, "{words:?}: {}", ran.stderr);
        assert!(ran.stderr.contains(needle), "{words:?}: {}", ran.stderr);
        assert!(ran.stdout.is_empty());
    }
    let hostile = run(&[
        "rds",
        "commitment",
        "offline",
        "--instance-type",
        "x",
        "--engine",
        "mysql\u{1b}[31m\nerror: forged",
    ]);
    assert_eq!(hostile.status, 2);
    assert_single_line_safe(&hostile.stderr);
    assert!(!hostile.stderr.contains("\nerror: forged"));
}

#[test]
fn rds_without_a_mode_prints_usage_and_a_live_lookup_needs_a_recording() {
    let ran = run(&["rds", "commitment"]);
    assert_eq!(ran.status, 0);
    assert!(
        ran.stdout.starts_with("usage: toolkit rds commitment"),
        "{}",
        ran.stdout
    );
    let live = run(&["rds", "commitment", "--instance", "prod-db"]);
    assert_eq!(
        live.status, 0,
        "the script reported lookup failures in the JSON"
    );
    let doc = parse(&live.stdout);
    assert_eq!(doc.get("instance_id"), Some(&Json::string("prod-db")));
    assert!(doc
        .get("error")
        .and_then(Json::as_str)
        .expect("error")
        .contains("no live AWS client"));
}

#[test]
fn rds_replays_a_recording_and_rejects_a_malformed_one() {
    let dir = TempDir::new("rds");
    let path = dir.write(
        "aws.json",
        r#"{"calls": [
          {"service": "rds", "operation": "DescribeDBInstances", "request": {"DBInstanceIdentifier": "prod-db"},
           "response": {"DBInstances": [{"Engine": "postgres", "DBInstanceClass": "db.m5.large", "EngineVersion": "16.2"}]}}]}"#,
    );
    let ran = run(&[
        "rds",
        "commitment",
        "--instance",
        "prod-db",
        "--recorded",
        &path,
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let doc = parse(&ran.stdout);
    assert_eq!(doc.get("engine"), Some(&Json::string("postgres")));
    assert_eq!(doc.get("engine_version"), Some(&Json::string("16.2")));
    let bad = dir.write(
        "bad.json",
        r#"{"calls": [{"service": "rds", "operation": "X", "request": null, "response": {}}]}"#,
    );
    let ran = run(&[
        "rds",
        "commitment",
        "offline",
        "--instance-type",
        "db.m5.large",
        "--engine",
        "mysql",
        "--recorded",
        &bad,
    ]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stderr
            .contains("\"request\" must be an object when present (it is null)"),
        "{}",
        ran.stderr
    );
}

#[test]
fn oracle_prints_usage_on_stdout_with_status_one_when_arguments_are_missing() {
    let ran = run(&["rds-oracle", "test-connection", "host", "1521"]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stdout
            .starts_with("Usage: toolkit rds-oracle test-connection <endpoint>"),
        "{}",
        ran.stdout
    );
    assert!(ran.stderr.is_empty());
}

#[test]
fn oracle_without_a_recording_opens_no_connection_and_exits_one() {
    let ran = run(&[
        "rds-oracle",
        "test-connection",
        "db.example.com",
        "1521",
        "ORCL",
        "admin",
    ]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stdout
            .contains("FAIL: this build includes no live Oracle driver"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains("(connection metadata hidden"),
        "{}",
        ran.stdout
    );
    assert!(!ran.stdout.contains("db.example.com"));
}

#[test]
fn oracle_replays_a_recorded_session() {
    let dir = TempDir::new("oracle");
    let path = dir.write(
        "session.json",
        r#"{"connect": {"version": "19c", "elapsed_seconds": 1.5},
            "probes": {"basic": {"rows": [["now", "ORCL", "ADMIN"]]}, "protocol": {"rows": [["tcps"]]},
                       "banners": {"rows": []}, "auth": {"rows": [["PASSWORD", "ADMIN", "h"]]}}}"#,
    );
    let ran = run(&[
        "rds-oracle",
        "test-connection",
        "db.example.com",
        "1521",
        "ORCL",
        "admin",
        "--recorded",
        &path,
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert!(
        ran.stdout.contains("PASS: Connected in 1.50s"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.ends_with("=== All tests passed ===\n"));
    assert!(!ran.stdout.contains("db.example.com"));
    let verbose = run(&[
        "rds-oracle",
        "test-connection",
        "db.example.com",
        "1521",
        "ORCL",
        "admin",
        "--verbose",
        "--recorded",
        &path,
    ]);
    assert!(
        verbose
            .stdout
            .contains("DSN:          db.example.com:1521/ORCL"),
        "{}",
        verbose.stdout
    );
    let failing = dir.write(
        "fail.json",
        r#"{"connect": {"error": {"code": 12541, "message": "TNS:no listener"}}}"#,
    );
    let ran = run(&[
        "rds-oracle",
        "test-connection",
        "db.example.com",
        "1521",
        "ORCL",
        "admin",
        "--recorded",
        &failing,
    ]);
    assert_eq!(ran.status, 1);
    assert!(ran.stdout.contains("Hint: No listener"), "{}", ran.stdout);
}

#[test]
fn oracle_rejects_connection_parameters_that_could_alter_the_connect_string() {
    for (args, needle) in [
        (["host/x", "1521", "ORCL", "admin"], "<endpoint>"),
        (["host", "0", "ORCL", "admin"], "<port>"),
        (["host", "65536", "ORCL", "admin"], "<port>"),
        (["host", "+1521", "ORCL", "admin"], "<port>"),
        (["host", "15 21", "ORCL", "admin"], "<port>"),
        (["host", "1521", "OR CL", "admin"], "<service_name>"),
        (["host", "1521", "ORCL?x=1", "admin"], "<service_name>"),
        (["host", "1521", "ORCL", "adm\"in"], "<username>"),
        (["host\u{1b}[31m", "1521", "ORCL", "admin"], "<endpoint>"),
    ] {
        let mut words = vec!["rds-oracle", "test-connection"];
        words.extend(args);
        let ran = run(&words);
        assert_eq!(ran.status, 2, "{args:?}");
        assert!(ran.stderr.contains(needle), "{args:?}: {}", ran.stderr);
        assert_single_line_safe(&ran.stderr);
        assert!(ran.stdout.is_empty());
    }
    let ran = run(&[
        "rds-oracle",
        "test-connection",
        "h",
        "1521",
        "ORCL",
        "a",
        "extra",
    ]);
    assert_eq!(ran.status, 2, "a fifth positional is a usage error");
}

#[test]
fn timestream_validate_input_exit_codes() {
    let ran = run(&["timestream-influxdb", "validate-input"]);
    assert_eq!(
        (ran.status, ran.stdout.as_str()),
        (0, "All inputs valid.\n")
    );
    let ran = run(&[
        "timestream-influxdb",
        "validate-input",
        "region=us-east-1",
        "storage_gb=20",
        "extra=anything",
    ]);
    assert_eq!(
        (ran.status, ran.stdout.as_str()),
        (0, "All inputs valid.\n")
    );
    let ran = run(&[
        "timestream-influxdb",
        "validate-input",
        "storage_gb=19",
        "instance_type=db.influx.large",
        "vpc_id=bad",
    ]);
    assert_eq!(ran.status, 1);
    assert_eq!(
        ran.stdout,
        "Validation FAILED:\n  storage_gb=19 \u{2014} Must be 20-16384\n  vpc_id=bad \u{2014} Must be vpc-{hex}\n"
    );
    // A value may hold `=`; only the first splits.
    let ran = run(&["timestream-influxdb", "validate-input", "instance_name=a=b"]);
    assert_eq!(ran.status, 1);
    assert!(ran.stdout.contains("instance_name=a=b"));
    let ran = run(&["timestream-influxdb", "validate-input", "region"]);
    assert_eq!(ran.status, 2);
    assert_eq!(
        ran.stderr.lines().next(),
        Some("error: argument 1: expected KEY=VALUE"),
        "the value is not echoed"
    );
}

#[test]
fn timestream_validate_input_never_prints_raw_control_text() {
    let ran = run(&[
        "timestream-influxdb",
        "validate-input",
        "region=\u{1b}[31m\r\nValidation FAILED:\u{202e}",
    ]);
    assert_eq!(ran.status, 1);
    assert_single_line_safe(&ran.stdout);
    assert_eq!(ran.stdout.lines().count(), 2, "{}", ran.stdout);
}

#[test]
fn timestream_instance_types_lists_and_recommends() {
    let ran = run(&["timestream-influxdb", "instance-types"]);
    assert_eq!(ran.status, 0);
    assert_eq!(ran.stdout.lines().count(), 11);
    assert!(
        ran.stdout.starts_with("Instance Type             "),
        "{}",
        ran.stdout
    );
    let ran = run(&[
        "timestream-influxdb",
        "instance-types",
        "recommend",
        "100000",
        "0",
        "366",
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert!(
        ran.stdout
            .starts_with("Recommended: db.influx.12xlarge (48 vCPU, 384 GiB)\n"),
        "{}",
        ran.stdout
    );
    for words in [
        vec!["timestream-influxdb", "instance-types", "recommend"],
        vec![
            "timestream-influxdb",
            "instance-types",
            "recommend",
            "1",
            "2",
        ],
        vec![
            "timestream-influxdb",
            "instance-types",
            "recommend",
            "1",
            "2",
            "x",
        ],
        vec![
            "timestream-influxdb",
            "instance-types",
            "recommend",
            "1",
            "2",
            "3.5",
        ],
        vec![
            "timestream-influxdb",
            "instance-types",
            "recomend",
            "1",
            "2",
            "3",
        ],
    ] {
        let ran = run(&words);
        assert_eq!(ran.status, 2, "{words:?}");
        assert!(ran.stdout.is_empty());
    }
}
