//! `test_oracle_connection.py`: the four checks, connection failures and hints.

use super::super::oracle::{
    run_checks, ConnectParams, NoLiveDriver, OracleDriver, Probe, RecordedDriver, Report,
};
use super::{assert_single_line_safe, json};

fn params() -> ConnectParams {
    ConnectParams {
        endpoint: "mydb.abc.us-east-1.rds.amazonaws.com".to_string(),
        port: 1521,
        service_name: "ORCL".to_string(),
        username: "admin".to_string(),
    }
}

fn driver(text: &str) -> RecordedDriver {
    RecordedDriver::from_document(&json(text)).expect("valid recording")
}

fn report(text: &str, verbose: bool) -> Report {
    run_checks(&mut driver(text), &params(), verbose)
}

const HEALTHY: &str = r#"{
  "driver": {"name": "oracledb", "version": "2.5.1", "thin": false},
  "connect": {"version": "19.0.0.0.0", "elapsed_seconds": 0.2149},
  "probes": {
    "basic": {"rows": [["2026-10-27 09:00:00", "ORCL", "ADMIN"]]},
    "protocol": {"rows": [["tcps"]]},
    "banners": {"rows": [["Encryption service for Linux"], ["Crypto-checksumming service"]]},
    "auth": {"rows": [["PASSWORD", "ADMIN", "ip-10-0-0-5"]]}
  }
}"#;

#[test]
fn a_healthy_session_prints_the_full_report_without_connection_metadata() {
    let result = report(HEALTHY, false);
    assert_eq!(result.status, 0);
    assert_eq!(
        result.lines.join("\n"),
        "\
=== Oracle Connection Test ===
(connection metadata hidden; pass --verbose to show endpoint/DSN/username)

Driver:       oracledb 2.5.1
Mode:         Thick

--- Connection Test ---
PASS: Connected in 0.21s
  Database version: 19.0.0.0.0

--- Query Test ---
PASS: Query succeeded
  Server time:  2026-10-27 09:00:00
  Database:     ORCL
  Session user: ADMIN

--- Encryption Status ---
  Network protocol: tcps
  PASS: Connection is SSL/TLS encrypted (TCPS)
  Banner: Encryption service for Linux
  PASS: Encryption is active (NNE or SSL)
  Banner: Crypto-checksumming service
  PASS: Encryption is active (NNE or SSL)

--- Authentication Info ---
  Auth method:  PASSWORD
  Identity:     ADMIN
  Client host:  ip-10-0-0-5

=== All tests passed ==="
    );
    let shown = result.lines.join("\n");
    for hidden in ["mydb.abc", "1521", "admin", "DSN:", "ORCL:"] {
        assert!(!shown.contains(hidden), "{hidden} leaked: {shown}");
    }
}

#[test]
fn verbose_shows_the_connection_metadata_and_the_dsn() {
    let result = report(HEALTHY, true);
    let shown = result.lines.join("\n");
    assert!(
        shown.contains("Endpoint:     mydb.abc.us-east-1.rds.amazonaws.com"),
        "{shown}"
    );
    assert!(shown.contains("Port:         1521"));
    assert!(shown.contains("Service Name: ORCL"));
    assert!(shown.contains("Username:     admin"));
    assert!(shown.contains("DSN:          mydb.abc.us-east-1.rds.amazonaws.com:1521/ORCL"));
    assert!(!shown.contains("hidden"));
    assert_eq!(
        params().dsn(),
        "mydb.abc.us-east-1.rds.amazonaws.com:1521/ORCL"
    );
}

