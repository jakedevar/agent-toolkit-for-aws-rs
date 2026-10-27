//! #4662: every caller-supplied value of every `cloudwatch render` renderer
//! is escaped once, when the arguments are parsed.
//!
//! The leaves under test are not a hand-written list. They are derived from
//! [`RENDERER_ARGUMENTS`], the table `Args::new` itself uses, walked over one
//! or more fixtures per renderer. The test fails when a renderer has no
//! fixture, or when a fixture leaves out an argument the table lists. Reading
//! an argument the table does not list panics in debug builds. A new field
//! therefore cannot skip this test. Every string, number and boolean leaf,
//! and every object key, inside a caller argument is replaced one at a time:
//! first by a benign token, then by [`HOSTILE`] (ESC, CR, LF, U+202E and a
//! fake marker). The hostile report must equal the benign report with the
//! token shown escaped on its line, or the input must be refused with a
//! single-line diagnostic. AWS response arguments are skipped: by the manager
//! ruling on #4380 their free text may keep newlines.

use toolkit_core::{parse_json, Json};

use super::cli::{render_report, ArgSpec, Origin, RENDERERS, RENDERER_ARGUMENTS};
use super::result::OpResult;

const TOKEN: &str = "CALLERTOKEN";
const HOSTILE: &str = "REVIEW_SENTINEL\u{1b}[2J\r\n\u{202e}FORGED_REPORT";
/// [`HOSTILE`] as a text report shows it (`safe_text`).
const SHOWN: &str = r"REVIEW_SENTINEL\x1b[2J\r\n‮FORGED_REPORT";
/// [`HOSTILE`] as a `json.dumps` report shows it (`ensure_ascii`).
const SHOWN_JSON: &str = r"REVIEW_SENTINEL\u001b[2J\r\n‮FORGED_REPORT";
/// [`SHOWN`] inside a Python `repr` (a caller string nested in a list or
/// dict that the legacy text prints with `str()`), which doubles backslashes.
const SHOWN_REPR: &str = r"REVIEW_SENTINEL\\x1b[2J\\r\\n\\u202eFORGED_REPORT";

