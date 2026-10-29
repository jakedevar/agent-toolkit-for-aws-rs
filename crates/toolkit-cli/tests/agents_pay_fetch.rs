//! x402 S5 (#5352): the `toolkit agents-pay fetch` route (HarnessCli).
//!
//! Handler behaviour runs through `agents_pay::run_with` over fixture
//! `Deps`; the built binary runs only the D32 path of a verification build,
//! which refuses before any configuration, network or credential access.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::process::{Command, Stdio};

use toolkit_cli::agents_pay::agentcore::FakePaymentsApi;
use toolkit_cli::agents_pay::deps::fixtures::{FakeClock, FakeMerchant, TempConfigStore};
use toolkit_cli::agents_pay::deps::{Deps, NO_PAYMENT_SUPPORT};
use toolkit_cli::agents_pay::policy::FixedResolver;
use toolkit_cli::{Invocation, Output};
use toolkit_core::{parse_json, Json};

const USAGE: &str = "usage: toolkit agents-pay fetch [URL] [--status] [--browser-handle URL] [--method GET|HEAD] [--purchase-id ID]";

fn invocation(positionals: &[&str], flags: &[&str], values: &[(&str, &str)]) -> Invocation {
    Invocation {
        positionals: positionals.iter().map(|s| (*s).to_owned()).collect(),
        flags: flags.iter().map(|s| (*s).to_owned()).collect(),
        values: values
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run_fixture(invocation: &Invocation, merchant: &FakeMerchant) -> Ran {
    let store = TempConfigStore::new(
        "cli",
        &parse_json(
            r#"{"resources":{},"policy":{"max_per_payment_usd":"0.10","allowed_networks":["eip155:84532"],"allowed_recipients":["0x1111111111111111111111111111111111111111"]}}"#,
        )
        .unwrap(),
    );
    let api = FakePaymentsApi::signing();
    let resolver = FixedResolver::new();
    let clock = FakeClock::at_unix(1_800_000_000);
    let deps = Deps {
        config: &store,
        payments: &api,
        merchant,
        resolver: &resolver,
        clock: &clock,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = {
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        match toolkit_cli::agents_pay::run_with(invocation, &deps, &mut output) {
            Ok(()) => output.exit_status(),
            Err(error) => error.exit_code(),
        }
    };
    assert_eq!(api.calls(), 0);
    Ran {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

fn single_json(stdout: &str) -> Json {
    assert!(stdout.ends_with('\n'), "{stdout:?}");
    assert_eq!(stdout.matches('\n').count(), 1, "{stdout:?}");
    parse_json(stdout.trim_end()).unwrap()
}

#[test]
fn harness_cli_refusal_exits_2_with_a_single_json_document() {
    let merchant = FakeMerchant::new();
    let ran = run_fixture(
        &invocation(&["https://169.254.169.254/latest/meta-data/"], &[], &[]),
        &merchant,
    );
    assert_eq!(ran.code, 2);
    let out = single_json(&ran.stdout);
    assert_eq!(out.get("refused"), Some(&Json::Bool(true)));
    assert_eq!(
        ran.stdout,
        "{\"paid\": false, \"refused\": true, \"reason\": \"Payment URL resolves to a non-public address.\"}\n"
    );
    assert!(ran.stderr.is_empty());
    assert!(merchant.requests().is_empty());
}

#[test]
fn harness_cli_status_wins_and_reports_unusable_without_a_session() {
    let merchant = FakeMerchant::new();
    let ran = run_fixture(
        &invocation(&["https://merchant.test/x"], &["--status"], &[]),
        &merchant,
    );
    assert_eq!(ran.code, 2);
    assert_eq!(
        single_json(&ran.stdout).get("usable"),
        Some(&Json::Bool(false))
    );
    assert!(merchant.requests().is_empty());
}

#[test]
fn harness_cli_browser_handle_wins_over_the_positional() {
    let merchant = FakeMerchant::new();
    merchant.respond(200, &[], b"free");
    let ran = run_fixture(
        &invocation(
            &["https://other.test/x"],
            &[],
            &[("--browser-handle", "https://93.184.216.34/paid")],
        ),
        &merchant,
    );
    assert_eq!(ran.code, 2);
    assert_eq!(
        ran.stdout,
        "{\"paid\": false, \"refused\": true, \"reason\": \"URL returned 200, not 402. No payment needed.\"}\n"
    );
    assert_eq!(merchant.requests()[0].url, "https://93.184.216.34/paid");
}

#[test]
fn harness_cli_a_free_url_exits_0() {
    let merchant = FakeMerchant::new();
    merchant.respond(200, &[], b"free");
    let ran = run_fixture(
        &invocation(&["https://93.184.216.34/free"], &[], &[("--method", "HEAD")]),
        &merchant,
    );
    assert_eq!(ran.code, 0, "{}", ran.stdout);
    assert_eq!(
        single_json(&ran.stdout).get("status_code"),
        Some(&Json::Int(200))
    );
    assert_eq!(merchant.requests()[0].method, "HEAD");
}

#[test]
fn harness_cli_no_url_and_no_flag_is_a_usage_error_exit_1() {
    let ran = run_fixture(&invocation(&[], &[], &[]), &FakeMerchant::new());
    assert_eq!(ran.code, 1);
    assert!(ran.stdout.is_empty());
    assert_eq!(ran.stderr, format!("{USAGE}\n\nGive a URL, or --status.\n"));
}

#[test]
fn harness_cli_method_is_strict() {
    for method in ["POST", "get", "head"] {
        let ran = run_fixture(
            &invocation(&["https://93.184.216.34/x"], &[], &[("--method", method)]),
            &FakeMerchant::new(),
        );
        assert_eq!(ran.code, 2, "{method}");
        assert!(ran.stdout.is_empty());
    }
}

// ------------------------------------------- the binary (D32 build only)

fn toolkit(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn harness_cli_binary_refuses_without_payment_support() {
    for args in [
        &["agents-pay", "fetch", "https://169.254.169.254/"][..],
        &["agents-pay", "fetch", "--browser-handle", "https://merchant.test/x"],
    ] {
        let out = toolkit(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            stdout,
            format!("{{\"paid\": false, \"refused\": true, \"reason\": \"{NO_PAYMENT_SUPPORT}\"}}\n")
        );
        assert!(out.stderr.is_empty());
    }
    let out = toolkit(&["agents-pay", "fetch", "--status"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{{\"usable\": false, \"reason\": \"{NO_PAYMENT_SUPPORT}\"}}\n")
    );
}

#[test]
fn harness_cli_binary_usage_errors() {
    let out = toolkit(&["agents-pay", "fetch"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("{USAGE}\n\nGive a URL, or --status.\n")
    );
    for args in [
        &["agents-pay", "fetch", "https://merchant.test/x", "--method", "POST"][..],
        &["agents-pay", "fetch", "a", "b"],
        &["agents-pay", "fetch", "--unknown"],
        &["agents-pay", "fetch", "https://merchant.test/x", "--purchase-id", "-"],
        &["agents-pay", "fetch", "https://merchant.test/x", "--secret-access-key=wJalrXUtnFEMI"],
    ] {
        let out = toolkit(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    // A pasted credential is refused before any work (R-SEC).
    let out = toolkit(&[
        "agents-pay",
        "fetch",
        "https://merchant.test/x",
        "--purchase-id",
        "AKIAIOSFODNN7EXAMPLE",
    ]);
    assert_ne!(out.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("AKIAIOSFODNN7EXAMPLE"));
}

#[test]
fn harness_cli_needs_no_framework_import() {
    let source = include_str!("../src/agents_pay/flow.rs").to_lowercase();
    for framework in ["strands", "langgraph", "langchain", "crewai"] {
        assert!(!source.contains(framework), "{framework}");
    }
}