#[test]
fn connection_failures_exit_one_and_name_the_hint_for_each_known_code() {
    for (code, hint) in [
        (
            12170,
            "TNS connect timeout \u{2014} check security groups, VPC routing, endpoint",
        ),
        (
            12541,
            "No listener \u{2014} check endpoint and port are correct",
        ),
        (1017, "Invalid username/password \u{2014} check credentials"),
        (
            12514,
            "Listener does not know of service \u{2014} check service name",
        ),
        (
            12505,
            "Listener does not know of SID \u{2014} try SERVICE_NAME instead of SID",
        ),
        (
            28000,
            "Account is locked \u{2014} unlock the user in the database",
        ),
    ] {
        let result = report(
            &format!(
                r#"{{"connect": {{"elapsed_seconds": 3, "error": {{"code": {code}, "message": "boom"}}}}}}"#
            ),
            false,
        );
        assert_eq!(result.status, 1, "{code}");
        let shown = result.lines.join("\n");
        assert!(
            shown.contains("FAIL: Connection failed after 3.00s"),
            "{shown}"
        );
        assert!(shown.contains(&format!("  ORA-{code}: boom")), "{shown}");
        assert!(shown.contains(&format!("  Hint: {hint}")), "{shown}");
        assert!(
            !shown.contains("--- Query Test ---"),
            "stops at the failed connection"
        );
    }
    // An unknown code has no hint; a failure without a code prints the message only.
    let unknown = report(
        r#"{"connect": {"error": {"code": 99999, "message": "odd"}}}"#,
        false,
    );
    assert_eq!(unknown.status, 1);
    let shown = unknown.lines.join("\n");
    assert!(
        shown.contains("ORA-99999: odd") && !shown.contains("Hint:"),
        "{shown}"
    );
    let codeless = report(r#"{"connect": {"error": {"message": "refused"}}}"#, false);
    let shown = codeless.lines.join("\n");
    assert!(
        shown.contains("\n  refused") && !shown.contains("ORA-"),
        "{shown}"
    );
}

#[test]
fn a_missing_driver_exits_one_before_any_connection() {
    let missing = report(r#"{"driver": {"installed": false}}"#, false);
    assert_eq!(missing.status, 1);
    let shown = missing.lines.join("\n");
    assert!(
        shown.contains("FAIL: the Oracle driver is not installed\n  Fix:"),
        "{shown}"
    );
    assert!(!shown.contains("--- Connection Test"));
    // The run without a recording behaves the same and sends nothing.
    let none = run_checks(&mut NoLiveDriver, &params(), true);
    assert_eq!(none.status, 1);
    assert!(none
        .lines
        .join("\n")
        .contains("FAIL: this build includes no live Oracle driver"));
    assert!(NoLiveDriver
        .query(Probe::Basic, "SELECT 1 FROM dual")
        .is_err());
    assert!(NoLiveDriver.connect(&params()).is_err());
}

#[test]
fn a_failed_check_after_the_connection_does_not_change_the_script_exit_status() {
    // The script printed "All tests passed" and exited 0 after query failures.
    let result = report(
        r#"{"probes": {
            "basic": {"error": {"code": 942, "message": "ORA-00942: table or view does not exist"}},
            "protocol": {"error": {"code": 1031, "message": "insufficient privileges"}},
            "auth": {"error": {"code": 1031, "message": "insufficient privileges"}}}}"#,
        false,
    );
    assert_eq!(result.status, 0);
    let shown = result.lines.join("\n");
    assert!(
        shown.contains(
            "FAIL: Query failed \u{2014} ORA-942: ORA-00942: table or view does not exist"
        ),
        "{shown}"
    );
    assert!(shown.contains("  WARN: Cannot check encryption \u{2014} ORA-1031: insufficient privileges\n  (This may require additional privileges)"), "{shown}");
    assert!(
        shown.contains("  WARN: Cannot check auth info \u{2014} ORA-1031\n"),
        "{shown}"
    );
    assert!(shown.ends_with("=== All tests passed ==="));
}

