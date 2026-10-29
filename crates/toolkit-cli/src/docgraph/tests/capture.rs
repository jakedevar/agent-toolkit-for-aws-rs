//! T-DG-08a..e (#5327, S5 of the #4886 item 2 design): the user-run
//! collection recipe and its offline Rust normalizer. Every capture is a
//! literal or a recorded fixture; no shell, database, socket or credential is
//! involved. Secret canaries are fake.

use std::fs;
use std::path::Path;

use toolkit_core::{parse_json, Json};

use super::super::capture::{
    admit_access, normalize, Access, CaptureError, Limits, Normalized, MANIFEST,
};
use super::super::dbchecks::run_db_checks;
use super::super::snapshot::read_analysis;
use super::{json, run, summary, TempDir, NOW};

const DOCDB5: &str = include_str!("capture_fixtures/docdb5_mongosh2.json");
const MONGODB7: &str = include_str!("capture_fixtures/mongodb7_mongosh1.json");
const LEGACY: &str = include_str!("capture_fixtures/legacy_shell_tojson.json");
const EXPECTED: &str = include_str!("capture_fixtures/expected_snapshot.json");
const MANIFEST_JSON: &str = include_str!(
    "../../../../../skills/specialized-skills/database-skills/amazon-documentdb/references/collection-manifest.json"
);
const RECIPE: &str = include_str!(
    "../../../../../skills/specialized-skills/database-skills/amazon-documentdb/references/collection-recipe.md"
);

const HEAD: &str = r#""format":"toolkit-docdb-collection-capture","version":1,"dialect":"mongosh-ejson-canonical""#;

fn long(n: i64) -> String {
    format!(r#"{{"$numberLong":"{n}"}}"#)
}

/// A capture of one database `app` holding the given collection bodies.
fn capture_of(collections: &str) -> String {
    format!(
        r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","collections":[{collections}]}}]}}"#
    )
}

/// One collection `c` (ordinal 1) with the three raw replies.
fn one(coll_stats: &str, index_stats: &str, indexes: &str) -> String {
    capture_of(&format!(
        r#"{{"ordinal":1,"name":"c","collStats":{coll_stats},"indexStats":{index_stats},"indexes":{indexes}}}"#
    ))
}

fn id_index() -> &'static str {
    r#"{"key":{"_id":{"$numberInt":"1"}},"name":"_id_"}"#
}

fn norm(text: &str) -> Result<Normalized, CaptureError> {
    normalize(&json(text), &Limits::default())
}

fn refused(text: &str) -> &'static str {
    match norm(text) {
        Ok(_) => panic!("accepted: {text}"),
        Err(error) => error.class(),
    }
}

fn at<'a>(value: &'a Json, path: &[&str]) -> &'a Json {
    path.iter().fold(value, |node, key| {
        node.get(key).unwrap_or_else(|| panic!("member {key}"))
    })
}

fn snapshot_of(text: &str) -> Json {
    norm(text).expect("normalizes").snapshot
}

// ---------------------------------------------------------------- T-DG-08a

#[test]
fn t08a_documentdb_fixture_normalizes_to_the_golden_snapshot() {
    let normalized = norm(DOCDB5).expect("normalizes");
    assert_eq!(normalized.snapshot, json(EXPECTED));
    assert_eq!(
        (
            normalized.databases,
            normalized.collections,
            normalized.incomplete
        ),
        (2, 4, 1)
    );
}

#[test]
fn t08a_exclusions_drop_admin_local_config_and_system_collections() {
    let snapshot = snapshot_of(DOCDB5);
    let Json::Object(databases) = &snapshot else {
        panic!("object")
    };
    let names: Vec<&str> = databases.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["app", "archive"]);
    let Json::Object(collections) = at(&snapshot, &["app"]) else {
        panic!("object")
    };
    assert!(collections
        .iter()
        .all(|(name, _)| !name.starts_with("system.")));
    // An excluded database is ignored even when it carries collection data.
    let text = format!(
        r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"local","collections":[{{"ordinal":1,"name":"oplog","collStats":{{"count":{{"$numberInt":"1"}}}}}}]}},{{"ordinal":2,"name":"app","collections":[{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[],"indexes":[]}}]}}]}}"#
    );
    assert_eq!(
        norm(&text)
            .expect("normalizes")
            .snapshot
            .to_compact_string(),
        snapshot_of(&capture_of(
            r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[]}"#
        ))
        .to_compact_string()
    );
}

#[test]
fn t08a_golden_database_checks_match_the_legacy_analysis_fixture() {
    let normalized = read_analysis(&snapshot_of(DOCDB5))
        .expect("analysis")
        .expect("data");
    let legacy = read_analysis(&json(EXPECTED))
        .expect("analysis")
        .expect("data");
    assert_eq!(normalized.len(), 3, "the denied collection is skipped");
    assert_eq!(
        summary(&run_db_checks(&normalized)),
        summary(&run_db_checks(&legacy))
    );
    let rendered = |checks: &super::super::checks::Findings| -> Vec<String> {
        checks
            .checks
            .iter()
            .map(|c| format!("{}|{}|{}", c.id, c.status.as_str(), c.detail))
            .collect()
    };
    assert_eq!(
        rendered(&run_db_checks(&normalized)),
        rendered(&run_db_checks(&legacy))
    );
}

