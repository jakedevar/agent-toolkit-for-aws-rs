//! `input_validator.py` and `instance_types.py`: rules, boundaries and the sizing ladder.

use super::super::timestream::{
    list_types, recommend, recommended_type, validate, validate_all, INSTANCE_TYPES,
};

fn ok(key: &str, value: &str) -> bool {
    validate(key, value).0
}

#[test]
fn instance_names_and_snapshot_ids_have_length_and_character_limits() {
    let name = |tail: usize| format!("a{}", "b".repeat(tail));
    assert!(ok("instance_name", "a"));
    assert!(ok("instance_name", "My-Instance-1"));
    assert!(ok("instance_name", &name(62)), "63 characters");
    assert!(!ok("instance_name", &name(63)), "64 characters");
    for bad in [
        "", "1abc", "-abc", "ab_c", "ab c", "ab.c", "abc\n", "abc ", "ab\u{e9}",
    ] {
        assert!(!ok("instance_name", bad), "{bad:?}");
        assert!(!ok("snapshot_id", bad), "{bad:?}");
    }
    assert!(ok("snapshot_id", &name(254)), "255 characters");
    assert!(!ok("snapshot_id", &name(255)), "256 characters");
    assert_eq!(
        validate("instance_name", "1").1,
        "Must start with letter, alphanumeric/hyphens, max 63 chars"
    );
    assert_eq!(
        validate("snapshot_id", "1").1,
        "Must start with letter, max 255 chars"
    );
}

#[test]
fn instance_types_accept_the_published_shapes_only() {
    for good in [
        "db.influx.medium",
        "db.influx.large",
        "db.influx.xlarge",
        "db.influx.2xlarge",
        "db.influx.24xlarge",
        "db.influx.100xlarge",
    ] {
        assert!(ok("instance_type", good), "{good}");
    }
    for bad in [
        "",
        "db.influx.",
        "db.influx.small",
        "db.influx.xxlarge",
        "db.influx.xlarge ",
        "db.influx.xlarge\n",
        "db.influx.Xlarge",
        "db.influx.-1xlarge",
        "db.influx.1.5xlarge",
        "db.influx.2xxlarge",
        "db.influx.xlarge2",
        "DB.influx.large",
        "db.r7g.large",
        "db.influx.\u{662}xlarge",
        "dbxinflux.large",
    ] {
        assert!(!ok("instance_type", bad), "{bad:?}");
    }
    // Every table entry passes the rule.
    for (name, ..) in INSTANCE_TYPES {
        assert!(ok("instance_type", name), "{name}");
    }
}

#[test]
fn regions_and_ids() {
    for good in ["us-east-1", "eu-west-3", "ap-south-9"] {
        assert!(ok("region", good), "{good}");
    }
    for bad in [
        "",
        "us-east",
        "us-east-10",
        "us-gov-west-1",
        "US-east-1",
        "u-east-1",
        "usa-east-1",
        "us-east-1\n",
        "us--1",
        "us-east-\u{661}",
        "us-east-1 ",
    ] {
        assert!(!ok("region", bad), "{bad:?}");
    }
    for (key, prefix) in [("vpc_id", "vpc"), ("subnet_id", "subnet"), ("sg_id", "sg")] {
        assert!(ok(key, &format!("{prefix}-0123abcd")), "8 hex digits");
        assert!(
            ok(key, &format!("{prefix}-0123456789abcdef0")),
            "17 hex digits"
        );
        for bad in [
            format!("{prefix}-0123abc"),
            format!("{prefix}-0123456789abcdef01"),
            format!("{prefix}-0123ABCD"),
            format!("{prefix}-0123abcg"),
            format!("{prefix}0123abcd"),
            format!("{prefix}-0123abcd\n"),
            format!("x{prefix}-0123abcd"),
            String::new(),
        ] {
            assert!(!ok(key, &bad), "{key} {bad:?}");
        }
    }
    assert!(!ok("vpc_id", "subnet-0123abcd"));
}

#[test]
fn storage_is_an_integer_from_20_to_16384() {
    for good in ["20", "16384", "100", " 100 ", "+100", "1_000", "0020"] {
        assert!(ok("storage_gb", good), "{good:?}");
    }
    for bad in [
        "19",
        "16385",
        "0",
        "-20",
        "",
        " ",
        "abc",
        "1.5",
        "1e3",
        "1__0",
        "_100",
        "100_",
        "20GB",
        "99999999999999999999",
        "-99999999999999999999",
    ] {
        assert!(!ok("storage_gb", bad), "{bad:?}");
    }
    assert_eq!(validate("storage_gb", "1").1, "Must be 20-16384");
}