/// Fixtures per renderer. Together they set every argument of
/// [`RENDERER_ARGUMENTS`], every member key of a nested spec, and all eight
/// capture limits of #4662.
const FIXTURES: &[(&str, &str)] = &[
    (
        "location",
        r#"{"location":{"CodeLocation":{"Language":"python","FilePath":"/app/a.py","ClassName":"Svc","MethodName":"run","LineNumber":12,"Extra":["x"]}},"location_hash":"0123456789abcdef"}"#,
    ),
    (
        "create-success",
        r#"{"response":{"LocationHash":"0123456789abcdef","ARN":"arn:aws:x","ExpiresAt":"2025-02-03T12:00:00Z"},"normalized_type":"BREAKPOINT","service":"svc","environment":"prod","location":{"CodeLocation":{"Language":"python","FilePath":"/app/a.py","MethodName":"run"}},"ttl_hours":2,"capture_arguments":["a","b"],"capture_locals":["c"],"is_line_level":false,"capture_return":true,"capture_stack_trace":false,"limits":{"MaxHits":1,"MaxStringLength":2,"MaxCollectionWidth":3,"MaxCollectionDepth":4,"MaxStackFrames":5,"MaxStackTraceSize":6,"MaxObjectDepth":7,"MaxFieldsPerObject":8},"attribute_filters":[{"Key":"k","Values":["v"]}],"now":"2025-02-03T10:00:00Z"}"#,
    ),
    (
        "list",
        r#"{"data":{},"normalized_type":"BREAKPOINT","service":"svc","environment":"prod"}"#,
    ),
    (
        "list",
        r#"{"data":{"LatestConfigurations":[{"LocationHash":"0123456789abcdef"}]},"normalized_type":"BREAKPOINT","service":"svc","environment":"prod"}"#,
    ),
    (
        "get",
        r#"{"config":{},"service":"svc","environment":"prod"}"#,
    ),
    (
        "batch-delete",
        r#"{"mode":"service","data":{"Errors":[{"ResourceArn":"arn:aws:x","Code":"C","Message":"m"}]},"instrumentation_type":"BREAKPOINT","service":"svc","environment":"prod"}"#,
    ),
    (
        "status-history",
        r#"{"data":{"Events":[{"Time":"2025-02-03T10:05:00Z","Status":"ACTIVE"}]},"normalized_type":"BREAKPOINT","service":"svc","environment":"prod","requested_status":"ACTIVE"}"#,
    ),
    (
        "status-assessment",
        r#"{"created_at":"2025-02-03T10:00:00Z","requested_start":"2025-02-03T09:00:00Z","query_end":"2025-02-03T11:00:00Z","location_hash":"0123456789abcdef","service":"svc","environment":"prod","normalized_type":"BREAKPOINT","checks":{"ACTIVE":{"has_events":true,"events":[{"Time":"2025-02-03T10:30:00Z"}]}}}"#,
    ),
    (
        "status-assessment",
        r#"{"created_at":"2025-02-03T10:00:00Z","requested_start":"2025-02-03T09:00:00Z","query_end":"2025-02-03T11:00:00Z","location_hash":"0123456789abcdef","service":"svc","environment":"prod","normalized_type":"BREAKPOINT","checks":{"ACTIVE":{"error":"active failed"},"READY":{"error":"ready failed"},"ERROR":{"events":[{"Time":"2025-02-03T09:45:00Z","ErrorCause":"boom"}]}}}"#,
    ),
    (
        "snapshot-search",
        r#"{"query_result":{"status":"Error","error":"e"},"service_name":"svc","environment":"prod","location_hash":"0123456789abcdef","custom_filters":["f"],"start_time_utc":"2025-02-03T09:00:00Z","end_time_utc":"2025-02-03T10:00:00Z","start_epoch":1,"end_epoch":2,"query_string":"fields @timestamp\n| limit 1"}"#,
    ),
    (
        "snapshot-sample",
        r#"{"query_result":{"status":"Error","error":"e"},"service_name":"svc","environment":"prod","location_hash":"0123456789abcdef","start_time_utc":"2025-02-03T09:00:00Z","end_time_utc":"2025-02-03T10:00:00Z","max_timeout":30,"query_string":"fields @timestamp\n| limit 1","include_raw":false}"#,
    ),
    ("snapshot-fields", r#"{"result":{}}"#),
    (
        "failure",
        r#"{"failure":{"kind":"client_error","code":"AccessDenied","message":"denied","display":"d"},"action":"read","attempted":{"service":"svc","names":["a"],"n":3},"attempted_label":"TRIED:","possible_causes":["c"],"troubleshooting":["t"],"trailer":"bye"}"#,
    ),
    (
        "failure",
        r#"{"failure":{"kind":"client_error","code":"AccessDenied","message":"denied"},"action":"read","attempted":{"service":"svc"}}"#,
    ),
    (
        "failure",
        r#"{"failure":{"kind":"credentials","type_name":"NoCredentialsError","display":"no creds"},"action":"read"}"#,
    ),
];

/// The escaping a caller leaf is owed, from its argument's [`Origin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Json,
}

#[derive(Debug, Clone)]
enum Step {
    Member(String),
    Index(usize),
    /// Rename this key of the object.
    Key(String),
}

fn spec_of(op: &str) -> ArgSpec {
    RENDERER_ARGUMENTS
        .iter()
        .find(|(name, _)| *name == op)
        .map(|(_, spec)| *spec)
        .unwrap_or_else(|| panic!("renderer {op} has no ArgSpec"))
}

fn origin_of(spec: ArgSpec, key: &str) -> Origin {
    spec.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, origin)| *origin)
        .unwrap_or_else(|| panic!("fixture key {key} is not in its ArgSpec"))
}

fn with_step(path: &[Step], step: Step) -> Vec<Step> {
    let mut next = path.to_vec();
    next.push(step);
    next
}

