//! `rds_commitment_pricing_analyzer.py`: fetch rules, arithmetic and boundaries.

use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::super::rds::client::{AwsRead, NoLiveClient, RecordedClient, Service};
use super::super::rds::{
    analyze_instance_live, build_comparison, family_of, fetch_dsp_rates, fetch_ri_offerings,
    on_demand_price, Comparison, Workload,
};
use super::json;

fn client(text: &str) -> RecordedClient {
    RecordedClient::from_document(&json(text)).expect("valid recording")
}

fn offline(
    client: &mut dyn AwsRead,
    instance_type: &str,
    engine: &str,
    num_instances: i64,
    region: &str,
    multi_az: bool,
) -> Comparison {
    build_comparison(
        client,
        &Workload {
            instance_type,
            engine,
            num_instances,
            region,
            multi_az,
        },
        None,
    )
}

fn field<'a>(document: &'a Json, path: &[&str]) -> &'a Json {
    path.iter().fold(document, |node, key| {
        node.get(key)
            .unwrap_or_else(|| panic!("missing {key} in {path:?}"))
    })
}

fn number(document: &Json, path: &[&str]) -> f64 {
    match field(document, path) {
        Json::Int(n) => *n as f64,
        Json::Float(n) => *n,
        other => panic!("{path:?} is {other:?}"),
    }
}

fn notes(document: &Json) -> Vec<String> {
    match field(document, &["notes"]) {
        Json::Array(items) => items
            .iter()
            .filter_map(|n| n.as_str().map(str::to_string))
            .collect(),
        other => panic!("{other:?}"),
    }
}

/// An RDS offerings call for `db.r7g.2xlarge` MySQL, single-AZ, with `body`.
fn offerings_call(marker: Option<&str>, body: &str) -> String {
    let marker = marker.map_or(String::new(), |m| format!(r#", "Marker": "{m}""#));
    format!(
        r#"{{"service": "rds", "operation": "DescribeReservedDBInstancesOfferings",
            "request": {{"DBInstanceClass": "db.r7g.2xlarge", "ProductDescription": "MySQL",
                         "MultiAZ": false{marker}}},
            "response": {body}}}"#
    )
}

fn dsp_call(token: Option<&str>, body: &str) -> String {
    let token = token.map_or(String::new(), |t| format!(r#", "nextToken": "{t}""#));
    format!(
        r#"{{"service": "savingsplans", "operation": "DescribeSavingsPlansOfferingRates",
            "request": {{"savingsPlanTypes": ["Database"], "products": ["RDS"],
                         "serviceCodes": ["AmazonRDS"],
                         "filters": [{{"name": "region", "values": ["us-east-1"]}},
                                     {{"name": "productDescription", "values": ["MySQL"]}}],
                         "maxResults": 1000{token}}},
            "response": {body}}}"#
    )
}

fn recording(calls: &[String]) -> RecordedClient {
    client(&format!(r#"{{"calls": [{}]}}"#, calls.join(",")))
}

fn offering(class: &str, duration: u64, kind: &str, fixed: &str, hourly: &str) -> String {
    format!(
        r#"{{"DBInstanceClass": "{class}", "Duration": {duration}, "OfferingType": "{kind}",
            "FixedPrice": {fixed}, "RecurringCharges": [{{"RecurringChargeAmount": {hourly}}}]}}"#
    )
}

#[test]
fn offline_without_a_client_uses_the_bundled_on_demand_price() {
    let result = offline(
        &mut NoLiveClient,
        "db.r7g.2xlarge",
        "mysql",
        2,
        "us-east-1",
        false,
    );
    let doc = &result.document;
    assert_eq!(number(doc, &["on_demand", "hourly"]), 0.912);
    assert_eq!(number(doc, &["on_demand", "monthly"]), 1331.52);
    for key in ["ri_1yr", "ri_3yr", "dsp_1yr", "dsp_3yr"] {
        assert_eq!(field(doc, &[key]), &Json::Null, "{key}");
    }
    assert_eq!(
        field(doc, &["recommendation", "best_option"]),
        &Json::string("on_demand")
    );
    assert_eq!(
        field(doc, &["recommendation", "reason"]),
        &Json::string("No RI or DSP offerings found.")
    );
    assert_eq!(field(doc, &["num_instances"]), &Json::Int(2));
    assert!(
        notes(doc).is_empty(),
        "r7g is DSP eligible: {:?}",
        notes(doc)
    );
    assert!(result.warnings.is_empty());
}

