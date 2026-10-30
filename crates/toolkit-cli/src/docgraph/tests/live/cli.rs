//! #5507: command dispatch over injected signing/HTTP seams only. The
//! production constructor is never called by these tests.

use std::cell::Cell;
use std::fs;

use toolkit_aws::testing::RecordingSleeper;
use toolkit_aws::{AwsError, NoJitter};
use toolkit_core::{Json, ToolkitError};

use super::{
    client, healthy_wire, run_infra_checks, FailingSigner, SeamFixture, Seen, ToolkitSigner, Wire,
    NOW, NOW_SECONDS, REGION, SECRET,
};
use crate::docgraph::checks::Findings;
use crate::docgraph::live::LiveAwsClient;
use crate::docgraph::tests::{run, Ran, TempDir};
use crate::docgraph::{report, wa_review_with_live};
use crate::{Invocation, Output};

fn invocation(dir: &TempDir) -> Invocation {
    Invocation {
        values: vec![
            ("--cluster-id".into(), "prod-docdb".into()),
            ("--region".into(), REGION.into()),
            ("--now".into(), NOW.into()),
            (
                "--output".into(),
                dir.join("reports").to_str().unwrap().into(),
            ),
        ],
        ..Invocation::default()
    }
}

fn dispatch(
    invocation: &Invocation,
    live: impl FnOnce(&str, &str, i64) -> Result<Findings, ToolkitError>,
) -> (Result<(), ToolkitError>, Ran) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut out = Output::with_diagnostics(&mut stdout, &mut stderr);
    let result = wa_review_with_live(invocation, &mut out, live);
    let status = result
        .as_ref()
        .map_or_else(ToolkitError::exit_code, |()| out.exit_status());
    (
        result,
        Ran {
            status,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
        },
    )
}

fn cli(invocation: &Invocation) -> Ran {
    let mut words = vec!["documentdb", "wa-review"];
    words.extend(invocation.flags.iter().map(String::as_str));
    for (flag, value) in &invocation.values {
        words.extend([flag.as_str(), value.as_str()]);
    }
    run(&words)
}

fn reports(dir: &TempDir) -> [Vec<u8>; 2] {
    [report::RESULTS_FILE, report::REPORT_FILE]
        .map(|name| fs::read(dir.join("reports").join(name)).unwrap())
}

/// An independent seam fixture, serialized in the existing recording schema.
fn recording() -> String {
    let mut fixture = SeamFixture;
    let mut seen = Seen {
        inner: &mut fixture,
        calls: Vec::new(),
    };
    run_infra_checks(&mut seen, "prod-docdb", NOW_SECONDS);
    Json::object(vec![(
        "calls",
        Json::Array(
            seen.calls
                .into_iter()
                .map(|(service, operation, request, response)| {
                    Json::object(vec![
                        ("service", Json::string(service.name())),
                        ("operation", Json::string(operation)),
                        ("request", request),
                        ("response", response.unwrap()),
                    ])
                })
                .collect(),
        ),
    )])
    .to_pretty_string()
}