#[test]
fn unknown_keys_have_no_validator_and_pass() {
    assert!(ok("anything", "whatever \n"));
    assert!(ok("Instance_Name", "1"), "keys are case sensitive");
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn validate_all_reports_each_failure_in_command_line_order() {
    let (lines, valid) = validate_all(&pairs(&[
        ("region", "us-east-1"),
        ("storage_gb", "20"),
        ("other", "x"),
    ]));
    assert!(valid);
    assert_eq!(lines, ["All inputs valid."]);
    let (lines, valid) = validate_all(&pairs(&[
        ("storage_gb", "5"),
        ("region", "us-east-1"),
        ("vpc_id", "nope"),
    ]));
    assert!(!valid);
    assert_eq!(
        lines,
        [
            "Validation FAILED:",
            "  storage_gb=5 \u{2014} Must be 20-16384",
            "  vpc_id=nope \u{2014} Must be vpc-{hex}",
        ]
    );
    assert_eq!(
        validate_all(&[]),
        (vec!["All inputs valid.".to_string()], true)
    );
}

#[test]
fn a_repeated_key_keeps_its_first_position_and_its_last_value() {
    let (lines, valid) = validate_all(&pairs(&[
        ("region", "bad"),
        ("vpc_id", "x"),
        ("region", "us-east-1"),
    ]));
    assert!(!valid);
    assert_eq!(
        lines,
        [
            "Validation FAILED:",
            "  vpc_id=x \u{2014} Must be vpc-{hex}"
        ]
    );
    let (lines, valid) = validate_all(&pairs(&[("region", "us-east-1"), ("region", "bad")]));
    assert!(!valid);
    assert_eq!(lines.len(), 2);
}

#[test]
fn failure_lines_escape_and_bound_the_echoed_text() {
    let hostile = format!(
        "us\u{1b}[31m\r\nValidation FAILED:\u{202e}{}",
        "z".repeat(500)
    );
    let (lines, valid) = validate_all(&pairs(&[("region", &hostile)]));
    assert!(!valid);
    assert_eq!(lines.len(), 2, "no forged extra line: {lines:?}");
    super::assert_single_line_safe(&lines[1]);
    assert!(
        lines[1].contains("region=us\\u001B[31m\\r\\nValidation FAILED:\\u202E"),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains("...[") && lines[1].len() < 300,
        "bounded: {}",
        lines[1].len()
    );
}

#[test]
fn the_instance_table_lists_every_type_in_order() {
    let lines = list_types();
    assert_eq!(
        lines[0],
        format!(
            "{:<25} {:>5} {:>13} {:<15}",
            "Instance Type", "vCPU", "Memory (GiB)", "Network"
        )
    );
    assert_eq!(lines[1], "-".repeat(60));
    assert_eq!(lines.len(), 2 + INSTANCE_TYPES.len());
    assert_eq!(
        lines[2],
        "db.influx.medium              1             8 Up to 10 Gbps  "
    );
    assert_eq!(
        lines[10],
        "db.influx.24xlarge           96           768 25 Gbps        "
    );
    let names: Vec<&str> = INSTANCE_TYPES.iter().map(|t| t.0).collect();
    assert_eq!(names[0], "db.influx.medium");
    assert_eq!(names[8], "db.influx.24xlarge");
}

fn pick(writes: i64, queries: i64, days: i64) -> &'static str {
    recommended_type(writes, queries, days).0
}

#[test]
fn the_sizing_thresholds_are_strict_at_each_boundary() {
    // medium: writes < 1000 and queries < 5
    assert_eq!(pick(999, 4, 30), "db.influx.medium");
    assert_eq!(pick(1000, 4, 30), "db.influx.large");
    assert_eq!(pick(999, 5, 30), "db.influx.large");
    // large: writes < 10000 and queries < 20
    assert_eq!(pick(9999, 19, 30), "db.influx.large");
    assert_eq!(pick(10_000, 19, 30), "db.influx.xlarge");
    assert_eq!(pick(9999, 20, 30), "db.influx.xlarge");
    // xlarge: writes < 50000 and queries < 50
    assert_eq!(pick(49_999, 49, 30), "db.influx.xlarge");
    assert_eq!(pick(50_000, 49, 30), "db.influx.2xlarge");
    assert_eq!(pick(49_999, 50, 30), "db.influx.2xlarge");
    // 2xlarge: writes < 100000 whatever the concurrency
    assert_eq!(pick(99_999, 1_000_000, 30), "db.influx.2xlarge");
    assert_eq!(pick(100_000, 0, 30), "db.influx.4xlarge");
    // Negative or zero workloads still map to the smallest type.
    assert_eq!(pick(0, 0, 0), "db.influx.medium");
    assert_eq!(pick(-5, -5, -5), "db.influx.medium");
}

#[test]
fn long_retention_moves_up_one_or_two_sizes_at_90_and_365_days() {
    assert_eq!(pick(0, 0, 90), "db.influx.medium");
    assert_eq!(pick(0, 0, 91), "db.influx.large");
    assert_eq!(pick(0, 0, 365), "db.influx.large");
    assert_eq!(pick(0, 0, 366), "db.influx.xlarge");
    assert_eq!(pick(100_000, 0, 91), "db.influx.8xlarge");
    assert_eq!(pick(100_000, 0, 366), "db.influx.12xlarge");
    assert_eq!(pick(100_000, 0, i64::MAX), "db.influx.12xlarge");
    assert_eq!(
        recommended_type(100_000, 0, 366),
        ("db.influx.12xlarge", 48, 384)
    );
}

#[test]
fn the_recommendation_prints_the_workload_and_the_caveat() {
    assert_eq!(
        recommend(5000, 10, 30),
        [
            "Recommended: db.influx.large (2 vCPU, 16 GiB)",
            "  Based on: 5000 writes/s, 10 concurrent queries, 30d retention",
            "  Note: This is a starting point. Monitor CloudWatch metrics and adjust.",
        ]
    );
}