#[test]
fn the_document_keeps_the_scripts_key_order() {
    let result = offline(
        &mut NoLiveClient,
        "db.m5.large",
        "postgres",
        1,
        "us-east-1",
        false,
    );
    let Json::Object(members) = &result.document else {
        panic!()
    };
    let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "engine",
            "instance_type",
            "num_instances",
            "multi_az",
            "on_demand",
            "ri_1yr",
            "ri_3yr",
            "dsp_1yr",
            "dsp_3yr",
            "recommendation",
            "notes"
        ]
    );
}

#[test]
fn price_boundaries_unknown_class_multi_az_and_other_regions() {
    // Unknown class: no static price, so the baseline is $0 and said so.
    let unknown = offline(
        &mut NoLiveClient,
        "db.z9.huge",
        "mysql",
        1,
        "us-east-1",
        false,
    );
    assert_eq!(number(&unknown.document, &["on_demand", "hourly"]), 0.0);
    let text = notes(&unknown.document);
    assert!(
        text[0].starts_with("No static on-demand price is bundled for db.z9.huge"),
        "{text:?}"
    );
    assert!(text[1].contains("do not cover the z9 family"), "{text:?}");
    // Multi-AZ doubles the price and adds its note.
    let multi = offline(
        &mut NoLiveClient,
        "db.r7g.2xlarge",
        "mysql",
        1,
        "us-east-1",
        true,
    );
    assert_eq!(number(&multi.document, &["on_demand", "hourly"]), 1.824);
    assert!(notes(&multi.document)
        .iter()
        .any(|n| n.starts_with("Multi-AZ pricing applied.")));
    // Another region warns once and notes the approximation.
    let eu = offline(
        &mut NoLiveClient,
        "db.r7g.2xlarge",
        "mysql",
        1,
        "eu-west-1",
        false,
    );
    assert_eq!(
        eu.warnings,
        ["On-demand price for db.r7g.2xlarge is based on us-east-1. Actual price in eu-west-1 may differ."]
    );
    assert!(notes(&eu.document)
        .iter()
        .any(|n| n.contains("may differ by 10-20%")));
    // No warning and no approximation note when there is no static price at all.
    let none = offline(
        &mut NoLiveClient,
        "db.z9.huge",
        "mysql",
        1,
        "eu-west-1",
        false,
    );
    assert!(none.warnings.is_empty());
    assert!(!notes(&none.document).iter().any(|n| n.contains("10-20%")));
    assert_eq!(
        on_demand_price("db.t3.medium", "us-east-1", false),
        (0.068, None)
    );
}

#[test]
fn the_dsp_family_note_lists_the_eligible_families_sorted() {
    let result = offline(
        &mut NoLiveClient,
        "db.t3.medium",
        "mysql",
        1,
        "us-east-1",
        false,
    );
    assert_eq!(
        notes(&result.document)[0],
        "Database Savings Plans do not cover the t3 family. Eligible families: \
         c7g, c7i, m7g, m7i, r7g, r7i, r8g, r8gd, x8g."
    );
    assert_eq!(family_of("db.r8gd.large"), "r8gd");
    assert_eq!(family_of("db.t3.medium"), "t3");
    for malformed in [
        "",
        "r7g.large",
        "db.",
        "db..x",
        "db.R7G.large",
        "db.r7g",
        "xdb.r7g.large",
    ] {
        assert_eq!(family_of(malformed), "", "{malformed:?}");
    }
}

