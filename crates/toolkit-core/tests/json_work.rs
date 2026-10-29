//! Parser work bounds for #4497 (slice S0 of the #4413 mcp-proxy design,
//! requirement R-WORK, pre-review finding #4998).
//!
//! Everything here is deterministic: work is asserted through the parser's
//! probe counter, never through timing or load.

use toolkit_core::json::{
    parse_json, parse_json_with_limits, parse_json_with_stats, Json, JsonLimits, MAX_JSON_DEPTH,
};

/// `n` distinct short keys in a scrambled (non-sorted, non-hash-friendly)
/// order, so a sorted or insertion-friendly shortcut cannot pass.
fn scrambled_keys(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("k{}", i * 7919 % 100_003)).collect()
}

fn wide_object(keys: &[String]) -> String {
    let members: Vec<String> = keys.iter().map(|key| format!("\"{key}\":0")).collect();
    format!("{{{}}}", members.join(","))
}

fn object_keys(json: &Json) -> Vec<String> {
    match json {
        Json::Object(pairs) => pairs.iter().map(|(key, _)| key.clone()).collect(),
        other => panic!("expected an object, got {other:?}"),
    }
}

// ---- operation count (T-WORK) ----------------------------------------------

#[test]
fn wide_object_of_20000_keys_needs_at_most_two_probes_per_key() {
    let n = 20_000;
    let keys = scrambled_keys(n);
    let input = wide_object(&keys);
    assert!(input.len() < 1 << 20, "the fixture must stay under 1 MiB");
    let (result, stats) = parse_json_with_stats(&input);
    let json = result.expect("distinct keys parse");
    assert_eq!(object_keys(&json).len(), n);
    // The quadratic scan needs n(n-1)/2 = 199,990,000 comparisons.
    assert!(
        stats.key_probes <= 2 * n as u64,
        "{} key probes for {n} keys",
        stats.key_probes
    );
}

#[test]
fn probe_count_grows_linearly_with_the_number_of_keys() {
    let probes = |n: usize| {
        let (result, stats) = parse_json_with_stats(&wide_object(&scrambled_keys(n)));
        result.expect("distinct keys parse");
        stats.key_probes
    };
    let small = probes(2_000);
    let large = probes(20_000);
    // Ten times the keys, at most about ten times the probes (quadratic
    // growth would be a hundred times).
    assert!(
        large <= small * 11,
        "{small} probes for 2,000, {large} for 20,000"
    );
}

#[test]
fn wide_object_nested_at_depth_100_stays_linear() {
    let n = 20_000;
    let object = wide_object(&scrambled_keys(n));
    let input = format!("{}{object}{}", "[".repeat(100), "]".repeat(100));
    let (result, stats) = parse_json_with_stats(&input);
    result.expect("depth 100 is inside the default limit");
    assert!(
        stats.key_probes <= 2 * n as u64,
        "{} probes",
        stats.key_probes
    );
}

#[test]
fn many_small_objects_cost_a_constant_per_key() {
    // 5,000 objects of 8 members: the small-object linear scan is bounded by
    // a constant, so the total stays linear in the key count.
    let object = wide_object(&scrambled_keys(8));
    let input = format!("[{}]", vec![object; 5_000].join(","));
    let (result, stats) = parse_json_with_stats(&input);
    result.expect("parses");
    let keys = 5_000 * 8;
    assert!(
        stats.key_probes <= 4 * keys,
        "{} probes for {keys} keys",
        stats.key_probes
    );
}

// ---- duplicate detection ----------------------------------------------------

/// An object with `n` members, one per line, where member `at` repeats the
/// key of member `of`.
fn object_with_duplicate(n: usize, at: usize, of: usize) -> String {
    let mut lines = Vec::new();
    for index in 0..n {
        let key = if index == at { of } else { index };
        lines.push(format!("  \"key{key}\": {index}"));
    }
    format!("{{\n{}\n}}", lines.join(",\n"))
}

fn assert_duplicate_at(n: usize, at: usize, of: usize) {
    let error =
        parse_json(&object_with_duplicate(n, at, of)).expect_err("a repeated key must be rejected");
    assert_eq!(
        error.message, "duplicate object key",
        "n={n} at={at} of={of}"
    );
    assert_eq!(error.format, "JSON");
    // Line 1 is `{`; member `i` is on line `i + 2`; the error points at the
    // opening quote of the repeated key.
    assert_eq!(error.line, at + 2, "n={n} at={at} of={of}");
    assert_eq!(error.column, 3, "n={n} at={at} of={of}");
}