#[test]
fn t08a_every_shell_and_engine_variant_normalizes_to_the_same_snapshot() {
    let expected = json(EXPECTED);
    for (shell, engine) in [
        (
            r#""name": "mongosh", "version": "2.3.1""#,
            r#""name": "documentdb", "version": "5.0""#,
        ),
        (
            r#""name": "mongosh", "version": "1.10.6""#,
            r#""name": "documentdb", "version": "8.0""#,
        ),
        (
            r#""name": "mongosh", "version": "2.5.0""#,
            r#""name": "mongodb", "version": "7.0""#,
        ),
    ] {
        let text = DOCDB5
            .replace(r#""name": "mongosh", "version": "2.3.1""#, shell)
            .replace(r#""name": "documentdb", "version": "5.0""#, engine);
        assert_eq!(snapshot_of(&text), expected, "{shell} / {engine}");
    }
    let without_versions = DOCDB5
        .replace(
            r#"  "shell": {"name": "mongosh", "version": "2.3.1"},
"#,
            "",
        )
        .replace(
            r#"  "engine": {"name": "documentdb", "version": "5.0"},
"#,
            "",
        );
    assert_eq!(snapshot_of(&without_versions), expected);
}

#[test]
fn t08a_mongodb_fixture_orders_usage_by_name_and_drops_a_null_ttl() {
    let snapshot = snapshot_of(MONGODB7);
    let orders = at(&snapshot, &["shop", "orders"]);
    let indexes = at(orders, &["indexes"]).to_compact_string();
    assert_eq!(
        indexes,
        r#"[{"name":"_id_","fields":{"_id":1},"ordered_fields":["_id"],"size":4096,"usage":{"ops":0,"potential_unused":false},"cardinality":{"is_low":false}},{"name":"sku_1","fields":{"sku":1},"ordered_fields":["sku"],"size":4096,"usage":{"ops":3,"potential_unused":false},"cardinality":{"is_low":false}}]"#
    );
    assert_eq!(
        at(orders, &["unusedStorageSize"]).to_compact_string(),
        r#"{"unusedBytes":3796,"unusedPercent":92.67578125}"#
    );
    assert_eq!(at(orders, &["compression"]), &json(r#"{"enabled":false}"#));
}

#[test]
fn t08a_compound_order_zero_values_and_missing_usage() {
    let stats = r#"{"count":{"$numberInt":"0"},"size":{"$numberInt":"0"},"storageSize":{"$numberInt":"0"},"indexSizes":{"_id_":{"$numberInt":"0"}}}"#;
    let indexes = format!(
        r#"[{id},{{"key":{{"b":{{"$numberInt":"1"}},"a":{{"$numberInt":"-1"}},"c":"text"}},"name":"b_1_a_-1_c_text"}},{{"key":{{"t":{{"$numberInt":"1"}}}},"name":"t_1","expireAfterSeconds":{{"$numberInt":"0"}}}}]"#,
        id = id_index()
    );
    let snapshot = snapshot_of(&one(stats, "[]", &indexes));
    let coll = at(&snapshot, &["app", "c"]);
    assert_eq!(at(coll, &["count"]), &Json::Int(0));
    assert_eq!(
        at(coll, &["unusedStorageSize", "unusedPercent"]),
        &Json::Int(0)
    );
    let entries = match at(coll, &["indexes"]) {
        Json::Array(items) => items,
        other => panic!("{other:?}"),
    };
    // Missing usage: ops 0, so only the _id_ index is not "potentially unused".
    let flags: Vec<bool> = entries
        .iter()
        .map(|e| matches!(at(e, &["usage", "potential_unused"]), Json::Bool(true)))
        .collect();
    assert_eq!(flags, [false, true, true]);
    assert_eq!(
        at(&entries[1], &["ordered_fields"]).to_compact_string(),
        r#"["b","a","c"]"#
    );
    assert_eq!(
        at(&entries[1], &["fields"]).to_compact_string(),
        r#"{"b":1,"a":-1,"c":"text"}"#
    );
    assert_eq!(at(&entries[2], &["expireAfterSeconds"]), &Json::Int(0));
    assert!(entries[0].get("expireAfterSeconds").is_none());
    // Unlisted index sizes default to 0.
    assert_eq!(at(&entries[1], &["size"]), &Json::Int(0));
}

#[test]
fn t08a_compression_aliases_follow_enabled_or_enable() {
    for (compression, expected) in [
        (r#""compression":{"enabled":true}"#, true),
        (r#""compression":{"enable":true}"#, true),
        (r#""compression":{"enabled":false,"enable":true}"#, true),
        (r#""compression":{"enabled":true,"enable":false}"#, true),
        (r#""compression":{"enabled":false,"enable":false}"#, false),
        (r#""compression":{}"#, false),
        (r#""x":1"#, false),
    ] {
        let stats = if compression.starts_with("\"x\"") {
            "{}".to_string()
        } else {
            format!("{{{compression}}}")
        };
        let snapshot = snapshot_of(&one(&stats, "[]", &format!("[{}]", id_index())));
        assert_eq!(
            at(&snapshot, &["app", "c", "compression", "enabled"]),
            &Json::Bool(expected),
            "{compression}"
        );
    }
}

#[test]
fn t08a_unused_storage_is_exact_and_guards_a_zero_denominator() {
    for (size, storage, bytes, percent) in [
        (100_i64, 100_i64, 0_i64, Some(0.0_f64)),
        (100, 50, 0, Some(0.0)),
        (50, 100, 50, Some(50.0)),
        (0, 200, 200, Some(100.0)),
        (1, 3, 2, Some(2.0 / 3.0 * 100.0)),
        (0, 0, 0, None),
        (
            9_007_199_254_740_993,
            9_007_199_254_740_995,
            2,
            Some(2.0 / 9_007_199_254_740_995_f64 * 100.0),
        ),
    ] {
        let stats = format!(
            r#"{{"size":{},"storageSize":{}}}"#,
            long(size),
            long(storage)
        );
        let snapshot = snapshot_of(&one(&stats, "[]", "[]"));
        let unused = at(&snapshot, &["app", "c", "unusedStorageSize"]);
        assert_eq!(
            at(unused, &["unusedBytes"]),
            &Json::Int(bytes),
            "{size}/{storage}"
        );
        match percent {
            Some(value) => assert_eq!(at(unused, &["unusedPercent"]), &Json::Float(value)),
            None => assert_eq!(at(unused, &["unusedPercent"]), &Json::Int(0)),
        }
    }
}

#[test]
fn t08a_normalized_file_feeds_wa_review_like_the_golden_snapshot() {
    let dir = TempDir::new("capture-e2e");
    let capture = write_private(&dir, "capture.json", DOCDB5);
    let normalized = dir.join("analysis.json").to_string_lossy().into_owned();
    let ran = run(&[
        "documentdb",
        "normalize-capture",
        "--capture",
        &capture,
        "--output",
        &normalized,
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert!(
        ran.stdout.contains("1 of 4 collection(s) incomplete"),
        "{}",
        ran.stdout
    );
    let golden = dir.write("golden.json", EXPECTED);
    let recording = dir.write("rec.json", &super::review::Cluster::default().recording());
    let review = |analysis: &str, out: &str| {
        let output = dir.join(out).to_string_lossy().into_owned();
        let ran = run(&[
            "documentdb",
            "wa-review",
            "--cluster-id",
            "prod-docdb",
            "--region",
            "us-east-1",
            "--recorded",
            &recording,
            "--analysis-data",
            analysis,
            "--output",
            &output,
            "--now",
            NOW,
        ]);
        assert_eq!(ran.status, 0, "{}", ran.stderr);
        fs::read_to_string(dir.join(out).join("wa_review_results.json")).expect("results")
    };
    assert_eq!(review(&normalized, "o1"), review(&golden, "o2"));
}

// ---------------------------------------------------------------- T-DG-08b

fn int_case(wrapper: &str, value: &str) -> Result<Json, &'static str> {
    let stats = format!(r#"{{"count":{{"{wrapper}":"{value}"}}}}"#);
    norm(&one(&stats, "[]", "[]"))
        .map(|n| at(&n.snapshot, &["app", "c", "count"]).clone())
        .map_err(super::super::capture::CaptureError::class)
}

#[test]
fn t08b_extended_json_integer_boundaries_are_exact() {
    assert_eq!(
        int_case("$numberInt", "2147483647"),
        Ok(Json::Int(2_147_483_647))
    );
    assert_eq!(int_case("$numberInt", "0"), Ok(Json::Int(0)));
    assert_eq!(int_case("$numberInt", "2147483648"), Err("invalid_number"));
    assert_eq!(int_case("$numberInt", "-2147483649"), Err("invalid_number"));
    assert_eq!(
        int_case("$numberLong", "9223372036854775807"),
        Ok(Json::Int(i64::MAX))
    );
    assert_eq!(
        int_case("$numberLong", "9007199254740993"),
        Ok(Json::Int(9_007_199_254_740_993)),
        "a 64-bit count is not rounded through a double"
    );
    assert_eq!(
        int_case("$numberLong", "9223372036854775808"),
        Err("invalid_number")
    );
    assert_eq!(
        int_case("$numberLong", "-9223372036854775809"),
        Err("invalid_number")
    );
    assert_eq!(int_case("$numberDouble", "12.5"), Ok(Json::Float(12.5)));
    for bad in ["", "+5", "1.5", "0x10", " 5", "5 ", "١٢"] {
        assert_eq!(
            int_case("$numberLong", bad),
            Err("invalid_number"),
            "{bad:?}"
        );
    }
}

#[test]
fn t08b_nonfinite_out_of_range_and_unsupported_numbers_are_refused() {
    for bad in ["NaN", "Infinity", "-Infinity", "1e999", "-1e999", "abc", ""] {
        assert_eq!(
            int_case("$numberDouble", bad),
            Err("invalid_number"),
            "{bad:?}"
        );
    }
    assert_eq!(int_case("$numberDecimal", "1.5"), Err("invalid_number"));
    // Sizes and counts are never negative.
    assert_eq!(int_case("$numberLong", "-1"), Err("invalid_number"));
    for stats in [
        r#"{"count":5}"#,
        r#"{"count":5.5}"#,
        r#"{"count":"5"}"#,
        r#"{"count":null}"#,
        r#"{"count":{"$numberInt":"1","extra":true}}"#,
        r#"{"count":{"$other":"1"}}"#,
        r#"{"count":{}}"#,
    ] {
        assert!(
            ["unsupported_format", "invalid_number", "malformed"]
                .contains(&refused(&one(stats, "[]", "[]"))),
            "{stats}"
        );
    }
    // A bare JSON number is the relaxed dialect, never silently accepted.
    assert_eq!(
        refused(&one(r#"{"count":5}"#, "[]", "[]")),
        "unsupported_format"
    );
}

#[test]
fn t08b_malformed_captures_are_refused_with_fixed_classes() {
    let good_coll = r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[]}"#;
    let cases: Vec<(String, &str)> = vec![
        ("[]".to_string(), "malformed"),
        ("{}".to_string(), "malformed"),
        (
            (r#"{"format":"other","version":1,"dialect":"mongosh-ejson-canonical","databases":[]}"#).to_string(),
            "unsupported_format",
        ),
        (
            (r#"{"format":"toolkit-docdb-collection-capture","version":2,"dialect":"mongosh-ejson-canonical","databases":[]}"#).to_string(),
            "unsupported_format",
        ),
        (format!(r#"{{{HEAD}}}"#), "malformed"),
        (format!(r#"{{{HEAD},"databases":{{}}}}"#), "malformed"),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":2,"name":"app","collections":[{good_coll}]}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","collections":[{good_coll}]}},{{"ordinal":1,"name":"b"}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","collections":[{good_coll}]}},{{"ordinal":2,"name":"app"}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","collections":[{good_coll},{good_coll}]}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"","collections":[{good_coll}]}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"a\nb","collections":[{good_coll}]}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":7,"collections":[{good_coll}]}}]}}"#
            ),
            "malformed",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":"1","name":"app","collections":[{good_coll}]}}]}}"#
            ),
            "malformed",
        ),
        (
            capture_of(r#"{"ordinal":1,"name":"c","collStats":[],"indexStats":[],"indexes":[]}"#),
            "malformed",
        ),
        (
            capture_of(r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":{},"indexes":[]}"#),
            "malformed",
        ),
        (
            capture_of(
                r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[{"key":{},"name":5}]}"#,
            ),
            "malformed",
        ),
        (
            capture_of(
                r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[{"key":{}}]}"#,
            ),
            "malformed",
        ),
        (
            capture_of(
                r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[{"key":[],"name":"x"}]}"#,
            ),
            "malformed",
        ),
        (
            capture_of(
                r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[{"accesses":{}}],"indexes":[]}"#,
            ),
            "malformed",
        ),
        (
            capture_of(
                r#"{"ordinal":1,"name":"c","collStats":{"compression":{"enabled":"yes"}},"indexStats":[],"indexes":[]}"#,
            ),
            "malformed",
        ),
        (
            capture_of(
                r#"{"ordinal":1,"name":"c","collStats":{"indexSizes":[]},"indexStats":[],"indexes":[]}"#,
            ),
            "malformed",
        ),
        (capture_of(r#"{"ordinal":1,"name":"c"}"#), "malformed"),
        (
            capture_of(r#"{"ordinal":1,"name":"c","collStats":{}}"#),
            "malformed",
        ),
        (
            capture_of(r#"{"ordinal":1,"name":"c","error":"unauthorized","collStats":{}}"#),
            "malformed",
        ),
        (
            format!(r#"{{{HEAD},"shell":{{"name":"mongosh","version":"2.3"}},"databases":[]}}"#),
            "malformed",
        ),
        (
            format!(r#"{{{HEAD},"shell":{{"name":"mongosh","version":"3.0.0"}},"databases":[]}}"#),
            "unsupported_format",
        ),
        (
            format!(r#"{{{HEAD},"shell":{{"name":"bash","version":"2.0.0"}},"databases":[]}}"#),
            "unsupported_format",
        ),
        (
            format!(
                r#"{{{HEAD},"engine":{{"name":"documentdb","version":"{}"}},"databases":[]}}"#,
                "9".repeat(40)
            ),
            "malformed",
        ),
    ];
    for (text, class) in cases {
        assert_eq!(refused(&text), class, "{text}");
    }
    // Duplicate JSON keys never reach the normalizer: the parser refuses them.
    assert!(parse_json(&format!(r#"{{{HEAD},"databases":[],"databases":[]}}"#)).is_err());
}

#[test]
fn t08b_top_level_and_database_failures_publish_nothing() {
    let dir = TempDir::new("capture-fail");
    for (label, text) in [
        (
            "top",
            format!(r#"{{{HEAD},"error":"connection_failed","databases":[]}}"#),
        ),
        (
            "db",
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","error":"unauthorized"}}]}}"#
            ),
        ),
    ] {
        assert_eq!(refused(&text), "incomplete", "{label}");
        let capture = write_private(&dir, &format!("{label}.json"), &text);
        let output = dir
            .join(format!("{label}.out"))
            .to_string_lossy()
            .into_owned();
        let ran = run(&[
            "documentdb",
            "normalize-capture",
            "--capture",
            &capture,
            "--output",
            &output,
        ]);
        assert_eq!(ran.status, 1, "{label}");
        assert!(ran.stderr.contains("(incomplete)"), "{}", ran.stderr);
        assert!(
            !Path::new(&output).exists(),
            "{label}: no snapshot published"
        );
    }
}

#[test]
fn t08b_per_collection_failures_keep_the_skip_marker_and_are_summarized() {
    let text = capture_of(
        r#"{"ordinal":1,"name":"ok","collStats":{},"indexStats":[],"indexes":[]},
           {"ordinal":2,"name":"bad","error":"timeout"}"#,
    );
    let normalized = norm(&text).expect("normalizes");
    assert_eq!(
        at(&normalized.snapshot, &["app", "bad"]).to_compact_string(),
        r#"{"error":"timeout"}"#
    );
    assert_eq!((normalized.collections, normalized.incomplete), (2, 1));
    // The wa-review reader skips the marker, so the summary is the visible record.
    let analysis = read_analysis(&normalized.snapshot)
        .expect("reads")
        .expect("data");
    assert_eq!(analysis.len(), 1);
    for code in [
        "connection_failed",
        "authentication_failed",
        "unauthorized",
        "command_failed",
        "unsupported_command",
        "timeout",
    ] {
        let text = capture_of(&format!(
            r#"{{"ordinal":1,"name":"ok","collStats":{{}},"indexStats":[],"indexes":[]}},{{"ordinal":2,"name":"bad","error":"{code}"}}"#
        ));
        assert!(norm(&text).is_ok(), "{code}");
    }
}

#[test]
fn t08b_a_capture_without_usable_collections_is_not_a_healthy_report() {
    assert_eq!(refused(&format!(r#"{{{HEAD},"databases":[]}}"#)), "empty");
    assert_eq!(
        refused(&format!(
            r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"admin"}},{{"ordinal":2,"name":"app","collections":[{{"ordinal":1,"name":"system.views"}}]}}]}}"#
        )),
        "empty"
    );
    assert_eq!(
        refused(&capture_of(
            r#"{"ordinal":1,"name":"a","error":"command_failed"}"#
        )),
        "empty",
        "every collection failed"
    );
}

#[test]
fn t08b_arbitrary_error_text_is_never_stored_or_echoed() {
    for code in [
        "connect ECONNREFUSED 10.0.0.9:27017",
        "mongodb://admin:hunter2@host/db",
        "Authentication failed.",
        "",
        "UNAUTHORIZED",
    ] {
        let text = capture_of(&format!(
            r#"{{"ordinal":1,"name":"a","collStats":{{}},"indexStats":[],"indexes":[]}},{{"ordinal":2,"name":"b","error":"{code}"}}"#
        ));
        let class = refused(&text);
        assert!(
            ["invalid_error_code", "secret_field"].contains(&class),
            "{code}: {class}"
        );
    }
}

#[test]
fn t08b_errors_are_fixed_text_that_never_carries_capture_values() {
    let dir = TempDir::new("capture-canary");
    let canary = "CANARY-5327-ZZZ";
    for text in [
        capture_of(&format!(
            r#"{{"ordinal":1,"name":"c","collStats":{{"count":{{"$numberInt":"{canary}"}}}},"indexStats":[],"indexes":[]}}"#
        )),
        capture_of(&format!(
            r#"{{"ordinal":1,"name":"c","collStats":{{"{canary}":1}},"indexStats":[],"indexes":[]}}"#
        )),
        capture_of(&format!(r#"{{"ordinal":1,"name":"{canary}\n"}}"#)),
        format!(r#"{{{HEAD},"{canary}":1,"databases":[]}}"#),
        format!(r#"{{"format":"{canary}","version":1,"dialect":"x","databases":[]}}"#),
        format!("{canary} transcript line without JSON"),
        capture_of(&format!(
            r#"{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[],"indexes":[],"x{canary}":1}}"#
        )),
    ] {
        let capture = write_private(&dir, "capture.json", &text);
        let output = dir.join("out.json").to_string_lossy().into_owned();
        let ran = run(&[
            "documentdb",
            "normalize-capture",
            "--capture",
            &capture,
            "--output",
            &output,
        ]);
        assert_eq!(ran.status, 1, "{}", ran.stderr);
        assert!(
            !ran.stderr.contains(canary) && !ran.stdout.contains(canary),
            "{}",
            ran.stderr
        );
        assert!(ran.stderr.contains("capture refused ("), "{}", ran.stderr);
        assert!(!Path::new(&output).exists());
    }
}

#[test]
fn t08b_size_and_count_limits_hold_at_the_boundary() {
    let coll = |n: usize| {
        format!(r#"{{"ordinal":{n},"name":"c{n}","collStats":{{}},"indexStats":[],"indexes":[]}}"#)
    };
    let limits = Limits {
        max_collections: 3,
        max_databases: 2,
        max_indexes: 2,
        ..Limits::default()
    };
    let capture =
        |colls: &[usize]| capture_of(&colls.iter().map(|n| coll(*n)).collect::<Vec<_>>().join(","));
    assert!(normalize(&json(&capture(&[1, 2, 3])), &limits).is_ok());
    assert_eq!(
        normalize(&json(&capture(&[1, 2, 3, 4])), &limits).map(|_| ()),
        Err(CaptureError::TooLarge)
    );
    let databases = |n: usize| {
        let dbs: Vec<String> = (1..=n)
            .map(|i| {
                format!(r#"{{"ordinal":{i},"name":"d{i}","collections":[{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[],"indexes":[]}}]}}"#)
            })
            .collect();
        format!(r#"{{{HEAD},"databases":[{}]}}"#, dbs.join(","))
    };
    assert!(normalize(&json(&databases(2)), &limits).is_ok());
    assert_eq!(
        normalize(&json(&databases(3)), &limits).map(|_| ()),
        Err(CaptureError::TooLarge)
    );
    let with_indexes = |n: usize| {
        let items: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"key":{{"f{i}":{{"$numberInt":"1"}}}},"name":"i{i}"}}"#))
            .collect();
        one("{}", "[]", &format!("[{}]", items.join(",")))
    };
    assert!(normalize(&json(&with_indexes(2)), &limits).is_ok());
    assert_eq!(
        normalize(&json(&with_indexes(3)), &limits).map(|_| ()),
        Err(CaptureError::TooLarge)
    );
}

#[cfg(unix)]
#[test]
fn t08b_an_oversized_capture_file_is_refused_before_it_is_read_whole() {
    let dir = TempDir::new("capture-big");
    let limits = Limits::default();
    let padding = " ".repeat(usize::try_from(limits.max_bytes).expect("fits") + 1);
    let capture = write_private(&dir, "big.json", &format!("{{}}{padding}"));
    let output = dir.join("out.json").to_string_lossy().into_owned();
    let ran = run(&[
        "documentdb",
        "normalize-capture",
        "--capture",
        &capture,
        "--output",
        &output,
    ]);
    assert_eq!(ran.status, 1);
    assert!(ran.stderr.contains("(too_large)"), "{}", ran.stderr);
    let missing = dir.join("absent.json").to_string_lossy().into_owned();
    let ran = run(&[
        "documentdb",
        "normalize-capture",
        "--capture",
        &missing,
        "--output",
        &output,
    ]);
    assert!(ran.stderr.contains("(unreadable)"), "{}", ran.stderr);
}

// ---------------------------------------------------------------- T-DG-08c

#[test]
fn t08c_the_three_refused_flags_fail_safely_on_both_commands() {
    let dir = TempDir::new("capture-flags");
    let out = dir.join("never").to_string_lossy().into_owned();
    let values = [
        "mongodb://admin:hunter2@db.example.invalid:27017/?tls=true",
        "mongodb+srv://u:p%40ss@cluster.example.invalid",
        "pw-line1\nmongodb://x:y@z",
        "",
    ];
    for flag in ["--uri", "--tls-ca-file", "--tls-allow-invalid-certs"] {
        for value in values {
            for base in [
                vec![
                    "documentdb",
                    "wa-review",
                    "--cluster-id",
                    "a",
                    "--region",
                    "us-east-1",
                    "--output",
                    out.as_str(),
                ],
                vec![
                    "documentdb",
                    "normalize-capture",
                    "--capture",
                    "x",
                    "--output",
                    out.as_str(),
                ],
            ] {
                let mut words = base.clone();
                words.push(flag);
                if !value.is_empty() {
                    words.push(value);
                }
                let ran = run(&words);
                assert_eq!(ran.status, 2, "{words:?}: {}", ran.stderr);
                assert!(ran.stdout.is_empty(), "{words:?}");
                for leaked in [
                    "hunter2",
                    "p%40ss",
                    "mongodb",
                    "pw-line1",
                    "example.invalid",
                ] {
                    assert!(
                        !ran.stderr.contains(leaked),
                        "{flag}: {leaked} in {}",
                        ran.stderr
                    );
                }
                assert!(!Path::new(&out).exists());
            }
        }
    }
}

#[test]
fn t08c_no_shell_launcher_database_driver_or_connection_dependency_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("manifest");
    for name in ["mongodb", "bson", "mongo", "pymongo"] {
        assert!(
            !manifest.contains(&format!("{name} =")) && !manifest.contains(&format!("\"{name}\"")),
            "{name}"
        );
    }
    let dir = root.join("src/docgraph");
    let forbidden = [
        "std::process",
        "Command::new",
        "TcpStream",
        "UdpSocket",
        "mongodb::",
        "bson::",
        "env::var",
        "std::env",
    ];
    for entry in fs::read_dir(&dir).expect("docgraph dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") || path.ends_with("tests.rs") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("source");
        for token in forbidden {
            assert!(!text.contains(token), "{} uses {token}", path.display());
        }
    }
}

// ---------------------------------------------------------------- T-DG-08d

fn manifest() -> Json {
    json(MANIFEST_JSON)
}

fn strings(value: &Json) -> Vec<String> {
    match value {
        Json::Array(items) => items
            .iter()
            .map(|item| item.as_str().expect("string").to_string())
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn t08d_manifest_matches_the_code_allowlist_and_envelope_schema() {
    let manifest = manifest();
    assert_eq!(at(&manifest, &["manifest_version"]), &Json::Int(1));
    let envelope = at(&manifest, &["envelope"]);
    assert_eq!(at(envelope, &["format"]).as_str(), Some(MANIFEST.format));
    assert_eq!(at(envelope, &["version"]), &Json::Int(MANIFEST.version));
    assert_eq!(at(envelope, &["dialect"]).as_str(), Some(MANIFEST.dialect));
    for (key, expected) in [
        ("envelope_keys", MANIFEST.envelope_keys),
        ("database_keys", MANIFEST.database_keys),
        ("collection_keys", MANIFEST.collection_keys),
        ("error_codes", MANIFEST.error_codes),
        ("excluded_databases", MANIFEST.excluded_databases),
    ] {
        assert_eq!(strings(at(envelope, &[key])), expected, "{key}");
    }
    assert_eq!(
        at(envelope, &["excluded_collection_prefix"]).as_str(),
        Some(MANIFEST.excluded_collection_prefix)
    );
    let Json::Array(rows) = at(&manifest, &["commands"]) else {
        panic!("commands")
    };
    assert_eq!(rows.len(), MANIFEST.commands.len());
    for (row, code) in rows.iter().zip(MANIFEST.commands) {
        assert_eq!(at(row, &["id"]).as_str(), Some(code.id));
        assert_eq!(at(row, &["slot"]).as_str(), Some(code.slot));
        assert_eq!(at(row, &["command"]).as_str(), Some(code.command));
        assert_eq!(strings(at(row, &["fields"])), code.fields, "{}", code.id);
        assert_eq!(strings(at(row, &["ignored"])), code.ignored, "{}", code.id);
        assert_eq!(at(row, &["read_only"]), &Json::Bool(true));
    }
    let ids: Vec<&str> = MANIFEST.commands.iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        [
            "list_databases",
            "list_collections",
            "coll_stats",
            "index_stats",
            "list_indexes"
        ]
    );
}

#[test]
fn t08d_the_manifest_commands_are_a_read_only_allowlist_of_single_expressions() {
    let allowed_calls = [
        "adminCommand",
        "getSiblingDB",
        "runCommand",
        "getCollection",
        "aggregate",
        "toArray",
        "getIndexes",
        "stringify",
    ];
    let allowed_verbs = [
        "listDatabases",
        "listCollections",
        "collStats",
        "$indexStats",
        "getIndexes",
    ];
    let write_verbs = [
        "insert",
        "update",
        "delete",
        "remove",
        "drop",
        "create",
        "rename",
        "save",
        "eval",
        "$out",
        "$merge",
        "collMod",
        "createIndexes",
        "dropIndexes",
        "reIndex",
        "shutdown",
        "setParameter",
        "killOp",
        "compact",
        "load(",
        "require",
        "process",
        "sleep",
    ];
    for command in MANIFEST.commands {
        let text = command.command;
        assert!(!text.contains(';') && !text.contains('\n'), "{text}");
        for loop_token in [
            "for ", "for(", "while", "forEach", "=>", "function", "map(", "filter(",
        ] {
            assert!(!text.contains(loop_token), "{text}");
        }
        for verb in write_verbs {
            assert!(!text.contains(verb), "{text} has {verb}");
        }
        let calls = identifiers_before_paren(text);
        for call in &calls {
            assert!(allowed_calls.contains(&call.as_str()), "{text}: {call}");
        }
        assert!(
            allowed_verbs.iter().any(|verb| text.contains(verb)),
            "{text} names no allowed server command"
        );
        if command.id.ends_with("_stats") || command.id == "list_indexes" {
            assert!(text.contains("relaxed: false"), "{text}: canonical export");
        }
    }
}

fn identifiers_before_paren(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '(' && index > 0 {
            let mut start = index;
            while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
                start -= 1;
            }
            if start < index {
                found.push(chars[start..index].iter().collect());
            }
        }
    }
    found
}

#[test]
fn t08d_documented_rows_equal_the_manifest_rows() {
    let begin = RECIPE
        .find("<!-- manifest-rows:begin -->")
        .expect("begin marker");
    let end = RECIPE
        .find("<!-- manifest-rows:end -->")
        .expect("end marker");
    let table = &RECIPE[begin..end];
    let documented: Vec<(String, String)> = table
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            (
                cells[1].trim_matches('`').to_string(),
                cells[2].trim_matches('`').to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = MANIFEST
        .commands
        .iter()
        .map(|c| (c.id.to_string(), c.command.to_string()))
        .collect();
    assert_eq!(documented, expected);
    for code in MANIFEST.error_codes {
        assert!(RECIPE.contains(&format!("`{code}`")), "{code} undocumented");
    }
    for key in MANIFEST
        .envelope_keys
        .iter()
        .chain(MANIFEST.database_keys)
        .chain(MANIFEST.collection_keys)
    {
        assert!(RECIPE.contains(&format!("`{key}`")), "{key} undocumented");
    }
}

#[test]
fn t08d_the_recipe_ships_no_loop_helper_secret_form_or_connection_string() {
    for needle in [
        "--password",
        "--uri",
        "mongodb://",
        "mongodb+srv://",
        "password=",
        "forEach",
        "while (",
        "for (",
        "=>",
        "function ",
        "load(",
        "#!/",
    ] {
        assert!(!RECIPE.contains(needle), "recipe contains {needle}");
    }
    assert!(RECIPE.contains("umask 077"));
    assert!(RECIPE.contains("0600"));
    assert!(RECIPE.contains("normalize-capture"));
    assert!(
        RECIPE.contains("prompts"),
        "interactive password prompt documented"
    );
    assert!(RECIPE.contains("mongosh"));
    assert!(
        RECIPE.contains("legacy `mongo` shell") && RECIPE.contains("unsupported"),
        "the unsupported shell is stated"
    );
    assert!(
        RECIPE.contains("one `collStats`"),
        "manual scale cost is stated"
    );
    let manifest = manifest();
    let Json::Array(shells) = at(&manifest, &["shells"]) else {
        panic!("shells")
    };
    let support: Vec<(&str, &str)> = shells
        .iter()
        .map(|s| {
            (
                at(s, &["name"]).as_str().expect("name"),
                at(s, &["support"]).as_str().expect("support"),
            )
        })
        .collect();
    assert_eq!(
        support,
        [("mongosh", "supported"), ("mongo", "unsupported")]
    );
}

#[test]
fn t08d_unsupported_dialects_fail_explicitly() {
    assert_eq!(refused(LEGACY), "unsupported_format");
    let relaxed = DOCDB5.replace(r#"{"$numberInt": "1000"}"#, "1000");
    assert_eq!(refused(&relaxed), "unsupported_format");
    let other_dialect = DOCDB5.replace("mongosh-ejson-canonical", "mongosh-ejson-relaxed");
    assert_eq!(refused(&other_dialect), "unsupported_format");
    assert!(norm(MONGODB7).is_ok());
}

// ---------------------------------------------------------------- T-DG-08e

#[test]
fn t08e_access_policy_admits_owner_only_and_refuses_everything_else() {
    for mode in [0o600, 0o400, 0o700, 0o500, 0o200, 0o100] {
        assert_eq!(admit_access(Access::UnixMode(mode)), Ok(()), "{mode:o}");
    }
    for mode in [
        0o640, 0o604, 0o660, 0o666, 0o644, 0o607, 0o670, 0o601, 0o610, 0o777,
    ] {
        assert_eq!(
            admit_access(Access::UnixMode(mode)),
            Err(CaptureError::Permissions),
            "{mode:o}"
        );
    }
    // File-type bits of a real `st_mode` do not mask the permission bits.
    assert_eq!(admit_access(Access::UnixMode(0o100_600)), Ok(()));
    assert_eq!(
        admit_access(Access::UnixMode(0o100_644)),
        Err(CaptureError::Permissions)
    );
    assert_eq!(admit_access(Access::Acl { owner_only: true }), Ok(()));
    assert_eq!(
        admit_access(Access::Acl { owner_only: false }),
        Err(CaptureError::Permissions)
    );
    assert_eq!(
        admit_access(Access::Unverifiable),
        Err(CaptureError::Permissions)
    );
}

#[cfg(unix)]
#[test]
fn t08e_group_or_world_readable_captures_are_refused_before_parsing() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("capture-perm");
    let output = dir.join("out.json").to_string_lossy().into_owned();
    // The content is not even JSON: a permissions refusal proves the check ran first.
    let capture = dir.write("capture.json", "not json at all");
    for mode in [0o644, 0o640, 0o604, 0o666, 0o770] {
        fs::set_permissions(&capture, fs::Permissions::from_mode(mode)).expect("chmod");
        let ran = run(&[
            "documentdb",
            "normalize-capture",
            "--capture",
            &capture,
            "--output",
            &output,
        ]);
        assert_eq!(ran.status, 1, "{mode:o}");
        assert!(
            ran.stderr.contains("(permissions)"),
            "{mode:o}: {}",
            ran.stderr
        );
        assert!(!ran.stderr.contains("not json"));
        assert!(!Path::new(&output).exists());
    }
    fs::set_permissions(&capture, fs::Permissions::from_mode(0o600)).expect("chmod");
    let ran = run(&[
        "documentdb",
        "normalize-capture",
        "--capture",
        &capture,
        "--output",
        &output,
    ]);
    assert!(ran.stderr.contains("(malformed)"), "{}", ran.stderr);
}

#[cfg(unix)]
#[test]
fn t08e_owner_only_capture_writes_a_private_snapshot_and_never_overwrites() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("capture-private");
    let capture = write_private(&dir, "capture.json", DOCDB5);
    let output = dir.join("analysis.json").to_string_lossy().into_owned();
    let words = [
        "documentdb",
        "normalize-capture",
        "--capture",
        capture.as_str(),
        "--output",
        output.as_str(),
    ];
    let ran = run(&words);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let mode = fs::metadata(&output).expect("output").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        parse_json(&fs::read_to_string(&output).expect("read")).expect("json"),
        json(EXPECTED)
    );
    assert!(!ran.stdout.contains("hunter2"));
    // A second run refuses to replace the private file.
    let again = run(&words);
    assert_eq!(again.status, 1);
    assert!(again.stderr.contains("(output_exists)"), "{}", again.stderr);
    // A symbolic link is not an acceptable output or capture.
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&capture, &link).expect("symlink");
    let ran = run(&[
        "documentdb",
        "normalize-capture",
        "--capture",
        &link.to_string_lossy(),
        "--output",
        &dir.join("o2.json").to_string_lossy(),
    ]);
    assert_eq!(ran.status, 1);
    assert!(ran.stderr.contains("(unreadable)"), "{}", ran.stderr);
}

#[test]
fn t08e_secret_bearing_envelopes_fail_without_echo_or_snapshot() {
    let canary = "hunter2-CANARY";
    let uri = format!("mongodb://admin:{canary}@db.example.invalid:27017/?tls=true");
    let good = r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[]}"#;
    let cases: Vec<(String, &str)> = vec![
        (
            format!(r#"{{{HEAD},"uri":"{uri}","databases":[]}}"#),
            "secret_field",
        ),
        (
            format!(r#"{{{HEAD},"connectionString":"{uri}","databases":[]}}"#),
            "secret_field",
        ),
        (
            format!(r#"{{{HEAD},"password":"{canary}","databases":[]}}"#),
            "secret_field",
        ),
        (
            format!(r#"{{{HEAD},"auth":{{"token":"{canary}"}},"databases":[]}}"#),
            "secret_field",
        ),
        (
            format!(r#"{{{HEAD},"AWS_SECRET_ACCESS_KEY":"{canary}","databases":[]}}"#),
            "secret_field",
        ),
        (
            format!(
                r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","credentials":"{canary}","collections":[{good}]}}]}}"#
            ),
            "secret_field",
        ),
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[],"indexes":[],"secret":"{canary}"}}"#
            )),
            "secret_field",
        ),
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{"password":"{canary}"}},"indexStats":[],"indexes":[]}}"#
            )),
            "secret_field",
        ),
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{"ns":"{uri}"}},"indexStats":[],"indexes":[]}}"#
            )),
            "secret_field",
        ),
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[{{"name":"_id_","host":"{uri}"}}],"indexes":[]}}"#
            )),
            "secret_field",
        ),
        (
            capture_of(&format!(r#"{{"ordinal":1,"name":"{uri}"}}"#)),
            "secret_field",
        ),
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[],"indexes":[{{"key":{{}},"name":"i","apiKey":"{canary}"}}]}}"#
            )),
            "secret_field",
        ),
        // Unknown but not credential-shaped: still refused, as a different class.
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{}},"indexStats":[],"indexes":[],"note":"{canary}"}}"#
            )),
            "unknown_field",
        ),
        (
            capture_of(&format!(
                r#"{{"ordinal":1,"name":"c","collStats":{{"wiredTiger":{{"x":"{canary}"}}}},"indexStats":[],"indexes":[]}}"#
            )),
            "unknown_field",
        ),
        (
            format!(
                r#"{{{HEAD},"transcript":"mongosh> db.runCommand(...) {canary}","databases":[]}}"#
            ),
            "unknown_field",
        ),
    ];
    let dir = TempDir::new("capture-secret");
    for (text, class) in cases {
        assert_eq!(refused(&text), class, "{text}");
        #[cfg(unix)]
        {
            let capture = write_private(&dir, "capture.json", &text);
            let output = dir.join("out.json").to_string_lossy().into_owned();
            let ran = run(&[
                "documentdb",
                "normalize-capture",
                "--capture",
                &capture,
                "--output",
                &output,
            ]);
            assert_eq!(ran.status, 1);
            for leaked in [canary, "example.invalid", "mongodb://", "admin:"] {
                assert!(
                    !ran.stderr.contains(leaked) && !ran.stdout.contains(leaked),
                    "{leaked} leaked: {}",
                    ran.stderr
                );
            }
            assert!(!Path::new(&output).exists());
        }
    }
}

