//! Unit tests of the plan generators' internals; the command-level behavior is
//! covered end to end by `tests/dynamodb_deploy.rs`.

use toolkit_aws::AwsError;
use toolkit_core::{parse_json, Json};

use super::design::{build_create_table, collect_attr_types, provisioned, validate_design};
use super::identity::{refuse_production, resolve_identity, Identity, IdentityClient};
use super::model::{list, py_int, text};
use super::names::{check_prefix, check_region};
use super::recorded::RecordedClient;
use super::teardown::{fill, sh_quote};

fn json(text: &str) -> Json {
    parse_json(text).expect("fixture json")
}

#[test]
fn shell_quoting_closes_and_reopens_around_a_quote() {
    assert_eq!(sh_quote("plain"), "'plain'");
    assert_eq!(sh_quote(""), "''");
    assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    assert_eq!(sh_quote("$(id) `id` \"x\" \\"), "'$(id) `id` \"x\" \\'");
}

#[test]
fn markers_are_filled_in_one_pass_and_unknown_markers_stay() {
    let out = fill(
        "a=%%one%% b=%%two%% c=%%none%% d=100%%",
        &[("one", "%%two%%".to_string()), ("two", "2".to_string())],
    );
    // A value that looks like a marker is never expanded.
    assert_eq!(out, "a=%%two%% b=2 c=%%none%% d=100%%");
}

#[test]
fn python_int_conversions() {
    assert_eq!(py_int(&Json::Int(7)), Some(7));
    assert_eq!(py_int(&Json::Float(7.9)), Some(7));
    assert_eq!(py_int(&Json::Float(-7.9)), Some(-7));
    assert_eq!(py_int(&Json::Bool(true)), Some(1));
    assert_eq!(py_int(&json("\" 12 \"")), Some(12));
    assert_eq!(py_int(&json("\"1e3\"")), None);
    assert_eq!(py_int(&Json::Null), None);
    assert_eq!(py_int(&json("[]")), None);
}

