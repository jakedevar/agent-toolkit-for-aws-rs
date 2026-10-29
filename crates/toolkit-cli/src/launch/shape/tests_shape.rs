//! T-SHAPE-1..8 and Python output parity of shaped values (R-API-SHAPE).
use super::shape;
use crate::aurora::pyfmt::dumps;

fn compact(name: &str, input: &str) -> String {
    shape(name, input).unwrap().to_compact_string()
}
fn python(name: &str, input: &str) -> String {
    dumps(&shape(name, input).unwrap())
}

#[test]
fn t_shape_1_unmodelled_members_are_dropped() {
    assert_eq!(
        compact("ListLaunchesOutput", r#"{"unknown":1,"items":[]}"#),
        r#"{"items":[]}"#
    );
    // Recursively, inside list items and nested structures.
    assert_eq!(
        compact(
            "LaunchCostEstimate",
            r#"{"x":1,"services":[{"monthlyCost":2,"service":"s","y":[]}],"region":"r","estimatedMonthlyCost":1}"#
        ),
        r#"{"estimatedMonthlyCost":1.0,"region":"r","services":[{"service":"s","monthlyCost":2.0}]}"#
    );
}
#[test]
fn t_shape_2_model_member_order() {
    assert_eq!(
        compact("ListLaunchesOutput", r#"{"nextToken":"n","items":[]}"#),
        r#"{"items":[],"nextToken":"n"}"#
    );
    // Order is the model's, not the response's, at every depth.
    assert_eq!(
        compact(
            "LaunchSummary",
            r#"{"updatedAt":0,"createdAt":0,"status":"s","name":"n","id":"i"}"#
        ),
        r#"{"id":"i","name":"n","status":"s","createdAt":"1970-01-01 00:00:00+00:00","updatedAt":"1970-01-01 00:00:00+00:00"}"#
    );
}
#[test]
fn t_shape_3_optional_null_is_dropped() {
    assert_eq!(
        compact("ListLaunchesOutput", r#"{"items":[],"nextToken":null}"#),
        r#"{"items":[]}"#
    );
}
#[test]
fn t_shape_4_timestamp_without_microseconds() {
    assert_eq!(
        compact("SyntheticTimestamp_date_time", r#""2026-06-15T12:34:56Z""#),
        r#""2026-06-15 12:34:56+00:00""#
    );
    assert_eq!(
        compact(
            "SyntheticTimestamp_date_time",
            r#""2026-06-15T12:34:56.000Z""#
        ),
        r#""2026-06-15 12:34:56+00:00""#
    );
}
#[test]
fn t_shape_5_timestamp_with_fraction_and_offset() {
    assert_eq!(
        compact(
            "SyntheticTimestamp_date_time",
            r#""2026-06-15T12:34:56.123+02:30""#
        ),
        r#""2026-06-15 12:34:56.123000+02:30""#
    );
    assert_eq!(
        compact(
            "SyntheticTimestamp_date_time",
            r#""2026-06-15T12:34:56.000001-05:00""#
        ),
        r#""2026-06-15 12:34:56.000001-05:00""#
    );
    // dateutil truncates digits beyond microseconds.
    assert_eq!(
        compact(
            "SyntheticTimestamp_date_time",
            r#""2026-06-15T12:34:56.1234569Z""#
        ),
        r#""2026-06-15 12:34:56.123456+00:00""#
    );
}
#[test]
fn t_shape_6_float_as_python_float() {
    for (wire, printed) in [
        ("12", "12.0"),
        ("0", "0.0"),
        ("-0.0", "-0.0"),
        ("0.1", "0.1"),
        ("1.5e-7", "1.5e-07"),
        ("1e16", "1e+16"),
        ("123456789.125", "123456789.125"),
        ("1234567890123456789", "1.2345678901234568e+18"),
    ] {
        assert_eq!(python("Float", wire), printed, "{wire}");
    }
    // Integer shapes stay integers; a fraction there is invalid.
    assert_eq!(python("Integer", "12"), "12");
    assert!(shape("Integer", "12.5").is_err());
    assert!(shape("Float", "\"12\"").is_err());
}
#[test]
fn t_shape_7_unknown_union_member() {
    assert_eq!(
        compact("LaunchSource", r#"{"future":{}}"#),
        r#"{"SDK_UNKNOWN_MEMBER":{"name":"future"}}"#
    );
    // botocore ignores `__type` and null members; more than one set is invalid.
    assert_eq!(
        compact(
            "LaunchSource",
            r#"{"__type":"x","other":null,"s3Upload":{"uploadId":"u"}}"#
        ),
        r#"{"s3Upload":{"uploadId":"u"}}"#
    );
    for invalid in ["{}", r#"{"s3Upload":{"uploadId":"u"},"future":{}}"#, "[]"] {
        assert!(shape("LaunchSource", invalid).is_err(), "{invalid}");
    }
}
#[test]
fn t_shape_8_required_null_and_wrong_type_refused() {
    for input in [
        r#"{"items":null}"#,
        r#"{"items":{}}"#,
        "{}",
        r#"{"items":[1]}"#,
        r#"{"items":[],"nextToken":1}"#,
    ] {
        assert!(shape("ListLaunchesOutput", input).is_err(), "{input}");
    }
    for input in ["", "{", "{\"items\":[]} x", "\u{feff}{\"items\":[]}"] {
        assert!(shape("ListLaunchesOutput", input).is_err());
    }
}

#[test]
fn t_shape_timestamp_epoch_rounding_boundaries_and_invalid_year() {
    for (wire, printed) in [
        ("0", "1970-01-01 00:00:00+00:00"),
        ("-1", "1969-12-31 23:59:59+00:00"),
        ("-1.5", "1969-12-31 23:59:58.500000+00:00"),
        ("1.5", "1970-01-01 00:00:01.500000+00:00"),
        ("1718454896.123456", "2024-06-15 12:34:56.123456+00:00"),
        ("0.9999999", "1970-01-01 00:00:01+00:00"),
        ("253402300799", "9999-12-31 23:59:59+00:00"),
    ] {
        assert_eq!(
            compact("SyntheticTimestamp_date_time", wire),
            format!("\"{printed}\""),
            "{wire}"
        );
    }
    for invalid in [
        r#""0000-01-01T00:00:00Z""#,
        "253402300800",
        "-62135596801",
        r#""2026-06-15 12:34:56""#,
        r#""2026-06-15T12:34:56""#,
        r#""not a date""#,
        "true",
        "null",
    ] {
        assert!(
            shape("SyntheticTimestamp_date_time", invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn t_shape_map_keeps_response_order_and_drops_null_values() {
    let shapes = super::shapes().unwrap();
    let map = match shapes {
        toolkit_core::Json::Object(members) => members
            .iter()
            .find(|(_, spec)| spec.get("type").and_then(toolkit_core::Json::as_str) == Some("map"))
            .map(|(name, _)| name.clone()),
        _ => None,
    };
    let map = map.expect("the model has a map shape");
    let value_shape = shapes
        .get(&map)
        .and_then(|s| s.get("value"))
        .and_then(|v| v.get("shape"))
        .and_then(toolkit_core::Json::as_str)
        .unwrap();
    if shapes
        .get(value_shape)
        .and_then(|s| s.get("type"))
        .and_then(toolkit_core::Json::as_str)
        == Some("string")
    {
        assert_eq!(
            compact(&map, r#"{"z":"1","a":null,"m":"2"}"#),
            r#"{"z":"1","m":"2"}"#
        );
    }
}