#[test]
fn reserved_instance_rates_are_priced_per_term_and_deduplicated() {
    let one_year = offering("db.r7g.2xlarge", 31_536_000, "All Upfront", "7000.0", "0.0");
    let one_year_partial = offering(
        "db.r7g.2xlarge",
        31_536_000,
        "Partial Upfront",
        "3000.0",
        "0.05",
    );
    let three_year = offering("db.r7g.2xlarge", 94_608_000, "No Upfront", "0.0", "0.4");
    let other_class = offering("db.r7g.4xlarge", 31_536_000, "All Upfront", "1.0", "0.0");
    let duplicate = offering("db.r7g.2xlarge", 31_536_000, "All Upfront", "7000.0", "0.0");
    let mut api = recording(&[offerings_call(
        None,
        &format!(
            r#"{{"ReservedDBInstancesOfferings": [{one_year}, {one_year_partial}, {three_year},
                {other_class}, {duplicate}]}}"#
        ),
    )]);
    let offers = fetch_ri_offerings(&mut api, "db.r7g.2xlarge", "mysql", "us-east-1", false);
    assert_eq!(offers.len(), 3, "foreign class skipped, duplicate dropped");
    assert_eq!(offers[0].term_years, 1);
    // 7000 / 8760 = 0.79908675... rounded to six places.
    assert_eq!(offers[0].effective_hourly, 0.799087);
    assert_eq!(offers[0].upfront_cost, 7000.0);
    assert_eq!(offers[1].effective_hourly, 0.392466);
    assert_eq!(
        offers[2].term_years, 3,
        "94,608,000 s is above the 94,000,000 s cut-off"
    );
    assert_eq!(offers[2].effective_hourly, 0.4);
}

