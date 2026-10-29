use super::shape;

#[test]
fn t_shape_1_unmodelled_members_are_dropped() {
    assert_eq!(shape("ListLaunchesOutput", r#"{"unknown":1,"items":[]}"#).unwrap(), r#"{"items":[]}"#);
}
#[test]
fn t_shape_2_model_member_order() {
    assert_eq!(shape("ListLaunchesOutput", r#"{"nextToken":"n","items":[]}"#).unwrap(), r#"{"items":[],"nextToken":"n"}"#);
}
#[test]
fn t_shape_3_optional_null_is_dropped() {
    assert_eq!(shape("ListLaunchesOutput", r#"{"items":[],"nextToken":null}"#).unwrap(), r#"{"items":[]}"#);
}
#[test]
fn t_shape_4_timestamp_without_microseconds() {
    assert_eq!(shape("SyntheticTimestamp_date_time", r#""2026-06-15T12:34:56Z""#).unwrap(), r#""2026-06-15 12:34:56+00:00""#);
}
#[test]
fn t_shape_5_timestamp_with_fraction_and_offset() {
    assert_eq!(shape("SyntheticTimestamp_date_time", r#""2026-06-15T12:34:56.123+02:30""#).unwrap(), r#""2026-06-15 12:34:56.123000+02:30""#);
}
#[test]
fn t_shape_6_float_as_float() {
    assert_eq!(shape("Float", "12").unwrap(), "12.0");
}
#[test]
fn t_shape_7_unknown_union_member() {
    assert_eq!(shape("LaunchSource", r#"{"future":{}}"#).unwrap(), r#"{"SDK_UNKNOWN_MEMBER":{"name":"future"}}"#);
}
#[test]
fn t_shape_8_required_null_and_wrong_type_refused() {
    for input in [r#"{"items":null}"#, r#"{"items":{}}"#, "{}"] {
        assert!(shape("ListLaunchesOutput", input).is_err());
    }
}