fn collect_leaves(value: &Json, kind: Kind, path: &[Step], out: &mut Vec<(Vec<Step>, Kind)>) {
    match value {
        Json::Null => {}
        Json::String(_) | Json::Int(_) | Json::Float(_) | Json::Bool(_) => {
            out.push((path.to_vec(), kind));
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_leaves(item, kind, &with_step(path, Step::Index(index)), out);
            }
        }
        Json::Object(pairs) => {
            for (key, item) in pairs {
                out.push((with_step(path, Step::Key(key.clone())), kind));
                collect_leaves(item, kind, &with_step(path, Step::Member(key.clone())), out);
            }
        }
    }
}

/// Every caller leaf of an arguments object, by the spec's origins.
fn collect_arguments(value: &Json, spec: ArgSpec, path: &[Step], out: &mut Vec<(Vec<Step>, Kind)>) {
    let Json::Object(pairs) = value else {
        panic!("arguments must be an object");
    };
    for (key, item) in pairs {
        let here = with_step(path, Step::Member(key.clone()));
        match origin_of(spec, key) {
            Origin::Caller => collect_leaves(item, Kind::Text, &here, out),
            Origin::JsonEncoded => collect_leaves(item, Kind::Json, &here, out),
            Origin::AwsResponse => {}
            Origin::EachMember(inner) => {
                let Json::Object(members) = item else {
                    panic!("{key} must be an object of argument objects");
                };
                for (member, arguments) in members {
                    collect_arguments(
                        arguments,
                        inner,
                        &with_step(&here, Step::Member(member.clone())),
                        out,
                    );
                }
            }
        }
    }
}

fn replace_at(value: &mut Json, path: &[Step], text: &str) {
    let Some((step, rest)) = path.split_first() else {
        *value = Json::String(text.to_string());
        return;
    };
    match (step, value) {
        (Step::Member(key), Json::Object(pairs)) => {
            let (_, item) = pairs
                .iter_mut()
                .find(|(name, _)| name == key)
                .expect("path member exists");
            replace_at(item, rest, text);
        }
        (Step::Index(index), Json::Array(items)) => replace_at(&mut items[*index], rest, text),
        (Step::Key(key), Json::Object(pairs)) => {
            assert!(rest.is_empty());
            let pair = pairs
                .iter_mut()
                .find(|(name, _)| name == key)
                .expect("path key exists");
            pair.0 = text.to_string();
        }
        (step, other) => panic!("path step {step:?} does not fit {other:?}"),
    }
}

fn render(op: &str, fixture: &Json, path: &[Step], text: &str) -> Result<OpResult, String> {
    let mut arguments = fixture.clone();
    replace_at(&mut arguments, path, text);
    render_report(op, &arguments).map_err(|error| error.message().to_string())
}

/// No raw control except the newline, no raw invisible format character, and
/// no line that starts with the forged marker.
fn assert_contained(text: &str, context: &str) {
    for c in text.chars() {
        assert!(
            c == '\n'
                || !(c.is_control()
                    || matches!(c, '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{061c}' | '\u{feff}')),
            "{context}: raw {c:?} in {text:?}"
        );
    }
    for line in text.lines() {
        assert!(
            !line.trim_start().starts_with("FORGED_REPORT"),
            "{context}: forged line in {text:?}"
        );
    }
}