#[test]
fn t_5507_live_review_matches_recorded_cli_with_and_without_analysis() {
    for (analysis, signing_region) in [(false, REGION), (true, "eu-west-1")] {
        let dir = TempDir::new("review-routing");
        let mut args = invocation(&dir);
        args.values.retain(|(flag, _)| flag != "--region");
        args.values.push(("--region".into(), signing_region.into()));
        args.flags.push("--live".into());
        if analysis {
            args.values.push((
                "--analysis-data".into(),
                dir.write(
                    "analysis.json",
                    include_str!("../capture_fixtures/expected_snapshot.json"),
                ),
            ));
        }
        let transport = Wire::new(healthy_wire);
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let constructions = Cell::new(0);
        let (result, live) = dispatch(&args, |region, cluster_id, now| {
            constructions.set(constructions.get() + 1);
            assert_eq!(
                (region, cluster_id, now),
                (signing_region, "prod-docdb", NOW_SECONDS)
            );
            let jitter = NoJitter;
            let mut client = LiveAwsClient::new(region, &transport, &signer, &sleeper, &jitter)
                .expect("fixture client");
            Ok(run_infra_checks(&mut client, cluster_id, now))
        });
        result.unwrap();
        assert_eq!(live.status, 0);
        assert!(live.stderr.is_empty(), "{}", live.stderr);
        assert_eq!(constructions.get(), 1);
        assert!(transport.sends() > 10);
        for service in ["rds", "monitoring", "ec2", "secretsmanager"] {
            assert!(
                transport.requests().iter().any(|request| {
                    request.url.starts_with(&format!(
                        "https://{service}.{signing_region}.amazonaws.com/"
                    ))
                }),
                "{service} was not exercised in {signing_region}"
            );
        }
        assert!(signer
            .log
            .borrow()
            .iter()
            .all(|entry| entry.scope.region == signing_region));
        assert_eq!(transport.sends(), signer.count());
        assert!(sleeper.sleeps().is_empty());
        assert_eq!(
            live.stdout.contains("Running database-level checks"),
            analysis
        );
        let live_reports = reports(&dir);
        #[cfg(unix)]
        if analysis {
            use std::os::unix::fs::PermissionsExt;
            for name in [report::RESULTS_FILE, report::REPORT_FILE] {
                assert_eq!(
                    fs::metadata(dir.join("reports").join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        fs::remove_dir_all(dir.join("reports")).unwrap();
        args.flags.clear();
        args.values.push((
            "--recorded".into(),
            dir.write("recording.json", &recording()),
        ));
        let recorded = cli(&args);
        assert_eq!(recorded.status, live.status, "{}", recorded.stderr);
        assert_eq!(recorded.stdout, live.stdout);
        assert_eq!(recorded.stderr, live.stderr);
        assert_eq!(reports(&dir), live_reports);
    }
}

#[test]
fn t_5507_recorded_and_no_live_paths_never_construct_the_live_adapter() {
    for recorded in [false, true] {
        let dir = TempDir::new("review-offline");
        let mut args = invocation(&dir);
        if recorded {
            args.values.push((
                "--recorded".into(),
                dir.write("recording.json", &recording()),
            ));
        }
        let baseline = cli(&args);
        if !recorded {
            assert_eq!(baseline.status, 1);
            assert!(baseline
                .stderr
                .contains("live data requires explicit --live"));
            assert!(!dir.join("reports").exists());
            let (result, got) = dispatch(&args, |_, _, _| {
                panic!("missing mode cannot construct an adapter")
            });
            let error = result.unwrap_err();
            assert!(error
                .message()
                .contains("live data requires explicit --live"));
            assert_eq!(got.status, baseline.status);
            assert_eq!(got.stdout, baseline.stdout);
            continue;
        }
        assert_eq!(baseline.status, 0, "{}", baseline.stderr);
        let baseline_reports = reports(&dir);
        fs::remove_dir_all(dir.join("reports")).unwrap();
        let (result, got) = dispatch(&args, |_, _, _| {
            panic!("offline paths cannot construct an adapter")
        });
        result.unwrap();
        assert_eq!(got.status, baseline.status);
        assert_eq!(got.stdout, baseline.stdout);
        assert_eq!(got.stderr, baseline.stderr);
        assert_eq!(reports(&dir), baseline_reports);
        assert!(got.stderr.is_empty());
    }
}

#[test]
fn t_5507_live_preflight_does_not_enter_the_adapter_factory() {
    let dir = TempDir::new("review-factory-preflight");
    for (key, value) in [
        ("--recorded", "/does/not/exist"),
        ("--network-policy", "/does/not/exist"),
        ("--region", ""),
        ("--region", "invalid"),
        ("--region", "cn-north-1"),
        ("--region", "us-gov-west-1"),
        ("--region", "us-iso-east-1"),
        ("--region", "eu-isoe-west-1"),
        ("--cluster-id", "bad/id"),
        ("--now", "invalid"),
    ] {
        let mut args = invocation(&dir);
        args.flags.push("--live".into());
        args.values.retain(|(flag, _)| flag != key);
        if !value.is_empty() {
            args.values.push((key.into(), value.into()));
        }
        args.values
            .push(("--analysis-data".into(), "/does/not/exist".into()));
        let (result, got) = dispatch(&args, |_, _, _| {
            panic!("preflight must finish before construction")
        });
        assert_eq!(result.unwrap_err().exit_code(), 2, "{key}={value}");
        assert!(got.stdout.is_empty());
        assert!(got.stderr.is_empty());
        assert!(!dir.join("reports").exists());
    }
    let mut args = invocation(&dir);
    args.flags.extend(["--live".into(), "--dry-run".into()]);
    let (result, got) = dispatch(&args, |_, _, _| panic!("preflight entered adapter"));
    assert_eq!(result.unwrap_err().exit_code(), 2);
    assert!(got.stdout.is_empty());
    assert!(got.stderr.is_empty());
    assert!(!dir.join("reports").exists());
}

#[test]
fn t_5507_live_credential_failure_is_a_redacted_finding_with_zero_sends() {
    let dir = TempDir::new("review-credential-failure");
    let mut args = invocation(&dir);
    args.flags.push("--live".into());
    let transport = Wire::new(|_, _| panic!("failed credentials cannot reach HTTP"));
    let signer = FailingSigner {
        error: AwsError::NoCredentials,
        calls: Cell::new(0),
    };
    let sleeper = RecordingSleeper::new();
    let (result, got) = dispatch(&args, |_, cluster_id, now| {
        Ok(run_infra_checks(
            &mut client(&transport, &signer, &sleeper),
            cluster_id,
            now,
        ))
    });
    result.unwrap();
    assert_eq!(
        got.status, 0,
        "findings retain the existing review exit contract"
    );
    assert!(got.stderr.is_empty());
    assert_eq!(signer.calls.get(), 1);
    assert_eq!(transport.sends(), 0);
    assert!(sleeper.sleeps().is_empty());
    let [json, markdown] = reports(&dir).map(|bytes| String::from_utf8(bytes).unwrap());
    assert!(json.contains("Cannot describe cluster: aws_no_credentials"));
    // The existing Markdown format summarizes Other/ERR findings in its
    // counts; it renders rows only for the six named pillars.
    assert!(markdown.contains("**Summary:** 0 pass, 0 warnings, 1 failures, 0 info"));
    for text in [&got.stdout, &got.stderr, &json, &markdown] {
        assert!(!text.contains("no live AWS client"));
        assert!(!text.contains(SECRET));
    }
}

#[test]
fn t_5507_live_setup_failure_is_runtime_error_without_input_reads_or_reports() {
    let dir = TempDir::new("review-setup-failure");
    let mut args = invocation(&dir);
    args.flags.push("--live".into());
    args.values
        .push(("--analysis-data".into(), "/does/not/exist".into()));
    let (result, got) = dispatch(&args, |_, _, _| {
        Err(ToolkitError::runtime(
            "docgraph_review",
            "configured TLS roots are invalid",
        ))
    });
    let error = result.unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert_eq!(error.code(), "docgraph_review");
    assert!(got
        .stdout
        .ends_with("  Running infrastructure checks (AWS APIs)...\n"));
    assert!(got.stderr.is_empty());
    assert!(!dir.join("reports").exists());
}
