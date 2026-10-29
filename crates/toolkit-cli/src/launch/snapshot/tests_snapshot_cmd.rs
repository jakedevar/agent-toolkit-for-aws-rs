//! The snapshot command's output format.
use super::*;
use std::path::Path;

#[test]
fn apply_output_is_ordered_json() {
    assert_eq!(
        render_applied(3, 1, Path::new("/repo \"x\"")),
        r#"{"applied":true,"files":3,"directories":1,"into":"/repo \"x\""}"#
    );
}