#[test]
fn duplicate_of_the_first_key_is_rejected_in_wide_and_small_objects() {
    for n in [3, 8, 9, 64, 2_000] {
        assert_duplicate_at(n, n - 1, 0);
    }
}

#[test]
fn duplicate_of_a_middle_key_is_rejected_in_wide_and_small_objects() {
    for n in [5, 8, 9, 65, 2_001] {
        assert_duplicate_at(n, n - 1, n / 2);
    }
}

#[test]
fn duplicate_of_the_last_key_is_rejected_in_wide_and_small_objects() {
    for n in [3, 8, 9, 64, 2_000] {
        assert_duplicate_at(n, n - 1, n - 2);
    }
}

#[test]
fn a_duplicate_in_the_middle_of_the_object_stops_the_parse_there() {
    // The repeated key sits mid-object, with valid members after it.
    for n in [9, 40, 1_000] {
        assert_duplicate_at(n, n / 2, 0);
        assert_duplicate_at(n, n / 2, n / 2 - 1);
    }
}

#[test]
fn a_duplicate_across_the_small_object_boundary_is_rejected() {
    // Members 1..=8 are distinct; the 9th and 10th repeat the first and the
    // eighth, which crosses the point where the index could be built.
    assert_duplicate_at(9, 8, 0);
    assert_duplicate_at(10, 9, 7);
    assert_duplicate_at(10, 8, 7);
}

#[test]
fn keys_that_differ_only_by_escape_form_are_still_duplicates() {
    let error = parse_json(&format!(
        "{{{}, \"a\": 1, \"\\u0061\": 2}}",
        (0..20)
            .map(|i| format!("\"x{i}\": 0"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .expect_err("the decoded keys are equal");
    assert_eq!(error.message, "duplicate object key");
}

#[test]
fn equal_keys_in_different_objects_are_not_duplicates() {
    let keys = scrambled_keys(50);
    let object = wide_object(&keys);
    let input = format!("{{\"outer\": {object}, \"inner\": [{object}, {object}]}}");
    assert!(parse_json(&input).is_ok());
}

#[test]
fn a_duplicate_in_a_nested_wide_object_is_rejected() {
    let mut keys = scrambled_keys(100);
    keys.push(keys[40].clone());
    let input = format!("[{{\"ok\": 1}}, {}]", wide_object(&keys));
    let error = parse_json(&input).expect_err("nested duplicate");
    assert_eq!(error.message, "duplicate object key");
}

// ---- ordering ---------------------------------------------------------------

#[test]
fn member_order_is_insertion_order_for_small_and_wide_objects() {
    for n in [0, 1, 7, 8, 9, 10, 100, 3_000] {
        let keys = scrambled_keys(n);
        let json = parse_json(&wide_object(&keys)).expect("parses");
        assert_eq!(object_keys(&json), keys, "n={n}");
    }
}

#[test]
fn values_stay_attached_to_their_keys_in_a_wide_object() {
    let members: Vec<String> = (0..500)
        .map(|i| format!("\"m{}\": {i}", (i * 37) % 500))
        .collect();
    let json = parse_json(&format!("{{{}}}", members.join(","))).expect("parses");
    let Json::Object(pairs) = &json else {
        panic!("object expected");
    };
    for (position, (key, value)) in pairs.iter().enumerate() {
        assert_eq!(*key, format!("m{}", (position * 37) % 500));
        assert_eq!(
            *value,
            Json::Int(i64::try_from(position).expect("small index"))
        );
    }
    assert_eq!(json.get("m0"), Some(&Json::Int(0)));
}

#[test]
fn wide_objects_round_trip_through_the_compact_writer() {
    let input = wide_object(&scrambled_keys(1_000));
    let json = parse_json(&input).expect("parses");
    assert_eq!(json.to_compact_string(), input);
}

// ---- limits -----------------------------------------------------------------

#[test]
fn default_limits_match_parse_json() {
    let limits = JsonLimits::default();
    assert_eq!(limits.max_depth, MAX_JSON_DEPTH);
    let input = wide_object(&scrambled_keys(100));
    assert_eq!(
        parse_json_with_limits(&input, &limits).expect("parses"),
        parse_json(&input).expect("parses")
    );
}

#[test]
fn parse_json_keeps_having_no_node_cap() {
    // 300,000 values is more than the proxy's 262,144 node cap; the default
    // entry point must keep accepting it.
    let input = format!("[{}0]", "0,".repeat(300_000));
    assert!(parse_json(&input).is_ok());
}

#[test]
fn parse_json_keeps_its_depth_limit() {
    let at_limit = format!("{}{}", "[".repeat(129), "]".repeat(129));
    assert!(parse_json(&at_limit).is_ok());
    let past_limit = format!("{}{}", "[".repeat(130), "]".repeat(130));
    let error = parse_json(&past_limit).expect_err("too deep");
    assert_eq!(error.message, "nesting is too deep");
}

fn limits(max_depth: usize, max_nodes: usize) -> JsonLimits {
    JsonLimits {
        max_depth,
        max_nodes,
    }
}

#[test]
fn node_cap_accepts_the_boundary_and_refuses_one_more_for_arrays() {
    // The array is one node and each element is one node: 1 + 3.
    let input = "[1, 2, 3]";
    assert!(parse_json_with_limits(input, &limits(MAX_JSON_DEPTH, 4)).is_ok());
    let error = parse_json_with_limits(input, &limits(MAX_JSON_DEPTH, 3)).expect_err("cap");
    assert_eq!(error.message, "too many JSON values");
    assert_eq!(error.format, "JSON");
}

#[test]
fn node_cap_counts_each_object_member_and_its_value() {
    // The object (1), two members (2) and two values (2) are five nodes.
    let input = "{\"a\": 1, \"b\": 2}";
    assert!(parse_json_with_limits(input, &limits(MAX_JSON_DEPTH, 5)).is_ok());
    let error = parse_json_with_limits(input, &limits(MAX_JSON_DEPTH, 4)).expect_err("cap");
    assert_eq!(error.message, "too many JSON values");
}

#[test]
fn node_cap_boundary_holds_for_a_wide_object() {
    let n = 1_000;
    let input = wide_object(&scrambled_keys(n));
    let nodes = 1 + 2 * n;
    assert!(parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, nodes)).is_ok());
    let error =
        parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, nodes - 1)).expect_err("cap");
    assert_eq!(error.message, "too many JSON values");
}

