//! Offline tests for the cost calculator port. Every numeric expectation is
//! derived by hand from the Python formulas in `calculate_costs.py` (see the
//! comments), never from running the port.

use std::collections::HashMap;

use toolkit_core::Json;

use super::cost::{
    build_entity_attr_sizes, calc_gsi_amplification_wru, calc_pattern_capacity,
    calculate_and_report, cap_items_per_request, gsi_write_wru, padded_table, pattern_monthly_cost,
    resolve_retention_days, resolve_write_action, rru_for_batch_read, rru_for_item, rru_for_query,
    storage_cost, validate_vector_model, vector_item_bytes, vector_search_bytes_per_call,
    vector_search_bytes_spend_gate_upper, vector_write_bytes_per_call, wru_for_batch_write,
    wru_for_item,
};
use super::pyfmt;
use super::test_support::{assert_close, json};

fn capacity(ap: &str) -> super::cost::Capacity {
    calc_pattern_capacity(&json(ap)).expect("capacity")
}

// ---------------------------------------------------------------------------
// Capacity formulas.
// ---------------------------------------------------------------------------

#[test]
fn write_units_round_up_per_kilobyte_with_a_one_unit_minimum() {
    assert_eq!(
        wru_for_item(0.0),
        1.0,
        "non-positive sizes cost the minimum"
    );
    assert_eq!(wru_for_item(-5.0), 1.0);
    assert_eq!(wru_for_item(1.0), 1.0);
    assert_eq!(wru_for_item(1024.0), 1.0, "exactly 1 KB is 1 WRU");
    assert_eq!(wru_for_item(1025.0), 2.0, "one byte over rounds up");
    assert_eq!(wru_for_item(3000.0), 3.0);
}

#[test]
fn read_units_round_to_4kb_and_halve_for_eventual_consistency() {
    assert_eq!(rru_for_item(1024.0, false), 0.5);
    assert_eq!(rru_for_item(1024.0, true), 1.0);
    assert_eq!(
        rru_for_item(4096.0, true),
        1.0,
        "exactly 4 KB is 1 strong RRU"
    );
    assert_eq!(rru_for_item(4097.0, true), 2.0);
    assert_eq!(rru_for_item(4097.0, false), 1.0);
    // A non-existent GetItem (size 0) still consumes the minimum.
    assert_eq!(rru_for_item(0.0, false), 0.5);
    assert_eq!(rru_for_item(0.0, true), 1.0);
}

#[test]
fn query_aggregates_bytes_before_rounding() {
    // 5 x 1 KB = 5120 B -> ceil(5120/4096) = 2 strong, 1.0 eventual; a
    // per-item rounding would have charged 5 x 0.5 = 2.5.
    assert_eq!(rru_for_query(5.0 * 1024.0, false), 1.0);
    assert_eq!(rru_for_query(5.0 * 1024.0, true), 2.0);
    assert_eq!(rru_for_batch_read(&[1024.0; 5], false), 2.5);
    // 10 x 512 B = 5120 B as well.
    assert_eq!(rru_for_query(10.0 * 512.0, false), 1.0);
}

#[test]
fn batch_operations_round_each_item_then_sum() {
    assert_eq!(rru_for_batch_read(&[1024.0, 5000.0], false), 1.5);
    assert_eq!(rru_for_batch_read(&[], false), 0.0);
    assert_eq!(wru_for_batch_write(&[100.0, 2000.0, 1024.0]), 4.0);
    assert_eq!(wru_for_batch_write(&[]), 0.0);
}

#[test]
fn gsi_writes_are_sized_by_the_projection_not_the_base_item() {
    assert_eq!(gsi_write_wru(4096.0, "ALL"), 4.0);
    // KEYS_ONLY: int(4096 * 0.1) = 409 -> 1 WRU.
    assert_eq!(gsi_write_wru(4096.0, "KEYS_ONLY"), 1.0);
    // INCLUDE: int(4096 * 0.3) = 1228 -> 2 WRU (the 0.3 default ratio exceeds
    // 1 KB on a 4 KB item).
    assert_eq!(gsi_write_wru(4096.0, "INCLUDE"), 2.0);
    assert_eq!(
        gsi_write_wru(4096.0, "SOMETHING_ELSE"),
        4.0,
        "unknown = ALL"
    );
    assert_eq!(
        gsi_write_wru(1.0, "KEYS_ONLY"),
        1.0,
        "int(0.1) = 0 clamps to 1"
    );
}