#[test]
fn absent_probes_fail_closed_with_a_recording_gap() {
    let result = report("{}", false);
    let shown = result.lines.join("\n");
    assert!(shown.contains("PASS: Connected in 0.00s"), "{shown}");
    assert!(
        shown.contains("FAIL: Query failed \u{2014} no recorded result for basic"),
        "{shown}"
    );
    assert!(
        shown.contains("WARN: Cannot check encryption \u{2014} no recorded result for protocol")
    );
    assert!(shown.contains("WARN: Cannot check auth info\n"));
    // A probe with no rows is a row-less result, not a crash.
    let empty = report(r#"{"probes": {"basic": {}, "auth": {"rows": []}}}"#, false);
    let shown = empty.lines.join("\n");
    assert!(
        shown.contains("FAIL: Query failed \u{2014} the query returned no row"),
        "{shown}"
    );
    assert!(shown.contains("WARN: Cannot check auth info\n"), "{shown}");
}

fn encryption_lines(protocol: &str, banners: &str) -> String {
    let result = report(
        &format!(
            r#"{{"probes": {{"protocol": {{"rows": [{protocol}]}}, "banners": {{"rows": [{banners}]}}}}}}"#
        ),
        false,
    );
    let shown = result.lines.join("\n");
    let start = shown.find("--- Encryption Status ---").expect("section");
    let end = shown.find("--- Authentication Info ---").expect("section");
    shown[start..end].to_string()
}

#[test]
fn the_encryption_section_distinguishes_tcps_tcp_and_other_protocols() {
    let tcp = encryption_lines(r#"["TCP"]"#, "");
    assert!(
        tcp.contains(
            "Network protocol: TCP\n  INFO: Connection is TCP (check if NNE is active below)"
        ),
        "{tcp}"
    );
    assert!(
        tcp.contains("INFO: No encryption banners found \u{2014} connection may be unencrypted")
    );
    let tcps = encryption_lines(r#"["TCPS"]"#, r#"["Plain banner"]"#);
    assert!(tcps.contains("PASS: Connection is SSL/TLS encrypted (TCPS)"));
    assert!(
        tcps.contains("  Banner: Plain banner\n\n"),
        "no PASS for a banner without crypto: {tcps}"
    );
    let other = encryption_lines(r#"["ipc"]"#, "");
    assert!(other.contains("INFO: Protocol is 'ipc'"), "{other}");
    let null = encryption_lines("[null]", "");
    assert!(
        null.contains("Network protocol: None") && null.contains("INFO: Protocol is 'None'"),
        "{null}"
    );
    let norow = encryption_lines("", "");
    assert!(norow.contains("Network protocol: None"), "{norow}");
}

#[test]
fn hostile_database_and_caller_text_cannot_forge_lines_or_leak_keys() {
    let hostile = "line\\u001b[31m\\r\\nPASS: forged\\u202e AKIAIOSFODNN7EXAMPLE";
    let result = report(
        &format!(
            r#"{{"connect": {{"version": "{hostile}"}},
               "probes": {{"basic": {{"rows": [["{hostile}", "{hostile}", null]]}},
                           "protocol": {{"rows": [["{hostile}"]]}},
                           "banners": {{"rows": [["{hostile}"]]}},
                           "auth": {{"rows": [["{hostile}", null, "{hostile}"]]}}}}}}"#
        ),
        true,
    );
    let shown = result.lines.join("\n");
    assert!(!shown.contains("AKIAIOSFODNN7EXAMPLE"), "{shown}");
    for line in &result.lines {
        assert_single_line_safe(line);
        assert!(!line.contains('\n') && !line.contains('\r'));
    }
    assert!(
        !result
            .lines
            .iter()
            .any(|line| line.starts_with("PASS: forged")),
        "{shown}"
    );
    assert!(
        shown.contains("\\r\\nPASS: forged"),
        "escaped, not removed: {shown}"
    );
    assert!(shown.contains("Session user: None"));
    // Hostile connection parameters are escaped the same way.
    let hostile_params = ConnectParams {
        endpoint: "host\u{1b}[31m\nPASS: x".to_string(),
        port: 1,
        service_name: "S\u{202e}".to_string(),
        username: "u\r\n".to_string(),
    };
    let result = run_checks(&mut driver("{}"), &hostile_params, true);
    for line in &result.lines {
        assert_single_line_safe(line);
    }
}

#[test]
fn recorded_sessions_reject_wrong_typed_or_conflicting_members() {
    assert!(RecordedDriver::from_document(&json("{}")).is_ok());
    for (text, reason) in [
        (r#"[]"#, "must be a JSON object"),
        (r#"{"driver": null}"#, "\"driver\" must be an object when present (it is null)"),
        (r#"{"driver": []}"#, "\"driver\" must be an object when present (it is array)"),
        (r#"{"driver": {"installed": "no"}}"#, "driver \"installed\" must be a boolean when present (it is string)"),
        (r#"{"driver": {"thin": null}}"#, "driver \"thin\" must be a boolean when present (it is null)"),
        (r#"{"driver": {"name": 5}}"#, "driver \"name\" must be a string when present (it is integer)"),
        (r#"{"driver": {"version": 2}}"#, "driver \"version\" must be a string when present (it is integer)"),
        (r#"{"connect": 5}"#, "\"connect\" must be an object when present (it is integer)"),
        (r#"{"connect": {"elapsed_seconds": "1"}}"#, "connect \"elapsed_seconds\" must be a finite non-negative number when present (it is string)"),
        (r#"{"connect": {"elapsed_seconds": -1}}"#, "connect \"elapsed_seconds\" must be a finite non-negative number"),
        (r#"{"connect": {"elapsed_seconds": null}}"#, "(it is null)"),
        (r#"{"connect": {"version": null}}"#, "connect \"version\" must be a string when present (it is null)"),
        (r#"{"connect": {"error": []}}"#, "connect \"error\" must be an object when present (it is array)"),
        (r#"{"connect": {"error": {"code": "12170"}}}"#, "connect \"code\" must be an integer when present (it is string)"),
        (r#"{"connect": {"error": {"code": 1.5}}}"#, "\"code\" must be an integer when present (it is number)"),
        (r#"{"connect": {"error": {"message": null}}}"#, "\"message\" must be a string when present (it is null)"),
        (r#"{"probes": []}"#, "\"probes\" must be an object when present (it is array)"),
        (r#"{"probes": {"basic": null}}"#, "probes \"basic\" must be an object when present (it is null)"),
        (r#"{"probes": {"basic": {"rows": null}}}"#, "probe \"basic\": \"rows\" must be an array when present (it is null)"),
        (r#"{"probes": {"basic": {"rows": ["x"]}}}"#, "probe \"basic\": every row must be an array"),
        (r#"{"probes": {"basic": {"rows": [[5]]}}}"#, "probe \"basic\": every cell must be a string or null (it is integer)"),
        (r#"{"probes": {"auth": {"rows": [], "error": {}}}}"#, "probe \"auth\": \"rows\" and \"error\" are exclusive"),
        (r#"{"probes": {"banners": {"error": 1}}}"#, "probe \"banners\": \"error\" must be an object when present (it is integer)"),
    ] {
        let Err(message) = RecordedDriver::from_document(&json(text)) else {
            panic!("{text} must be rejected");
        };
        assert!(message.contains(reason), "{text}: {message}");
    }
}