#[test]
fn node_cap_stops_the_parse_before_the_whole_input_is_read() {
    // A cap of 100 on a 20,000-key object must stop after about 100 nodes.
    let input = wide_object(&scrambled_keys(20_000));
    let error = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 100)).expect_err("cap");
    assert_eq!(error.message, "too many JSON values");
    assert_eq!(error.line, 1);
    assert!(
        error.column < 1_000,
        "the error is reported near the start, not at column {}",
        error.column
    );
}

#[test]
fn nodes_in_nested_containers_count_toward_the_same_cap() {
    // Outer array (1) + inner arrays (2) + scalars (4) = 7.
    let input = "[[1, 2], [3, 4]]";
    assert!(parse_json_with_limits(input, &limits(MAX_JSON_DEPTH, 7)).is_ok());
    assert!(parse_json_with_limits(input, &limits(MAX_JSON_DEPTH, 6)).is_err());
}

#[test]
fn depth_cap_accepts_the_boundary_and_refuses_one_more_level() {
    // `parse_json` accepts 129 levels at MAX_JSON_DEPTH = 128 (a value is
    // refused when its zero-based depth exceeds the cap); the same rule
    // applies to an injected cap.
    let nest = |levels: usize| format!("{}{}", "[".repeat(levels), "]".repeat(levels));
    assert!(parse_json_with_limits(&nest(4), &limits(3, usize::MAX)).is_ok());
    let error = parse_json_with_limits(&nest(5), &limits(3, usize::MAX)).expect_err("too deep");
    assert_eq!(error.message, "nesting is too deep");
    assert_eq!(error.format, "JSON");
}