#[test]
fn pattern_capacity_matches_each_operation_rule() {
    // GetItem 1 KB eventual / strong.
    let get = capacity(r#"{"operation":"GetItem","estimated_item_size_bytes":1024}"#);
    assert_eq!((get.rcus, get.wcus, get.strong), (0.5, 0.0, false));
    let strong = capacity(
        r#"{"operation":"GetItem","estimated_item_size_bytes":1024,"consistency":"strong"}"#,
    );
    assert_eq!((strong.rcus, strong.strong), (1.0, true));
    // Query: 10 x 1 KB = 10240 B -> 3 strong -> 1.5 eventual; notes.
    let query = capacity(
        r#"{"operation":"Query","estimated_item_size_bytes":1024,"items_per_request":10,"filter":"x","projection":["a"]}"#,
    );
    assert_eq!(query.rcus, 1.5);
    assert_eq!(
        query.notes,
        vec![
            "FilterExpression does not reduce cost.",
            "ProjectionExpression does not reduce cost."
        ]
    );
    let scan =
        capacity(r#"{"operation":"Scan","estimated_item_size_bytes":1024,"items_per_request":10}"#);
    assert_eq!(scan.rcus, 1.5);
    assert_eq!(
        scan.notes,
        vec!["Scan reads every evaluated item regardless of filter."]
    );
    // BatchGetItem: explicit sizes beat the repeated default.
    let batch_get = capacity(
        r#"{"operation":"BatchGetItem","estimated_item_size_bytes":1024,"items_per_request":3,"item_sizes":[1024,5000]}"#,
    );
    assert_eq!(batch_get.rcus, 1.5);
    let batch_default = capacity(
        r#"{"operation":"BatchGetItem","estimated_item_size_bytes":1024,"items_per_request":3}"#,
    );
    assert_eq!(batch_default.rcus, 1.5, "3 x 0.5");
    // TransactGetItems: always strong per item, then 2x.
    let txn_get = capacity(
        r#"{"operation":"TransactGetItems","estimated_item_size_bytes":1500,"items_per_request":2}"#,
    );
    assert_eq!(txn_get.rcus, 4.0);
    assert!(txn_get.transactional);
    assert_eq!(txn_get.notes, vec!["Transactional reads consume 2× RRUs."]);
    // Writes.
    assert_eq!(
        capacity(r#"{"operation":"PutItem","estimated_item_size_bytes":500,"previous_item_size_bytes":3000}"#).wcus,
        3.0,
        "a replacement is sized on the LARGER of before/after"
    );
    assert_eq!(
        capacity(r#"{"operation":"PutItem","estimated_item_size_bytes":1025}"#).wcus,
        2.0
    );
    assert_eq!(
        capacity(r#"{"operation":"UpdateItem","estimated_item_size_bytes":2048}"#).wcus,
        2.0,
        "update without a previous size is sized on the item itself"
    );
    assert_eq!(
        capacity(r#"{"operation":"UpdateItem","estimated_item_size_bytes":100,"previous_item_size_bytes":5000}"#).wcus,
        5.0
    );
    assert_eq!(
        capacity(r#"{"operation":"DeleteItem"}"#).wcus,
        1.0,
        "default 1024 B"
    );
    assert_eq!(
        capacity(r#"{"operation":"BatchWriteItem","item_sizes":[100,2000,1024]}"#).wcus,
        4.0
    );
    let txn_write = capacity(
        r#"{"operation":"TransactWriteItems","estimated_item_size_bytes":1500,"items_per_request":2}"#,
    );
    assert_eq!(txn_write.wcus, 8.0, "(2 + 2) x 2");
    assert!(txn_write.transactional);
}

#[test]
fn notes_cover_non_existent_reads_and_conditional_failures() {
    let missing = capacity(r#"{"operation":"GetItem","non_existent_rate":0.1}"#);
    assert_eq!(
        missing.notes,
        vec!["Non-existent items still consume the minimum RRU."]
    );
    let conditional = capacity(r#"{"operation":"PutItem","conditional_fail_rate":0.25}"#);
    assert_eq!(
        conditional.notes,
        vec!["Conditional failures (rate=0.25) still consume the same WRUs."]
    );
    let whole = capacity(r#"{"operation":"PutItem","conditional_fail_rate":1}"#);
    assert_eq!(
        whole.notes,
        vec!["Conditional failures (rate=1) still consume the same WRUs."]
    );
    // Reads and a zero rate add no note.
    assert!(
        capacity(r#"{"operation":"GetItem","conditional_fail_rate":0.5}"#)
            .notes
            .is_empty()
    );
    assert!(
        capacity(r#"{"operation":"PutItem","conditional_fail_rate":0}"#)
            .notes
            .is_empty()
    );
}

#[test]
fn malformed_patterns_are_model_errors_not_panics() {
    for (source, expected) in [
        (r#"{"estimated_item_size_bytes":1}"#, "missing `operation`"),
        (r#"{"operation":5}"#, "`operation` must be a string"),
        (
            r#"{"operation":"GetItem","estimated_item_size_bytes":"big"}"#,
            "`estimated_item_size_bytes` must be a number, got 'big'",
        ),
        (
            r#"{"operation":"GetItem","items_per_request":null}"#,
            "`items_per_request` must be a number, got None",
        ),
        (
            r#"{"operation":"BatchGetItem","items_per_request":2.5}"#,
            "`items_per_request` must be an integer, got 2.5",
        ),
        (
            r#"{"operation":"BatchGetItem","item_sizes":"abc"}"#,
            "`item_sizes` must be a list of numbers, got 'abc'",
        ),
        (
            r#"{"operation":"BatchWriteItem","item_sizes":[1,"x"]}"#,
            "`item_sizes` entries must be numbers, got 'x'",
        ),
    ] {
        let error = calc_pattern_capacity(&json(source)).expect_err(source);
        assert_eq!(error.code(), "dynamodb_model_invalid", "{source}");
        assert_eq!(error.exit_code(), 1);
        assert!(error.message().contains(expected), "{source}: {error}");
    }
}

// ---------------------------------------------------------------------------
// Page-size cap.
// ---------------------------------------------------------------------------

fn sizes_for(model: &str) -> super::cost::EntitySizes {
    let model = json(model);
    let Some(Json::Array(tables)) = model.get("tables") else {
        panic!("fixture has tables");
    };
    build_entity_attr_sizes(tables).expect("sizes")
}

#[test]
fn query_items_are_capped_at_the_900kb_page_by_projected_size() {
    let sizes = sizes_for(
        r#"{"tables":[{"table_name":"T","entities":[{"entity_name":"E","attributes":[
            {"name":"a","type":"S"},{"name":"b","type":"N"},{"name":"c"}]}]}]}"#,
    );
    // S = 100 + 10, N = 8 + 10, default type S.
    assert_eq!(
        sizes[0].1,
        vec![
            ("a".to_string(), 110),
            ("b".to_string(), 18),
            ("c".to_string(), 110)
        ]
    );

    let cap = |ap: &str| cap_items_per_request(&json(ap), &sizes).expect("cap");
    // projected a+b = 128 B -> 921600 // 128 = 7200 items per page.
    assert_eq!(
        cap(r#"{"operation":"Query","items_per_request":10000,"projection":["a","b"]}"#),
        7200.0
    );
    assert_eq!(
        cap(r#"{"operation":"Scan","items_per_request":100,"projection":["a","b"]}"#),
        100.0
    );
    // An unknown attribute falls back to 100 B -> 9216 per page.
    assert_eq!(
        cap(r#"{"operation":"Query","items_per_request":10000,"projection":["zz"]}"#),
        9216.0
    );
    // Non-string projection entries also use the fallback.
    assert_eq!(
        cap(r#"{"operation":"Query","items_per_request":10000,"projection":[7]}"#),
        9216.0
    );
    // No projection, or another operation: unchanged.
    assert_eq!(
        cap(r#"{"operation":"Query","items_per_request":10000}"#),
        10000.0
    );
    assert_eq!(
        cap(r#"{"operation":"GetItem","items_per_request":10000,"projection":["a"]}"#),
        10000.0
    );
    // Huge projections still return at least one item per page.
    let wide = sizes_for(
        r#"{"tables":[{"table_name":"T","entities":[{"entity_name":"E","attributes":[{"name":"a","type":"S"}]}]}]}"#,
    );
    let many = format!(
        r#"{{"operation":"Query","items_per_request":50,"projection":[{}]}}"#,
        vec![r#""a""#; 9000].join(",")
    );
    assert_eq!(
        cap_items_per_request(&json(&many), &wide).expect("cap"),
        1.0
    );
}

#[test]
fn the_first_entity_declaring_a_projected_attribute_wins() {
    let sizes = sizes_for(
        r#"{"tables":[{"table_name":"T","entities":[
            {"entity_name":"E1","attributes":[{"name":"a","type":"S"}]},
            {"entity_name":"E2","attributes":[{"name":"a","type":"N"}]}]}]}"#,
    );
    // 110 B (E1) rather than 18 B (E2): 921600 // 110 = 8378.
    let capped = cap_items_per_request(
        &json(r#"{"operation":"Query","items_per_request":100000,"projection":["a"]}"#),
        &sizes,
    )
    .expect("cap");
    assert_eq!(capped, 8378.0);
}

#[test]
fn entity_size_table_rejects_nameless_entities_and_attributes() {
    let tables = |source: &str| match json(source) {
        Json::Array(items) => items,
        other => panic!("fixture must be a list, got {other:?}"),
    };
    let error = build_entity_attr_sizes(&tables(
        r#"[{"table_name":"T","entities":[{"attributes":[]}]}]"#,
    ))
    .expect_err("entity without a name");
    assert!(error.message().contains("`entity_name`"), "{error}");
    let error = build_entity_attr_sizes(&tables(
        r#"[{"table_name":"T","entities":[{"entity_name":"E","attributes":[{"type":"S"}]}]}]"#,
    ))
    .expect_err("attribute without a name");
    assert!(
        error.message().contains("missing its string `name`"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Monthly cost.
// ---------------------------------------------------------------------------

fn monthly(ap: &str, table: Option<&str>) -> super::cost::PatternCost {
    let table = table.map(json);
    pattern_monthly_cost(&json(ap), table.as_ref(), None).expect("monthly cost")
}

#[test]
fn monthly_cost_uses_the_published_unit_prices() {
    // GetItem 1 KB eventual = 0.5 RRU; 0.5 x $0.125/M x 100 rps x 2,592,000 s.
    let get = monthly(
        r#"{"operation":"GetItem","estimated_item_size_bytes":1024,"peak_rps":100}"#,
        None,
    );
    assert_close(get.base_cost, 16.2, 1e-9);
    assert_close(get.total_cost, 16.2, 1e-9);
    assert_eq!(
        get.expected_cost, get.total_cost,
        "avg_rps defaults to the peak"
    );
    // Strong doubles it.
    let strong = monthly(
        r#"{"operation":"GetItem","estimated_item_size_bytes":1024,"peak_rps":100,"consistency":"strong"}"#,
        None,
    );
    assert_close(strong.base_cost, 32.4, 1e-9);
    // PutItem 3000 B = 3 WRU; 3 x $0.625/M x 10 rps x 2,592,000 s.
    let put = monthly(
        r#"{"operation":"PutItem","estimated_item_size_bytes":3000,"peak_rps":10}"#,
        None,
    );
    assert_close(put.base_cost, 48.6, 1e-9);
    // A pattern with no traffic costs nothing.
    assert_eq!(monthly(r#"{"operation":"GetItem"}"#, None).total_cost, 0.0);
}

#[test]
fn average_volume_and_conditional_failures_scale_the_cost() {
    let put = monthly(
        r#"{"operation":"PutItem","estimated_item_size_bytes":1024,"peak_rps":10,"avg_rps":5}"#,
        None,
    );
    assert_close(put.total_cost, 16.2, 1e-9);
    assert_close(put.expected_cost, 8.1, 1e-9);
    // conditional_fail_rate 0.5 multiplies a write by 1.5; reads are exempt.
    let conditional = monthly(
        r#"{"operation":"PutItem","estimated_item_size_bytes":1024,"peak_rps":10,"conditional_fail_rate":0.5}"#,
        None,
    );
    assert_close(conditional.total_cost, 24.3, 1e-9);
    let read = monthly(
        r#"{"operation":"GetItem","estimated_item_size_bytes":1024,"peak_rps":100,"conditional_fail_rate":0.5}"#,
        None,
    );
    assert_close(read.total_cost, 16.2, 1e-9);
}

#[test]
fn query_cost_uses_the_page_capped_item_count() {
    let sizes = sizes_for(
        r#"{"tables":[{"table_name":"T","entities":[{"entity_name":"E","attributes":[
            {"name":"a","type":"S"},{"name":"b","type":"N"}]}]}]}"#,
    );
    let ap = json(
        r#"{"operation":"Query","estimated_item_size_bytes":1,"items_per_request":10000,"projection":["a","b"],"peak_rps":1}"#,
    );
    let cost = pattern_monthly_cost(&ap, None, Some(&sizes)).expect("cost");
    // Capped to 7200 items x 1 B = 7200 B -> ceil(7200/4096) = 2 -> 1.0 RRU.
    assert_eq!(cost.cap.rcus, 1.0);
    assert_eq!(cost.ap.get("items_per_request"), Some(&Json::Int(7200)));
    // The caller's pattern is untouched.
    assert_eq!(ap.get("items_per_request"), Some(&Json::Int(10000)));
}

// ---------------------------------------------------------------------------
// GSI amplification.
// ---------------------------------------------------------------------------

const SINGLE_TABLE: &str = r#"{
    "table_name": "T",
    "key_schema": {"partition_key": "pk", "sort_key": "sk"},
    "entities": [
        {"entity_name": "A", "attributes": [{"name": "pk"}, {"name": "sk"}, {"name": "gk"}, {"name": "status"}]},
        {"entity_name": "B", "attributes": [{"name": "pk"}, {"name": "sk"}]}
    ],
    "gsis": [
        {"index_name": "ByG", "partition_key": "gk", "projection": {"type": "ALL"}},
        {"index_name": "ByStatus", "partition_key": "status", "projection": {"type": "KEYS_ONLY"}},
        {"index_name": "Inc", "partition_key": "gk", "projection": {"type": "INCLUDE", "attributes": ["title"]}}
    ]
}"#;

fn amplification(ap: &str, table: &str) -> (f64, Vec<String>) {
    calc_gsi_amplification_wru(&json(ap), &json(table)).expect("amplification")
}

#[test]
fn writes_amplify_only_to_gsis_whose_key_the_entity_carries() {
    // Entity A carries gk and status: ByG (ALL, 2 KB item = 2 WRU),
    // ByStatus (KEYS_ONLY, int(204.8) = 204 -> 1 WRU) and Inc (INCLUDE,
    // int(614.4) = 614 -> 1 WRU) each fire once.
    let (total, details) = amplification(
        r#"{"operation":"PutItem","estimated_item_size_bytes":2048,"entities_written":["A"]}"#,
        SINGLE_TABLE,
    );
    assert_eq!(total, 2.0 + 1.0 + 1.0);
    assert_eq!(
        details,
        vec![
            "ByG (×1 index write)",
            "ByStatus (×1 index write)",
            "Inc (×1 index write)"
        ]
    );
    // Entity B carries no GSI key: nothing amplifies.
    let (total, details) = amplification(
        r#"{"operation":"PutItem","estimated_item_size_bytes":2048,"entities_written":["B"]}"#,
        SINGLE_TABLE,
    );
    assert_eq!(total, 0.0);
    assert_eq!(
        details,
        vec!["no GSI amplification — written item(s) carry no GSI key"]
    );
    // An undeclared entity name amplifies nowhere (visibly low, not full fan-out).
    let (total, _) = amplification(
        r#"{"operation":"PutItem","estimated_item_size_bytes":2048,"entities_written":["Nope"]}"#,
        SINGLE_TABLE,
    );
    assert_eq!(total, 0.0);
    // Counts repeat an entity; object form is accepted.
    let (total, details) = amplification(
        r#"{"operation":"TransactWriteItems","estimated_item_size_bytes":2048,"entities_written":[{"entity":"A","count":3},"B"]}"#,
        SINGLE_TABLE,
    );
    assert_eq!(total, 3.0 * 4.0);
    assert_eq!(details[0], "ByG (×3 index writes)");
}

#[test]
fn update_item_gates_gsi_writes_on_the_attributes_written() {
    let ap = |written: &str| {
        format!(
            r#"{{"operation":"UpdateItem","estimated_item_size_bytes":4096,"entities_written":["A"],"attributes_written":{written}}}"#
        )
    };
    // Changing the GSI key deletes and reinserts the index entry: 2 writes.
    // ByStatus is KEYS_ONLY (int(409.6) = 409 -> 1 WRU) x 2 = 2; ByG (ALL)
    // re-projects the 4 KB item once = 4; Inc projects only `title`.
    let (total, details) = amplification(&ap(r#"["status"]"#), SINGLE_TABLE);
    assert_eq!(total, 6.0);
    assert_eq!(
        details,
        vec!["ByG (×1 index write)", "ByStatus (×2 index writes)"]
    );
    // A non-key attribute: ALL re-projects the item (4 WRU); INCLUDE only if
    // the attribute is projected; KEYS_ONLY never.
    let (total, details) = amplification(&ap(r#"["body"]"#), SINGLE_TABLE);
    assert_eq!(total, 4.0);
    assert_eq!(details, vec!["ByG (×1 index write)"]);
    let (total, details) = amplification(&ap(r#"["title"]"#), SINGLE_TABLE);
    // ByG (ALL) 4 WRU + Inc (INCLUDE int(1228.8) = 1228 -> 2 WRU).
    assert_eq!(total, 6.0);
    assert_eq!(
        details,
        vec!["ByG (×1 index write)", "Inc (×1 index write)"]
    );
    // An empty write set touches nothing a KEYS_ONLY/INCLUDE index projects.
    let (total, _) = amplification(&ap("[]"), SINGLE_TABLE);
    assert_eq!(total, 4.0, "only the ALL index re-projects");
    // Member entity but nothing projected: the projection gate is named.
    let keys_only = r#"{"table_name":"T","entities":[{"entity_name":"A","attributes":[{"name":"pk"},{"name":"gk"}]}],
        "gsis":[{"index_name":"K","partition_key":"gk","projection":{"type":"KEYS_ONLY"}}]}"#;
    let (total, details) = amplification(
        r#"{"operation":"UpdateItem","estimated_item_size_bytes":1024,"attributes_written":["body"]}"#,
        keys_only,
    );
    assert_eq!(total, 0.0);
    assert_eq!(
        details,
        vec!["no GSI amplification — UpdateItem touched no attribute projected into K (projection gate)"]
    );
}

#[test]
fn a_single_entity_table_needs_no_entities_written() {
    let single = r#"{"table_name":"T","entities":[{"entity_name":"A","attributes":[{"name":"pk"},{"name":"gk"}]}],
        "gsis":[{"index_name":"G","partition_key":"gk"}]}"#;
    // Batch of 3 items x 2 WRU each (a missing projection means ALL).
    let (total, details) = amplification(
        r#"{"operation":"BatchWriteItem","estimated_item_size_bytes":2048,"items_per_request":3}"#,
        single,
    );
    assert_eq!(total, 6.0);
    assert_eq!(details, vec!["G (×3 index writes)"]);
}

#[test]
fn undeterminable_membership_is_charged_conservatively_and_flagged() {
    let thin = r#"{"table_name":"T","entities":[{"entity_name":"A","attributes":[{"name":"pk"}]}],
        "gsis":[{"index_name":"G","partition_key":"declared_nowhere"}]}"#;
    let (total, details) = amplification(
        r#"{"operation":"PutItem","estimated_item_size_bytes":2048}"#,
        thin,
    );
    assert_eq!(total, 2.0);
    assert_eq!(
        details,
        vec!["G (×1 index write, membership unverified — GSI key not declared on any entity)"]
    );
    // A multi-entity table without entities_written is an upper bound.
    let (total, details) = amplification(
        r#"{"operation":"PutItem","table":"T","estimated_item_size_bytes":2048}"#,
        SINGLE_TABLE,
    );
    assert_eq!(total, 2.0 + 1.0 + 1.0);
    assert!(
        details[0].starts_with("UPPER BOUND — 'T' has multiple entities"),
        "{details:?}"
    );
    // Batch writes multiply the bound by items_per_request.
    let (total, _) = amplification(
        r#"{"operation":"BatchWriteItem","table":"T","estimated_item_size_bytes":2048,"items_per_request":5}"#,
        SINGLE_TABLE,
    );
    assert_eq!(total, 5.0 * 4.0);
}

#[test]
fn reads_and_tables_without_gsis_never_amplify() {
    assert_eq!(
        amplification(r#"{"operation":"GetItem"}"#, SINGLE_TABLE),
        (0.0, Vec::new())
    );
    assert_eq!(
        amplification(
            r#"{"operation":"PutItem"}"#,
            r#"{"table_name":"T","entities":[{"entity_name":"A"}]}"#
        ),
        (0.0, Vec::new())
    );
    let error = calc_gsi_amplification_wru(
        &json(r#"{"operation":"PutItem","entities_written":"A"}"#),
        &json(SINGLE_TABLE),
    )
    .expect_err("entities_written must be a list");
    assert!(
        error
            .message()
            .contains("`entities_written` must be a list"),
        "{error}"
    );
}

#[test]
fn composite_gsis_need_both_keys_and_table_keys_count_as_carried() {
    let table = r#"{"table_name":"T","key_schema":{"partition_key":"pk","sort_key":"sk"},
        "entities":[{"entity_name":"HasBoth","attributes":[{"name":"pk"},{"name":"g"},{"name":"gs"}]},
                    {"entity_name":"PkOnly","attributes":[{"name":"pk"},{"name":"g"}]}],
        "gsis":[{"index_name":"Comp","partition_key":"g","sort_key":"gs"},
                {"index_name":"OnTableKey","partition_key":"sk"}]}"#;
    let put = |entity: &str| {
        amplification(
            &format!(
                r#"{{"operation":"PutItem","estimated_item_size_bytes":1024,"entities_written":["{entity}"]}}"#
            ),
            table,
        )
    };
    // Both GSI keys present -> Comp fires; sk is a table key so OnTableKey
    // fires for every entity.
    assert_eq!(
        put("HasBoth").1,
        vec!["Comp (×1 index write)", "OnTableKey (×1 index write)"]
    );
    // PK without the GSI sort key is NOT indexed by the composite GSI.
    assert_eq!(put("PkOnly").1, vec!["OnTableKey (×1 index write)"]);
}

// ---------------------------------------------------------------------------
// Vector indexes.
// ---------------------------------------------------------------------------

fn vector_table(entity_size: u32, indexes: &str) -> Json {
    json(&format!(
        r#"{{"table_name":"V","entities":[{{"entity_name":"E","estimated_item_size_bytes":{entity_size},
            "estimated_item_count":1000,"attributes":[{{"name":"title","type":"S"}}]}}],
            "vector_indexes":{indexes}}}"#
    ))
}

fn vector_index(dimensions: u32, projection: &str) -> String {
    format!(
        r#"{{"index_name":"I","vector_attribute":"Emb","dimensions":{dimensions},"distance_function":"COSINE","projection":{projection}}}"#
    )
}

fn first_index(table: &Json) -> &Json {
    match table.get("vector_indexes") {
        Some(Json::Array(items)) => &items[0],
        _ => panic!("fixture has vector indexes"),
    }
}

#[test]
fn vector_item_bytes_follow_the_projection() {
    // KEYS_ONLY: dims x 4 + 35 key overhead.
    let table = vector_table(
        1000,
        &format!("[{}]", vector_index(1024, r#"{"type":"KEYS_ONLY"}"#)),
    );
    assert_eq!(
        vector_item_bytes(first_index(&table), &table).expect("bytes"),
        4131
    );
    // The bare-string shorthand is accepted, case-insensitively.
    let table = vector_table(1000, &format!("[{}]", vector_index(1024, r#""keys_only""#)));
    assert_eq!(
        vector_item_bytes(first_index(&table), &table).expect("bytes"),
        4131
    );
    // INCLUDE: + declared attribute size (S = 100) or the 100 B default.
    let table = vector_table(
        1000,
        &format!(
            "[{}]",
            vector_index(
                1024,
                r#"{"type":"INCLUDE","attributes":["title","undeclared"]}"#
            )
        ),
    );
    assert_eq!(
        vector_item_bytes(first_index(&table), &table).expect("bytes"),
        4131 + 200
    );
    // ALL: + the mean declared entity size (the vector is excluded from it).
    let table = vector_table(
        1000,
        &format!("[{}]", vector_index(1024, r#"{"type":"ALL"}"#)),
    );
    assert_eq!(
        vector_item_bytes(first_index(&table), &table).expect("bytes"),
        4131 + 1000
    );
    // A second vector attribute on the table is copied at 5.5 B/number.
    let two = format!(
        "[{}, {}]",
        vector_index(1024, r#"{"type":"ALL"}"#),
        r#"{"index_name":"J","vector_attribute":"Other","dimensions":128,"distance_function":"COSINE"}"#
    );
    let table = vector_table(1000, &two);
    // 4096 + 35 + 1000 + 128 x 5.5 = 5835.
    assert_eq!(
        vector_item_bytes(first_index(&table), &table).expect("bytes"),
        5835
    );
}

#[test]
fn vector_writes_apply_the_one_kilobyte_floor_once_per_request() {
    let table = vector_table(
        1000,
        &format!("[{}]", vector_index(128, r#"{"type":"KEYS_ONLY"}"#)),
    );
    // 128 x 4 + 35 = 547 B, below the 1 KB floor.
    let (bytes, details) =
        vector_write_bytes_per_call(&json(r#"{"operation":"PutItem"}"#), &table).expect("write");
    assert_eq!(bytes, 1024.0);
    assert_eq!(
        details,
        vec!["I: 547 B (upper bound: attributes_written absent)"]
    );
    // Naming the vector attribute removes the upper-bound flag.
    let (bytes, details) = vector_write_bytes_per_call(
        &json(r#"{"operation":"PutItem","attributes_written":["Emb"]}"#),
        &table,
    )
    .expect("write");
    assert_eq!(bytes, 1024.0);
    assert_eq!(details, vec!["I: 547 B"]);
    // A write that does not touch the vector attribute pays nothing.
    let (bytes, details) = vector_write_bytes_per_call(
        &json(r#"{"operation":"PutItem","attributes_written":["title"]}"#),
        &table,
    )
    .expect("write");
    assert_eq!(bytes, 0.0);
    assert_eq!(details, vec!["I: not touched by this write"]);
    // Large vectors are not floored: 1024 x 4 + 35.
    let big = vector_table(
        1000,
        &format!("[{}]", vector_index(1024, r#"{"type":"KEYS_ONLY"}"#)),
    );
    let (bytes, _) =
        vector_write_bytes_per_call(&json(r#"{"operation":"PutItem"}"#), &big).expect("write");
    assert_eq!(bytes, 4131.0);
}

#[test]
fn standard_ia_charges_125_percent_on_vector_writes() {
    let build = |class: &str| {
        let mut table = vector_table(
            1000,
            &format!("[{}]", vector_index(1024, r#"{"type":"KEYS_ONLY"}"#)),
        );
        if let Json::Object(pairs) = &mut table {
            pairs.push(("table_class".to_string(), Json::string(class)));
        }
        table
    };
    let ap = json(r#"{"operation":"PutItem","peak_rps":10,"attributes_written":["Emb"]}"#);
    let standard = pattern_monthly_cost(&ap, Some(&build("STANDARD")), None).expect("cost");
    // 4131 B x $0.52/GB x 10 rps x 2,592,000 s.
    assert_close(
        standard.vector_write_cost,
        4131.0 * 0.52e-9 * 10.0 * 2_592_000.0,
        1e-6,
    );
    for class in ["STANDARD_INFREQUENT_ACCESS", "standard_ia", "IA"] {
        let infrequent = pattern_monthly_cost(&ap, Some(&build(class)), None).expect("cost");
        assert_close(
            infrequent.vector_write_cost,
            standard.vector_write_cost * 1.25,
            1e-6,
        );
    }
}

#[test]
fn search_reports_returned_bytes_and_never_a_dollar_price() {
    let all = vector_table(
        13048,
        &format!("[{}]", vector_index(1024, r#"{"type":"ALL"}"#)),
    );
    let keys = vector_table(
        13048,
        &format!("[{}]", vector_index(1024, r#"{"type":"KEYS_ONLY"}"#)),
    );
    let ap = |top_k: u32| {
        json(&format!(
            r#"{{"operation":"SearchVectors","top_k":{top_k}}}"#
        ))
    };
    let (bytes, basis) =
        vector_search_bytes_per_call(&ap(100), first_index(&keys), &keys).expect("bytes");
    assert_eq!(bytes, 8900.0);
    assert_eq!(
        basis,
        "TopK 100 x 89 B/result — KEYS_ONLY, keys only (~89 B measured)"
    );
    // ALL returns the whole projected item: 4096 + 35 + 13048 per result.
    let (bytes, basis) =
        vector_search_bytes_per_call(&ap(1), first_index(&all), &all).expect("bytes");
    assert_eq!(bytes, 17179.0);
    assert_eq!(
        basis,
        "TopK 1 x 17,179 B/result — ALL, the whole projected item is returned per result"
    );
    // A search pattern is priced at $0 and flagged, not "free".
    let cost = pattern_monthly_cost(
        &json(r#"{"operation":"SearchVectors","index":"I","top_k":10,"peak_rps":50}"#),
        Some(&keys),
        None,
    )
    .expect("cost");
    assert!(cost.is_vector_search);
    assert_eq!(
        (cost.total_cost, cost.vector_search_cost, cost.base_cost),
        (0.0, 0.0, 0.0)
    );
    assert_eq!(cost.vector_search_bytes, 890.0);
    // An unknown target index is reported, not priced.
    let missing = pattern_monthly_cost(
        &json(r#"{"operation":"SearchVectors","index":"Nope","peak_rps":50}"#),
        Some(&keys),
        None,
    )
    .expect("cost");
    assert!(missing.is_vector_search);
    assert_eq!(missing.vector_search_basis, "target index not declared");
    assert_eq!(
        missing.cap.notes,
        vec!["SearchVectors target index not found in the model — cost not estimated"]
    );
}

#[test]
fn spend_gate_bound_reproduces_the_documented_measurements() {
    // The calculate_costs.py docstring table (declared item size 13,048 B):
    //   ALL 1024 TopK 100 -> 2,608,645; ALL 1024 TopK 1 -> 57,564;
    //   KEYS_ONLY 1024 TopK 100 -> 45,145; TopK 1 -> 31,929;
    //   KEYS_ONLY 128 TopK 1 -> 4,108; INCLUDE 1024 TopK 100 -> 60,145.
    let gate = |dims: u32, projection: &str, top_k: u32| {
        let table = vector_table(13048, &format!("[{}]", vector_index(dims, projection)));
        let ap = json(&format!(
            r#"{{"operation":"SearchVectors","top_k":{top_k}}}"#
        ));
        vector_search_bytes_spend_gate_upper(&ap, first_index(&table), &table).expect("gate")
    };
    assert_close(gate(1024, r#"{"type":"ALL"}"#, 100), 2_608_645.2, 0.5);
    assert_close(gate(1024, r#"{"type":"ALL"}"#, 1), 57_563.7, 0.5);
    assert_close(gate(1024, r#"{"type":"KEYS_ONLY"}"#, 100), 45_145.2, 0.5);
    assert_close(gate(1024, r#"{"type":"KEYS_ONLY"}"#, 1), 31_928.7, 0.5);
    assert_close(gate(128, r#"{"type":"KEYS_ONLY"}"#, 1), 4_107.9, 0.5);
    assert_close(
        gate(
            1024,
            r#"{"type":"INCLUDE","attributes":["undeclared"]}"#,
            100,
        ),
        60_145.2,
        0.5,
    );
    // The gate never goes below the 1 KB metering floor.
    assert_eq!(gate(1, r#"{"type":"KEYS_ONLY"}"#, 1), 1024.0);
}

fn vector_errors(tables: &str, patterns: &str) -> Vec<String> {
    let (Json::Array(tables), Json::Array(patterns)) = (json(tables), json(patterns)) else {
        panic!("fixtures must be lists");
    };
    validate_vector_model(&tables, &patterns).expect("validation runs")
}

const GOOD_INDEX: &str =
    r#"{"index_name":"I","vector_attribute":"Emb","dimensions":1024,"distance_function":"COSINE"}"#;

#[test]
fn a_valid_vector_model_has_no_errors() {
    let tables = format!(r#"[{{"table_name":"T","vector_indexes":[{GOOD_INDEX}]}}]"#);
    let patterns = r#"[{"pattern_id":"P","operation":"SearchVectors","table":"T","index":"I","top_k":50},
                       {"pattern_id":"G","operation":"GetItem"}]"#;
    assert!(vector_errors(&tables, patterns).is_empty());
    // Documented int() behavior: a float or numeric string is truncated, and
    // lower-case distance functions are accepted.
    let tables = r#"[{"table_name":"T","vector_indexes":[
        {"index_name":"I","vector_attribute":"E","dimensions":"128","distance_function":"cosine"},
        {"index_name":"J","vector_attribute":"F","dimensions":12.7,"distance_function":"euclidean"}]}]"#;
    assert!(vector_errors(tables, "[]").is_empty());
    // Tables without vector indexes are ignored entirely.
    assert!(vector_errors(r#"[{"table_name":"T"}]"#, "[]").is_empty());
}

#[test]
fn invalid_vector_indexes_are_each_reported_with_the_python_message() {
    let one = |index: &str, extra: &str| {
        vector_errors(
            &format!(r#"[{{"table_name":"T"{extra},"vector_indexes":[{index}]}}]"#),
            "[]",
        )
    };
    assert_eq!(
        one(r#"{"name":"x","dimensions":128,"distance_function":"COSINE","vector_attribute":"E"}"#, ""),
        vec![
            "T: a vector index is missing `index_name` (found keys: ['dimensions', 'distance_function', 'name', 'vector_attribute']) — the field is `index_name`, not `name`"
        ]
    );
    for (dimensions, shown) in [
        (r#""1k""#, "'1k'"),
        ("true", "True"),
        ("0", "0"),
        ("4097", "4097"),
        ("null", "None"),
        ("[1]", "[1]"),
    ] {
        assert_eq!(
            one(
                &format!(
                    r#"{{"index_name":"I","vector_attribute":"E","dimensions":{dimensions},"distance_function":"COSINE"}}"#
                ),
                ""
            ),
            vec![format!(
                "T.I: dimensions must be an integer 1-4096, got {shown}"
            )],
            "{dimensions}"
        );
    }
    assert_eq!(
        one(
            r#"{"index_name":"I","vector_attribute":"E","dimensions":8,"distance_function":"MANHATTAN"}"#,
            ""
        ),
        vec!["T.I: distance_function must be COSINE, EUCLIDEAN or DOT_PRODUCT, got 'MANHATTAN'"]
    );
    assert_eq!(
        one(
            r#"{"index_name":"I","vector_attribute":"E","dimensions":8}"#,
            ""
        ),
        vec!["T.I: distance_function must be COSINE, EUCLIDEAN or DOT_PRODUCT, got None"]
    );
    assert_eq!(
        one(
            r#"{"index_name":"I","dimensions":8,"distance_function":"COSINE"}"#,
            ""
        ),
        vec!["T.I: vector_attribute is required"]
    );
    assert_eq!(
        one(GOOD_INDEX, r#","provisioned_capacity":{"read":5}"#),
        vec!["T: vector indexes require on-demand capacity; this table declares provisioned_capacity"]
    );
    let two_dims = format!(
        "{GOOD_INDEX},{}",
        r#"{"index_name":"J","vector_attribute":"Emb","dimensions":256,"distance_function":"COSINE"}"#
    );
    assert_eq!(
        one(&two_dims, ""),
        vec!["T: indexes on attribute 'Emb' declare differing dimensions [256, 1024] — DynamoDB rejects this ('Attributes cannot be redefined')"]
    );
    let six = vec![
        r#"{"index_name":"I","vector_attribute":"E","dimensions":8,"distance_function":"COSINE"}"#;
        6
    ]
    .join(",");
    assert_eq!(
        one(&six, ""),
        vec!["T: 6 vector indexes exceeds the per-table limit of 5"]
    );
}

#[test]
fn invalid_search_patterns_are_each_reported_with_the_python_message() {
    let tables = format!(r#"[{{"table_name":"T","vector_indexes":[{GOOD_INDEX}]}}]"#);
    let check = |pattern: &str| vector_errors(&tables, &format!("[{pattern}]"));
    for (top_k, shown) in [
        ("0", "0"),
        ("101", "101"),
        (r#""x""#, "'x'"),
        ("true", "True"),
        ("null", "None"),
    ] {
        assert_eq!(
            check(&format!(
                r#"{{"pattern_id":"P","operation":"SearchVectors","table":"T","index":"I","top_k":{top_k}}}"#
            )),
            vec![format!("P: top_k must be an integer 1-100, got {shown}")],
            "{top_k}"
        );
    }
    assert_eq!(
        check(r#"{"pattern_id":"P","operation":"SearchVectors","index":"I"}"#),
        vec!["P: operation SearchVectors has no `table` (found keys: ['index', 'operation', 'pattern_id']) — a vector index belongs to a table, so the pattern must name it just as every other access pattern does"]
    );
    assert_eq!(
        check(r#"{"pattern_id":"P","operation":"SearchVectors","table":"T","vector_index":"I"}"#),
        vec!["P: operation SearchVectors has no `index` (found keys: ['operation', 'pattern_id', 'table', 'vector_index']) — the field is `index`, not `vector_index`; it must name an entry in the table's vector_indexes"]
    );
    assert_eq!(
        check(r#"{"pattern_id":"P","operation":"SearchVectors","table":"T","index":"Nope"}"#),
        vec!["P: operation SearchVectors targets T.Nope, which is not declared in that table's vector_indexes"]
    );
    assert_eq!(
        check(r#"{"pattern_id":"P","operation":"SearchVectors","table":"Other","index":"I"}"#),
        vec!["P: operation SearchVectors targets Other.I, which is not declared in that table's vector_indexes"]
    );
}

#[test]
fn an_unnamed_index_never_satisfies_a_search_without_an_index() {
    // Two bugs must not cancel: the unnamed index and the pattern missing
    // `index` are both reported, and no "targets" match is invented.
    let errors = vector_errors(
        r#"[{"table_name":"T","vector_indexes":[{"vector_attribute":"E","dimensions":8,"distance_function":"COSINE"}]}]"#,
        r#"[{"pattern_id":"P","operation":"SearchVectors","table":"T"}]"#,
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("missing `index_name`"));
    assert!(errors[1].contains("has no `index`"));
}

// ---------------------------------------------------------------------------
// Storage, write actions and retention.
// ---------------------------------------------------------------------------

fn tables_of(source: &str) -> Vec<Json> {
    match json(source) {
        Json::Array(items) => items,
        other => panic!("fixture must be a list, got {other:?}"),
    }
}

#[test]
fn storage_falls_back_to_entity_counts_at_the_full_public_rate() {
    let tables = tables_of(
        r#"[{"table_name":"T","entities":[{"entity_name":"E","estimated_item_size_bytes":1024,"estimated_item_count":1000000}]}]"#,
    );
    let (rows, total) = storage_cost(&tables, &HashMap::new()).expect("storage");
    // 1e6 x (1024 + 100) B = 1.124 GB -> $0.281 at $0.25/GB-month.
    assert_eq!(rows, vec![vec!["T", "Table", "1.12", "$0.28"]]);
    assert_close(total, 0.281, 1e-12);
    // Global tables add 48 B per item: 1.172 GB.
    let global = tables_of(
        r#"[{"table_name":"T","global_tables":true,"entities":[{"entity_name":"E","estimated_item_size_bytes":1024,"estimated_item_count":1000000}]}]"#,
    );
    let (rows, total) = storage_cost(&global, &HashMap::new()).expect("storage");
    assert_eq!(rows[0][2..], ["1.17", "$0.29"]);
    assert_close(total, 0.293, 1e-12);
    // Write-driven bytes win over the fallback (no free tier is applied).
    let by_table = HashMap::from([("T".to_string(), 4_000_000_000.0)]);
    let (rows, total) = storage_cost(&tables, &by_table).expect("storage");
    assert_eq!(rows, vec![vec!["T", "Table", "4.00", "$1.00"]]);
    assert_eq!(total, 1.0);
}

#[test]
fn gsi_storage_scales_by_projection_and_member_entity_bytes() {
    let tables = tables_of(
        r#"[{"table_name":"T","entities":[
            {"entity_name":"A","estimated_item_size_bytes":1000,"estimated_item_count":1000,"attributes":[{"name":"pk"},{"name":"gk"}]},
            {"entity_name":"B","estimated_item_size_bytes":3000,"estimated_item_count":1000,"attributes":[{"name":"pk"}]}],
          "gsis":[{"index_name":"ALLg","partition_key":"gk","projection":{"type":"ALL"}},
                  {"index_name":"KEYSg","partition_key":"gk","projection":{"type":"KEYS_ONLY"}},
                  {"index_name":"Thin","partition_key":"nowhere","projection":{"type":"INCLUDE"}}]}]"#,
    );
    let by_table = HashMap::from([("T".to_string(), 4_000_000_000.0)]);
    let (rows, total) = storage_cost(&tables, &by_table).expect("storage");
    // Only entity A (1e6 of 4e6 bytes) carries gk -> 25% of 4 GB = 1.00 GB;
    // KEYS_ONLY is a further 10% = 0.10 GB. `nowhere` is declared on no entity,
    // so membership is indeterminate and the INCLUDE index keeps the full 30%.
    assert_eq!(
        rows,
        vec![
            vec!["T", "Table", "4.00", "$1.00"],
            vec!["ALLg", "GSI", "1.00", "$0.25"],
            vec!["KEYSg", "GSI", "0.10", "$0.03"],
            vec!["Thin", "GSI", "1.20", "$0.30"],
        ]
    );
    assert_close(total, 1.0 + 0.25 + 0.025 + 0.3, 1e-12);
}

#[test]
fn vector_index_storage_uses_item_counts() {
    let declared = tables_of(
        r#"[{"table_name":"V","entities":[{"entity_name":"E","estimated_item_count":200000}],
            "vector_indexes":[{"index_name":"I","vector_attribute":"Emb","dimensions":1024,"projection":"KEYS_ONLY","estimated_indexed_items":1000000}]}]"#,
    );
    let by_table = HashMap::from([("V".to_string(), 1.0)]);
    let (rows, _) = storage_cost(&declared, &by_table).expect("storage");
    // 1e6 items x 4131 B = 4.131 GB -> $1.03275.
    assert_eq!(rows[1], vec!["I", "Vector index", "4.13", "$1.03"]);
    // Without a declared count the entity counts are an upper bound, starred.
    let inferred = tables_of(
        r#"[{"table_name":"V","entities":[{"entity_name":"E","estimated_item_count":200000}],
            "vector_indexes":[{"index_name":"I","vector_attribute":"Emb","dimensions":1024,"projection":"KEYS_ONLY"}]}]"#,
    );
    let (rows, _) = storage_cost(&inferred, &by_table).expect("storage");
    assert_eq!(rows[1], vec!["I", "Vector index*", "0.83", "$0.21"]);
}

#[test]
fn write_actions_come_from_the_pattern_then_requirements_then_the_operation() {
    let action = |ap: &str, requirements: Option<&str>| {
        let requirements = requirements.map(json);
        resolve_write_action(&json(ap), requirements.as_ref()).expect("action")
    };
    assert_eq!(
        action(r#"{"operation":"PutItem","write_action":"update"}"#, None),
        "update"
    );
    let requirements = r#"{"updates":{"REQ-1":{"write_action":"delete"},"REQ-2":{}}}"#;
    assert_eq!(
        action(
            r#"{"operation":"PutItem","source_requirement":"REQ-1"}"#,
            Some(requirements)
        ),
        "delete"
    );
    // An unknown or action-less requirement falls through to the operation.
    assert_eq!(
        action(
            r#"{"operation":"PutItem","source_requirement":"REQ-9"}"#,
            Some(requirements)
        ),
        "create"
    );
    assert_eq!(
        action(
            r#"{"operation":"PutItem","source_requirement":"REQ-2"}"#,
            Some(requirements)
        ),
        "create"
    );
    for (op, expected) in [
        ("PutItem", "create"),
        ("UpdateItem", "update"),
        ("DeleteItem", "delete"),
        ("BatchWriteItem", "mixed"),
        ("TransactWriteItems", "mixed"),
    ] {
        assert_eq!(
            action(&format!(r#"{{"operation":"{op}"}}"#), None),
            expected
        );
    }
}

#[test]
fn retention_prefers_the_pattern_then_the_entity_then_the_default() {
    let tables =
        tables_of(r#"[{"table_name":"T","entities":[{"entity_name":"E"}]},{"table_name":"U"}]"#);
    let days = |ap: &str, requirements: Option<&str>| {
        let requirements = requirements.map(json);
        resolve_retention_days(&json(ap), &tables, requirements.as_ref()).expect("days")
    };
    let requirements =
        r#"{"entities":{"E":{"retention_days":90}},"metadata":{"retention_days_default":45}}"#;
    assert_eq!(
        days(r#"{"table":"T","retention_days":365}"#, Some(requirements)),
        365.0
    );
    assert_eq!(days(r#"{"table":"T"}"#, Some(requirements)), 90.0);
    assert_eq!(
        days(r#"{"table":"U"}"#, Some(requirements)),
        45.0,
        "no entity: metadata default"
    );
    assert_eq!(days(r#"{"table":"T"}"#, Some(r#"{"metadata":{}}"#)), 30.0);
    assert_eq!(days(r#"{"table":"T"}"#, None), 30.0);
    assert_eq!(
        days(
            r#"{"table":"T"}"#,
            Some(r#"{"entities":{"E":{"retention_days":12.5}}}"#)
        ),
        12.5,
        "requirements values are not truncated"
    );
    // The per-pattern value is int()'d like Python.
    assert_eq!(days(r#"{"table":"T","retention_days":"7"}"#, None), 7.0);
    let error = resolve_retention_days(&json(r#"{"retention_days":"soon"}"#), &tables, None)
        .expect_err("not a number");
    assert!(
        error
            .message()
            .contains("`retention_days` must be an integer"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// The report.
// ---------------------------------------------------------------------------

const REPORT_MODEL: &str = r#"{
    "tables": [{"table_name": "Orders", "key_schema": {"partition_key": "pk"},
        "entities": [{"entity_name": "Order", "estimated_item_size_bytes": 1024,
                      "estimated_item_count": 1000, "attributes": [{"name": "pk", "type": "S"}]}]}],
    "access_patterns": [
        {"pattern_id": "GetOrder", "operation": "GetItem", "table": "Orders",
         "peak_rps": 100, "estimated_item_size_bytes": 1024},
        {"pattern_id": "PutOrder", "operation": "PutItem", "table": "Orders",
         "peak_rps": 10, "estimated_item_size_bytes": 2000}]
}"#;

fn report(model: &str) -> Vec<String> {
    calculate_and_report(&json(model), None)
        .expect("report")
        .split('\n')
        .map(ToString::to_string)
        .collect()
}

#[test]
fn report_headline_tables_and_concentration_notes() {
    // Get = 0.5 RRU -> $16.20; Put = 2000 B = 2 WRU -> $32.40; rw $48.60.
    // Put storage: (2000 + 100) B x 10 rps x 86,400 s x 30 d = 54.432 GB
    // -> $13.608; headline $62.208.
    let lines = report(REPORT_MODEL);
    assert_eq!(lines[0], "# DynamoDB Cost Report");
    assert!(lines[2].starts_with("> **Disclaimer:** This estimate covers **read/write request costs** and **storage costs** only"));
    assert!(lines[2]
        .ends_with("[Amazon DynamoDB Pricing](https://aws.amazon.com/dynamodb/pricing/) page."));
    assert!(lines.contains(
        &"**Peak-Sustained Monthly Cost: $62.21**  *(every pattern at its declared `peak_rps`, sustained 24/7; set `avg_rps` on patterns for a realistic average-volume figure)*".to_string()
    ));
    let source = |a: &str, b: &str| format!("| {a:<23} | {b:<12} |");
    assert!(lines.contains(&source("Source", "Monthly Cost")));
    assert!(lines.contains(&source("Storage", "$13.61")));
    assert!(lines.contains(&source("Read and write requests", "$48.60")));
    assert!(lines.contains(&"**Monthly Cost:** $13.61".to_string()));
    assert!(lines.contains(&"**Monthly Cost:** $48.60".to_string()));

    let storage =
        |a: &str, b: &str, c: &str, d: &str| format!("| {a:<8} | {b:<5} | {c:<12} | {d:<12} |");
    assert!(lines.contains(&storage("Resource", "Type", "Storage (GB)", "Monthly Cost")));
    assert!(lines.contains(&storage("Orders", "Table", "54.43", "$13.61")));

    // Highest cost first, with the column widths of the widest cell.
    let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str| {
        format!("| {a:<8} | {b:<9} | {c:<11} | {d:<8} | {e:<7} | {f:<12} |")
    };
    let put = lines
        .iter()
        .position(|l| *l == row("PutOrder", "PutItem", "Orders", "10.0", "2.00", "$32.40"))
        .expect("put row");
    let get = lines
        .iter()
        .position(|l| *l == row("GetOrder", "GetItem", "Orders", "100.0", "0.50", "$16.20"))
        .expect("get row");
    assert_eq!(get, put + 1, "Put ($32.40) sorts before Get ($16.20)");
    assert!(lines.contains(&row(
        "Pattern",
        "Operation",
        "Table/Index",
        "Peak RPS",
        "RRU/WRU",
        "Monthly Cost"
    )));

    // Both patterns exceed 30% of the read/write cost: 67% and 33%.
    assert!(lines.contains(&"- `PutOrder` contributes 67% of read/write cost. A 50% error in `estimated_item_size_bytes` on this pattern would move the headline number by ~33%. Revisit its attribute walkthrough if you haven't already, or rely on live validation to confirm the number.".to_string()));
    assert!(lines.contains(&"- `GetOrder` contributes 33% of read/write cost. A 50% error in `estimated_item_size_bytes` on this pattern would move the headline number by ~17%. Revisit its attribute walkthrough if you haven't already, or rely on live validation to confirm the number.".to_string()));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("> **Cost concentration — item-size sensitivity.**")));
    // No GSI or vector sections for this model.
    assert!(!lines
        .iter()
        .any(|l| l.starts_with("¹ ") || l.contains("Vector Index Capacity")));
}

#[test]
fn report_has_no_trailing_newline_and_is_deterministic() {
    let first = calculate_and_report(&json(REPORT_MODEL), None).expect("report");
    let second = calculate_and_report(&json(REPORT_MODEL), None).expect("report");
    assert_eq!(first, second);
    assert!(!first.ends_with('\n'));
}

#[test]
fn average_volume_adds_the_expected_headline_and_column() {
    let with_avg = REPORT_MODEL.replace(r#""peak_rps": 10,"#, r#""peak_rps": 10, "avg_rps": 5,"#);
    let lines = report(&with_avg);
    // Peak is unchanged; expected: Get $16.20 + Put $16.20 requests and
    // (2000 + 100) x 5 x 86,400 x 30 = 27.216 GB -> $6.804 storage.
    assert!(lines.contains(
        &"**Peak-Sustained Monthly Cost: $62.21**  *(every pattern at its declared `peak_rps`, sustained 24/7 — the worst case)*".to_string()
    ));
    assert!(lines.contains(
        &"**Expected Monthly Cost (your stated average volume): $39.20**  *(patterns driven at `avg_rps`; on-demand bills per request, so this is the realistic figure)*".to_string()
    ));
    let row = |a: &str, b: &str, c: &str| format!("| {a:<23} | {b:<14} | {c:<8} |");
    assert!(lines.contains(&row("Source", "Peak-Sustained", "Expected")));
    assert!(lines.contains(&row("Storage", "$13.61", "$6.80")));
    assert!(lines.contains(&row("Read and write requests", "$48.60", "$32.40")));
}

#[test]
fn report_lists_gsi_write_amplification_with_its_footnote() {
    let model = r#"{
        "tables": [{"table_name": "T",
            "entities": [{"entity_name": "E", "attributes": [{"name": "pk"}, {"name": "gk"}]}],
            "gsis": [{"index_name": "ByG", "partition_key": "gk", "projection": {"type": "ALL"}}]}],
        "access_patterns": [{"pattern_id": "PutX", "operation": "PutItem", "table": "T",
                             "peak_rps": 10, "estimated_item_size_bytes": 2048}]
    }"#;
    // Base 2 WRU = $32.40 and the ALL index repeats it: 2 WRU = $32.40.
    let lines = report(model);
    let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str| {
        format!("| {a:<7} | {b:<10} | {c:<11} | {d:<8} | {e:<7} | {f:<12} |")
    };
    assert!(lines.contains(&row("PutX", "PutItem", "T", "10.0", "2.00", "$32.40")));
    assert!(lines.contains(&row("PutX¹", "GSI writes", "T", "10.0", "-", "$32.40")));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("¹ **GSI additional writes**")));
    assert!(lines.contains(&"**Monthly Cost:** $64.80".to_string()));
}

#[test]
fn report_renders_vector_search_as_not_priced_never_as_free() {
    let model = r#"{
        "tables": [{"table_name": "P",
            "entities": [{"entity_name": "E", "estimated_item_size_bytes": 1000, "estimated_item_count": 1000,
                          "attributes": [{"name": "title", "type": "S"}]}],
            "vector_indexes": [{"index_name": "I", "vector_attribute": "Emb", "dimensions": 1024,
                                "distance_function": "COSINE", "projection": {"type": "KEYS_ONLY"},
                                "estimated_indexed_items": 1000}]}],
        "access_patterns": [
            {"pattern_id": "Find", "operation": "SearchVectors", "table": "P", "index": "I",
             "peak_rps": 60, "top_k": 100},
            {"pattern_id": "PutP", "operation": "PutItem", "table": "P", "peak_rps": 200,
             "attributes_written": ["Emb"]}]
    }"#;
    let lines = report(model);
    // 4131 B x $0.52/GB x 200 rps x 2,592,000 s = $1113.59 vector writes.
    assert!(lines
        .iter()
        .any(|l| l.starts_with("| Find") && l.contains("| not priced² ")));
    assert!(!lines
        .iter()
        .any(|l| l.starts_with("| Find") && l.contains("$0.00")));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("² **Vector search is not priced here, and is NOT $0**")));
    assert!(lines.contains(&"## Vector Index Capacity".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("| PutP")
        && l.contains("| 4,131 ")
        && l.contains("$1113.59")
        && l.contains("I: 4,131 B")));
    assert!(lines.iter().any(|l| l.starts_with("| Find")
        && l.contains("| 8.9 KB ")
        && l.contains("TopK 100 x 89 B/result — KEYS_ONLY, keys only (~89 B measured)")));
    assert!(lines.iter().any(|l| l.starts_with(
        "**Get the real number before you quote one.** Vector search bills at $0.002/GB."
    )));
    // The storage table carries the vector index (declared count: no star).
    assert!(lines
        .iter()
        .any(|l| l.starts_with("| I ") && l.contains("| Vector index ")));
}

#[test]
fn report_rejects_models_that_cannot_be_priced() {
    let error = calculate_and_report(
        &json(r#"{"tables":[{"table_name":"T"}],"access_patterns":[{"operation":"GetItem"}]}"#),
        None,
    )
    .expect_err("pattern without an id");
    assert!(error.message().contains("missing `pattern_id`"), "{error}");
    let error = calculate_and_report(&json(r#"{"tables":[{}],"access_patterns":[]}"#), None)
        .expect_err("table without a name");
    assert!(error.message().contains("`table_name`"), "{error}");
    let error = calculate_and_report(&json(r#"{"tables":"nope"}"#), None).expect_err("tables list");
    assert!(
        error.message().contains("`tables` must be a list"),
        "{error}"
    );
}

#[test]
fn requirements_change_the_storage_estimate() {
    let model = json(
        r#"{"tables":[{"table_name":"T","entities":[{"entity_name":"E"}]}],
            "access_patterns":[{"pattern_id":"P","operation":"UpdateItem","table":"T",
                                "peak_rps":1,"estimated_item_size_bytes":900,"source_requirement":"R1"}]}"#,
    );
    // No requirements: UpdateItem creates nothing -> entity-count fallback
    // (100,000 x (1024 + 100) B = 0.11 GB).
    let plain = calculate_and_report(&model, None).expect("report");
    assert!(
        plain.contains("| T        | Table | 0.11         | $0.03"),
        "{plain}"
    );
    // The requirement turns it into creates: 1000 B x 1 rps x 86,400 x 30 =
    // 2.592 GB -> $0.648.
    let requirements = json(r#"{"updates":{"R1":{"write_action":"create"}}}"#);
    let with = calculate_and_report(&model, Some(&requirements)).expect("report");
    assert!(
        with.contains("| T        | Table | 2.59         | $0.65"),
        "{with}"
    );
}

#[test]
fn padded_tables_pad_by_character_count() {
    let table = padded_table(
        &["A", "Longer"],
        &[
            vec!["wide cell".to_string(), "x".to_string()],
            vec!["¹".to_string()],
        ],
    );
    assert_eq!(
        table,
        "| A         | Longer |\n| --------- | ------ |\n| wide cell | x      |\n| ¹         |        |"
    );
    assert_eq!(pyfmt::commas(1234.5, 1), "1,234.5");
}