#[test]
fn the_three_year_cut_off_is_strictly_above_94_million_seconds() {
    let at = offering("db.r7g.2xlarge", 94_000_000, "x", "0.0", "0.1");
    let above = offering("db.r7g.2xlarge", 94_000_001, "y", "0.0", "0.1");
    let mut api = recording(&[offerings_call(
        None,
        &format!(r#"{{"ReservedDBInstancesOfferings": [{at}, {above}]}}"#),
    )]);
    let offers = fetch_ri_offerings(&mut api, "db.r7g.2xlarge", "mysql", "us-east-1", false);
    assert_eq!(
        offers.iter().map(|o| o.term_years).collect::<Vec<_>>(),
        [1, 3]
    );
}

#[test]
fn reserved_instance_pages_are_followed_by_marker() {
    let first = offering("db.r7g.2xlarge", 31_536_000, "All Upfront", "7000.0", "0.0");
    let second = offering(
        "db.r7g.2xlarge",
        94_608_000,
        "All Upfront",
        "15000.0",
        "0.0",
    );
    let mut api = recording(&[
        offerings_call(
            None,
            &format!(r#"{{"ReservedDBInstancesOfferings": [{first}], "Marker": "m1"}}"#),
        ),
        offerings_call(
            Some("m1"),
            &format!(r#"{{"ReservedDBInstancesOfferings": [{second}]}}"#),
        ),
    ]);
    let offers = fetch_ri_offerings(&mut api, "db.r7g.2xlarge", "mysql", "us-east-1", false);
    assert_eq!(offers.len(), 2);
}

#[test]
fn every_reserved_instance_failure_yields_no_offerings() {
    let good = offering("db.r7g.2xlarge", 31_536_000, "All Upfront", "7000.0", "0.0");
    let bad_fixed = r#"{"DBInstanceClass": "db.r7g.2xlarge", "Duration": 1, "FixedPrice": null}"#;
    let bad_duration = r#"{"DBInstanceClass": "db.r7g.2xlarge", "Duration": "long"}"#;
    let bad_charges = r#"{"DBInstanceClass": "db.r7g.2xlarge", "RecurringCharges": 3}"#;
    for (label, body) in [
        (
            "non-array",
            r#"{"ReservedDBInstancesOfferings": "none"}"#.to_string(),
        ),
        (
            "null FixedPrice",
            format!(r#"{{"ReservedDBInstancesOfferings": [{good}, {bad_fixed}]}}"#),
        ),
        (
            "string Duration",
            format!(r#"{{"ReservedDBInstancesOfferings": [{good}, {bad_duration}]}}"#),
        ),
        (
            "bad charges",
            format!(r#"{{"ReservedDBInstancesOfferings": [{good}, {bad_charges}]}}"#),
        ),
        (
            "numeric marker",
            format!(r#"{{"ReservedDBInstancesOfferings": [{good}], "Marker": 5}}"#),
        ),
    ] {
        let mut api = recording(&[offerings_call(None, &body)]);
        let offers = fetch_ri_offerings(&mut api, "db.r7g.2xlarge", "mysql", "us-east-1", false);
        assert!(
            offers.is_empty(),
            "{label}: a partial result must not survive"
        );
    }
    // A connection failure, an unmatched request and the no-live client too.
    let mut down = client(
        r#"{"calls": [{"service": "rds", "operation": "DescribeReservedDBInstancesOfferings",
            "request": {"DBInstanceClass": "db.r7g.2xlarge", "ProductDescription": "MySQL", "MultiAZ": false},
            "error": {"kind": "connect", "message": "connection refused"}}]}"#,
    );
    assert!(
        fetch_ri_offerings(&mut down, "db.r7g.2xlarge", "mysql", "us-east-1", false).is_empty()
    );
    assert!(
        fetch_ri_offerings(&mut down, "db.r7g.2xlarge", "postgres", "us-east-1", false).is_empty()
    );
    assert!(fetch_ri_offerings(
        &mut NoLiveClient,
        "db.r7g.2xlarge",
        "mysql",
        "us-east-1",
        false
    )
    .is_empty());
    // A stuck pagination token is cut off, not followed forever.
    let item = offering("db.r7g.2xlarge", 31_536_000, "x", "1.0", "0.0");
    let mut stuck = recording(&[
        offerings_call(
            None,
            &format!(r#"{{"ReservedDBInstancesOfferings": [{item}], "Marker": "m"}}"#),
        ),
        offerings_call(
            Some("m"),
            &format!(r#"{{"ReservedDBInstancesOfferings": [{item}], "Marker": "m"}}"#),
        ),
    ]);
    assert!(
        fetch_ri_offerings(&mut stuck, "db.r7g.2xlarge", "mysql", "us-east-1", false).is_empty()
    );
}

fn rate(usage: &str, seconds: u64, payment: &str, rate: &str) -> String {
    format!(
        r#"{{"savingsPlanOffering": {{"durationSeconds": {seconds}, "paymentOption": "{payment}"}},
            "usageType": "{usage}", "rate": {rate}}}"#
    )
}

#[test]
fn savings_plan_rates_are_grouped_by_class_and_filtered() {
    let entries = [
        rate(
            "USE2-InstanceUsage:db.r7g.2xlarge",
            31_536_000,
            "No Upfront",
            r#""0.5""#,
        ),
        rate(
            "InstanceUsage:db.r7g.2xlarge",
            94_608_000,
            "All Upfront",
            r#""0.3""#,
        ),
        rate(
            "InstanceUsage:db.r7g.4xlarge",
            31_536_000,
            "No Upfront",
            "1.0",
        ),
        // Skipped: zero, negative, unparsable, null and missing-class entries.
        rate(
            "InstanceUsage:db.r7g.2xlarge",
            31_536_000,
            "No Upfront",
            r#""0""#,
        ),
        rate(
            "InstanceUsage:db.r7g.2xlarge",
            31_536_000,
            "No Upfront",
            r#""-1""#,
        ),
        rate(
            "InstanceUsage:db.r7g.2xlarge",
            31_536_000,
            "No Upfront",
            r#""abc""#,
        ),
        rate(
            "InstanceUsage:db.r7g.2xlarge",
            31_536_000,
            "No Upfront",
            "null",
        ),
        rate(
            "SomethingElse:db.r7g.2xlarge",
            31_536_000,
            "No Upfront",
            r#""0.9""#,
        ),
        rate(
            "InstanceUsage:db.r7g.",
            31_536_000,
            "No Upfront",
            r#""0.9""#,
        ),
    ]
    .join(",");
    let mut api = recording(&[dsp_call(
        None,
        &format!(r#"{{"searchResults": [{entries}]}}"#),
    )]);
    let rates = fetch_dsp_rates(&mut api, "mysql", "us-east-1");
    let (class, list) = &rates[0];
    assert_eq!(class, "db.r7g.2xlarge");
    assert_eq!(list.len(), 2);
    assert_eq!((list[0].term_years, list[0].rate_per_hour), (1, 0.5));
    assert_eq!((list[1].term_years, list[1].rate_per_hour), (3, 0.3));
    assert_eq!(rates.len(), 2);
    assert_eq!(rates[1].0, "db.r7g.4xlarge");
}

#[test]
fn savings_plan_pages_failures_and_partial_results() {
    let first = rate(
        "InstanceUsage:db.r7g.2xlarge",
        31_536_000,
        "No Upfront",
        r#""0.5""#,
    );
    let second = rate(
        "InstanceUsage:db.r7g.2xlarge",
        94_608_000,
        "No Upfront",
        r#""0.3""#,
    );
    let mut paged = recording(&[
        dsp_call(
            None,
            &format!(r#"{{"searchResults": [{first}], "nextToken": "t1"}}"#),
        ),
        dsp_call(Some("t1"), &format!(r#"{{"searchResults": [{second}]}}"#)),
    ]);
    assert_eq!(
        fetch_dsp_rates(&mut paged, "mysql", "us-east-1")[0].1.len(),
        2
    );

    // A failure while paging loses everything (the script built the list first).
    let mut broken = recording(&[dsp_call(
        None,
        &format!(r#"{{"searchResults": [{first}], "nextToken": "t1"}}"#),
    )]);
    assert!(fetch_dsp_rates(&mut broken, "mysql", "us-east-1").is_empty());
    assert!(fetch_dsp_rates(&mut NoLiveClient, "mysql", "us-east-1").is_empty());

    // A malformed entry stops the walk and keeps the entries before it.
    let malformed = r#"{"savingsPlanOffering": [], "usageType": "InstanceUsage:db.r7g.2xlarge", "rate": "0.1"}"#;
    let after = rate(
        "InstanceUsage:db.r7g.4xlarge",
        31_536_000,
        "No Upfront",
        r#""0.9""#,
    );
    let mut partial = recording(&[dsp_call(
        None,
        &format!(r#"{{"searchResults": [{first}, {malformed}, {after}]}}"#),
    )]);
    let rates = fetch_dsp_rates(&mut partial, "mysql", "us-east-1");
    assert_eq!(rates.len(), 1, "{rates:?}");
    assert_eq!(rates[0].1.len(), 1);
}

#[test]
fn the_savings_plan_request_names_the_region_and_engine_filters() {
    // A request for another engine or region has no recording: no rates.
    let entry = rate(
        "InstanceUsage:db.r7g.2xlarge",
        31_536_000,
        "No Upfront",
        r#""0.5""#,
    );
    let mut api = recording(&[dsp_call(
        None,
        &format!(r#"{{"searchResults": [{entry}]}}"#),
    )]);
    assert!(fetch_dsp_rates(&mut api, "postgres", "us-east-1").is_empty());
    assert!(fetch_dsp_rates(&mut api, "mysql", "eu-west-1").is_empty());
    assert_eq!(fetch_dsp_rates(&mut api, "mysql", "us-east-1").len(), 1);
}

#[test]
fn the_best_option_is_the_cheapest_and_the_numbers_round_like_python() {
    let one_year = offering(
        "db.r7g.2xlarge",
        31_536_000,
        "Partial Upfront",
        "3000.0",
        "0.05",
    );
    let costly = offering("db.r7g.2xlarge", 31_536_000, "No Upfront", "0.0", "0.8");
    let three_year = offering(
        "db.r7g.2xlarge",
        94_608_000,
        "All Upfront",
        "12000.0",
        "0.0",
    );
    let dsp = rate(
        "InstanceUsage:db.r7g.2xlarge",
        94_608_000,
        "No Upfront",
        r#""0.40""#,
    );
    let mut api = recording(&[
        offerings_call(
            None,
            &format!(r#"{{"ReservedDBInstancesOfferings": [{costly}, {one_year}, {three_year}]}}"#),
        ),
        dsp_call(None, &format!(r#"{{"searchResults": [{dsp}]}}"#)),
    ]);
    let result = offline(&mut api, "db.r7g.2xlarge", "mysql", 2, "us-east-1", false);
    let doc = &result.document;
    // The cheaper of two 1-year offerings wins: 3000/8760 + 0.05 = 0.392466.
    assert_eq!(
        number(doc, &["ri_1yr", "effective_hourly_per_instance"]),
        0.3925
    );
    assert_eq!(number(doc, &["ri_1yr", "upfront_total"]), 6000.0);
    assert_eq!(
        field(doc, &["ri_1yr", "payment_option"]),
        &Json::string("Partial Upfront")
    );
    // monthly = 0.392466 * 730 * 2 = 573.00...
    assert_eq!(number(doc, &["ri_1yr", "monthly"]), 573.0);
    // 12000 / 26280 = 0.456621; monthly = 0.456621 * 730 * 2 = 666.67.
    assert_eq!(number(doc, &["ri_3yr", "monthly"]), 666.67);
    assert_eq!(
        field(doc, &["dsp_3yr", "payment_option"]),
        &Json::string("No Upfront")
    );
    assert_eq!(number(doc, &["dsp_3yr", "rate_per_hour"]), 0.4);
    assert_eq!(number(doc, &["dsp_3yr", "monthly"]), 584.0);
    assert_eq!(field(doc, &["dsp_1yr"]), &Json::Null);
    // On-demand is 0.912 * 730 * 2 = 1331.52; the 1-year RI at 573.00 is cheapest.
    assert_eq!(
        field(doc, &["recommendation", "best_option"]),
        &Json::string("1yr RI")
    );
    assert_eq!(number(doc, &["recommendation", "best_monthly_cost"]), 573.0);
    assert!(number(doc, &["recommendation", "savings_pct"]) > 49.0);
}

#[test]
fn multi_az_doubles_the_savings_plan_cost_like_on_demand_and_reserved() {
    let dsp = rate(
        "InstanceUsage:db.r7g.2xlarge",
        31_536_000,
        "No Upfront",
        r#""0.5""#,
    );
    let calls = |multi: bool| {
        let request = format!(
            r#"{{"DBInstanceClass": "db.r7g.2xlarge", "ProductDescription": "MySQL", "MultiAZ": {multi}}}"#
        );
        vec![
            format!(
                r#"{{"service": "rds", "operation": "DescribeReservedDBInstancesOfferings",
                   "request": {request}, "response": {{"ReservedDBInstancesOfferings": []}}}}"#
            ),
            dsp_call(None, &format!(r#"{{"searchResults": [{dsp}]}}"#)),
        ]
    };
    let single = offline(
        &mut recording(&calls(false)),
        "db.r7g.2xlarge",
        "mysql",
        1,
        "us-east-1",
        false,
    );
    let multi = offline(
        &mut recording(&calls(true)),
        "db.r7g.2xlarge",
        "mysql",
        1,
        "us-east-1",
        true,
    );
    assert_eq!(number(&single.document, &["dsp_1yr", "monthly"]), 365.0);
    assert_eq!(number(&multi.document, &["dsp_1yr", "monthly"]), 730.0);
}

#[test]
fn savings_percentages_are_integer_zero_without_a_baseline() {
    let dsp = rate(
        "InstanceUsage:db.z9.huge",
        31_536_000,
        "No Upfront",
        r#""0.5""#,
    );
    let mut api = recording(&[
        r#"{"service": "rds", "operation": "DescribeReservedDBInstancesOfferings",
            "request": {"DBInstanceClass": "db.z9.huge", "ProductDescription": "MySQL", "MultiAZ": false},
            "response": {}}"#
            .to_string(),
        dsp_call(None, &format!(r#"{{"searchResults": [{dsp}]}}"#)),
    ]);
    let result = offline(&mut api, "db.z9.huge", "mysql", 1, "us-east-1", false);
    // Python: `pct = ... if od > 0 else 0`, and round(0, 1) is the int 0.
    assert_eq!(
        field(&result.document, &["dsp_1yr", "savings_pct"]),
        &Json::Int(0)
    );
    assert_eq!(
        field(&result.document, &["recommendation", "savings_pct"]),
        &Json::Int(0)
    );
    assert_eq!(
        number(
            &result.document,
            &["recommendation", "savings_vs_on_demand"]
        ),
        -365.0
    );
}

#[test]
fn the_engine_selects_the_product_description() {
    let mut api = client(
        r#"{"calls": [{"service": "rds", "operation": "DescribeReservedDBInstancesOfferings",
            "request": {"DBInstanceClass": "db.m5.large", "ProductDescription": "PostgreSQL", "MultiAZ": false},
            "response": {"ReservedDBInstancesOfferings": [
              {"DBInstanceClass": "db.m5.large", "Duration": 31536000, "OfferingType": "No Upfront",
               "FixedPrice": 0.0, "RecurringCharges": [{"RecurringChargeAmount": 0.1}]}]}}]}"#,
    );
    let pg = fetch_ri_offerings(&mut api, "db.m5.large", "postgres", "us-east-1", false);
    assert_eq!(pg.len(), 1);
    // An unknown engine falls back to MySQL, which has no recording here.
    assert!(
        fetch_ri_offerings(&mut api, "db.m5.large", "oracle-ee", "us-east-1", false).is_empty()
    );
}

const DESCRIBE: &str = r#"{"service": "rds", "operation": "DescribeDBInstances",
    "request": {"DBInstanceIdentifier": "prod-db"}"#;

#[test]
fn a_live_instance_is_analyzed_with_its_engine_version_and_replica_note() {
    let mut api = client(&format!(
        r#"{{"calls": [{DESCRIBE}, "response": {{"DBInstances": [{{"Engine": "mysql",
            "DBInstanceClass": "db.r6g.xlarge", "MultiAZ": true, "EngineVersion": "8.0.35",
            "ReadReplicaDBInstanceIdentifiers": ["r1", "r2"]}}]}}}}]}}"#
    ));
    let result = analyze_instance_live(&mut api, "prod-db", "us-east-1");
    let doc = &result.document;
    assert_eq!(field(doc, &["instance_id"]), &Json::string("prod-db"));
    assert_eq!(field(doc, &["engine_version"]), &Json::string("8.0.35"));
    assert_eq!(field(doc, &["multi_az"]), &Json::Bool(true));
    assert_eq!(number(doc, &["on_demand", "hourly"]), 0.87);
    let Json::Object(members) = doc else { panic!() };
    let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(&keys[keys.len() - 2..], ["instance_id", "engine_version"]);
    let last = notes(doc).pop().expect("notes");
    assert_eq!(
        last,
        "Instance has 2 read replica(s). Consider separate RI/DSP for each replica (Single-AZ pricing)."
    );
}

#[test]
fn a_live_lookup_that_fails_is_reported_in_the_document() {
    let error_of = |text: &str| -> String {
        let mut api = client(text);
        let result = analyze_instance_live(&mut api, "prod-db", "us-east-1");
        let Json::Object(members) = &result.document else {
            panic!()
        };
        assert_eq!(members.len(), 2, "{members:?}");
        field(&result.document, &["error"])
            .as_str()
            .expect("text")
            .to_string()
    };
    assert_eq!(
        error_of(&format!(r#"{{"calls": [{DESCRIBE}, "error": {{"code": "DBInstanceNotFound", "message": "no such instance"}}}}]}}"#)),
        "An error occurred (DBInstanceNotFound) when calling the DescribeDBInstances operation: no such instance"
    );
    assert_eq!(
        error_of(&format!(
            r#"{{"calls": [{DESCRIBE}, "response": {{"DBInstances": []}}}}]}}"#
        )),
        "instance not found"
    );
    assert!(error_of(&format!(
        r#"{{"calls": [{DESCRIBE}, "error": {{"kind": "no_credentials"}}}}]}}"#
    ))
    .contains("credentials"));
    assert!(
        error_of(r#"{"calls": []}"#).contains("no recorded response for rds DescribeDBInstances")
    );
    for body in [
        r#"{"DBInstances": [{"MultiAZ": "yes"}]}"#,
        r#"{"DBInstances": [{"Engine": 5}]}"#,
        r#"{"DBInstances": ["x"]}"#,
        r#"{"DBInstances": {"a": 1}}"#,
        r#"{"DBInstances": [{"ReadReplicaDBInstanceIdentifiers": "r1"}]}"#,
    ] {
        let message = error_of(&format!(
            r#"{{"calls": [{DESCRIBE}, "response": {body}}}]}}"#
        ));
        assert!(
            message.starts_with("invalid DescribeDBInstances response: "),
            "{message}"
        );
    }
    let mut none = NoLiveClient;
    let result = analyze_instance_live(&mut none, "prod-db", "us-east-1");
    assert!(field(&result.document, &["error"])
        .as_str()
        .expect("text")
        .contains("no live AWS client"));
}

#[test]
fn error_text_from_aws_is_redacted() {
    let mut api = client(&format!(
        r#"{{"calls": [{DESCRIBE}, "error": {{"code": "AccessDenied",
            "message": "denied for AKIAIOSFODNN7EXAMPLE"}}}}]}}"#
    ));
    let result = analyze_instance_live(&mut api, "prod-db", "us-east-1");
    let text = field(&result.document, &["error"])
        .as_str()
        .expect("text")
        .to_string();
    assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"), "{text}");
}

