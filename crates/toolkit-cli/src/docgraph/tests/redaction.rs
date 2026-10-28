//! Credential redaction in every diagnostic of the slice (#4907, #4908):
//! caller paths, rejected validator values and recorded event text are
//! redacted in full first, then bounded, then escaped. The cases are review
//! #4904's reproductions; every credential here is fake.

use std::fs;

use super::review::{call, Cluster};
use super::{assert_no_userinfo, find, review, run, userinfo_url, TempDir, FAKE, NOW};
use crate::docgraph::checks::Status;
use crate::docgraph::neptune_input::validate_all;

/// An AWS access-key id that starts `pad` characters into the text, so a
/// 120-character bound cuts it in half when `pad` is about 110.
fn key_across_the_bound(pad: usize) -> String {
    format!("{} AKIAQQQQQQQQQQQQQQQQ tail", "y".repeat(pad))
}

// ------------------------------------------------------------ #4908

#[test]
fn a_rejected_value_is_redacted_before_it_is_bounded() {
    let value = userinfo_url(200, "");
    for key in ["cluster_id", "graph_id", "kms_key", "s3_uri", "snapshot_id"] {
        let errors = validate_all(&[(key.to_string(), value.clone())]);
        assert_eq!(errors.len(), 1, "{key}");
        assert_no_userinfo(&errors[0]);
        assert!(errors[0].contains("[REDACTED]"), "{}", errors[0]);
    }
    let errors = validate_all(&[("subnet_ids".to_string(), format!("subnet-1,{value}"))]);
    assert_no_userinfo(&errors.join("\n"));

    // An access-key id that straddles the bound is not shown in part.
    let errors = validate_all(&[("cluster_id".to_string(), key_across_the_bound(110))]);
    assert!(!errors[0].contains("AKIAQQQQ"), "{}", errors[0]);
}

#[test]
fn the_validate_command_never_echoes_userinfo_past_the_bound() {
    let value = userinfo_url(200, "");
    let ran = run(&["neptune", "validate-input", &format!("cluster_id={value}")]);
    assert_eq!(ran.status, 1);
    assert_no_userinfo(&ran.stdout);
    assert_no_userinfo(&ran.stderr);

    // Without '=' the whole argument is echoed as invalid.
    let ran = run(&["neptune", "validate-input", &value]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stdout.starts_with("Invalid argument: "),
        "{}",
        ran.stdout
    );
    assert_no_userinfo(&ran.stdout);

    // As an unknown key.
    let ran = run(&["neptune", "validate-input", &format!("{value}=1")]);
    assert_eq!(ran.status, 1);
    assert_no_userinfo(&ran.stdout);
}