#[test]
fn depth_cap_below_the_default_is_enforced_for_objects_too() {
    let input = "{\"a\": {\"b\": {\"c\": {\"d\": 1}}}}";
    assert!(parse_json_with_limits(input, &limits(4, usize::MAX)).is_ok());
    let error = parse_json_with_limits(input, &limits(2, usize::MAX)).expect_err("too deep");
    assert_eq!(error.message, "nesting is too deep");
}

#[test]
fn limits_do_not_change_the_duplicate_key_error() {
    let input = "{\n  \"a\": 1,\n  \"a\": 2\n}";
    let plain = parse_json(input).expect_err("duplicate");
    let limited = parse_json_with_limits(input, &limits(10, 100)).expect_err("duplicate");
    assert_eq!(plain, limited);
}

// #5026: an exhausted node budget must refuse a member at its opening quote,
// before the key is scanned, decoded or allocated.

const NODE_CAP_MESSAGE: &str = "too many JSON values";

#[test]
fn exhausted_budget_refuses_a_long_first_key_at_its_opening_quote() {
    // Only the root object fits; the member is over budget. The key is valid,
    // so the old order scanned and copied all of it and failed at the colon.
    let input = format!("{{\"{}\":0}}", "k".repeat(5_000));
    let error = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 1)).expect_err("cap");
    assert_eq!(error.message, NODE_CAP_MESSAGE);
    assert_eq!(error.format, "JSON");
    assert_eq!(error.line, 1);
    assert_eq!(error.column, 2, "the opening quote of the first member");
}

#[test]
fn exhausted_budget_is_reported_before_a_bad_escape_in_the_first_key() {
    let error = parse_json_with_limits("{\"\\q\":0}", &limits(MAX_JSON_DEPTH, 1)).expect_err("cap");
    assert_eq!(error.message, NODE_CAP_MESSAGE);
    assert_eq!((error.line, error.column), (1, 2));
}

#[test]
fn over_budget_later_member_stops_before_its_key_is_decoded() {
    // Root, first member and its value fit (3 nodes). The second member is
    // over budget; its key is moderately long and ends in an invalid escape,
    // which would be the error if the key were decoded first.
    let input = format!("{{\"a\":1,\"{}\\q\":2}}", "x".repeat(64));
    let error = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 3)).expect_err("cap");
    assert_eq!(error.message, NODE_CAP_MESSAGE);
    assert_eq!(error.line, 1);
    assert_eq!(error.column, 8, "the opening quote of the second member");
}

#[test]
fn over_budget_later_member_is_refused_after_a_newline_at_its_quote() {
    let input = format!("{{\n  \"a\": 1,\n  \"{}\\q\": 2\n}}", "y".repeat(64));
    let error = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 3)).expect_err("cap");
    assert_eq!(error.message, NODE_CAP_MESSAGE);
    assert_eq!((error.line, error.column), (3, 3));
}

#[test]
fn within_budget_bad_escape_in_a_key_keeps_its_decoding_error() {
    let input = format!("{{\"{}\\q\":0}}", "x".repeat(64));
    let capped = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 2)).expect_err("escape");
    let plain = parse_json(&input).expect_err("escape");
    assert_eq!(capped.message, "invalid escape sequence");
    assert_eq!(capped, plain);
}

#[test]
fn default_parse_reports_a_bad_key_escape_where_it_always_did() {
    let error = parse_json("{\"\\q\":0}").expect_err("escape");
    assert_eq!(error.message, "invalid escape sequence");
}

#[test]
fn exhausted_budget_refuses_an_array_element_before_reading_it() {
    // The array uses the whole budget; the long element is refused at its
    // first byte, and a bad escape inside it is never reached.
    let input = format!("[\"{}\\q\"]", "z".repeat(5_000));
    let error = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 1)).expect_err("cap");
    assert_eq!(error.message, NODE_CAP_MESSAGE);
    assert_eq!((error.line, error.column), (1, 2));
}

#[test]
fn over_budget_later_array_element_is_refused_at_its_start() {
    let input = format!("[1, \"{}\\q\"]", "z".repeat(64));
    let error = parse_json_with_limits(&input, &limits(MAX_JSON_DEPTH, 2)).expect_err("cap");
    assert_eq!(error.message, NODE_CAP_MESSAGE);
    assert_eq!((error.line, error.column), (1, 5));
}