#[test]
fn the_recorded_client_matches_service_region_and_request_and_validates_members() {
    let mut api = client(
        r#"{"calls": [{"service": "rds", "region": "eu-west-1", "operation": "DescribeDBInstances",
            "request": {"b": 1, "a": 2}, "response": {"ok": true}}]}"#,
    );
    let request = json(r#"{"a": 2, "b": 1}"#);
    assert!(api
        .call(Service::Rds, "eu-west-1", "DescribeDBInstances", &request)
        .is_ok());
    for (service, region, op) in [
        (Service::Rds, "us-east-1", "DescribeDBInstances"),
        (Service::SavingsPlans, "eu-west-1", "DescribeDBInstances"),
        (Service::Rds, "eu-west-1", "DescribeDBClusters"),
    ] {
        let error = api
            .call(service, region, op, &request)
            .expect_err("unmatched");
        assert!(matches!(error, AwsError::Connect { .. }), "{error:?}");
    }
    let Err(message) = RecordedClient::from_document(&json("{}")) else {
        panic!()
    };
    assert!(message.contains("\"calls\""));
    for (text, reason) in [
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "response": {}}]}"#,
            "rds or savingsplans",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "", "response": {}}]}"#,
            "non-empty string",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "request": null, "response": {}}]}"#,
            "\"request\" must be an object when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "region": 5, "response": {}}]}"#,
            "\"region\" must be a string when present (it is integer)",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "region": null, "response": {}}]}"#,
            "\"region\" must be a string when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X"}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "response": {}, "error": {}}]}"#,
            "exactly one of",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "error": {"kind": 1}}]}"#,
            "\"kind\" must be a string when present (it is integer)",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "error": {"kind": "boom"}}]}"#,
            "client, connect or no_credentials",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "error": {"code": null}}]}"#,
            "\"code\" must be a string when present (it is null)",
        ),
        (
            r#"{"calls": [{"service": "rds", "operation": "X", "error": {"message": []}}]}"#,
            "\"message\" must be a string when present (it is array)",
        ),
    ] {
        let Err(message) = RecordedClient::from_document(&json(text)) else {
            panic!("{text} must be rejected");
        };
        assert!(message.contains(reason), "{text}: {message}");
        assert!(message.starts_with("recorded call 0:"), "{message}");
    }
}

#[test]
fn no_live_client_sends_nothing_and_fails_every_call() {
    let mut none = NoLiveClient;
    let error = none
        .call(
            Service::SavingsPlans,
            "us-east-1",
            "DescribeSavingsPlansOfferingRates",
            &Json::Null,
        )
        .expect_err("no live client");
    assert!(error.to_string().contains("no live AWS client"), "{error}");
}
