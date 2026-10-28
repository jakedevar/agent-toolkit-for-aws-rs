//! `query_metering.py`: record extraction, filters, aggregations and the
//! completeness signals.

use super::*;
use crate::marketplace::query::{aggregate, extract_records};

const PINNED: &str = "2026-01-31T12:30:00Z";

fn ct_event(records: &str, results: &str, extra: &str) -> String {
    format!(
        r#"{{"requestParameters": {{"usageRecords": [{records}]}}, "responseElements": {{"results": [{results}]}} {extra}}}"#
    )
}

fn usage(account: &str, dimension: &str, day: &str, quantity: i64) -> String {
    format!(
        r#"{{"customerAWSAccountId": "{account}", "dimension": "{dimension}", "timestamp": "{day}T10:00:00Z", "licenseArn": "{LICENSE}", "quantity": {quantity}}}"#
    )
}

fn result(account: &str, dimension: &str, day: &str, status: &str, id: &str) -> String {
    format!(
        r#"{{"usageRecord": {{"customerAWSAccountId": "{account}", "dimension": "{dimension}", "timestamp": "{day}T10:00:00Z", "licenseArn": "{LICENSE}"}}, "status": "{status}", "meteringRecordId": "{id}"}}"#
    )
}

/// A `LookupEvents` page holding the given CloudTrail event bodies.
fn page(bodies: &[String], token: Option<&str>) -> String {
    let events: Vec<String> = bodies
        .iter()
        .map(|body| {
            format!(
                r#"{{"EventTime": 1769862000, "CloudTrailEvent": {}}}"#,
                Json::string(body.clone()).to_compact_string()
            )
        })
        .collect();
    let token = token.map_or_else(String::new, |t| format!(r#", "NextToken": "{t}""#));
    format!(r#"{{"Events": [{}]{token}}}"#, events.join(","))
}

fn lookup(response: &str) -> String {
    step("cloudtrail", "LookupEvents", None, response)
}

fn sample_events() -> Vec<String> {
    vec![
        ct_event(
            &[
                usage("111111111111", "Requests", "2026-01-30", 5),
                usage("222222222222", "Requests", "2026-01-31", 7),
            ]
            .join(","),
            &[
                result("111111111111", "Requests", "2026-01-30", "Success", "m1"),
                result(
                    "222222222222",
                    "Requests",
                    "2026-01-31",
                    "CustomerNotSubscribed",
                    "m2",
                ),
            ]
            .join(","),
            "",
        ),
        ct_event(&usage("111111111111", "GBHours", "2026-01-29", 2), "", ""),
    ]
}

fn query_cli(extra: &[&str], recording: &str) -> (Cli, TempFile) {
    let file = TempFile::new("query", recording);
    let path = file.path();
    let mut words = vec![
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
        "--recorded",
        path.as_str(),
    ];
    words.extend_from_slice(extra);
    (toolkit(&words), file)
}

fn recording(pages: &[String]) -> String {
    format!(
        r#"{{"calls": [{}]}}"#,
        pages
            .iter()
            .map(|p| lookup(p))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn output(cli: &Cli) -> Json {
    assert_eq!(cli.status, 0, "stderr: {}", cli.stderr);
    j(&cli.stdout)
}

fn get<'j>(value: &'j Json, key: &str) -> &'j Json {
    value
        .get(key)
        .unwrap_or_else(|| panic!("member {key} in {}", value.to_compact_string()))
}

#[test]
fn records_resolve_status_and_product_from_the_event() {
    let body = ct_event(
        &usage("111111111111", "Requests", "2026-01-30", 5),
        &result("111111111111", "Requests", "2026-01-30", "Success", "m1"),
        &format!(
            r#", "additionalEventData": {{"licenseArnToProductCode": {{"{LICENSE}": "prod-x"}}}}"#
        ),
    );
    let records = extract_records("t", &j(&body)).expect("records");
    assert_eq!(records.len(), 1);
    let json = aggregate(&records, "detail", 10, 50).to_compact_string();
    assert!(json.contains(r#""status":"Success""#) && json.contains(r#""meteringRecordId":"m1""#));
    assert!(json.contains(r#""productCode":"prod-x""#));
    // A request-level ProductCode (legacy metering) wins over the mapping.
    let legacy = ct_event(&usage("1", "R", "2026-01-30", 1), "", "").replace(
        r#""usageRecords""#,
        r#""productCode": "legacy-p", "usageRecords""#,
    );
    let json = aggregate(
        &extract_records("t", &j(&legacy)).expect("records"),
        "detail",
        10,
        50,
    )
    .to_compact_string();
    assert!(json.contains(r#""productCode":"legacy-p""#));
    assert!(
        json.contains(r#""status":"unknown""#),
        "an unmatched record has no status"
    );
}

#[test]
fn a_null_response_or_a_list_mapping_does_not_crash() {
    let errored = r#"{"requestParameters": {"usageRecords": [{"customerAWSAccountId": "1", "dimension": "R", "quantity": 1}]}, "responseElements": null, "additionalEventData": null}"#;
    let records =
        extract_records("t", &j(errored)).expect("a request-level exception has no results");
    assert!(aggregate(&records, "detail", 10, 50)
        .to_compact_string()
        .contains(r#""status":"unknown""#));
    let listed = format!(
        r#"{{"requestParameters": {{"usageRecords": [{{"licenseArn": "{LICENSE}", "quantity": 1}}]}}, "additionalEventData": {{"productCodeByLicenseArn": [{{"licenseArn": "{LICENSE}", "productCode": "p9"}}, 5]}}}}"#
    );
    let json = aggregate(
        &extract_records("t", &j(&listed)).expect("records"),
        "detail",
        10,
        50,
    )
    .to_compact_string();
    assert!(json.contains(r#""productCode":"p9""#));
    // An event without metering parameters holds no records.
    assert!(extract_records("t", &j("{}")).expect("none").is_empty());
    // A non-numeric quantity or a record that is not an object is refused, not summed.
    let bad = r#"{"requestParameters": {"usageRecords": [{"quantity": "x"}]}}"#;
    assert!(extract_records("t", &j(bad)).is_err());
    assert!(extract_records("t", &j(r#"{"requestParameters": {"usageRecords": [5]}}"#)).is_err());
    assert!(extract_records("t", &j(r#"{"requestParameters": [1]}"#)).is_err());
}

#[test]
fn the_summary_counts_by_status_dimension_and_customer() {
    let (cli, _file) = query_cli(&[], &recording(&[page(&sample_events(), None)]));
    let out = output(&cli);
    assert_eq!(get(&out, "totalRecords"), &Json::Int(3));
    assert_eq!(get(&out, "totalUsageQuantity"), &Json::Int(14));
    assert_eq!(
        get(&out, "byStatus").to_compact_string(),
        r#"{"Success":1,"CustomerNotSubscribed":1,"unknown":1}"#
    );
    assert_eq!(
        get(&out, "byDimension").to_compact_string(),
        r#"{"Requests":{"records":2,"totalQuantity":12},"GBHours":{"records":1,"totalQuantity":2}}"#
    );
    assert_eq!(get(&out, "uniqueCustomers"), &Json::Int(2));
    assert_eq!(
        get(&out, "topCustomersByUsage").to_compact_string(),
        r#"[{"customer":"111111111111","totalQuantity":7},{"customer":"222222222222","totalQuantity":7}]"#
    );
    assert_eq!(
        get(&out, "dateRange").to_compact_string(),
        r#"{"earliest":"2026-01-29","latest":"2026-01-31"}"#
    );
    assert_eq!(
        get(&out, "source").as_str(),
        Some("CloudTrail Event History")
    );
    assert_eq!(get(&out, "region").as_str(), Some("us-east-1"));
    assert_eq!(
        get(&out, "timeRange").to_compact_string(),
        r#"{"start":"2026-01-24 12:30:00 UTC","end":"2026-01-31 12:30:00 UTC"}"#
    );
    assert_eq!(get(&out, "complete"), &Json::Bool(true));
    assert_eq!(get(&out, "truncated"), &Json::Bool(false));
    assert_eq!(get(&out, "windowClipped"), &Json::Bool(false));
    assert_eq!(
        get(&out, "fetch").to_compact_string(),
        r#"{"pagesFetched":1,"maxPages":20,"eventsProcessed":2,"recordsFetched":3}"#
    );
    assert!(cli.stderr.contains("Querying CloudTrail Event History\n  Region: us-east-1\n  Range: 2026-01-24 12:30 to 2026-01-31 12:30\n"));
    assert!(cli
        .stderr
        .contains("Processed 2 CloudTrail events \u{2192} 3 metering records\n"));
    assert!(cli.stderr.contains("After filters: 3 records\n"));
}

#[test]
fn the_window_is_sent_as_epoch_seconds() {
    let request = r#"{"LookupAttributes": [{"AttributeKey": "EventSource", "AttributeValue": "metering-marketplace.amazonaws.com"}], "StartTime": 1769257800, "EndTime": 1769862600, "MaxResults": 50}"#;
    let strict = format!(
        r#"{{"calls": [{}]}}"#,
        step(
            "cloudtrail",
            "LookupEvents",
            Some(request),
            &page(&[], None)
        )
    );
    let (cli, _file) = query_cli(&[], &strict);
    let out = output(&cli);
    assert_eq!(get(&out, "totalRecords"), &Json::Int(0));
    assert!(get(&out, "hint").as_str().is_some());
    // A different recorded request is a failure, not a lucky match.
    let wrong = strict.replace("1769257800", "1");
    let (cli, _file) = query_cli(&[], &wrong);
    assert_eq!(cli.status, 1);
    assert!(cli.stderr.contains("aws_connect"));
}

#[test]
fn filters_narrow_the_records() {
    let events = page(&sample_events(), None);
    let count = |extra: &[&str]| {
        let (cli, _file) = query_cli(extra, &recording(std::slice::from_ref(&events)));
        get(&output(&cli), "totalRecords").clone()
    };
    assert_eq!(count(&["--customer-id", "111111111111"]), Json::Int(2));
    assert_eq!(count(&["--status", "Success"]), Json::Int(1));
    assert_eq!(count(&["--dimension", "GBHours"]), Json::Int(1));
    assert_eq!(count(&["--metering-record-id", "m2"]), Json::Int(1));
    assert_eq!(count(&["--start-date", "2026-01-30"]), Json::Int(2));
    assert_eq!(count(&["--end-date", "2026-01-30"]), Json::Int(2));
    assert_eq!(count(&["--customer-id", "nobody"]), Json::Int(0));
}

#[test]
fn query_types_shape_the_output() {
    let events = page(&sample_events(), None);
    let run_type = |query_type: &str, extra: &[&str]| {
        let mut words = vec!["--query-type", query_type];
        words.extend_from_slice(extra);
        let (cli, _file) = query_cli(&words, &recording(std::slice::from_ref(&events)));
        output(&cli)
    };
    let count = run_type("count", &[]);
    assert!(count.get("byStatus").is_some() && count.get("byDimension").is_some());
    assert!(count.get("topCustomersByUsage").is_none());
    let top = run_type("top_customers", &["--top-n", "1"]);
    assert_eq!(
        get(&top, "topCustomersByUsage").to_compact_string(),
        r#"[{"customer":"111111111111","totalQuantity":7}]"#
    );
    assert_eq!(
        get(&top, "topCustomersByRecordCount").to_compact_string(),
        r#"[{"customer":"111111111111","records":2}]"#
    );
    let failures = run_type("list_failures", &[]);
    assert_eq!(get(&failures, "totalFailures"), &Json::Int(2));
    assert_eq!(
        get(&failures, "byStatus").to_compact_string(),
        r#"{"CustomerNotSubscribed":1,"unknown":1}"#
    );
    let detail = run_type("detail", &["--limit", "2"]);
    assert_eq!(get(&detail, "showing"), &Json::Int(2));
    assert_eq!(
        detail
            .get("records")
            .map(|r| r.to_compact_string().matches("eventTime").count()),
        Some(2)
    );
}

#[test]
fn a_negative_limit_slices_like_python() {
    let records = extract_records("t", &j(&sample_events()[0])).expect("records");
    let detail = aggregate(&records, "detail", 10, -1);
    assert_eq!(get(&detail, "showing"), &Json::Int(-1));
    assert_eq!(
        get(&detail, "records")
            .to_compact_string()
            .matches("eventTime")
            .count(),
        1
    );
    let none = aggregate(&records, "detail", 10, -9);
    assert_eq!(get(&none, "records").to_compact_string(), "[]");
}

#[test]
fn a_page_cap_marks_the_result_partial() {
    let pages = recording(&[page(&sample_events(), Some("next"))]);
    let (cli, _file) = query_cli(&["--max-pages", "1"], &pages);
    let out = output(&cli);
    assert_eq!(get(&out, "truncated"), &Json::Bool(true));
    assert_eq!(get(&out, "complete"), &Json::Bool(false));
    assert!(get(&out, "completenessNote")
        .as_str()
        .is_some_and(|n| n.starts_with("PARTIAL RESULT: the --max-pages cap (1)")));
    assert!(get(&out, "aggregateWarning")
        .as_str()
        .is_some_and(|w| w.contains("LOWER BOUND over the fetched sample")));
    assert!(cli
        .stderr
        .contains("WARNING: hit the --max-pages cap (1 pages)"));
    // A detail query is partial too but has no aggregate warning.
    let (cli, _file) = query_cli(&["--max-pages", "1", "--query-type", "detail"], &pages);
    assert!(output(&cli).get("aggregateWarning").is_none());
    // Two pages with a cap of two end exactly at the cap with a token left: truncated.
    let two = recording(&[page(&[], Some("a")), page(&[], Some("b"))]);
    let (cli, _file) = query_cli(&["--max-pages", "2"], &two);
    assert_eq!(get(&output(&cli), "truncated"), &Json::Bool(true));
    // A walk that ends before the cap is complete.
    let one = recording(&[page(&[], Some("a")), page(&[], None)]);
    let (cli, _file) = query_cli(&["--max-pages", "5"], &one);
    let out = output(&cli);
    assert_eq!(get(&out, "complete"), &Json::Bool(true));
    assert_eq!(get(&out, "fetch").get("pagesFetched"), Some(&Json::Int(2)));
}

#[test]
fn the_ninety_day_window_is_clipped_or_refused() {
    let (cli, _file) = query_cli(
        &["--start-date", "2025-01-01"],
        &recording(&[page(&[], None)]),
    );
    let out = output(&cli);
    assert_eq!(get(&out, "windowClipped"), &Json::Bool(true));
    assert_eq!(get(&out, "complete"), &Json::Bool(false));
    assert!(get(&out, "windowClippedNote").as_str().is_some_and(
        |n| n.contains("covers only 2025-11-02 onward (the requested start was 2025-01-01)")
    ));
    assert!(cli
        .stderr
        .contains("NOTE: CloudTrail Event History retains only 90 days"));
    assert_eq!(
        get(&out, "timeRange").get("start").and_then(Json::as_str),
        Some("2025-11-02 12:30:00 UTC")
    );
    let (cli, _file) = query_cli(
        &["--start-date", "2025-01-01"],
        &recording(&[page(&sample_events(), None)]),
    );
    assert!(get(&output(&cli), "aggregateWarning")
        .as_str()
        .is_some_and(|w| w.starts_with("Totals/rankings cover only the in-retention window")));

    // Entirely older than 90 days: refused, nothing is queried.
    let none = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
        "--start-date",
        "2020-01-01",
        "--end-date",
        "2020-02-01",
    ]);
    let out = output(&none);
    assert_eq!(get(&out, "query").as_str(), Some("not-performed"));
    assert!(get(&out, "detail")
        .as_str()
        .is_some_and(|d| d.contains("ends 2020-02-01")));
    assert!(get(&out, "recommendation")
        .as_str()
        .is_some_and(|r| r.contains("Seller Reports")));
}

#[test]
fn inconsistent_dates_fail() {
    let reversed = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
        "--start-date",
        "2026-02-10",
        "--end-date",
        "2026-02-01",
    ]);
    assert_eq!(reversed.status, 1);
    assert!(reversed.stderr.contains("start_time (2026-02-10) is after end_time (2026-02-01). Provide --start-date when using a past --end-date."));
    // A past end date without a start date: the default 7-day start is after it.
    let past_end = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
        "--end-date",
        "2026-01-01",
    ]);
    assert_eq!(past_end.status, 1);
    let bad = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
        "--start-date",
        "yesterday",
    ]);
    assert_eq!(bad.status, 1);
    assert!(bad.stderr.contains("--start-date must be YYYY-MM-DD"));
    assert!(!bad.stderr.contains("yesterday"));
    // The end date is clamped to 23:59:59 of its day and 90 days back is the boundary.
    let edge = toolkit(&[
        "marketplace",
        "query-metering",
        "--region",
        "us-east-1",
        "--now",
        PINNED,
        "--start-date",
        "2025-10-01",
        "--end-date",
        "2025-11-01",
    ]);
    assert_eq!(get(&output(&edge), "query").as_str(), Some("not-performed"));
}

#[test]
fn product_queries_need_a_mapping() {
    let events = page(&sample_events(), None);
    let (cli, _file) = query_cli(
        &["--product-code", "prod-x"],
        &recording(std::slice::from_ref(&events)),
    );
    let out = output(&cli);
    assert_eq!(get(&out, "query").as_str(), Some("not-performed"));
    assert_eq!(
        get(&out, "supportedNonProductQueries")
            .to_compact_string()
            .matches("by ")
            .count(),
        4
    );
    // The deployment context stamps the product on every record that has none.
    let (cli, _file) = query_cli(
        &[
            "--product-code",
            "prod-x",
            "--deployment-product-code",
            "prod-x",
        ],
        &recording(std::slice::from_ref(&events)),
    );
    assert_eq!(get(&output(&cli), "totalRecords"), &Json::Int(3));
    let (cli, _file) = query_cli(
        &[
            "--product-code",
            "prod-y",
            "--deployment-product-code",
            "prod-x",
        ],
        &recording(&[events]),
    );
    assert_eq!(get(&output(&cli), "totalRecords"), &Json::Int(0));
    // With no records at all there is nothing to refuse.
    let (cli, _file) = query_cli(
        &["--product-code", "prod-x"],
        &recording(&[page(&[], None)]),
    );
    assert_eq!(get(&output(&cli), "totalRecords"), &Json::Int(0));
}

#[test]
fn undecodable_events_are_skipped_and_wrong_shapes_refused() {
    let page_text = r#"{"Events": [{"EventTime": 1, "CloudTrailEvent": "{not json"}, {"CloudTrailEvent": "{}"}, {"EventTime": 1769862000}]}"#;
    let (cli, _file) = query_cli(&[], &recording(&[page_text.to_string()]));
    let out = output(&cli);
    assert_eq!(
        get(&out, "fetch").get("eventsProcessed"),
        Some(&Json::Int(1)),
        "only the event without a body is parsed as {{}}"
    );
    let wrong = r#"{"Events": [{"EventTime": 1, "CloudTrailEvent": 5}]}"#;
    let (cli, _file) = query_cli(&[], &recording(&[wrong.to_string()]));
    assert_eq!(cli.status, 1);
    assert!(cli
        .stderr
        .contains("CloudTrailEvent must be a string when present (it is integer)"));
    let (cli, _file) = query_cli(&[], &recording(&[r#"{"Events": null}"#.to_string()]));
    assert_eq!(cli.status, 1);
}