#[test]
fn design_members_follow_python_or_semantics() {
    let model = json(r#"{"a": null, "b": [], "c": 0, "d": [1], "e": "x", "f": {}}"#);
    for key in ["a", "b", "c", "missing"] {
        assert!(list(&model, key).expect("not set").is_empty(), "{key}");
    }
    assert_eq!(list(&model, "d").expect("list").len(), 1);
    assert!(list(&model, "e").is_err(), "a truthy non-list is rejected");
    assert_eq!(text(&model, "a").expect("null is unset"), None);
    assert_eq!(text(&model, "e").expect("text"), Some("x"));
    assert!(text(&model, "d").is_err());
}

#[test]
fn names_that_reach_arns_and_scripts_are_checked() {
    assert!(check_prefix("ddb-skill-bench-a_b-").is_ok());
    for bad in [
        "ddb-skill-bench-a*",
        "ddb-skill-bench-a/b",
        "ddb-skill-bench-a b",
        "ddb-skill-bench-a'b",
        "other-",
        "DDB-skill-bench-",
    ] {
        assert!(check_prefix(bad).is_err(), "{bad}");
    }
    assert!(check_region("us-east-1").is_ok() && check_region("eu-central-2").is_ok());
    for bad in [
        "",
        "us-east",
        "US-EAST-1",
        "us east 1",
        "us-east-1\n",
        "-east-1",
    ] {
        assert!(check_region(bad).is_err(), "{bad:?}");
    }
}

fn identity(arn: &str, alias: &str) -> Identity {
    Identity {
        account: "111122223333".into(),
        alias: alias.into(),
        arn: arn.into(),
    }
}

#[test]
fn production_markers_match_case_insensitively_across_arn_and_alias() {
    assert!(refuse_production(&identity("arn:aws:iam::1:user/dev", "sandbox")).is_ok());
    assert!(refuse_production(&identity("arn:aws:iam::1:user/PRODuctOwner", "")).is_err());
    assert!(refuse_production(&identity("arn:aws:iam::1:user/dev", "Live-Account")).is_err());
    assert!(refuse_production(&identity("arn:aws:iam::1:user/dev", "")).is_ok());
}

struct Scripted {
    caller: Option<Result<Json, AwsError>>,
    aliases: Option<Result<Json, AwsError>>,
    unused: usize,
}

impl IdentityClient for Scripted {
    fn get_caller_identity(&mut self) -> Result<Json, AwsError> {
        self.caller.take().expect("one STS call")
    }

    fn list_account_aliases(&mut self) -> Result<Json, AwsError> {
        self.aliases.take().expect("one IAM call")
    }

    fn unconsumed(&self) -> usize {
        self.unused
    }
}

#[test]
fn the_identity_preflight_reads_account_alias_and_arn() {
    let mut client = Scripted {
        caller: Some(Ok(json(
            r#"{"Account": "111122223333", "Arn": "arn:x", "UserId": "u"}"#,
        ))),
        aliases: Some(Ok(json(r#"{"AccountAliases": ["one", "two"]}"#))),
        unused: 0,
    };
    let found = resolve_identity(&mut client, "p").expect("identity");
    assert_eq!(found, identity("arn:x", "one"));
    // An empty list and an absent member are both "no alias".
    for body in ["{}", r#"{"AccountAliases": []}"#] {
        let mut client = Scripted {
            caller: Some(Ok(json(r#"{"Account": "111122223333"}"#))),
            aliases: Some(Ok(json(body))),
            unused: 0,
        };
        assert_eq!(
            resolve_identity(&mut client, "p").expect("identity").alias,
            ""
        );
    }
    // A failed IAM call is tolerated; a failed STS call is not.
    let mut client = Scripted {
        caller: Some(Ok(json(r#"{"Account": "111122223333"}"#))),
        aliases: Some(Err(AwsError::NoCredentials)),
        unused: 0,
    };
    assert!(resolve_identity(&mut client, "p").is_ok());
    let mut client = Scripted {
        caller: Some(Err(AwsError::NoCredentials)),
        aliases: None,
        unused: 0,
    };
    assert!(resolve_identity(&mut client, "p").is_err());
    let mut client = Scripted {
        caller: Some(Ok(json(r#"{"Account": "111122223333"}"#))),
        aliases: Some(Ok(json("{}"))),
        unused: 2,
    };
    let error = resolve_identity(&mut client, "p").expect_err("unused steps");
    assert!(error.message().contains("2 step(s) this run never used"));
}

#[test]
fn a_recording_replays_each_step_once_in_order() {
    let document = json(
        r#"{"calls": [
          {"service": "sts", "operation": "GetCallerIdentity", "response": {"Account": "111122223333", "Arn": "first"}},
          {"service": "sts", "operation": "GetCallerIdentity", "response": {"Account": "111122223333", "Arn": "second"}}]}"#,
    );
    let mut client = RecordedClient::from_document(&document).expect("recording");
    assert_eq!(client.unconsumed(), 2);
    let first = client.get_caller_identity().expect("first");
    assert_eq!(first.get("Arn").and_then(Json::as_str), Some("first"));
    let second = client.get_caller_identity().expect("second");
    assert_eq!(second.get("Arn").and_then(Json::as_str), Some("second"));
    assert!(client.get_caller_identity().is_err(), "exhausted");
    assert_eq!(client.unconsumed(), 0);
}

fn table(body: &str) -> Json {
    json(&format!(
        r#"{{"table_name": "T", "key_schema": {{"partition_key": "p"}}{body}}}"#
    ))
}

#[test]
fn attribute_types_prefer_the_table_level_definition() {
    let model = table(
        r#", "entities": [{"attributes": [{"name": "a", "type": "n"}, {"name": "b"}, {"name": "c", "type": "x"}]},
                          {"attributes": [{"name": "a", "type": "B"}]}],
            "attribute_definitions": [{"name": "b", "type": "N"}, {"attribute_name": "d", "attribute_type": "b"}]"#,
    );
    let types = collect_attr_types(&model).expect("types");
    let pairs: Vec<(&str, &str)> = types
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // Later entity wins for `a`; unknown types fall back to S; definitions win.
    assert_eq!(pairs, [("a", "B"), ("b", "N"), ("c", "S"), ("d", "B")]);
}

#[test]
fn on_demand_is_the_default_and_ttl_pitr_and_sse_are_never_requested() {
    let request = build_create_table(&table(""), "ddb-skill-bench-x-", &[], None).expect("request");
    let rendered = request.to_compact_string();
    assert!(rendered.contains("\"BillingMode\":\"PAY_PER_REQUEST\""));
    assert!(rendered.contains("\"DeletionProtectionEnabled\":false"));
    for absent in [
        "SSESpecification",
        "TimeToLive",
        "PointInTime",
        "ProvisionedThroughput",
        "StreamSpecification",
    ] {
        assert!(!rendered.contains(absent), "{absent} in {rendered}");
    }
}

#[test]
fn provisioned_capacity_defaults_to_five_and_rejects_non_positive_units() {
    assert_eq!(provisioned(&json(r#"{"read": 3}"#)).expect("units"), (3, 5));
    assert_eq!(
        provisioned(&json(r#"{"write": "9"}"#)).expect("units"),
        (5, 9)
    );
    for bad in [
        r#"{"read": 0}"#,
        r#"{"read": -1}"#,
        r#"{"write": null}"#,
        r#"{"read": "x"}"#,
        "[]",
        "5",
    ] {
        assert!(provisioned(&json(bad)).is_err(), "{bad}");
    }
    // The table's own capacity wins over the config-level one.
    let own = table(r#", "provisioned_capacity": {"read": 2, "write": 2}"#);
    let request =
        build_create_table(&own, "ddb-skill-bench-x-", &[], Some((9, 9))).expect("request");
    assert!(request
        .to_compact_string()
        .contains("\"ReadCapacityUnits\":2"));
}

#[test]
fn vector_index_limits_and_stream_views_are_enforced() {
    let many: Vec<String> = (0..6)
        .map(|i| {
            format!(
                r#"{{"index_name": "V{i}", "vector_attribute": "v", "dimensions": 8, "distance_function": "COSINE"}}"#
            )
        })
        .collect();
    let model = table(&format!(r#", "vector_indexes": [{}]"#, many.join(",")));
    let error = build_create_table(&model, "ddb-skill-bench-x-", &[], None).expect_err("limit");
    assert!(error
        .message()
        .contains("declares 6 vector indexes; the per-table limit is 5"));
    let bad_view = table(r#", "streams": {"enabled": true, "view_type": "EVERYTHING"}"#);
    assert!(build_create_table(&bad_view, "ddb-skill-bench-x-", &[], None).is_err());
    let off = table(r#", "streams": {"enabled": false, "view_type": "EVERYTHING"}"#);
    assert!(build_create_table(&off, "ddb-skill-bench-x-", &[], None).is_ok());
}

#[test]
fn wrong_typed_design_members_are_diagnosed_not_crashed_on() {
    for (label, model) in [
        ("model array", "[]".to_string()),
        ("tables object", r#"{"tables": {"a": 1}, "access_patterns": [{"peak_rps": 1}]}"#.to_string()),
        ("pattern string", r#"{"tables": [{"table_name": "T", "key_schema": {"partition_key": "p"}}], "access_patterns": ["x"]}"#.to_string()),
        ("partition key number", r#"{"tables": [{"table_name": "T", "key_schema": {"partition_key": 5}}], "access_patterns": [{"peak_rps": 1}]}"#.to_string()),
        ("duplicate tables", r#"{"tables": [{"table_name": "T", "key_schema": {"partition_key": "p"}}, {"table_name": "T", "key_schema": {"partition_key": "p"}}], "access_patterns": [{"table": "T", "peak_rps": 1}]}"#.to_string()),
        ("table name missing", r#"{"tables": [{"key_schema": {"partition_key": "p"}}], "access_patterns": [{"peak_rps": 1}]}"#.to_string()),
        ("table name wildcard", r#"{"tables": [{"table_name": "T*", "key_schema": {"partition_key": "p"}}], "access_patterns": [{"peak_rps": 1}]}"#.to_string()),
    ] {
        let error = validate_design(&json(&model)).expect_err(label);
        assert_eq!(error.exit_code(), 2, "{label}");
    }
}