/// #5348: a secret-shaped key is refused as a class, never echoed. Every
/// location below holds a key that is secret-shaped (it carries a credential
/// word) and also carries a unique canary, so any message that quotes the
/// offending key is visible. The complete stderr is pinned.
#[test]
fn t08e_secret_shaped_keys_are_refused_without_echoing_the_key() {
    type Build<'a> = Box<dyn Fn(&str) -> String + 'a>;
    const KEY_CANARY: &str = "KEYCANARY5348";
    const FIXED_STDERR: &str = "error: capture refused (secret_field): the capture contains a connection, credential or secret-shaped field or value\n";
    let good = r#"{"ordinal":1,"name":"c","collStats":{},"indexStats":[],"indexes":[]}"#;
    let coll = |body: &str| capture_of(&format!(r#"{{"ordinal":1,"name":"c",{body}}}"#));
    // (location, builder from the offending key to the whole capture text)
    let cases: Vec<(&str, Build)> = vec![
        (
            "envelope",
            Box::new(|k| format!(r#"{{{HEAD},"{k}":"x","databases":[]}}"#)),
        ),
        (
            "envelope shell",
            Box::new(|k| {
                format!(r#"{{{HEAD},"shell":{{"name":"mongosh","{k}":"x"}},"databases":[]}}"#)
            }),
        ),
        (
            "envelope engine",
            Box::new(|k| {
                format!(r#"{{{HEAD},"engine":{{"name":"mongodb","{k}":"x"}},"databases":[]}}"#)
            }),
        ),
        (
            "database",
            Box::new(move |k| {
                format!(
                    r#"{{{HEAD},"databases":[{{"ordinal":1,"name":"app","{k}":"x","collections":[{good}]}}]}}"#
                )
            }),
        ),
        (
            "collection",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{}},"indexStats":[],"indexes":[],"{k}":"x""#
                ))
            }),
        ),
        (
            "collStats",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{"{k}":1}},"indexStats":[],"indexes":[]"#
                ))
            }),
        ),
        (
            "collStats compression",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{"compression":{{"{k}":true}}}},"indexStats":[],"indexes":[]"#
                ))
            }),
        ),
        (
            "collStats ignored member, nested",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{"ns":{{"a":[{{"{k}":"x"}}]}}}},"indexStats":[],"indexes":[]"#
                ))
            }),
        ),
        (
            "indexStats item",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{}},"indexStats":[{{"name":"_id_","{k}":"x"}}],"indexes":[]"#
                ))
            }),
        ),
        (
            "indexStats accesses",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{}},"indexStats":[{{"name":"_id_","accesses":{{"ops":{{"$numberInt":"1"}},"{k}":"x"}}}}],"indexes":[]"#
                ))
            }),
        ),
        (
            "indexStats ignored member, nested",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{}},"indexStats":[{{"name":"_id_","spec":{{"deep":{{"{k}":"x"}}}}}}],"indexes":[]"#
                ))
            }),
        ),
        (
            "index item",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{}},"indexStats":[],"indexes":[{{"key":{{}},"name":"i","{k}":"x"}}]"#
                ))
            }),
        ),
        (
            "index ignored member, nested",
            Box::new(|k| {
                coll(&format!(
                    r#""collStats":{{}},"indexStats":[],"indexes":[{{"key":{{}},"name":"i","v":{{"x":{{"{k}":"x"}}}}}}]"#
                ))
            }),
        ),
    ];
    // Secret-shaped spellings: the credential word is the whole key, a
    // suffix, a prefix, or split by case and punctuation.
    let spellings = [
        format!("password{KEY_CANARY}"),
        format!("{KEY_CANARY}_token"),
        format!("{KEY_CANARY}.apiKey"),
        format!("{KEY_CANARY}-Secret-{KEY_CANARY}"),
        format!("authorization{KEY_CANARY}"),
    ];
    let dir = TempDir::new("capture-key-echo");
    for (location, build) in &cases {
        for key in &spellings {
            let text = build(key);
            assert_eq!(refused(&text), "secret_field", "{location}: {key}");
            let capture = write_private(&dir, "capture.json", &text);
            let output = dir.join("out.json").to_string_lossy().into_owned();
            let ran = run(&[
                "documentdb",
                "normalize-capture",
                "--capture",
                &capture,
                "--output",
                &output,
            ]);
            assert_eq!(ran.status, 1, "{location}: {key}: {}", ran.stderr);
            assert_eq!(ran.stdout, "", "{location}: {key}");
            assert_eq!(ran.stderr, FIXED_STDERR, "{location}: {key}");
            assert!(!ran.stderr.contains(KEY_CANARY), "{location}");
            assert!(!Path::new(&output).exists(), "{location}: no snapshot");
        }
    }
}

#[test]
fn t08e_a_shell_transcript_or_script_is_not_an_envelope() {
    for text in [
        "mongosh> db.adminCommand({listDatabases:1})\n{ databases: [ { name: 'app' } ] }\n",
        "#!/bin/sh\nmongosh --host x --password hunter2\n",
        "",
        "{\"format\":",
    ] {
        let parsed = parse_json(text);
        if let Ok(value) = parsed {
            assert!(normalize(&value, &Limits::default()).is_err());
        }
    }
}

/// Writes a capture with owner-only permissions, as the recipe's `umask 077`
/// would.
fn write_private(dir: &TempDir, name: &str, text: &str) -> String {
    let path = dir.write(name, text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    path
}