fn failover_review(message: &str) -> String {
    let events =
        format!(r#"{{"Events": [{{"Message": "{message}", "EventCategories": ["failover"]}}]}}"#);
    let text = Cluster::default().recording().replace(
        &call("docdb", "DescribeEvents", None, r#"{"Events": []}"#),
        &call("docdb", "DescribeEvents", None, &events),
    );
    let findings = review(&text);
    let rel7 = &find(&findings, "REL7")[0];
    assert_eq!(rel7.status, Status::Warn, "{}", rel7.label);
    rel7.detail.clone()
}

#[test]
fn a_recorded_failover_message_is_redacted_before_it_is_bounded() {
    let detail = failover_review(&format!("Failover to {}", userinfo_url(200, "")));
    assert_no_userinfo(&detail);
    assert!(
        detail.starts_with("Most recent: Failover to https://"),
        "{detail}"
    );

    let detail = failover_review(&format!("failover{}", key_across_the_bound(100)));
    assert!(!detail.contains("AKIAQQQQ"), "{detail}");

    // An ordinary message keeps the script's 120-character prefix.
    let plain = format!("failover {}", "z".repeat(200));
    let detail = failover_review(&plain);
    assert_eq!(
        detail,
        format!(
            "Most recent: {}",
            plain.chars().take(120).collect::<String>()
        )
    );
}

// ------------------------------------------------------------ #4907

/// The caller paths of the reproductions: userinfo right before the host and
/// userinfo pushed 200 characters past the password.
fn userinfo_paths(tail: &str) -> [String; 2] {
    [userinfo_url(0, tail), userinfo_url(200, tail)]
}

fn wa_review(extra: &[&str], output: &str) -> super::Ran {
    let mut words = vec![
        "documentdb",
        "wa-review",
        "--cluster-id",
        "prod-docdb",
        "--region",
        "us-east-1",
        "--output",
        output,
        "--now",
        NOW,
    ];
    words.extend_from_slice(extra);
    run(&words)
}

#[test]
fn a_missing_recorded_file_is_named_without_its_userinfo() {
    let dir = TempDir::new("redact-recorded");
    let output = dir.join("o").to_string_lossy().into_owned();
    for path in userinfo_paths("missing.json") {
        let ran = run(&["neptune", "seed", "--confirm-wipe", "--recorded", &path]);
        assert_eq!(ran.status, 1);
        assert_no_userinfo(&ran.stderr);
        assert_no_userinfo(&ran.stdout);
        assert!(ran.stderr.contains("[REDACTED]"), "{}", ran.stderr);
        assert!(ran.stderr.contains("missing.json"), "{}", ran.stderr);

        let ran = wa_review(&["--recorded", &path], &output);
        assert_eq!(ran.status, 1);
        assert_no_userinfo(&ran.stderr);
        assert_no_userinfo(&ran.stdout);
        assert!(ran.stderr.contains("missing.json"), "{}", ran.stderr);
    }
}

#[test]
fn a_missing_or_malformed_analysis_file_is_named_without_its_userinfo() {
    let dir = TempDir::new("redact-analysis");
    let output = dir.join("o").to_string_lossy().into_owned();
    let recorded = dir.write("r.json", &Cluster::default().recording());
    for path in userinfo_paths("missing.json") {
        let ran = wa_review(
            &["--recorded", &recorded, "--analysis-data", &path],
            &output,
        );
        assert_eq!(ran.status, 1);
        // The progress line and the error both name the file.
        assert!(
            ran.stdout.contains("Loading database stats from"),
            "{}",
            ran.stdout
        );
        assert_no_userinfo(&ran.stdout);
        assert_no_userinfo(&ran.stderr);
        assert!(ran.stderr.contains("missing.json"), "{}", ran.stderr);
    }
    // An existing file below a userinfo-shaped directory, not valid JSON.
    let malformed = format!(
        "{}/{}",
        dir.to_string_lossy(),
        userinfo_url(200, "bad.json")
    );
    let parent = std::path::Path::new(&malformed).parent().expect("parent");
    fs::create_dir_all(parent).expect("create userinfo-shaped directory");
    fs::write(&malformed, "{not json").expect("write malformed file");
    let ran = wa_review(
        &["--recorded", &recorded, "--analysis-data", &malformed],
        &output,
    );
    assert_eq!(ran.status, 1);
    assert!(ran.stderr.contains("invalid JSON"), "{}", ran.stderr);
    assert_no_userinfo(&ran.stderr);
    assert_no_userinfo(&ran.stdout);
    let ran = run(&[
        "neptune",
        "seed",
        "--confirm-wipe",
        "--recorded",
        &malformed,
    ]);
    assert_eq!(ran.status, 1);
    assert!(ran.stderr.contains("invalid JSON"), "{}", ran.stderr);
    assert_no_userinfo(&ran.stderr);
}

#[test]
fn the_output_banners_name_the_files_without_userinfo() {
    let dir = TempDir::new("redact-output");
    let recorded = dir.write("r.json", &Cluster::default().recording());
    let output = format!("{}/{}", dir.to_string_lossy(), userinfo_url(200, "out"));
    let ran = wa_review(&["--recorded", &recorded], &output);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains("  Results: "), "{}", ran.stdout);
    assert!(
        ran.stdout.contains("/out/wa_review_results.json"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains("/out/wa_review_report.md"),
        "{}",
        ran.stdout
    );
    assert_no_userinfo(&ran.stdout);
    assert_no_userinfo(&ran.stderr);
}

#[test]
fn labelled_credentials_and_newlines_in_a_path_stay_on_one_redacted_line() {
    let dir = TempDir::new("redact-labelled");
    let output = dir.join("o").to_string_lossy().into_owned();
    let path = format!("dir/password={FAKE}\nerror: forged/missing.json");
    let ran = run(&["neptune", "seed", "--confirm-wipe", "--recorded", &path]);
    assert_eq!(ran.status, 1);
    assert!(!ran.stderr.contains(FAKE), "{}", ran.stderr);
    assert_eq!(ran.stderr.trim_end().lines().count(), 1, "{}", ran.stderr);
    let ran = wa_review(&["--recorded", &path], &output);
    assert_eq!(ran.status, 1);
    assert!(!ran.stderr.contains(FAKE), "{}", ran.stderr);
    assert_eq!(ran.stderr.trim_end().lines().count(), 1, "{}", ran.stderr);

    // A normal path is shown as given.
    let normal = dir
        .join("plain-missing.json")
        .to_string_lossy()
        .into_owned();
    let ran = run(&["neptune", "seed", "--confirm-wipe", "--recorded", &normal]);
    assert_eq!(ran.status, 1);
    assert!(ran.stderr.contains(&normal), "{}", ran.stderr);
}