fn describe(path: &[Step]) -> String {
    path.iter()
        .map(|step| match step {
            Step::Member(key) => key.clone(),
            Step::Index(index) => format!("[{index}]"),
            Step::Key(key) => format!("<key {key}>"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Every (renderer, fixture) pair with its caller leaves, after checking that
/// the fixtures cover every renderer and every listed argument.
fn fixture_leaves() -> Vec<(&'static str, Json, Vec<(Vec<Step>, Kind)>)> {
    for renderer in RENDERERS {
        assert!(
            FIXTURES.iter().any(|(op, _)| op == renderer),
            "renderer {renderer} has no caller-text fixture"
        );
    }
    let mut cases = Vec::new();
    for (op, source) in FIXTURES {
        let fixture = parse_json(source).expect("fixture is JSON");
        let mut leaves = Vec::new();
        collect_arguments(&fixture, spec_of(op), &[], &mut leaves);
        cases.push((*op, fixture, leaves));
    }
    for (op, spec) in RENDERER_ARGUMENTS {
        for (key, origin) in *spec {
            let fixtures: Vec<&Json> = cases
                .iter()
                .filter(|(name, _, _)| name == op)
                .map(|(_, fixture, _)| fixture)
                .collect();
            assert!(
                fixtures
                    .iter()
                    .any(|fixture| !matches!(fixture.get(key), None | Some(Json::Null))),
                "no {op} fixture sets argument {key}"
            );
            if let Origin::EachMember(inner) = origin {
                for (member_key, _) in *inner {
                    assert!(
                        fixtures.iter().any(|fixture| match fixture.get(key) {
                            Some(Json::Object(members)) => members
                                .iter()
                                .any(|(_, member)| member.get(member_key).is_some()),
                            _ => false,
                        }),
                        "no {op} fixture sets {key}.*.{member_key}"
                    );
                }
            }
        }
    }
    cases
}

#[test]
fn every_caller_leaf_of_every_renderer_is_escaped_at_parse_time_4662() {
    let mut shown_paths: Vec<String> = Vec::new();
    let mut leaf_count = 0;
    for (op, fixture, leaves) in fixture_leaves() {
        let mut shown_here = 0;
        for (path, kind) in &leaves {
            leaf_count += 1;
            let context = format!("{op} {}", describe(path));
            let benign = render(op, &fixture, path, TOKEN);
            let hostile = render(op, &fixture, path, HOSTILE);
            match (benign, hostile) {
                (_, Err(message)) => {
                    assert!(!message.contains('\n'), "{context}: {message:?}");
                    assert_contained(&message, &context);
                }
                (Err(message), Ok(_)) => {
                    panic!("{context}: refused the token ({message}) but took the hostile value");
                }
                (Ok(benign), Ok(hostile)) => {
                    assert_eq!(benign.ok, hostile.ok, "{context}");
                    assert_contained(&hostile.text, &context);
                    if benign.text.contains(TOKEN) {
                        shown_here += 1;
                        shown_paths.push(format!("{op} {}", describe(path)));
                        let forms: &[&str] = match kind {
                            Kind::Text => &[SHOWN, SHOWN_REPR],
                            Kind::Json => &[SHOWN_JSON],
                        };
                        assert!(
                            forms
                                .iter()
                                .any(|shown| hostile.text == benign.text.replace(TOKEN, shown)),
                            "{context}: hostile report is not the benign report with the \
                             value escaped\nbenign:\n{}\nhostile:\n{}",
                            benign.text,
                            hostile.text
                        );
                    } else {
                        assert_eq!(hostile.text, benign.text, "{context}");
                    }
                }
            }
        }
        if !leaves.is_empty() {
            assert!(
                shown_here > 0,
                "{op}: no caller leaf of this fixture is shown"
            );
        }
    }
    assert!(
        leaf_count >= 100,
        "only {leaf_count} caller leaves exercised"
    );
    // The eight caller capture limits of #4662 each reach the report.
    for limit in [
        "MaxHits",
        "MaxStringLength",
        "MaxCollectionWidth",
        "MaxCollectionDepth",
        "MaxStackFrames",
        "MaxStackTraceSize",
        "MaxObjectDepth",
        "MaxFieldsPerObject",
    ] {
        let path = format!("create-success limits.{limit}");
        assert!(
            shown_paths.contains(&path),
            "{path} is not shown: {shown_paths:?}"
        );
    }
}

#[test]
fn the_limits_reproduction_from_4662_stays_on_one_line() {
    let arguments = parse_json(
        r#"{"normalized_type":"BREAKPOINT","service":"svc","environment":"prod","now":"2025-02-03T10:00:00Z","limits":{"MaxHits":"REVIEW_SENTINEL\u001b[2J\nFORGED_REPORT"}}"#,
    )
    .expect("JSON");
    let report = render_report("create-success", &arguments).expect("renders");
    assert!(report.ok);
    assert!(
        report.text.contains(
            "\nCAPTURE LIMITS:\n- Max Hits: REVIEW_SENTINEL\\x1b[2J\\nFORGED_REPORT\n\nNOTE:"
        ),
        "{}",
        report.text
    );
    assert_contained(&report.text, "4662 reproduction");
}
