//! Offline, deterministic tests for `toolkit bedrock-probes` (#4404).
//!
//! They port `test_iam_policy.py`, `test_preflight_bedrock.py`,
//! `test_source_baseline.py` and the `iam_policy` / `preflight_bedrock` cases of
//! `test_astra_execution_contract.py` (#4608), and add the negative cases of the
//! Rust contract: recorded inputs only (no network, no credentials, no token),
//! fail-closed probes, no wildcard beyond the ones the Python script emitted,
//! links and a bare `-` refused, and hostile caller text never reaching a
//! diagnostic or a row raw.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_aws::AwsError;
use toolkit_core::{parse_json, Json};

use super::baseline::{self, Provider};
use super::client::{ProbeClient, ProbeError, Service};
use super::http::{HttpClient, HttpFailure};
use super::iam;
use super::preflight::{self, Options, Verdict};
use crate::bedrock_resolve::image::base64_encode;
use crate::safe_text::is_unsafe;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-bedrock-probes-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn file(&self, name: &str, content: &str) -> String {
        self.bytes(name, content.as_bytes())
    }

    fn bytes(&self, name: &str, content: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, content).expect("write fixture");
        path.to_string_lossy().into_owned()
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn link(&self, name: &str, target: &str) -> String {
        let path = self.0.join(name);
        std::os::unix::fs::symlink(target, &path).expect("create symlink");
        path.to_string_lossy().into_owned()
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exec(args: &[&str]) -> (u8, String, String) {
    let os: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = crate::run(&os, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn json(text: &str) -> Json {
    parse_json(text).expect("valid JSON")
}

fn strings(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

/// Text that tries every trick the diagnostic rule guards against.
const HOSTILE: &str = "x\u{1b}[31m\u{7}\r\nFORGED: line\u{202e}gnp.exe";
/// A fake credential; it must never be printed.
const SECRET: &str = "FAKE-SECRET-MARKER-9f3a7c1e";

/// No output line holds an unsafe character, and no line was forged.
fn assert_clean(label: &str, text: &str) {
    for line in text.lines() {
        assert!(
            !line.chars().any(is_unsafe),
            "{label} holds an unsafe character: {line:?}"
        );
        assert!(
            !line.starts_with("FORGED"),
            "{label} holds a forged line: {line:?}"
        );
    }
    assert!(!text.contains(SECRET), "{label} holds the secret: {text:?}");
}

const REGION: &str = "us-east-1";
const ACCOUNT: &str = "123456789012";

// ---------------------------------------------------------------------------
// iam_policy (test_iam_policy.py)

fn policy(models: &[&str], region: &str, account: &str) -> Json {
    iam::generate_policy(&strings(models), region, account)
}

fn statements(policy: &Json) -> Vec<Json> {
    match policy.get("Statement") {
        Some(Json::Array(items)) => items.clone(),
        other => panic!("no statements: {other:?}"),
    }
}

fn sid(statement: &Json) -> &str {
    statement.get("Sid").and_then(Json::as_str).expect("a Sid")
}

fn resources(statement: &Json) -> Vec<String> {
    match statement.get("Resource") {
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| item.as_str().expect("a text resource").to_string())
            .collect(),
        Some(Json::String(text)) => vec![text.clone()],
        other => panic!("no resource: {other:?}"),
    }
}

fn actions(statement: &Json) -> Vec<String> {
    match statement.get("Action") {
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| item.as_str().expect("a text action").to_string())
            .collect(),
        other => panic!("no actions: {other:?}"),
    }
}

fn sids(policy: &Json) -> Vec<String> {
    statements(policy)
        .iter()
        .map(|statement| sid(statement).to_string())
        .collect()
}

fn statement_named(policy: &Json, name: &str) -> Json {
    statements(policy)
        .into_iter()
        .find(|statement| sid(statement) == name)
        .unwrap_or_else(|| panic!("no statement {name}"))
}

#[test]
fn inference_profile_detection() {
    assert!(iam::is_inference_profile("us.anthropic.claude-sonnet-5"));
    assert!(iam::is_inference_profile(
        "eu.anthropic.claude-haiku-4-5-20251001-v1:0"
    ));
    for prefix in ["apac", "au", "jp", "global"] {
        assert!(iam::is_inference_profile(&format!(
            "{prefix}.amazon.nova-lite-v1:0"
        )));
    }
    assert!(!iam::is_inference_profile(
        "anthropic.claude-haiku-4-5-20251001-v1:0"
    ));
    assert!(!iam::is_inference_profile("amazon.nova-lite-v1:0"));
    // `usa.` is not a geo prefix.
    assert!(!iam::is_inference_profile("usa.anthropic.claude"));
}

#[test]
fn profile_arns_are_recognized_exactly() {
    let arn = "arn:aws:bedrock:us-west-2:111122223333:inference-profile/us.openai.gpt-6-astra";
    assert!(iam::is_inference_profile(arn));
    for bad in [
        "arn:aws:bedrock:us-west-2:11112222333:inference-profile/x",
        "arn:aws:bedrock:us-west-2:1111222233334:inference-profile/x",
        "arn:aws:bedrock:us-west-2:11112222333a:inference-profile/x",
        "arn:aws:bedrock::111122223333:inference-profile/x",
        "arn:aws:bedrock:us-west-2:111122223333:inference-profile/",
        "arn:aws:bedrock:us-west-2:111122223333:inference-profile/a/b",
        "arn:aws:bedrock:us-west-2:111122223333:foundation-model/x",
        "arn:aws-cn:bedrock:cn-north-1:111122223333:inference-profile/x",
    ] {
        assert!(!iam::is_inference_profile(bad), "{bad}");
    }
}

#[test]
fn arn_generation() {
    assert_eq!(
        iam::foundation_model_arn("anthropic.claude-haiku-4-5-20251001-v1:0", ""),
        "arn:aws:bedrock:*::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0"
    );
    assert_eq!(
        iam::inference_profile_arn("us.anthropic.claude-sonnet-5", REGION, ACCOUNT),
        format!(
            "arn:aws:bedrock:{REGION}:{ACCOUNT}:inference-profile/us.anthropic.claude-sonnet-5"
        )
    );
    assert_eq!(
        iam::default_project_arn(REGION, ACCOUNT),
        format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:project/default")
    );
}

#[test]
fn partition_follows_the_region() {
    assert_eq!(
        iam::mantle_project_arn("us-gov-west-1", ACCOUNT),
        format!("arn:aws-us-gov:bedrock-mantle:us-gov-west-1:{ACCOUNT}:project/*")
    );
    assert_eq!(
        iam::foundation_model_arn("amazon.nova-lite-v1:0", "cn-north-1"),
        "arn:aws-cn:bedrock:*::foundation-model/amazon.nova-lite-v1:0"
    );
    assert_eq!(
        iam::default_project_arn("eu-west-2", ACCOUNT),
        format!("arn:aws:bedrock:eu-west-2:{ACCOUNT}:project/default")
    );
}

#[test]
fn single_foundation_model() {
    let document = policy(
        &["anthropic.claude-haiku-4-5-20251001-v1:0"],
        REGION,
        ACCOUNT,
    );
    assert_eq!(
        document.get("Version").and_then(Json::as_str),
        Some("2012-10-17")
    );
    let all = statements(&document);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].get("Effect").and_then(Json::as_str), Some("Allow"));
    assert_eq!(
        actions(&all[0]),
        [
            "bedrock:InvokeModel",
            "bedrock:InvokeModelWithResponseStream"
        ]
    );
    assert_eq!(
        resources(&all[0]),
        ["arn:aws:bedrock:*::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0"]
    );
}

#[test]
fn inference_profile_generates_dual_arns() {
    let document = policy(&["us.anthropic.claude-sonnet-5"], REGION, ACCOUNT);
    let found = resources(&statements(&document)[0]);
    assert_eq!(found.len(), 2);
    assert!(found.iter().any(|r| r.contains("foundation-model/")));
    assert!(found.iter().any(|r| r.contains("inference-profile/")));
}

#[test]
fn mixed_models_and_deduplication() {
    let document = policy(
        &["us.anthropic.claude-sonnet-5", "amazon.nova-lite-v1:0"],
        REGION,
        ACCOUNT,
    );
    let found = resources(&statements(&document)[0]);
    assert!(found.iter().any(|r| r.contains("nova-lite")));
    assert!(found.iter().any(|r| r.contains("inference-profile/")));
    let twice = policy(
        &["amazon.nova-lite-v1:0", "amazon.nova-lite-v1:0"],
        REGION,
        ACCOUNT,
    );
    assert_eq!(resources(&statements(&twice)[0]).len(), 1);
}

#[test]
fn an_empty_model_list_keeps_the_legacy_shape() {
    let document = policy(&[], REGION, ACCOUNT);
    let all = statements(&document);
    assert_eq!(all.len(), 1);
    assert!(resources(&all[0]).is_empty());
}

#[test]
fn resources_are_sorted_and_unique() {
    let document = policy(
        &["us.amazon.nova-lite-v1:0", "amazon.nova-lite-v1:0"],
        REGION,
        ACCOUNT,
    );
    // The profile's base model and the plain id are the same foundation model.
    let found = resources(&statements(&document)[0]);
    let mut expected = found.clone();
    expected.sort();
    expected.dedup();
    assert_eq!(found, expected);
    assert_eq!(found.len(), 2);
}

#[test]
fn mantle_model_detection() {
    for id in [
        "openai.gpt-5.6-terra",
        "openai.gpt-5.5",
        "openai.gpt-6-astra",
        "openai.gpt-6-sol",
        "openai.gpt-6-luna",
        "openai.gpt-6.1-sol",
        "OpenAI.GPT-6-Astra",
    ] {
        assert!(iam::is_mantle_model(id, REGION), "{id}");
        assert!(preflight::is_mantle_model(id), "{id}");
    }
    for id in [
        "openai.gpt-oss-120b-1:0",
        "anthropic.claude-sonnet-4-6",
        "us.openai.gpt-6-sol",
        "global.openai.gpt-6.1-sol",
        "us.openai.gpt-5.6-sol",
        "in.openai.gpt-5.6-luna",
        "global.openai.gpt-5.6-terra",
        "amazon.nova-lite-v1:0",
    ] {
        assert!(!iam::is_mantle_model(id, REGION), "{id}");
        assert!(!preflight::is_mantle_model(id), "{id}");
    }
}

#[test]
fn bare_opus_5_5_is_mantle_except_in_eu_west_2() {
    assert!(iam::is_mantle_model(
        "anthropic.claude-opus-5-5",
        "us-west-2"
    ));
    assert!(iam::is_mantle_model("anthropic.claude-opus-5-5", ""));
    assert!(!iam::is_mantle_model(
        "anthropic.claude-opus-5-5",
        "eu-west-2"
    ));
    let eu = policy(&["anthropic.claude-opus-5-5"], "eu-west-2", ACCOUNT);
    assert_eq!(sids(&eu), ["BedrockInvokeModelScoped"]);
    let us = policy(&["anthropic.claude-opus-5-5"], "us-west-2", ACCOUNT);
    assert_eq!(
        sids(&us),
        ["BedrockMantleInference", "BedrockMantleCallWithBearerToken"]
    );
}

#[test]
fn bare_gpt6_gets_the_mantle_statements_only() {
    let document = policy(&["openai.gpt-6-sol"], REGION, ACCOUNT);
    let names = sids(&document);
    assert!(names.contains(&"BedrockMantleInference".to_string()));
    assert!(!names.contains(&"BedrockInvokeModelScoped".to_string()));
}

#[test]
fn an_all_mantle_run_never_emits_an_empty_resource_statement() {
    let document = policy(&["openai.gpt-5.6-terra"], REGION, "111122223333");
    let names = sids(&document);
    assert!(names.contains(&"BedrockMantleInference".to_string()));
    assert!(names.contains(&"BedrockMantleCallWithBearerToken".to_string()));
    assert!(!names.contains(&"BedrockInvokeModelScoped".to_string()));
    for statement in statements(&document) {
        assert!(!resources(&statement).is_empty(), "{}", sid(&statement));
    }
}

#[test]
fn bearer_token_statement_is_star_and_inference_is_account_scoped() {
    let document = policy(&["openai.gpt-5.5"], "us-west-2", "999988887777");
    let bearer = statement_named(&document, "BedrockMantleCallWithBearerToken");
    assert_eq!(resources(&bearer), ["*"]);
    assert_eq!(actions(&bearer), ["bedrock-mantle:CallWithBearerToken"]);
    let inference = statement_named(&document, "BedrockMantleInference");
    assert_eq!(
        resources(&inference),
        ["arn:aws:bedrock-mantle:us-west-2:999988887777:project/*"]
    );
    assert_eq!(
        actions(&inference),
        [
            "bedrock-mantle:CreateInference",
            "bedrock-mantle:Get*",
            "bedrock-mantle:List*"
        ]
    );
}

#[test]
fn mixed_targets_emit_runtime_and_mantle_statements() {
    let document = policy(
        &["openai.gpt-5.6-luna", "us.anthropic.claude-sonnet-4-6"],
        REGION,
        "111122223333",
    );
    let names = sids(&document);
    assert_eq!(
        names,
        [
            "BedrockInvokeModelScoped",
            "BedrockMantleInference",
            "BedrockMantleCallWithBearerToken"
        ]
    );
    let invoke = statement_named(&document, "BedrockInvokeModelScoped");
    assert!(!resources(&invoke).iter().any(|r| r.contains("gpt-5")));
}

#[test]
fn gpt_oss_stays_on_the_runtime_path() {
    let document = policy(&["openai.gpt-oss-120b-1:0"], REGION, "111122223333");
    assert_eq!(sids(&document), ["BedrockInvokeModelScoped"]);
}

#[test]
fn astra_bare_id_grants_mantle_project_and_bearer_actions() {
    let document = policy(&["openai.gpt-6-astra"], "us-west-2", ACCOUNT);
    assert_eq!(
        sids(&document),
        ["BedrockMantleInference", "BedrockMantleCallWithBearerToken"]
    );
    let inference = statement_named(&document, "BedrockMantleInference");
    assert_eq!(
        resources(&inference),
        [format!(
            "arn:aws:bedrock-mantle:us-west-2:{ACCOUNT}:project/*"
        )]
    );
    assert!(actions(&inference).contains(&"bedrock-mantle:CreateInference".to_string()));
}

#[test]
fn astra_cris_ids_grant_runtime_foundation_profile_and_project_arns() {
    for prefix in ["us", "global"] {
        let id = format!("{prefix}.openai.gpt-6-astra");
        assert!(!iam::is_mantle_model(&id, "us-west-2"));
        let document = iam::generate_policy(std::slice::from_ref(&id), "us-west-2", ACCOUNT);
        let all = statements(&document);
        assert_eq!(all.len(), 1);
        assert_eq!(
            actions(&all[0]),
            [
                "bedrock:InvokeModel",
                "bedrock:InvokeModelWithResponseStream"
            ]
        );
        let mut expected = vec![
            "arn:aws:bedrock:*::foundation-model/openai.gpt-6-astra".to_string(),
            format!("arn:aws:bedrock:us-west-2:{ACCOUNT}:inference-profile/{id}"),
            format!("arn:aws:bedrock:us-west-2:{ACCOUNT}:project/default"),
        ];
        expected.sort();
        assert_eq!(resources(&all[0]), expected);
    }
}

#[test]
fn gpt6_cris_detection() {
    for id in [
        "global.openai.gpt-6-sol",
        "us.openai.gpt-6-astra",
        "us.openai.gpt-6.1-sol",
        "global.openai.gpt-6-luna",
    ] {
        assert!(iam::is_gpt6_cris_model(id), "{id}");
        let found = resources(&statements(&policy(&[id], REGION, ACCOUNT))[0]);
        assert!(found.contains(&iam::default_project_arn(REGION, ACCOUNT)));
        assert!(found.iter().any(|r| r.contains("inference-profile/")));
    }
    for id in [
        "openai.gpt-6-sol",
        "us.anthropic.claude-sonnet-5",
        "us.openai.gpt-5.6-sol",
    ] {
        assert!(!iam::is_gpt6_cris_model(id), "{id}");
    }
    let plain = policy(&["us.anthropic.claude-sonnet-5"], REGION, ACCOUNT);
    assert!(!resources(&statements(&plain)[0]).contains(&iam::default_project_arn(REGION, ACCOUNT)));
}

#[test]
fn system_profile_arn_keeps_its_arn_and_base_model_and_default_project() {
    // test_astra_system_profile_arn_keeps_invocation_price_and_permissions
    // (the IAM half; the pricing half is #4403, the preflight half is below).
    for prefix in ["us", "global"] {
        let arn = format!(
            "arn:aws:bedrock:us-west-2:111122223333:inference-profile/{prefix}.openai.gpt-6-astra"
        );
        let document =
            iam::generate_policy(std::slice::from_ref(&arn), "us-west-2", "111122223333");
        let mut expected = vec![
            arn,
            "arn:aws:bedrock:*::foundation-model/openai.gpt-6-astra".to_string(),
            "arn:aws:bedrock:us-west-2:111122223333:project/default".to_string(),
        ];
        expected.sort();
        assert_eq!(resources(&statements(&document)[0]), expected);
    }
}

/// Every policy string with an IAM wildcard, over a policy that uses every
/// statement. The Python script emitted exactly these (the migration document
/// lists each one for review); nothing else may carry one.
#[test]
fn the_only_wildcards_are_the_documented_ones() {
    let document = policy(
        &[
            "openai.gpt-6-astra",
            "us.openai.gpt-6-astra",
            "amazon.nova-lite-v1:0",
        ],
        REGION,
        ACCOUNT,
    );
    let mut wild: Vec<String> = Vec::new();
    for statement in statements(&document) {
        for text in actions(&statement).into_iter().chain(resources(&statement)) {
            if text.contains('*') {
                wild.push(format!("{}: {text}", sid(&statement)));
            }
        }
    }
    wild.sort();
    wild.dedup();
    let expected = vec![
        "BedrockInvokeModelScoped: arn:aws:bedrock:*::foundation-model/amazon.nova-lite-v1:0"
            .to_string(),
        "BedrockInvokeModelScoped: arn:aws:bedrock:*::foundation-model/openai.gpt-6-astra"
            .to_string(),
        "BedrockMantleCallWithBearerToken: *".to_string(),
        format!("BedrockMantleInference: arn:aws:bedrock-mantle:{REGION}:{ACCOUNT}:project/*"),
        "BedrockMantleInference: bedrock-mantle:Get*".to_string(),
        "BedrockMantleInference: bedrock-mantle:List*".to_string(),
    ];
    assert_eq!(wild, expected);
}

#[test]
fn the_policy_text_matches_the_script_byte_for_byte() {
    let (status, out, _) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "openai.gpt-6-astra",
        "--region",
        "us-west-2",
        "--account-id",
        ACCOUNT,
    ]);
    assert_eq!(status, 0);
    let expected = r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "BedrockMantleInference",
      "Effect": "Allow",
      "Action": [
        "bedrock-mantle:CreateInference",
        "bedrock-mantle:Get*",
        "bedrock-mantle:List*"
      ],
      "Resource": "arn:aws:bedrock-mantle:us-west-2:123456789012:project/*"
    },
    {
      "Sid": "BedrockMantleCallWithBearerToken",
      "Effect": "Allow",
      "Action": [
        "bedrock-mantle:CallWithBearerToken"
      ],
      "Resource": "*"
    }
  ]
}
"#;
    // `print(json.dumps(policy, indent=2) + "\n")`: one blank line at the end.
    assert_eq!(out, format!("{expected}\n"));
}

#[test]
fn iam_policy_cli_writes_the_file_and_reports_on_stderr() {
    let dir = TempDir::new("iam-file");
    let out_path = dir.path("policy.json");
    let (status, out, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "us.anthropic.claude-sonnet-5,amazon.nova-lite-v1:0",
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
        "--output",
        &out_path,
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(out.is_empty());
    assert_eq!(err, format!("Policy written to {out_path}\n"));
    let written = fs::read_to_string(&out_path).expect("policy file");
    assert!(written.ends_with("}\n"));
    let document = json(&written);
    assert_eq!(resources(&statements(&document)[0]).len(), 3);
}

#[test]
fn iam_policy_cli_prints_the_scoped_statement_to_stdout() {
    let (status, out, _) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "amazon.nova-lite-v1:0",
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
    ]);
    assert_eq!(status, 0);
    let document = json(&out);
    assert_eq!(sid(&statements(&document)[0]), "BedrockInvokeModelScoped");
}

#[test]
fn iam_policy_cli_refuses_inputs_that_would_widen_the_policy() {
    let base = |models: &'static str, region: &'static str, account: &'static str| {
        exec(&[
            "bedrock-probes",
            "iam-policy",
            "--models",
            models,
            "--region",
            region,
            "--account-id",
            account,
        ])
    };
    for (models, region, account) in [
        ("*", REGION, ACCOUNT),
        ("amazon.nova-*", REGION, ACCOUNT),
        ("amazon.nova-lite-v1:?", REGION, ACCOUNT),
        ("amazon.${aws:username}", REGION, ACCOUNT),
        ("amazon.nova-lite-v1:0", "us-east-*", ACCOUNT),
        ("amazon.nova-lite-v1:0", "*", ACCOUNT),
        ("amazon.nova-lite-v1:0", "not a region", ACCOUNT),
        ("amazon.nova-lite-v1:0", REGION, "*"),
        ("amazon.nova-lite-v1:0", REGION, "12345678901"),
        ("amazon.nova-lite-v1:0", REGION, "1234567890123"),
        (
            "arn:aws:bedrock:*:111122223333:inference-profile/x",
            REGION,
            ACCOUNT,
        ),
    ] {
        let (status, out, err) = base(models, region, account);
        assert_eq!(status, 2, "{models} {region} {account}");
        assert!(out.is_empty());
        assert!(err.starts_with("error: invalid arguments"), "{err}");
    }
}

#[test]
fn iam_policy_cli_never_treats_a_bare_dash_as_a_file() {
    let before = Path::new("-").exists();
    for args in [
        vec!["--output", "-"],
        vec!["--output=-"],
        vec!["--models", "-"],
        vec!["--region", "-"],
    ] {
        let mut full = vec!["bedrock-probes", "iam-policy"];
        let defaults = [
            ("--models", "amazon.nova-lite-v1:0"),
            ("--region", REGION),
            ("--account-id", ACCOUNT),
        ];
        full.extend(args.iter().copied());
        for (flag, value) in defaults {
            if !args.iter().any(|arg| arg.starts_with(flag)) {
                full.extend([flag, value]);
            }
        }
        let (status, out, _) = exec(&full);
        assert_eq!(status, 2, "{args:?}");
        assert!(out.is_empty());
    }
    let (status, _, _) = exec(&["bedrock-probes", "iam-policy", "-"]);
    assert_eq!(status, 2);
    assert_eq!(
        Path::new("-").exists(),
        before,
        "no file named - was created"
    );
}

#[cfg(unix)]
#[test]
fn iam_policy_cli_refuses_to_write_through_a_link() {
    let dir = TempDir::new("iam-link");
    let target = dir.file("target.json", "keep");
    let link = dir.link("policy.json", &target);
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "amazon.nova-lite-v1:0",
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
        "--output",
        &link,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("cannot write"), "{err}");
    assert_eq!(fs::read_to_string(&target).expect("target"), "keep");
}

#[test]
fn iam_policy_cli_needs_every_input_and_bounds_the_model_list() {
    let (status, _, err) = exec(&["bedrock-probes", "iam-policy", "--models", "a"]);
    assert_eq!(status, 2);
    assert!(err.contains("--region is required"), "{err}");
    let many = (0..300)
        .map(|n| format!("amazon.m{n}"))
        .collect::<Vec<_>>()
        .join(",");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        &many,
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
    ]);
    assert_eq!(status, 2);
    assert!(err.contains("at most 256"), "{err}");
}

#[test]
fn iam_policy_hostile_text_never_reaches_output() {
    let (status, out, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        HOSTILE,
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
    ]);
    assert_eq!(status, 2);
    assert_clean("stdout", &out);
    assert_clean("stderr", &err);
    // A path with hostile characters is escaped in the report line.
    let dir = TempDir::new("iam-hostile");
    let hostile_path = format!("{}/{HOSTILE}.json", dir.display());
    let (status, out, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "amazon.nova-lite-v1:0",
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
        "--output",
        &hostile_path,
    ]);
    assert_clean("stdout", &out);
    assert_clean("stderr", &err);
    assert!(status == 0 || status == 1);
}

// ---------------------------------------------------------------------------
// preflight classifiers (test_preflight_bedrock.py)

#[test]
fn access_denied_maps_to_authz_or_the_console_fix() {
    let verdict = preflight::classify_invoke_error(
        "AccessDeniedException",
        "not authorized to perform bedrock:InvokeModel",
    );
    assert!(!verdict.ok);
    assert_eq!(verdict.reason, "authz");
    assert!(verdict.detail.contains("bedrock:InvokeModel"));
    let verdict = preflight::classify_invoke_error(
        "AccessDeniedException",
        "You don't have access to the model with the specified model ID. Enable model access in the Amazon Bedrock console.",
    );
    assert_eq!(verdict.reason, "model_access");
    assert!(verdict.detail.to_lowercase().contains("console"));
    let verdict = preflight::classify_invoke_error(
        "AccessDeniedException",
        "User: arn:aws:iam::123:user/x is not authorized to perform: bedrock:InvokeModel on resource ...",
    );
    assert_eq!(verdict.reason, "authz");
    assert!(verdict.detail.contains("bedrock:InvokeModel"));
}

#[test]
fn other_service_codes_map_as_in_the_script() {
    let unavailable =
        preflight::classify_invoke_error("ValidationException", "model identifier is invalid");
    assert_eq!(unavailable.reason, "model_unavailable");
    assert!(!unavailable.ok);
    assert!(unavailable.detail.contains("us.<model-id>"));
    for code in ["ThrottlingException", "ServiceQuotaExceededException"] {
        let verdict = preflight::classify_invoke_error(code, "rate exceeded");
        assert!(verdict.ok, "{code}");
        assert_eq!(verdict.reason, "throttled_ok");
    }
    for code in [
        "UnrecognizedClientException",
        "InvalidSignatureException",
        "ExpiredTokenException",
        "ExpiredToken",
    ] {
        let verdict = preflight::classify_invoke_error(
            code,
            "The security token included in the request is expired",
        );
        assert!(!verdict.ok);
        assert_eq!(verdict.reason, "credentials");
    }
    let unknown = preflight::classify_invoke_error("InternalServerException", "boom");
    assert_eq!(
        unknown,
        Verdict {
            ok: false,
            reason: "unknown",
            detail: "InternalServerException: boom".to_string()
        }
    );
}

#[test]
fn mantle_errors_point_at_mantle_actions() {
    let denied = preflight::classify_mantle_error(
        Some(403),
        "User is not authorized to perform bedrock-mantle:CreateInference",
    );
    assert_eq!(denied.reason, "authz");
    assert!(denied.detail.contains("bedrock-mantle"));
    assert!(denied.detail.contains("does NOT authorize"));
    let unauthenticated = preflight::classify_mantle_error(Some(401), "nope");
    assert_eq!(unauthenticated.reason, "authz");
    let access = preflight::classify_mantle_error(
        Some(403),
        "You do not have access to the model with the specified model ID",
    );
    assert_eq!(access.reason, "model_access");
    assert!(access.detail.to_lowercase().contains("console"));
    let not_authorized =
        preflight::classify_mantle_error(Some(401), "caller is not authorized to use this model");
    assert_eq!(not_authorized.reason, "model_access");
}

#[test]
fn mantle_404_remedy_is_family_split() {
    let verdict = preflight::classify_mantle_error(Some(404), "model not found");
    assert!(!verdict.ok);
    assert_eq!(verdict.reason, "model_unavailable");
    assert!(verdict.detail.contains("in-region only"));
    assert!(verdict.detail.contains("CRIS") && verdict.detail.contains("us./in./global."));
    assert!(verdict.detail.contains("GPT-5.5/5.4"));
}

#[test]
fn mantle_throttle_passes_and_other_statuses_are_unknown() {
    let verdict = preflight::classify_mantle_error(Some(429), "too many tokens per minute");
    assert!(verdict.ok);
    assert_eq!(verdict.reason, "throttled_ok");
    let unknown = preflight::classify_mantle_error(Some(500), "oops");
    assert_eq!(unknown.reason, "unknown");
    assert_eq!(unknown.detail, "500: oops");
    assert_eq!(
        preflight::classify_mantle_error(None, "x").detail,
        "None: x"
    );
}

#[test]
fn caller_text_in_a_verdict_is_redacted_bounded_and_escaped() {
    let message = format!("denied Bearer {SECRET}\r\n{HOSTILE}{}", "z".repeat(2000));
    for verdict in [
        preflight::classify_invoke_error("AccessDeniedException", &message),
        preflight::classify_invoke_error("ValidationException", &message),
        preflight::classify_invoke_error("OtherThing", &message),
        preflight::classify_mantle_error(Some(403), &message),
        preflight::classify_mantle_error(Some(404), &message),
        preflight::classify_mantle_error(Some(500), &message),
    ] {
        assert_clean("detail", &verdict.detail);
        assert!(!verdict.detail.contains('\n') && !verdict.detail.contains('\r'));
        assert!(verdict.detail.contains("more chars"), "{}", verdict.detail);
        assert!(verdict.detail.len() < 1500, "{}", verdict.detail.len());
    }
}

#[test]
fn embedding_detection() {
    assert!(preflight::is_embedding_model(
        "amazon.titan-embed-text-v2:0"
    ));
    assert!(preflight::is_embedding_model("cohere.embed-english-v3"));
    assert!(!preflight::is_embedding_model(
        "us.anthropic.claude-haiku-4-5-20251001-v1:0"
    ));
}

fn dispatch_ids(
    path: Option<&str>,
    ids: &[&str],
    runtime_api: Option<&str>,
) -> Result<String, String> {
    preflight::normalize_api_path(path, &strings(ids), runtime_api)
}

#[test]
fn dispatch_and_resume_share_the_api_default() {
    // test_dispatch_and_resume_share_api_default
    let cases: [(Option<&str>, &str, &str); 8] = [
        (None, "openai.gpt-6-astra", "mantle_openai_responses"),
        (Some(""), "openai.gpt-5.6-sol", "mantle_openai_responses"),
        (None, "us.openai.gpt-6-astra", "converse"),
        (None, "global.openai.gpt-6-astra", "converse"),
        (
            Some("mantle_openai_chat"),
            "openai.gpt-6-astra",
            "mantle_openai_chat",
        ),
        (
            Some("mantle_openai_responses"),
            "openai.gpt-6-astra",
            "mantle_openai_responses",
        ),
        (
            Some("runtime_openai_cris"),
            "us.openai.gpt-6-astra",
            "runtime_openai_cris",
        ),
        (
            Some("mantle_messages"),
            "anthropic.claude-sonnet-5",
            "mantle_messages",
        ),
    ];
    for (path, id, expected) in cases {
        let selected = dispatch_ids(path, &[id], None).expect("resolves");
        assert_eq!(selected, expected, "{path:?} {id}");
        assert_eq!(dispatch_ids(path, &[id], None).expect("again"), selected);
    }
    // A bare `mantle` plan over mantle ids selects Responses.
    assert_eq!(
        dispatch_ids(Some("mantle"), &["openai.gpt-6-astra"], None).expect("ok"),
        "mantle_openai_responses"
    );
    // Over runtime ids (or none) it is kept as given.
    assert_eq!(
        dispatch_ids(Some("mantle"), &[], None).expect("ok"),
        "mantle"
    );
}

#[test]
fn mixed_or_empty_legacy_targets_require_resolution() {
    // test_mixed_or_empty_legacy_targets_require_resolution
    let mixed = dispatch_ids(None, &["openai.gpt-6-astra", "amazon.nova-lite-v1:0"], None)
        .expect_err("mixed");
    assert!(mixed.contains("Mixed Mantle and runtime"), "{mixed}");
    let empty = dispatch_ids(None, &[], None).expect_err("empty");
    assert!(empty.contains("No validated target"), "{empty}");
}

#[test]
fn explicit_runtime_api_survives_normalization() {
    let cases: [(Option<&str>, &str); 4] = [
        (None, "runtime_openai_cris"),
        (Some("converse"), "runtime_openai_cris"),
        (Some("responses"), "runtime_openai_responses"),
        (Some("chat_completions"), "runtime_openai_chat"),
    ];
    for (surface, path) in cases {
        assert_eq!(
            dispatch_ids(
                Some("runtime_openai_cris"),
                &["global.openai.gpt-6-astra"],
                surface
            )
            .expect("ok"),
            path
        );
    }
    for surface in ["unknown", ""] {
        let error =
            dispatch_ids(Some("runtime_openai_cris"), &[], Some(surface)).expect_err("unknown");
        assert!(error.contains("Unknown runtime API"), "{error}");
    }
}

#[test]
fn api_handoff_classifier_is_consistent() {
    // The is_mantle_model asserts of
    // test_api_handoff_paths_and_real_evaluator_classifier_are_consistent.
    assert!(preflight::is_mantle_model("openai.gpt-6-astra"));
    assert!(!preflight::is_mantle_model("global.openai.gpt-6-astra"));
    let (status, out, _) = exec(&[
        "bedrock-probes",
        "classify",
        "--models",
        "openai.gpt-6-astra,global.openai.gpt-6-astra",
    ]);
    assert_eq!(status, 0);
    let rows = json(&out);
    let flags: Vec<Option<bool>> = match rows.get("models") {
        Some(Json::Array(items)) => items
            .iter()
            .map(|row| match row.get("mantle") {
                Some(Json::Bool(flag)) => Some(*flag),
                _ => None,
            })
            .collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(flags, [Some(true), Some(false)]);
}

#[test]
fn api_path_cli_resolves_or_fails_with_the_script_message() {
    let (status, out, _) = exec(&[
        "bedrock-probes",
        "api-path",
        "--models",
        "us.openai.gpt-6-astra",
    ]);
    assert_eq!(status, 0);
    assert_eq!(json(&out), json(r#"{"api_path": "converse"}"#));
    let (status, out, err) = exec(&[
        "bedrock-probes",
        "api-path",
        "--models",
        "openai.gpt-6-astra,amazon.nova-lite-v1:0",
    ]);
    assert_eq!(status, 1);
    assert!(out.is_empty());
    assert_eq!(
        err,
        "error: Mixed Mantle and runtime targets require an explicit migration plan\n"
    );
    let (status, _, err) = exec(&["bedrock-probes", "api-path"]);
    assert_eq!(status, 1);
    assert!(err.contains("No validated target model IDs"), "{err}");
    let (status, out, _) = exec(&[
        "bedrock-probes",
        "api-path",
        "--plan-path",
        "runtime_openai_cris",
        "--runtime-api",
        "chat_completions",
    ]);
    assert_eq!(status, 0);
    assert_eq!(json(&out), json(r#"{"api_path": "runtime_openai_chat"}"#));
}

fn quota(name: &str, value: &str) -> Json {
    json(&format!(r#"{{"QuotaName": "{name}", "Value": {value}}}"#))
}

#[test]
fn quota_rpm_matches_a_model_name_token() {
    let quotas = vec![
        quota(
            "On-demand model inference requests per minute for Anthropic Claude Haiku 4.5",
            "50.0",
        ),
        quota(
            "On-demand model inference requests per minute for Amazon Nova Lite",
            "1000.0",
        ),
        quota(
            "Cross-region model inference tokens per day for Anthropic Claude",
            "9.9e9",
        ),
    ];
    assert_eq!(
        preflight::quota_rpm(&quotas, "us.anthropic.claude-haiku-4-5-20251001-v1:0").expect("ok"),
        Some(50)
    );
    let nova_only = vec![quota(
        "On-demand model inference requests per minute for Amazon Nova Lite",
        "1000.0",
    )];
    assert_eq!(
        preflight::quota_rpm(&nova_only, "us.anthropic.claude-haiku-4-5-20251001-v1:0")
            .expect("ok"),
        None
    );
}

#[test]
fn quota_rpm_takes_the_lowest_match_and_rejects_malformed_values() {
    let quotas = vec![
        quota(
            "On-demand model inference requests per minute for Claude A",
            "70",
        ),
        quota(
            "On-demand model inference requests per minute for Claude B",
            "20.9",
        ),
        json(r#"{"QuotaName": "On-demand model inference requests per minute for Claude C"}"#),
    ];
    // The third has no Value: `int(q.get("Value", 0))` is 0.
    assert_eq!(
        preflight::quota_rpm(&quotas, "anthropic.claude-x").expect("ok"),
        Some(0)
    );
    let two = &quotas[..2];
    assert_eq!(
        preflight::quota_rpm(two, "anthropic.claude-x").expect("ok"),
        Some(20)
    );
    for bad in [
        quota("On-demand requests per minute for Claude", r#""50""#),
        quota("On-demand requests per minute for Claude", "null"),
        quota("On-demand requests per minute for Claude", "[1]"),
    ] {
        let error = preflight::quota_rpm(&[bad], "anthropic.claude-x").expect_err("bad value");
        assert!(error.contains("Value"), "{error}");
    }
    let wrong_name = json(r#"{"QuotaName": 5, "Value": 1}"#);
    assert!(preflight::quota_rpm(&[wrong_name], "anthropic.claude-x").is_err());
}

fn model_result(ok: bool, reason: &'static str, detail: &str, id: &str) -> preflight::ModelResult {
    preflight::ModelResult {
        verdict: Verdict {
            ok,
            reason,
            detail: detail.to_string(),
        },
        model_id: id.to_string(),
        rpm_quota: None,
        quota_note: None,
        quota_warning: None,
    }
}

#[test]
fn the_first_failing_reason_is_lifted_to_the_top_level() {
    let results = vec![
        model_result(true, "ok", "fine", "m1"),
        model_result(false, "model_unavailable", "nope", "m2"),
        model_result(false, "authz", "denied", "m3"),
    ];
    let lifted = preflight::aggregate_failure(&results);
    let get = |name: &str| {
        lifted
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("reason"), Some(Json::string("model_unavailable")));
    assert_eq!(get("detail"), Some(Json::string("nope")));
    assert_eq!(
        get("failing_models"),
        Some(Json::Array(vec![Json::string("m2"), Json::string("m3")]))
    );
    assert!(preflight::aggregate_failure(&[model_result(true, "ok", "", "m")]).is_empty());
}

// ---------------------------------------------------------------------------
// probes against a scripted client

type Call = (Service, String, Json);

/// A client that records every call and answers with `answer`.
type Answer = Box<dyn FnMut(Service, &str, &Json) -> Result<Json, ProbeError>>;

struct Scripted {
    calls: Vec<Call>,
    answer: Answer,
}

impl Scripted {
    fn new(answer: impl FnMut(Service, &str, &Json) -> Result<Json, ProbeError> + 'static) -> Self {
        Self {
            calls: Vec::new(),
            answer: Box::new(answer),
        }
    }

    fn accepting() -> Self {
        Self::new(|_, _, _| Ok(Json::object(vec![])))
    }

    fn failing(error: ProbeError) -> Self {
        Self::new(move |_, _, _| Err(error.clone()))
    }

    fn operations(&self) -> Vec<String> {
        self.calls
            .iter()
            .filter(|(service, _, _)| *service != Service::Quotas)
            .map(|(service, operation, _)| format!("{} {operation}", service.name()))
            .collect()
    }

    fn probe_requests(&self) -> Vec<Json> {
        self.calls
            .iter()
            .filter(|(service, _, _)| *service != Service::Quotas)
            .map(|(_, _, request)| request.clone())
            .collect()
    }
}

impl ProbeClient for Scripted {
    fn call(
        &mut self,
        service: Service,
        operation: &str,
        request: &Json,
    ) -> Result<Json, ProbeError> {
        self.calls
            .push((service, operation.to_string(), request.clone()));
        (self.answer)(service, operation, request)
    }
}

fn service_error(code: &str, message: &str) -> ProbeError {
    ProbeError::Aws(AwsError::Service(Box::new(toolkit_aws::ServiceError::new(
        "POST",
        "/",
        400,
        Some(code),
        message,
        None,
    ))))
}

fn status_error(status: u16, message: &str) -> ProbeError {
    ProbeError::Aws(AwsError::Service(Box::new(toolkit_aws::ServiceError::new(
        "POST", "/", status, None, message, None,
    ))))
}

#[test]
fn a_chat_model_is_probed_with_a_one_token_converse_call() {
    let mut client = Scripted::accepting();
    let verdict = preflight::probe_model(&mut client, REGION, "amazon.nova-lite-v1:0");
    assert_eq!(
        verdict,
        Verdict {
            ok: true,
            reason: "ok",
            detail: "InvokeModel authorized.".to_string()
        }
    );
    assert_eq!(client.operations(), ["bedrock-runtime Converse"]);
    assert_eq!(
        client.probe_requests()[0],
        json(
            r#"{"modelId": "amazon.nova-lite-v1:0",
                "messages": [{"role": "user", "content": [{"text": "ping"}]}],
                "inferenceConfig": {"maxTokens": 1}}"#
        )
    );
}

#[test]
fn embedding_models_go_to_invoke_model_with_their_family_body() {
    let mut client = Scripted::accepting();
    let verdict = preflight::probe_model(&mut client, REGION, "amazon.titan-embed-text-v2:0");
    assert!(verdict.ok);
    assert_eq!(verdict.detail, "InvokeModel (embedding) authorized.");
    assert_eq!(client.operations(), ["bedrock-runtime InvokeModel"]);
    assert_eq!(
        client.probe_requests()[0],
        json(
            r#"{"modelId": "amazon.titan-embed-text-v2:0", "body": "{\"inputText\": \"ping\"}",
                "contentType": "application/json", "accept": "application/json"}"#
        )
    );
    let mut cohere = Scripted::accepting();
    preflight::probe_model(&mut cohere, REGION, "us.cohere.embed-english-v3");
    assert_eq!(
        cohere.probe_requests()[0].get("body"),
        Some(&Json::string(
            r#"{"texts": ["ping"], "input_type": "search_document"}"#
        ))
    );
}

#[test]
fn an_unknown_embedding_family_passes_with_a_caveat_and_sends_nothing() {
    let mut client = Scripted::accepting();
    let verdict = preflight::probe_model(&mut client, REGION, "somevendor.embed-x-v1:0");
    assert!(verdict.ok);
    assert_eq!(verdict.reason, "embedding_unprobed");
    assert!(client.calls.is_empty());
}

#[test]
fn bare_opus_5_5_outside_eu_west_2_needs_a_profile_and_sends_nothing() {
    let mut client = Scripted::accepting();
    let verdict = preflight::probe_model(&mut client, "us-west-2", "anthropic.claude-opus-5-5");
    assert!(!verdict.ok);
    assert_eq!(verdict.reason, "inference_profile_required");
    assert!(client.calls.is_empty());
    let verdict = preflight::probe_model(&mut client, "eu-west-2", "anthropic.claude-opus-5-5");
    assert!(verdict.ok);
    assert_eq!(client.operations(), ["bedrock-runtime Converse"]);
}

#[test]
fn a_transport_failure_is_a_json_verdict_not_a_trace() {
    let mut client = Scripted::failing(ProbeError::Aws(AwsError::NoCredentials));
    let verdict = preflight::probe_model(&mut client, REGION, "any.model-v1:0");
    assert!(!verdict.ok);
    assert_eq!(verdict.reason, "credentials");
    assert!(
        verdict.detail.contains("NoCredentialsError"),
        "{}",
        verdict.detail
    );
    assert!(verdict.detail.contains("aws configure"));
    let mut connect = Scripted::failing(ProbeError::Aws(AwsError::connect("could not connect")));
    let verdict = preflight::probe_model(&mut connect, REGION, "any.model-v1:0");
    assert_eq!(verdict.reason, "credentials");
    assert!(verdict
        .detail
        .starts_with("EndpointConnectionError: could not connect"));
}

#[test]
fn a_service_error_is_classified_by_its_code() {
    let mut client = Scripted::failing(service_error(
        "AccessDeniedException",
        "not authorized to perform bedrock:InvokeModel",
    ));
    let verdict = preflight::probe_model(&mut client, REGION, "amazon.nova-lite-v1:0");
    assert_eq!(verdict.reason, "authz");
    let mut throttled = Scripted::failing(service_error("ThrottlingException", "slow down"));
    let verdict = preflight::probe_model(&mut throttled, REGION, "amazon.nova-lite-v1:0");
    assert!(verdict.ok);
    assert_eq!(verdict.reason, "throttled_ok");
    // A response without a code or message is `Unknown` with `str(ClientError)`.
    let mut bare = Scripted::failing(status_error(400, ""));
    let verdict = preflight::probe_model(&mut bare, REGION, "amazon.nova-lite-v1:0");
    assert_eq!(verdict.reason, "unknown");
    assert!(
        verdict.detail.starts_with(
            "Unknown: An error occurred (Unknown) when calling the Converse operation"
        ),
        "{}",
        verdict.detail
    );
}

#[test]
fn without_a_client_every_probe_fails_closed() {
    for id in ["amazon.nova-lite-v1:0", "amazon.titan-embed-text-v2:0"] {
        let mut client = Scripted::failing(ProbeError::NoLive);
        let verdict = preflight::probe_model(&mut client, REGION, id);
        assert!(!verdict.ok, "{id}");
        assert_eq!(verdict.reason, preflight::REASON_NO_LIVE);
        assert!(verdict.detail.contains("NOT verified"));
    }
    let mut client = Scripted::failing(ProbeError::NoLive);
    let verdict = preflight::probe_mantle_model(&mut client, "openai.gpt-5.6-terra", REGION);
    assert!(!verdict.ok);
    assert!(verdict.detail.contains("NOT verified"));
    let verdict = preflight::probe_runtime_openai_model(
        &mut client,
        "us.openai.gpt-6-astra",
        REGION,
        "responses",
    );
    assert!(!verdict.ok);
}

#[test]
fn the_mantle_probe_sends_the_responses_request_and_classifies_statuses() {
    let mut client = Scripted::accepting();
    let verdict = preflight::probe_mantle_model(&mut client, "openai.gpt-6-astra", "us-west-2");
    assert_eq!(
        verdict,
        Verdict {
            ok: true,
            reason: "ok",
            detail: "Mantle Responses API authorized.".to_string()
        }
    );
    assert_eq!(client.operations(), ["bedrock-mantle Responses"]);
    assert_eq!(
        client.probe_requests()[0],
        json(
            r#"{"region": "us-west-2", "body": {"model": "openai.gpt-6-astra", "input": "ping",
                "max_output_tokens": 16, "store": false}}"#
        )
    );
    for (status, reason, ok) in [
        (403, "authz", false),
        (404, "model_unavailable", false),
        (429, "throttled_ok", true),
    ] {
        let mut failing = Scripted::failing(status_error(status, "x"));
        let verdict = preflight::probe_mantle_model(&mut failing, "openai.gpt-5.5", REGION);
        assert_eq!((verdict.ok, verdict.reason), (ok, reason), "{status}");
    }
    // No HTTP status: the script blames credentials, region or network.
    let mut transport = Scripted::failing(ProbeError::Aws(AwsError::connect("down")));
    let verdict = preflight::probe_mantle_model(&mut transport, "openai.gpt-5.5", REGION);
    assert_eq!(verdict.reason, "credentials");
    assert!(verdict.detail.contains("bedrock-mantle endpoint"));
}

#[test]
fn the_runtime_openai_probe_keeps_the_wire_contract() {
    // test_runtime_openai_client_uses_real_sdk_wire_contract
    for (surface, tail, operation) in [
        ("responses", "responses", "Responses"),
        ("chat_completions", "chat/completions", "ChatCompletions"),
    ] {
        let mut client = Scripted::accepting();
        let verdict = preflight::probe_runtime_openai_model(
            &mut client,
            "global.openai.gpt-6-astra",
            "us-west-2",
            surface,
        );
        assert!(verdict.ok);
        assert_eq!(verdict.detail, format!("Runtime {surface} API authorized."));
        assert_eq!(
            client.operations(),
            [format!("bedrock-runtime-openai {operation}")]
        );
        let request = &client.probe_requests()[0];
        assert_eq!(
            request.get("url").and_then(Json::as_str),
            Some(
                format!("https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1/{tail}")
                    .as_str()
            )
        );
        let body = request.get("body").expect("a body");
        assert_eq!(
            body.get("model").and_then(Json::as_str),
            Some("global.openai.gpt-6-astra")
        );
        assert_eq!(
            body.get("messages").is_some(),
            surface == "chat_completions"
        );
        assert_eq!(body.get("input").is_some(), surface == "responses");
    }
}

#[test]
fn runtime_openai_statuses_use_the_botocore_codes() {
    for (status, reason, ok) in [
        (401, "credentials", false),
        (403, "authz", false),
        (404, "model_unavailable", false),
        (429, "throttled_ok", true),
        (500, "unknown", false),
    ] {
        let mut client = Scripted::failing(status_error(status, "msg"));
        let verdict = preflight::probe_runtime_openai_model(
            &mut client,
            "us.openai.gpt-6-astra",
            REGION,
            "responses",
        );
        assert_eq!((verdict.ok, verdict.reason), (ok, reason), "{status}");
    }
    let mut transport = Scripted::failing(ProbeError::Aws(AwsError::connect("down")));
    let verdict = preflight::probe_runtime_openai_model(
        &mut transport,
        "us.openai.gpt-6-astra",
        REGION,
        "responses",
    );
    assert_eq!(verdict.detail, "unknown: down");
}

// ---------------------------------------------------------------------------
// the preflight run (main)

fn run_with(
    client: &mut Scripted,
    region: &str,
    models: &str,
    dataset_size: i64,
    api: &str,
) -> (Json, bool) {
    preflight::run(
        &Options {
            region,
            models,
            dataset_size,
            runtime_api: api,
        },
        client,
    )
    .expect("a report")
}

fn model_entry(report: &Json, index: usize) -> Json {
    match report.get("models") {
        Some(Json::Array(items)) => items[index].clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn astra_dispatch_preserves_the_id_and_the_endpoint() {
    // test_astra_preflight_dispatch_preserves_id_and_endpoint
    for (id, mantle) in [
        ("openai.gpt-6-astra", true),
        ("us.openai.gpt-6-astra", false),
        ("global.openai.gpt-6-astra", false),
    ] {
        let mut client = Scripted::accepting();
        assert_eq!(preflight::is_mantle_model(id), mantle);
        let (report, ok) = run_with(&mut client, "us-west-2", id, 0, "converse");
        assert!(ok);
        let expected_op = if mantle {
            "bedrock-mantle Responses"
        } else {
            "bedrock-runtime Converse"
        };
        assert_eq!(client.operations(), [expected_op]);
        let entry = model_entry(&report, 0);
        assert_eq!(entry.get("model_id").and_then(Json::as_str), Some(id));
        let note = entry
            .get("quota_note")
            .and_then(Json::as_str)
            .expect("a note");
        if mantle {
            assert!(note.contains("Do not assume GPT-5.6"), "{note}");
            assert!(note.contains("us-west-2"));
        } else {
            assert!(note.contains("input tokens + 10 * output tokens"), "{note}");
        }
        let request = &client.probe_requests()[0];
        let sent = if mantle {
            request.get("body").and_then(|body| body.get("model"))
        } else {
            request.get("modelId")
        };
        assert_eq!(sent.and_then(Json::as_str), Some(id));
    }
}

#[test]
fn mantle_models_never_use_bedrock_runtime_and_get_no_rpm_warning() {
    let mut client = Scripted::accepting();
    let (report, ok) = run_with(
        &mut client,
        REGION,
        "openai.gpt-5.6-terra",
        9999,
        "converse",
    );
    assert!(ok);
    assert_eq!(client.operations(), ["bedrock-mantle Responses"]);
    let entry = model_entry(&report, 0);
    assert_eq!(entry.get("ok"), Some(&Json::Bool(true)));
    assert_eq!(entry.get("rpm_quota"), Some(&Json::Null));
    assert!(entry.get("quota_warning").is_none());
    assert!(entry
        .get("quota_note")
        .and_then(Json::as_str)
        .expect("note")
        .contains("no RPM quota"));
}

#[test]
fn an_empty_model_list_is_a_failure_not_a_vacuous_pass() {
    let mut client = Scripted::accepting();
    let (report, ok) = run_with(&mut client, REGION, " , ", 0, "converse");
    assert!(!ok);
    assert!(client.calls.is_empty());
    assert_eq!(
        report.get("reason").and_then(Json::as_str),
        Some("no_models")
    );
    assert_eq!(report.get("ok"), Some(&Json::Bool(false)));
    assert_eq!(
        report,
        json(
            r#"{"ok": false, "region": "us-east-1", "models": [], "reason": "no_models",
                "detail": "--models resolved to an empty list; nothing to probe."}"#
        )
    );
}

#[test]
fn a_malformed_region_is_a_credentials_verdict_and_sends_nothing() {
    for region in [
        "not a region",
        "-bad",
        "bad-",
        "us_east_1",
        "us-east-1\n",
        "",
    ] {
        let mut client = Scripted::accepting();
        let (report, ok) = run_with(&mut client, region, "amazon.nova-lite-v1:0", 0, "converse");
        assert!(!ok, "{region:?}");
        assert!(client.calls.is_empty());
        assert_eq!(
            report.get("reason").and_then(Json::as_str),
            Some("credentials")
        );
        let detail = report.get("detail").and_then(Json::as_str).expect("detail");
        assert!(
            detail.starts_with("InvalidRegionError: Provided region_name '"),
            "{detail}"
        );
        assert_clean("detail", detail);
    }
    let mut client = Scripted::accepting();
    let (report, _) = run_with(&mut client, HOSTILE, "amazon.nova-lite-v1:0", 0, "converse");
    assert_clean("report", &report.to_compact_string());
}

#[test]
fn the_report_has_the_scripts_member_order_and_failure_summary() {
    let mut client = Scripted::new(|_, operation, request| {
        if operation == "Converse"
            && request.get("modelId").and_then(Json::as_str) == Some("amazon.bad-v1:0")
        {
            Err(service_error("ValidationException", "bad id"))
        } else {
            Ok(Json::object(vec![]))
        }
    });
    let (report, ok) = run_with(
        &mut client,
        REGION,
        "amazon.nova-lite-v1:0, amazon.bad-v1:0 ,amazon.nova-lite-v1:0",
        0,
        "converse",
    );
    assert!(!ok);
    let keys: Vec<&str> = match &report {
        Json::Object(members) => members.iter().map(|(key, _)| key.as_str()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        keys,
        [
            "ok",
            "region",
            "probe_cost_note",
            "models",
            "reason",
            "detail",
            "failing_models"
        ]
    );
    assert_eq!(
        report.get("reason").and_then(Json::as_str),
        Some("model_unavailable")
    );
    assert_eq!(
        report.get("failing_models"),
        Some(&Json::Array(vec![Json::string("amazon.bad-v1:0")]))
    );
    let entry = model_entry(&report, 0);
    let entry_keys: Vec<&str> = match &entry {
        Json::Object(members) => members.iter().map(|(key, _)| key.as_str()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        entry_keys,
        ["ok", "reason", "detail", "model_id", "rpm_quota"]
    );
    // The repeated id is probed twice (the list is not de-duplicated).
    assert_eq!(client.operations().len(), 3);
}

fn quota_page(items: &str, token: Option<&str>) -> Json {
    let tail = token.map_or(String::new(), |token| {
        format!(r#", "NextToken": "{token}""#)
    });
    json(&format!(r#"{{"Quotas": {items}{tail}}}"#))
}

#[test]
fn quotas_are_paged_and_drive_the_pacing_warning() {
    let page_one = quota_page(
        r#"[{"QuotaName": "On-demand model inference requests per minute for Amazon Nova Lite", "Value": 100.0}]"#,
        Some("t1"),
    );
    let page_two = quota_page(
        r#"[{"QuotaName": "On-demand model inference requests per minute for Amazon Nova Pro", "Value": 30.0}]"#,
        None,
    );
    let mut client = Scripted::new(move |service, _, request| {
        if service != Service::Quotas {
            return Ok(Json::object(vec![]));
        }
        match request.get("NextToken").and_then(Json::as_str) {
            None => Ok(page_one.clone()),
            Some("t1") => Ok(page_two.clone()),
            Some(other) => panic!("unexpected token {other}"),
        }
    });
    let (report, _) = run_with(&mut client, REGION, "amazon.nova-lite-v1:0", 50, "converse");
    let entry = model_entry(&report, 0);
    // Both quotas match the `amazon` token; the lowest wins.
    assert_eq!(entry.get("rpm_quota"), Some(&Json::Int(30)));
    assert_eq!(
        entry.get("quota_warning").and_then(Json::as_str),
        Some("Dataset (50) exceeds ~30 RPM quota — Eval will pace with backoff and may be slow.")
    );
    let requests: Vec<&Json> = client
        .calls
        .iter()
        .filter(|(s, _, _)| *s == Service::Quotas)
        .map(|(_, _, r)| r)
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], &json(r#"{"ServiceCode": "bedrock"}"#));
}

#[test]
fn a_failed_or_endless_quota_listing_is_empty_but_a_malformed_one_is_an_error() {
    let mut failing = Scripted::new(|service, _, _| {
        if service == Service::Quotas {
            Err(service_error("AccessDeniedException", "no quotas"))
        } else {
            Ok(Json::object(vec![]))
        }
    });
    let (report, ok) = run_with(
        &mut failing,
        REGION,
        "amazon.nova-lite-v1:0",
        10,
        "converse",
    );
    assert!(ok);
    assert_eq!(model_entry(&report, 0).get("rpm_quota"), Some(&Json::Null));
    let mut stuck = Scripted::new(|service, _, _| {
        if service == Service::Quotas {
            Ok(quota_page("[]", Some("same")))
        } else {
            Ok(Json::object(vec![]))
        }
    });
    let (report, _) = run_with(&mut stuck, REGION, "amazon.nova-lite-v1:0", 10, "converse");
    assert_eq!(model_entry(&report, 0).get("rpm_quota"), Some(&Json::Null));
    for bad in [
        r#"{"Quotas": null}"#,
        r#"{"Quotas": [1]}"#,
        r#"{"Quotas": {}}"#,
        r#"{"NextToken": 5}"#,
    ] {
        let document = json(bad);
        let mut malformed = Scripted::new(move |service, _, _| {
            if service == Service::Quotas {
                Ok(document.clone())
            } else {
                Ok(Json::object(vec![]))
            }
        });
        let error = preflight::run(
            &Options {
                region: REGION,
                models: "amazon.nova-lite-v1:0",
                dataset_size: 0,
                runtime_api: "converse",
            },
            &mut malformed,
        )
        .err()
        .unwrap_or_else(|| panic!("{bad} must be rejected"));
        assert!(
            error.starts_with("invalid ListServiceQuotas response"),
            "{error}"
        );
    }
}

#[test]
fn the_runtime_api_selects_the_openai_probe_for_runtime_gpt_ids_only() {
    // test_runtime_preflight_uses_selected_api
    for (api, operation) in [
        ("responses", "Responses"),
        ("chat_completions", "ChatCompletions"),
    ] {
        let mut client = Scripted::accepting();
        let id = "global.openai.gpt-6-astra";
        let (report, ok) = run_with(&mut client, "us-west-2", id, 0, api);
        assert!(ok);
        assert_eq!(
            client.operations(),
            [format!("bedrock-runtime-openai {operation}")]
        );
        let body = client.probe_requests()[0]
            .get("body")
            .cloned()
            .expect("body");
        assert_eq!(body.get("model").and_then(Json::as_str), Some(id));
        assert_eq!(model_entry(&report, 0).get("ok"), Some(&Json::Bool(true)));
    }
    // gpt-oss and non-GPT ids keep Converse whatever `--runtime-api` says.
    for id in ["us.openai.gpt-oss-120b-1:0", "amazon.nova-lite-v1:0"] {
        let mut client = Scripted::accepting();
        run_with(&mut client, REGION, id, 0, "responses");
        assert_eq!(client.operations(), ["bedrock-runtime Converse"], "{id}");
    }
}

#[test]
fn a_system_profile_arn_is_probed_by_its_arn_with_the_astra_quota_note() {
    // The preflight half of
    // test_astra_system_profile_arn_keeps_invocation_price_and_permissions.
    for prefix in ["us", "global"] {
        let arn = format!(
            "arn:aws:bedrock:us-west-2:111122223333:inference-profile/{prefix}.openai.gpt-6-astra"
        );
        let mut client = Scripted::accepting();
        let (report, ok) = run_with(&mut client, "us-west-2", &arn, 0, "converse");
        assert!(ok);
        assert_eq!(
            client.probe_requests()[0]
                .get("modelId")
                .and_then(Json::as_str),
            Some(arn.as_str())
        );
        let entry = model_entry(&report, 0);
        assert!(entry
            .get("quota_note")
            .and_then(Json::as_str)
            .expect("note")
            .contains("input tokens + 10 * output tokens"));
    }
}

#[test]
fn without_a_recording_the_run_fails_closed_for_every_endpoint() {
    let mut client = Scripted::failing(ProbeError::NoLive);
    let (report, ok) = run_with(
        &mut client,
        REGION,
        "amazon.nova-lite-v1:0,openai.gpt-5.5,us.openai.gpt-6-astra",
        0,
        "responses",
    );
    assert!(!ok);
    assert_eq!(
        report.get("reason").and_then(Json::as_str),
        Some(preflight::REASON_NO_LIVE)
    );
    for index in 0..3 {
        assert_eq!(
            model_entry(&report, index).get("ok"),
            Some(&Json::Bool(false))
        );
    }
}

// ---------------------------------------------------------------------------
// the preflight command and the recording

fn recording(dir: &TempDir, text: &str) -> String {
    dir.file("recorded.json", text)
}

fn preflight_cli(args: &[&str]) -> (u8, Json, String) {
    let mut full = vec!["bedrock-probes", "preflight"];
    full.extend_from_slice(args);
    let (status, out, err) = exec(&full);
    let report = if out.is_empty() {
        Json::Null
    } else {
        json(&out)
    };
    (status, report, err)
}

#[test]
fn preflight_replays_a_recording_and_exits_by_the_verdict() {
    let dir = TempDir::new("pre-recorded");
    let recorded = recording(
        &dir,
        r#"{"calls": [
            {"service": "bedrock-runtime", "operation": "Converse",
             "request": {"modelId": "amazon.nova-lite-v1:0",
                         "inferenceConfig": {"maxTokens": 1},
                         "messages": [{"content": [{"text": "ping"}], "role": "user"}]},
             "response": {}},
            {"service": "bedrock-runtime", "operation": "Converse",
             "request": {"modelId": "amazon.denied-v1:0",
                         "inferenceConfig": {"maxTokens": 1},
                         "messages": [{"role": "user", "content": [{"text": "ping"}]}]},
             "error": {"code": "AccessDeniedException",
                       "message": "not authorized to perform bedrock:InvokeModel"}},
            {"service": "service-quotas", "operation": "ListServiceQuotas",
             "request": {"ServiceCode": "bedrock"},
             "response": {"Quotas": [{"QuotaName": "On-demand model inference requests per minute for Amazon Nova Lite", "Value": 20}]}}
        ]}"#,
    );
    let (status, report, err) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0",
        "--dataset-size",
        "25",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(report.get("ok"), Some(&Json::Bool(true)));
    let entry = model_entry(&report, 0);
    assert_eq!(entry.get("rpm_quota"), Some(&Json::Int(20)));
    assert!(entry.get("quota_warning").is_some());
    let (status, report, _) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0,amazon.denied-v1:0",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 1);
    assert_eq!(report.get("reason").and_then(Json::as_str), Some("authz"));
}

#[test]
fn an_unrecorded_request_fails_closed_naming_service_and_operation_only() {
    let dir = TempDir::new("pre-unrecorded");
    // The recorded request differs from the one the probe sends.
    let recorded = recording(
        &dir,
        r#"{"calls": [{"service": "bedrock-runtime", "operation": "Converse",
                       "request": {"modelId": "amazon.other-v1:0"}, "response": {}}]}"#,
    );
    let model = format!("amazon.{SECRET}-v1:0");
    let (status, report, _) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        &model,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 1);
    let detail = report.get("detail").and_then(Json::as_str).expect("detail");
    assert_eq!(
        report.get("reason").and_then(Json::as_str),
        Some(preflight::REASON_UNRECORDED)
    );
    assert!(detail.contains("bedrock-runtime Converse"), "{detail}");
    assert!(!detail.contains(SECRET));
}

#[test]
fn a_failure_entry_compares_its_request_before_it_is_replayed() {
    let dir = TempDir::new("pre-failure-request");
    let recorded = recording(
        &dir,
        r#"{"calls": [{"service": "bedrock-runtime", "operation": "Converse",
                       "request": {"modelId": "amazon.other-v1:0"},
                       "error": {"code": "AccessDeniedException", "message": "denied"}}]}"#,
    );
    let (status, report, _) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 1);
    assert_eq!(
        report.get("reason").and_then(Json::as_str),
        Some(preflight::REASON_UNRECORDED)
    );
}

#[test]
fn recorded_error_text_is_redacted_before_it_reaches_the_verdict() {
    let dir = TempDir::new("pre-redacted");
    let message = format!(
        "denied Bearer {SECRET} for the principal\\r\\n{}",
        "y".repeat(1500)
    );
    let recorded = recording(
        &dir,
        &format!(
            r#"{{"calls": [{{"service": "bedrock-runtime", "operation": "Converse",
                           "error": {{"code": "AccessDeniedException", "message": "{message}"}}}}]}}"#
        ),
    );
    let (status, report, err) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0",
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 1, "{err}");
    let text = report.to_compact_string();
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(report.get("reason").and_then(Json::as_str), Some("authz"));
}

#[test]
fn the_recording_rejects_wrong_typed_members() {
    let dir = TempDir::new("pre-shape");
    let base = |extra: &str| {
        format!(
            r#"{{"calls": [{{"service": "bedrock-runtime", "operation": "Converse", {extra}}}]}}"#
        )
    };
    for (text, needle) in [
        (base(r#""request": null, "response": {}"#), "\"request\" must be an object"),
        (base(r#""request": [], "response": {}"#), "\"request\" must be an object"),
        (base(r#""response": null"#), "exactly one of"),
        (base(r#""response": {}, "error": {}"#), "exactly one of"),
        (base(r#""error": null"#), "exactly one of"),
        (base(r#""error": {"kind": "weird"}"#), "error.kind"),
        (base(r#""error": {"code": 5}"#), "error \"code\" must be a string"),
        (base(r#""error": {"message": null}"#), "error \"message\" must be a string"),
        (base(r#""error": {"status": "403"}"#), "error \"status\" must be an integer"),
        (r#"{"calls": null}"#.to_string(), "\"calls\" must be a list"),
        (r#"{"calls": {}}"#.to_string(), "\"calls\" must be a list"),
        (r#"{"calls": [1]}"#.to_string(), "recorded call 0"),
        (
            r#"{"calls": [{"service": "ec2", "operation": "X", "response": {}}]}"#.to_string(),
            "\"service\"",
        ),
        (
            r#"{"calls": [{"service": "bedrock-runtime", "operation": "Nope", "response": {}}]}"#.to_string(),
            "\"operation\"",
        ),
        (
            r#"{"calls": [{"service": "bedrock-mantle", "operation": "Responses", "error": {"message": "m"}}]}"#.to_string(),
            "HTTP \"status\"",
        ),
    ] {
        let path = recording(&dir, &text);
        let (status, out, err) = exec(&[
            "bedrock-probes", "preflight", "--region", REGION, "--models", "amazon.nova-lite-v1:0", "--recorded", &path,
        ]);
        assert_eq!(status, 1, "{text}");
        assert!(out.is_empty());
        assert!(err.contains("invalid recording"), "{err}");
        assert!(err.contains(needle), "{needle} not in {err}");
    }
}

#[test]
fn a_missing_or_unreadable_recording_is_a_clear_error() {
    let dir = TempDir::new("pre-unreadable");
    let missing = dir.path("absent.json");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "preflight",
        "--region",
        REGION,
        "--models",
        "m.x",
        "--recorded",
        &missing,
    ]);
    assert_eq!(status, 1);
    assert!(
        err.contains("cannot read") && err.contains("file does not exist"),
        "{err}"
    );
    let bad = dir.file("bad.json", "{not json");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "preflight",
        "--region",
        REGION,
        "--models",
        "m.x",
        "--recorded",
        &bad,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("not valid JSON"), "{err}");
    #[cfg(unix)]
    {
        let target = dir.file("real.json", r#"{"calls": []}"#);
        let link = dir.link("link.json", &target);
        let (status, _, err) = exec(&[
            "bedrock-probes",
            "preflight",
            "--region",
            REGION,
            "--models",
            "m.x",
            "--recorded",
            &link,
        ]);
        assert_eq!(status, 1);
        assert!(err.contains("symbolic links are not followed"), "{err}");
    }
}

#[test]
fn preflight_without_a_recording_fails_closed_with_a_verdict() {
    let (status, report, err) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0,openai.gpt-5.5",
    ]);
    assert_eq!(status, 1, "{err}");
    assert_eq!(report.get("ok"), Some(&Json::Bool(false)));
    assert_eq!(
        report.get("reason").and_then(Json::as_str),
        Some(preflight::REASON_NO_LIVE)
    );
}

#[test]
fn preflight_arguments_follow_argparse() {
    let (status, _, err) = preflight_cli(&["--models", "m.x"]);
    assert_eq!(status, 2);
    assert!(err.contains("--region is required"), "{err}");
    let (status, _, err) = preflight_cli(&["--region", REGION]);
    assert_eq!(status, 2);
    assert!(err.contains("--models is required"), "{err}");
    let (status, _, err) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "m.x",
        "--runtime-api",
        "bogus",
    ]);
    assert_eq!(status, 2);
    assert!(err.contains("--runtime-api must be one of"), "{err}");
    for bad in ["abc", "1.5", "", "_1", "1_", "1__0", "0x10"] {
        let (status, _, err) = preflight_cli(&[
            "--region",
            REGION,
            "--models",
            "m.x",
            &format!("--dataset-size={bad}"),
        ]);
        assert_eq!(status, 2, "{bad:?}");
        let _ = err;
    }
    // Python `int()`: a sign, surrounding space and single underscores.
    for good in ["+5", " 7 ", "1_0", "007"] {
        let (status, _, _) = preflight_cli(&[
            "--region",
            REGION,
            "--models",
            "m.x",
            &format!("--dataset-size={good}"),
        ]);
        assert_eq!(status, 1, "{good:?} parses (the probe fails closed)");
    }
    // A negative size, a dash or an empty value is refused by the router.
    let (status, _, _) =
        preflight_cli(&["--region", REGION, "--models", "m.x", "--dataset-size=-5"]);
    assert_eq!(status, 2);
    let (status, _, _) = preflight_cli(&["--region", REGION, "--models", "-"]);
    assert_eq!(status, 2);
    let (status, _, _) = preflight_cli(&["--region", REGION, "--models", "m.x", "--recorded", "-"]);
    assert_eq!(status, 2);
}

#[test]
fn preflight_hostile_text_never_reaches_output() {
    let dir = TempDir::new("pre-hostile");
    let recorded = recording(
        &dir,
        &format!(
            r#"{{"calls": [{{"service": "bedrock-runtime", "operation": "Converse",
                           "error": {{"code": "{HOSTILE_JSON}", "message": "{HOSTILE_JSON} Bearer {SECRET}"}}}}]}}"#
        ),
    );
    for models in [HOSTILE, "amazon.nova-lite-v1:0", "openai.gpt-5.5"] {
        let (_, out, err) = exec(&[
            "bedrock-probes",
            "preflight",
            "--region",
            REGION,
            "--models",
            models,
            "--recorded",
            &recorded,
        ]);
        assert_clean("stdout", &out);
        assert_clean("stderr", &err);
    }
    let (_, out, err) = exec(&[
        "bedrock-probes",
        "preflight",
        "--region",
        HOSTILE,
        "--models",
        "m.x",
    ]);
    assert_clean("stdout", &out);
    assert_clean("stderr", &err);
}

const HOSTILE_JSON: &str = "x\\u001b[31m\\u0007\\r\\nFORGED: line\\u202egnp.exe";

#[test]
fn classify_names_the_endpoint_family_of_each_id() {
    let (status, out, _) = exec(&[
        "bedrock-probes", "classify", "--models",
        "openai.gpt-5.5, us.openai.gpt-6-astra,amazon.titan-embed-text-v2:0,anthropic.claude-opus-5-5", "--region", "us-west-2",
    ]);
    assert_eq!(status, 0);
    let rows = json(&out);
    let rows = match rows.get("models") {
        Some(Json::Array(items)) => items.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(rows.len(), 4);
    let flag = |row: &Json, name: &str| row.get(name) == Some(&Json::Bool(true));
    assert!(flag(&rows[0], "mantle") && flag(&rows[0], "iam_mantle"));
    assert!(
        !flag(&rows[1], "mantle")
            && flag(&rows[1], "inference_profile")
            && flag(&rows[1], "gpt6_cris")
    );
    assert!(flag(&rows[2], "embedding"));
    assert!(!flag(&rows[3], "mantle") && flag(&rows[3], "iam_mantle"));
    let (status, _, err) = exec(&["bedrock-probes", "classify"]);
    assert_eq!(status, 2);
    assert!(err.contains("--models is required"), "{err}");
}

// ---------------------------------------------------------------------------
// source_baseline (test_source_baseline.py)

fn headers_of(request: &baseline::Request) -> Vec<(&str, &str)> {
    request
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

#[test]
fn the_openai_shape_uses_max_completion_tokens() {
    let request =
        baseline::build_openai_request("gpt-4o", "sys", "hi", None, "sk-test").expect("builds");
    assert_eq!(request.url, "https://api.openai.com/v1/chat/completions");
    assert_eq!(
        request.body.get("max_completion_tokens"),
        Some(&Json::Int(baseline::MAX_TOKENS))
    );
    assert!(
        request.body.get("max_tokens").is_none(),
        "gpt-5.x rejects max_tokens (HTTP 400)"
    );
    assert_eq!(
        request.body.get("messages"),
        Some(&json(
            r#"[{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}]"#
        ))
    );
    assert!(headers_of(&request).contains(&("Authorization", "Bearer sk-test")));
    let bare = baseline::build_openai_request("gpt-4o", "", "hi", None, "sk-test").expect("builds");
    assert_eq!(
        bare.body.get("messages"),
        Some(&json(r#"[{"role": "user", "content": "hi"}]"#))
    );
}

#[test]
fn the_anthropic_shape_uses_a_top_level_system() {
    let request =
        baseline::build_anthropic_request("claude-3-5-sonnet", "sys", "hi", None, "sk-ant")
            .expect("builds");
    assert_eq!(request.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(
        request.body.get("system").and_then(Json::as_str),
        Some("sys")
    );
    assert_eq!(
        request.body.get("max_tokens"),
        Some(&Json::Int(baseline::MAX_TOKENS))
    );
    assert!(headers_of(&request).contains(&("anthropic-version", "2023-06-01")));
    assert!(headers_of(&request).contains(&("x-api-key", "sk-ant")));
    let bare = baseline::build_anthropic_request("m", "", "hi", None, "k").expect("builds");
    assert!(bare.body.get("system").is_none());
}

#[test]
fn the_gemini_key_is_a_header_never_in_the_url() {
    let request = baseline::build_gemini_request("gemini-1.5-pro", "sys", "hi", None, "AIza-test")
        .expect("builds");
    assert!(
        !request.url.contains("AIza-test"),
        "the key must never be a URL query parameter"
    );
    assert!(!request.url.contains("key="));
    assert_eq!(
        request.url,
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-pro:generateContent"
    );
    assert!(headers_of(&request).contains(&("x-goog-api-key", "AIza-test")));
    assert_eq!(
        request.body.get("systemInstruction"),
        Some(&json(r#"{"parts": [{"text": "sys"}]}"#))
    );
    assert_eq!(
        request
            .body
            .get("generationConfig")
            .and_then(|c| c.get("maxOutputTokens")),
        Some(&Json::Int(baseline::MAX_TOKENS))
    );
}

#[test]
fn every_provider_url_is_an_official_host_and_a_key_goes_nowhere_else() {
    let urls = [
        baseline::build_openai_request("m", "", "x", None, "KEY-A")
            .expect("a")
            .url,
        baseline::build_anthropic_request("m", "", "x", None, "KEY-B")
            .expect("b")
            .url,
        baseline::build_gemini_request("m", "", "x", None, "KEY-C")
            .expect("c")
            .url,
    ];
    for url in &urls {
        assert!(
            baseline::OFFICIAL_URL_PREFIXES
                .iter()
                .any(|prefix| url.starts_with(prefix)),
            "{url}"
        );
        assert!(!url.contains("KEY-"));
    }
    // A model id cannot add a path, a query, a fragment or another host.
    for model in [
        "../x",
        "a/b",
        "m?key=1",
        "m#x",
        "m@evil.example",
        "m:generateContent",
        "",
        "m m",
        "m\n",
    ] {
        assert!(
            baseline::build_gemini_request(model, "", "x", None, "k").is_err(),
            "{model:?}"
        );
    }
    assert!(baseline::is_gemini_model_id("gemini-2.0-flash-001"));
}

#[test]
fn image_requests_keep_the_bytes_and_the_normalized_mime() {
    let raw = b"test-image-bytes";
    let encoded = base64_encode(raw);
    let image = Some(("receipt.jpg", raw.as_slice()));
    let openai = baseline::build_openai_request("m", "Be precise", "Read the total", image, "k")
        .expect("openai");
    let messages = openai.body.get("messages").cloned().expect("messages");
    assert_eq!(
        messages,
        json(&format!(
            r#"[{{"role": "system", "content": "Be precise"}},
                {{"role": "user", "content": [{{"type": "text", "text": "Read the total"}},
                  {{"type": "image_url", "image_url": {{"url": "data:image/jpeg;base64,{encoded}", "detail": "auto"}}}}]}}]"#
        ))
    );
    let anthropic =
        baseline::build_anthropic_request("m", "Be precise", "Read the total", image, "k")
            .expect("anthropic");
    assert_eq!(
        anthropic.body.get("messages"),
        Some(&json(&format!(
            r#"[{{"role": "user", "content": [
                {{"type": "image", "source": {{"type": "base64", "media_type": "image/jpeg", "data": "{encoded}"}}}},
                {{"type": "text", "text": "Read the total"}}]}}]"#
        )))
    );
    let gemini = baseline::build_gemini_request("m", "Be precise", "Read the total", image, "k")
        .expect("gemini");
    assert_eq!(
        gemini.body.get("contents"),
        Some(&json(&format!(
            r#"[{{"parts": [{{"text": "Read the total"}},
                {{"inlineData": {{"mimeType": "image/jpeg", "data": "{encoded}"}}}}]}}]"#
        )))
    );
    for build in [
        baseline::build_openai_request,
        baseline::build_anthropic_request,
        baseline::build_gemini_request,
    ] {
        let error = build("m", "", "x", Some(("receipt.txt", raw.as_slice())), "k")
            .expect_err("unsupported");
        assert!(error.contains("unsupported image type"), "{error}");
    }
}

/// A client that records the requests it is sent and answers with `reply`.
type SentRequest = (String, Vec<(String, String)>, Json);
type Reply = Box<dyn FnMut(usize) -> Result<Json, HttpFailure>>;

struct Replies {
    sent: Vec<SentRequest>,
    reply: Reply,
}

impl Replies {
    fn new(reply: impl FnMut(usize) -> Result<Json, HttpFailure> + 'static) -> Self {
        Self {
            sent: Vec::new(),
            reply: Box::new(reply),
        }
    }

    fn text(answer: &'static str) -> Self {
        Self::new(move |_| {
            Ok(json(&format!(
                r#"{{"choices": [{{"message": {{"content": "{answer}"}}}}]}}"#
            )))
        })
    }
}

impl HttpClient for Replies {
    fn send(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        body: &Json,
    ) -> Result<Json, HttpFailure> {
        self.sent
            .push((url.to_string(), headers.to_vec(), body.clone()));
        let index = self.sent.len() - 1;
        (self.reply)(index)
    }
}

fn config(secrets: &[String]) -> baseline::Run<'_> {
    baseline::Run {
        provider: Provider::OpenAi,
        model: "source-model",
        api_key: "offline-test-key",
        secrets,
    }
}

fn row_status(rows: &[Json], index: usize) -> String {
    rows[index]
        .get("status")
        .and_then(Json::as_str)
        .expect("status")
        .to_string()
}

fn prompts(lines: &[&str]) -> Vec<Json> {
    baseline::parse_dataset(&lines.join("\n")).expect("dataset")
}

#[test]
fn the_image_cases_land_a_row_or_an_error_without_a_call() {
    // test_live_baseline_preserves_image_or_records_error, per case.
    let dir = TempDir::new("baseline-images");
    let jpg = dir.bytes("receipt.jpg", b"test-image-bytes");
    let txt = dir.bytes("receipt.txt", b"test-image-bytes");
    let missing = dir.path("absent.jpg");
    let secrets = vec!["offline-test-key".to_string()];
    for (path, live) in [(&jpg, true), (&txt, false), (&missing, false)] {
        let line = format!(
            r#"{{"id": "image-case", "user_prompt": "Read the total", "system_prompt": "Be precise", "image_path": "{}"}}"#,
            path.replace('\\', "\\\\")
        );
        let mut client = Replies::text("42.00");
        let rows = baseline::run(
            &config(&secrets),
            Vec::new(),
            &prompts(&[&line]),
            &mut client,
        );
        assert_eq!(rows.len(), 1);
        if live {
            assert_eq!(row_status(&rows, 0), "live");
            assert_eq!(client.sent.len(), 1);
            let url = client.sent[0].2.get("messages").cloned().expect("messages");
            assert!(url.to_compact_string().contains("data:image/jpeg;base64,"));
        } else {
            assert!(
                row_status(&rows, 0).starts_with("error:"),
                "{}",
                row_status(&rows, 0)
            );
            assert_eq!(
                rows[0].get("source_response").and_then(Json::as_str),
                Some("")
            );
            assert!(client.sent.is_empty());
        }
    }
    let status = {
        let line = format!(r#"{{"id": "m", "user_prompt": "p", "image_path": "{missing}"}}"#);
        let rows = baseline::run(
            &config(&secrets),
            Vec::new(),
            &prompts(&[&line]),
            &mut Replies::text("x"),
        );
        row_status(&rows, 0)
    };
    assert!(
        status.starts_with("error: FileNotFoundError: [Errno 2] No such file or directory"),
        "{status}"
    );
}

#[test]
fn the_env_file_loader_ignores_blank_and_malformed_lines() {
    let pairs = crate::bedrock_resolve::source::parse_env_file(
        "\n# comment-ish\nOPENAI_API_KEY=sk-live\n  \nGEMINI_API_KEY = spaced\n",
    );
    assert_eq!(
        pairs,
        [
            ("OPENAI_API_KEY".to_string(), "sk-live".to_string()),
            ("GEMINI_API_KEY ".to_string(), " spaced".to_string())
        ]
    );
}

#[test]
fn the_provider_is_selected_from_the_file_and_the_stated_provider_is_authoritative() {
    use crate::bedrock_resolve::source::pick_provider;
    let file = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    };
    let both = file(&[("OPENAI_API_KEY", "a"), ("ANTHROPIC_API_KEY", "b")]);
    assert_eq!(pick_provider(&both, "anthropic"), Some("ANTHROPIC_API_KEY"));
    assert_eq!(pick_provider(&both, "openai"), Some("OPENAI_API_KEY"));
    assert_eq!(
        pick_provider(&both, "google"),
        None,
        "a stated provider without a key is never guessed"
    );
    assert_eq!(pick_provider(&both, ""), Some("OPENAI_API_KEY"));
    let gemini = file(&[("GEMINI_API_KEY", "AIza-file")]);
    assert_eq!(pick_provider(&gemini, ""), Some("GEMINI_API_KEY"));
    assert_eq!(
        Provider::from_key_name("GEMINI_API_KEY"),
        Some(Provider::Gemini)
    );
}

#[test]
fn error_detail_extracts_message_and_param_and_is_bounded() {
    let body = br#"{"error": {"message": "Unsupported parameter: max_tokens", "param": "max_tokens", "type": "invalid_request_error"}}"#;
    assert_eq!(
        baseline::error_detail(400, body, &[]),
        "Unsupported parameter: max_tokens (param: max_tokens)"
    );
    let long = b"<html>upstream error</html>".repeat(200);
    let detail = baseline::error_detail(502, &long, &[]);
    assert!(detail.starts_with("<html>upstream error"));
    assert!(detail.chars().count() <= 200);
    assert_eq!(baseline::error_detail(400, b"", &[]), "");
    assert_eq!(baseline::error_detail(400, br#"{"other": 1}"#, &[]), "");
    // A message that is not text falls back to the raw body.
    assert_eq!(
        baseline::error_detail(400, br#"{"error": {"message": null}}"#, &[]),
        r#"{"error": {"message": null}}"#
    );
    assert_eq!(
        baseline::error_detail(400, br#"{"error": "plain"}"#, &[]),
        r#"{"error": "plain"}"#
    );
    assert_eq!(
        baseline::error_detail(400, br#"{"error": {"message": "m", "param": 0}}"#, &[]),
        "m"
    );
}

#[test]
fn auth_error_bodies_are_suppressed_entirely() {
    let body = br#"{"error": {"message": "bad key sk-live-SECRETKEY123"}}"#;
    for code in [401, 403] {
        assert_eq!(
            baseline::error_detail(code, body, &["sk-live-SECRETKEY123".to_string()]),
            ""
        );
    }
}

#[test]
fn an_echoed_key_never_reaches_a_detail() {
    let secrets = vec!["sk-live-SECRETKEY123".to_string()];
    let json_body = br#"{"error": {"message": "invalid key sk-live-SECRETKEY123 for model"}}"#;
    let detail = baseline::error_detail(400, json_body, &secrets);
    assert!(!detail.contains("SECRETKEY123") && detail.contains("***"));
    let text_body = b"upstream said: sk-live-SECRETKEY123 rejected";
    let detail = baseline::error_detail(500, text_body, &secrets);
    assert!(!detail.contains("SECRETKEY123") && detail.contains("***"));
    let with_param = br#"{"error": {"message": "m", "param": "sk-live-SECRETKEY123"}}"#;
    assert!(!baseline::error_detail(400, with_param, &secrets).contains("SECRETKEY123"));
}

#[test]
fn a_failed_request_row_keeps_the_http_prefix_and_never_the_key() {
    let secrets = vec!["offline-test-key".to_string()];
    let mut client = Replies::new(|_| {
        Err(HttpFailure::Status {
            code: 400,
            reason: "Bad Request".to_string(),
            body: br#"{"error": {"message": "Unsupported parameter"}}"#.to_vec(),
        })
    });
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[r#"{"id": 1, "user_prompt": "p"}"#]),
        &mut client,
    );
    let status = row_status(&rows, 0);
    assert!(status.starts_with("http_400: Bad Request"), "{status}");
    assert!(status.contains("Unsupported parameter"));
    let mut auth = Replies::new(|_| {
        Err(HttpFailure::Status {
            code: 401,
            reason: "Unauthorized".to_string(),
            body: b"sk key echo offline-test-key".to_vec(),
        })
    });
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[r#"{"id": 1, "user_prompt": "p"}"#]),
        &mut auth,
    );
    assert_eq!(row_status(&rows, 0), "http_401: Unauthorized");
    let mut transport = Replies::new(|_| {
        Err(HttpFailure::Transport {
            kind: "ValueError".to_string(),
            message: "Invalid header value b'Bearer offline-test-key\\rX'".to_string(),
        })
    });
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[r#"{"id": 1, "user_prompt": "p"}"#]),
        &mut transport,
    );
    let status = row_status(&rows, 0);
    assert!(
        !status.contains("offline-test-key") && status.contains("***"),
        "{status}"
    );
}

#[test]
fn failure_text_in_a_row_is_escaped_bounded_and_on_one_line() {
    let secrets = vec!["offline-test-key".to_string(), SECRET.to_string()];
    let mut client = Replies::new(|_| {
        Err(HttpFailure::Transport {
            kind: "URLError".to_string(),
            message: format!("{HOSTILE} offline-test-key {SECRET}{}", "z".repeat(5000)),
        })
    });
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[r#"{"id": "a", "user_prompt": "p"}"#]),
        &mut client,
    );
    let status = row_status(&rows, 0);
    assert_clean("status", &status);
    assert!(!status.contains('\n') && !status.contains('\r'));
    assert!(!status.contains("offline-test-key"));
    assert!(status.len() < 1500, "{}", status.len());
}

#[test]
fn live_rows_are_kept_and_failed_rows_are_retried() {
    let secrets = Vec::new();
    let text = concat!(
        "{\"id\": \"a\", \"source_response\": \"kept\", \"status\": \"live\"}\n",
        "\n",
        "{\"id\": \"b\", \"source_response\": \"\", \"status\": \"http_500: Server Error\"}\n",
    );
    let kept = baseline::parse_live_rows(text).expect("rows");
    assert_eq!(kept.len(), 1);
    let dataset = prompts(&[
        r#"{"id": "a", "user_prompt": "one"}"#,
        r#"{"id": "b", "user_prompt": "two"}"#,
        "",
        r#"{"id": "c", "user_prompt": "three"}"#,
    ]);
    let mut client = Replies::text("fresh");
    let rows = baseline::run(&config(&secrets), kept, &dataset, &mut client);
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row.get("id").and_then(Json::as_str).expect("id"))
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(
        client.sent.len(),
        2,
        "the provider is not called again for a live row"
    );
    assert_eq!(
        rows[0].get("source_response").and_then(Json::as_str),
        Some("kept")
    );
    assert_eq!(
        rows[1].get("source_response").and_then(Json::as_str),
        Some("fresh")
    );
    // An id match is exact: 1 is not "1".
    let kept = baseline::parse_live_rows(r#"{"id": 1, "source_response": "x", "status": "live"}"#)
        .expect("rows");
    let mut client = Replies::text("fresh");
    let rows = baseline::run(
        &config(&secrets),
        kept,
        &prompts(&[r#"{"id": "1", "user_prompt": "p"}"#]),
        &mut client,
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn a_live_response_that_echoes_the_key_is_redacted() {
    let secrets = vec!["offline-test-key".to_string()];
    let mut client = Replies::text("the key is offline-test-key");
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[r#"{"id": 1, "user_prompt": "p"}"#]),
        &mut client,
    );
    assert_eq!(
        rows[0].get("source_response").and_then(Json::as_str),
        Some("the key is ***")
    );
    assert_eq!(row_status(&rows, 0), "live");
}

#[test]
fn dataset_errors_name_the_line_not_the_text() {
    let error =
        baseline::parse_dataset(&format!("{{\"id\": 1}}\n{{bad {SECRET}")).expect_err("bad line");
    assert_eq!(error, "dataset line 2 is not valid JSON");
    for text in [
        r#"{"user_prompt": "p"}"#,
        r#"{"id": [1]}"#,
        r#"{"id": {}}"#,
        "[1]",
        r#""s""#,
    ] {
        let error = baseline::parse_dataset(text).expect_err("no id");
        assert_eq!(error, "dataset line 1 has no scalar \"id\"", "{text}");
    }
    assert!(baseline::parse_live_rows("{bad").is_err());
}

#[test]
fn a_prompt_without_text_is_an_error_row_not_a_crash() {
    let secrets = Vec::new();
    let mut client = Replies::text("x");
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[
            r#"{"id": 1}"#,
            r#"{"id": 2, "user_prompt": 5}"#,
            r#"{"id": 3, "user_prompt": "p", "image_path": 5}"#,
            r#"{"id": 4, "user_prompt": "p", "system_prompt": null, "image_path": ""}"#,
        ]),
        &mut client,
    );
    assert_eq!(row_status(&rows, 0), "error: KeyError: 'user_prompt'");
    assert!(row_status(&rows, 1).starts_with("error: TypeError"));
    assert!(row_status(&rows, 2).starts_with("error: TypeError"));
    assert_eq!(row_status(&rows, 3), "live");
    assert_eq!(client.sent.len(), 1);
}

#[test]
fn provider_replies_are_read_for_visible_text_only() {
    // Anthropic: thinking blocks are not the answer; an incomplete reply is refused.
    let reply = json(
        r#"{"content": [{"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": "4"}, {"type": "text", "text": "2"}], "stop_reason": "end_turn"}"#,
    );
    assert_eq!(
        baseline::extract_anthropic_text(&reply).expect("text"),
        "42"
    );
    for stop in [
        "max_tokens",
        "model_context_window_exceeded",
        "refusal",
        "tool_use",
    ] {
        let reply = json(&format!(
            r#"{{"content": [{{"type": "text", "text": "partial"}}], "stop_reason": "{stop}"}}"#
        ));
        let error = baseline::extract_anthropic_text(&reply).expect_err("refused");
        assert!(error.contains(&format!("stop_reason={stop}")), "{error}");
    }
    let empty = json(r#"{"content": [{"type": "thinking", "thinking": "x"}]}"#);
    assert!(baseline::extract_anthropic_text(&empty)
        .expect_err("no text")
        .contains("stop_reason=unknown"));
    assert!(baseline::extract_anthropic_text(&json(r#"{"content": null}"#)).is_err());
    assert!(baseline::extract_anthropic_text(&json(r#"{"content": [{"type": "text"}]}"#)).is_err());
    // OpenAI and Gemini: the first choice or candidate; a null content is not a response.
    assert_eq!(
        baseline::extract_openai_text(&json(r#"{"choices": [{"message": {"content": "a"}}]}"#))
            .expect("a"),
        "a"
    );
    for bad in [
        r"{}",
        r#"{"choices": []}"#,
        r#"{"choices": null}"#,
        r#"{"choices": [{"message": {"content": null}}]}"#,
        r#"{"choices": [1]}"#,
    ] {
        assert!(baseline::extract_openai_text(&json(bad)).is_err(), "{bad}");
    }
    let gemini = json(r#"{"candidates": [{"content": {"parts": [{"text": "g"}]}}]}"#);
    assert_eq!(baseline::extract_gemini_text(&gemini).expect("g"), "g");
    for bad in [
        r"{}",
        r#"{"candidates": [{"content": {}}]}"#,
        r#"{"candidates": [{"content": {"parts": [{}]}}]}"#,
    ] {
        assert!(baseline::extract_gemini_text(&json(bad)).is_err(), "{bad}");
    }
}

// ---------------------------------------------------------------------------
// the source-baseline command

struct Case {
    dir: TempDir,
    env: String,
    dataset: String,
    output: String,
}

fn make_case(key_line: &str, rows: &[&str]) -> Case {
    let dir = TempDir::new("baseline-cli");
    let env = dir.file("source.env", key_line);
    let dataset = dir.file("prompts.jsonl", &rows.join("\n"));
    let output = dir.path("out/rows.jsonl");
    Case {
        dir,
        env,
        dataset,
        output,
    }
}

fn baseline_cli(case: &Case, extra: &[&str]) -> (u8, String, String) {
    let mut args = vec![
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--source-model-id",
        "source-model",
        "--dataset",
        &case.dataset,
        "--output",
        &case.output,
    ];
    args.extend_from_slice(extra);
    exec(&args)
}

fn output_rows(case: &Case) -> Vec<Json> {
    fs::read_to_string(&case.output)
        .expect("output file")
        .lines()
        .map(json)
        .collect()
}

const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";

fn recorded_openai(case: &Case, calls: &str) -> String {
    case.dir
        .file("recorded.json", &format!(r#"{{"calls": [{calls}]}}"#))
}

#[test]
fn source_baseline_replays_a_recording_into_the_output_file() {
    let case = make_case(
        "OPENAI_API_KEY=offline-test-key\n",
        &[
            r#"{"id": "a", "user_prompt": "hello", "system_prompt": "Be precise"}"#,
            r#"{"id": "b", "user_prompt": "boom"}"#,
        ],
    );
    let recorded = recorded_openai(
        &case,
        &format!(
            r#"{{"url": "{OPENAI_URL}",
                "request": {{"model": "source-model", "max_completion_tokens": 4096,
                            "messages": [{{"role": "system", "content": "Be precise"}}, {{"role": "user", "content": "hello"}}]}},
                "response": {{"choices": [{{"message": {{"content": "42.00"}}}}]}}}},
               {{"url": "{OPENAI_URL}",
                "error": {{"status": 400, "reason": "Bad Request",
                          "body": "{{\"error\": {{\"message\": \"Unsupported parameter\", \"param\": \"max_tokens\"}}}}"}}}}"#
        ),
    );
    let (status, out, err) = baseline_cli(&case, &["--recorded", &recorded]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(out, "live source baselines: 1/2\n");
    let rows = output_rows(&case);
    assert_eq!(
        rows[0],
        json(r#"{"id": "a", "source_response": "42.00", "status": "live"}"#)
    );
    assert_eq!(
        row_status(&rows, 1),
        "http_400: Bad Request — Unsupported parameter (param: max_tokens)"
    );
    assert!(!fs::read_to_string(&case.output)
        .expect("rows")
        .contains("offline-test-key"));
}

#[test]
fn source_baseline_resumes_without_calling_the_provider_for_live_rows() {
    let case = make_case(
        "ANTHROPIC_API_KEY=offline-test-key\n",
        &[
            r#"{"id": "a", "user_prompt": "one"}"#,
            r#"{"id": "b", "user_prompt": "two"}"#,
        ],
    );
    fs::create_dir_all(Path::new(&case.output).parent().expect("parent")).expect("dir");
    fs::write(
        &case.output,
        "{\"id\": \"a\", \"source_response\": \"kept\", \"status\": \"live\"}\n{\"id\": \"b\", \"source_response\": \"\", \"status\": \"http_500: x\"}\n",
    )
    .expect("previous output");
    let recorded = recorded_openai(
        &case,
        r#"{"url": "https://api.anthropic.com/v1/messages",
            "response": {"content": [{"type": "text", "text": "two!"}], "stop_reason": "end_turn"}}"#,
    );
    let (status, out, err) = baseline_cli(&case, &["--recorded", &recorded]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        out,
        "RESUME: 1 live baselines kept, 1 to fetch\nlive source baselines: 2/2\n"
    );
    let rows = output_rows(&case);
    assert_eq!(
        rows[0].get("source_response").and_then(Json::as_str),
        Some("kept")
    );
    assert_eq!(
        rows[1].get("source_response").and_then(Json::as_str),
        Some("two!")
    );
}

#[test]
fn source_baseline_without_a_recording_sends_nothing_and_records_error_rows() {
    let case = make_case(
        "GEMINI_API_KEY=offline-test-key\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let (status, out, err) = baseline_cli(&case, &[]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(out, "live source baselines: 0/1\n");
    let status = row_status(&output_rows(&case), 0);
    assert!(
        status.starts_with("error: ConnectionError: this build includes no live HTTP client"),
        "{status}"
    );
}

#[test]
fn source_baseline_validates_its_inputs_before_any_request() {
    let case = make_case(
        "OPENAI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    // No provider key for the stated provider: FAIL on stderr, status 2.
    let (status, out, err) = baseline_cli(&case, &["--source-provider", "anthropic"]);
    assert_eq!((status, out.as_str()), (2, ""));
    assert_eq!(
        err,
        "FAIL: no usable provider key in the env file (check SOURCE_PROVIDER)\n"
    );
    assert!(!Path::new(&case.output).exists());
    // An unsafe Gemini model id is refused before the URL is built.
    let gemini = make_case(
        "GEMINI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "x"}"#],
    );
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &gemini.env,
        "--source-model-id",
        "m/../x",
        "--dataset",
        &gemini.dataset,
        "--output",
        &gemini.output,
    ]);
    assert_eq!(status, 2);
    assert!(
        err.starts_with("FAIL: the source model id is not valid for the Gemini endpoint"),
        "{err}"
    );
    // A missing input names its variable (the flag is the alternative).
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--dataset",
        &case.dataset,
        "--output",
        &case.output,
    ]);
    if std::env::var("SOURCE_MODEL_ID").is_err() {
        assert_eq!(status, 2);
        assert_eq!(err, "FAIL: required env var 'SOURCE_MODEL_ID' not set\n");
    }
    // Bad dataset and bad previous output name the line only.
    let bad = case.dir.file("bad.jsonl", &format!("{{bad {SECRET}"));
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--source-model-id",
        "m",
        "--dataset",
        &bad,
        "--output",
        &case.output,
    ]);
    assert_eq!(status, 1);
    assert_eq!(err, "error: dataset line 1 is not valid JSON\n");
    let missing_env = case.dir.path("none.env");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &missing_env,
        "--source-model-id",
        "m",
        "--dataset",
        &case.dataset,
        "--output",
        &case.output,
    ]);
    assert_eq!(status, 1);
    assert!(
        err.contains("cannot read") && err.contains("file does not exist"),
        "{err}"
    );
}

#[test]
fn source_baseline_creates_the_output_directory_and_accepts_a_bare_file_name_parent() {
    let case = make_case(
        "OPENAI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    assert!(!Path::new(&case.output).parent().expect("parent").exists());
    let (status, _, err) = baseline_cli(&case, &[]);
    assert_eq!(status, 0, "{err}");
    assert!(Path::new(&case.output).exists());
}

#[cfg(unix)]
#[test]
fn source_baseline_refuses_links_for_every_owned_read_and_write() {
    let case = make_case(
        "OPENAI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let env_link = case.dir.link("env-link", &case.env);
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &env_link,
        "--source-model-id",
        "m",
        "--dataset",
        &case.dataset,
        "--output",
        &case.output,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("symbolic links are not followed"), "{err}");
    let dataset_link = case.dir.link("dataset-link", &case.dataset);
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--source-model-id",
        "m",
        "--dataset",
        &dataset_link,
        "--output",
        &case.output,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("symbolic links are not followed"), "{err}");
    let victim = case.dir.file("victim.jsonl", "keep");
    let output_link = case.dir.link("output-link", &victim);
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--source-model-id",
        "m",
        "--dataset",
        &case.dataset,
        "--output",
        &output_link,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("symbolic links are not followed"), "{err}");
    assert_eq!(fs::read_to_string(&victim).expect("victim"), "keep");
    // An image that is a link is an error row, not a read through the link.
    let secret_file = case.dir.file("secret.jpg", "bytes");
    let image_link = case.dir.link("image.jpg", &secret_file);
    let line = format!(r#"{{"id": "i", "user_prompt": "p", "image_path": "{image_link}"}}"#);
    let secrets = Vec::new();
    let mut client = Replies::text("x");
    let rows = baseline::run(
        &config(&secrets),
        Vec::new(),
        &prompts(&[&line]),
        &mut client,
    );
    assert!(
        row_status(&rows, 0).starts_with("error: OSError: refusing to read a symbolic link"),
        "{}",
        row_status(&rows, 0)
    );
    assert!(client.sent.is_empty());
}

#[test]
fn source_baseline_never_treats_a_bare_dash_as_a_file() {
    let case = make_case(
        "OPENAI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let before = Path::new("-").exists();
    for args in [
        vec!["bedrock-probes", "source-baseline", "-"],
        vec![
            "bedrock-probes",
            "source-baseline",
            &case.env,
            "--dataset",
            "-",
        ],
        vec![
            "bedrock-probes",
            "source-baseline",
            &case.env,
            "--output",
            "-",
        ],
        vec![
            "bedrock-probes",
            "source-baseline",
            &case.env,
            "--recorded",
            "-",
        ],
        vec![
            "bedrock-probes",
            "source-baseline",
            &case.env,
            "--source-model-id=-",
        ],
    ] {
        let (status, out, _) = exec(&args);
        assert_eq!(status, 2, "{args:?}");
        assert!(out.is_empty());
    }
    assert_eq!(Path::new("-").exists(), before);
}

#[test]
fn the_http_recording_rejects_wrong_typed_members() {
    let case = make_case(
        "OPENAI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let url = |rest: &str| format!(r#"{{"url": "{OPENAI_URL}", {rest}}}"#);
    let cases: Vec<(String, &str)> = vec![
        (r#"{"response": {}}"#.to_string(), "\"url\""),
        (
            url(r#""request": null, "response": {}"#),
            "\"request\" must be an object",
        ),
        (url(r#""response": null"#), "exactly one of"),
        (
            url(r#""error": {"reason": "x"}"#),
            "needs an integer \"status\"",
        ),
        (url(r#""error": {"status": 99}"#), "error \"status\""),
        (
            url(r#""error": {"status": 400, "reason": null}"#),
            "error \"reason\"",
        ),
        (url(r#""error": {"kind": "other"}"#), "error.kind"),
        (
            url(r#""error": {"kind": "transport", "type": 5}"#),
            "error \"type\"",
        ),
    ];
    for (calls, needle) in &cases {
        let recorded = recorded_openai(&case, calls);
        let (status, _, err) = baseline_cli(&case, &["--recorded", &recorded]);
        assert_eq!(status, 1, "{calls}");
        assert!(
            err.contains("invalid recording") && err.contains(*needle),
            "{needle} not in {err}"
        );
    }
}

#[test]
fn an_unrecorded_request_is_an_error_row_that_names_no_request_value() {
    let case = make_case(
        "OPENAI_API_KEY=k\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let recorded = recorded_openai(
        &case,
        &format!(r#"{{"url": "{OPENAI_URL}", "request": {{"model": "other"}}, "response": {{}}}}"#),
    );
    let (status, _, _) = baseline_cli(&case, &["--recorded", &recorded]);
    assert_eq!(status, 0);
    assert_eq!(
        row_status(&output_rows(&case), 0),
        "error: ConnectionError: no recorded response for this request"
    );
}

// ---------------------------------------------------------------------------
// review #4937: env-file configuration (#4938), the error-body cut (#4939) and
// caller paths in diagnostics (#4940)

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn invocation(values: &[(&str, &str)]) -> crate::Invocation {
    crate::Invocation {
        positionals: vec!["source.env".to_string()],
        flags: Vec::new(),
        values: pairs(values),
    }
}

/// An ambient environment that is never the process's own.
fn ambient(items: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |var: &str| {
        items
            .iter()
            .find(|(name, _)| *name == var)
            .map(|(_, value)| (*value).to_string())
    }
}

#[test]
fn source_baseline_reads_its_run_inputs_from_the_env_file() {
    // source_baseline.py:42-55 installed every file pair before main read
    // SOURCE_MODEL_ID, GOLDEN_DATASET_PATH and OUTPUT_PATH: a file that
    // carries them needs no flag and no ambient variable.
    let case = make_case("", &[r#"{"id": "a", "user_prompt": "hello"}"#]);
    let env = case.dir.file(
        "full.env",
        &format!(
            "OPENAI_API_KEY=offline-test-key\nSOURCE_MODEL_ID=file-model\nGOLDEN_DATASET_PATH={}\nOUTPUT_PATH={}\n",
            case.dataset, case.output
        ),
    );
    let recorded = recorded_openai(
        &case,
        &format!(
            r#"{{"url": "{OPENAI_URL}",
                "request": {{"model": "file-model", "max_completion_tokens": 4096,
                            "messages": [{{"role": "user", "content": "hello"}}]}},
                "response": {{"choices": [{{"message": {{"content": "hi"}}}}]}}}}"#
        ),
    );
    let (status, out, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &env,
        "--recorded",
        &recorded,
    ]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(out, "live source baselines: 1/1\n");
    assert_eq!(
        output_rows(&case)[0],
        json(r#"{"id": "a", "source_response": "hi", "status": "live"}"#)
    );
}

#[test]
fn source_baseline_takes_the_stated_provider_from_the_env_file() {
    // Both keys in the file and SOURCE_PROVIDER=anthropic in the file: the
    // script asked Anthropic, never the first key (OpenAI).
    let case = make_case(
        "OPENAI_API_KEY=openai-test-key\nANTHROPIC_API_KEY=anthropic-test-key\nSOURCE_PROVIDER=anthropic\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let recorded = recorded_openai(
        &case,
        &format!(
            r#"{{"url": "{ANTHROPIC_URL}",
                "response": {{"content": [{{"type": "text", "text": "from anthropic"}}], "stop_reason": "end_turn"}}}}"#
        ),
    );
    let (status, out, err) = baseline_cli(&case, &["--recorded", &recorded]);
    assert_eq!(status, 0, "{err}");
    assert_eq!(out, "live source baselines: 1/1\n");
    assert_eq!(
        output_rows(&case)[0]
            .get("source_response")
            .and_then(Json::as_str),
        Some("from anthropic")
    );
}

#[test]
fn env_file_values_shadow_the_ambient_environment_and_flags_win() {
    static AMBIENT: &[(&str, &str)] = &[
        ("SOURCE_PROVIDER", "openai"),
        ("SOURCE_MODEL_ID", "ambient-model"),
        ("GOLDEN_DATASET_PATH", "ambient-dataset.jsonl"),
        ("OUTPUT_PATH", "ambient-out.jsonl"),
    ];
    let file = pairs(&[
        ("OPENAI_API_KEY", "openai-test-key"),
        ("ANTHROPIC_API_KEY", "anthropic-test-key"),
        ("SOURCE_PROVIDER", "Anthropic"),
        ("SOURCE_MODEL_ID", "file-model"),
        ("OUTPUT_PATH", "file-out.jsonl"),
    ]);
    // The file beats the ambient value; an input the file lacks falls back
    // to the ambient one.
    let inputs =
        super::baseline_inputs(&invocation(&[]), &file, &ambient(AMBIENT)).expect("inputs resolve");
    assert_eq!(inputs.provider, Provider::Anthropic);
    assert_eq!(inputs.api_key, "anthropic-test-key");
    assert_eq!(inputs.model, "file-model");
    assert_eq!(inputs.output_path, "file-out.jsonl");
    assert_eq!(inputs.dataset_path, "ambient-dataset.jsonl");
    // A flag beats both.
    let flags = invocation(&[
        ("--source-provider", "openai"),
        ("--source-model-id", "flag-model"),
        ("--output", "flag-out.jsonl"),
    ]);
    let inputs = super::baseline_inputs(&flags, &file, &ambient(AMBIENT)).expect("inputs resolve");
    assert_eq!(inputs.provider, Provider::OpenAi);
    assert_eq!(inputs.model, "flag-model");
    assert_eq!(inputs.output_path, "flag-out.jsonl");
    // Nothing anywhere: the script's FAIL line.
    let missing = super::baseline_inputs(&invocation(&[]), &file[..2], &ambient(&[]));
    assert_eq!(
        missing.err().as_deref(),
        Some("FAIL: required env var 'SOURCE_MODEL_ID' not set")
    );
}

#[test]
fn an_empty_env_file_value_still_shadows_the_ambient_one() {
    // os.environ[k] = "" hid the ambient value: an empty SOURCE_PROVIDER in
    // the file means "not stated" (first key of the file), and an empty
    // SOURCE_MODEL_ID is unset (migration divergence 10).
    static AMBIENT: &[(&str, &str)] = &[
        ("SOURCE_PROVIDER", "anthropic"),
        ("SOURCE_MODEL_ID", "ambient-model"),
        ("GOLDEN_DATASET_PATH", "d.jsonl"),
        ("OUTPUT_PATH", "o.jsonl"),
    ];
    let file = pairs(&[
        ("OPENAI_API_KEY", "openai-test-key"),
        ("ANTHROPIC_API_KEY", "anthropic-test-key"),
        ("SOURCE_PROVIDER", ""),
    ]);
    let flags = invocation(&[("--source-model-id", "m")]);
    let inputs = super::baseline_inputs(&flags, &file, &ambient(AMBIENT)).expect("inputs resolve");
    assert_eq!(inputs.provider, Provider::OpenAi);
    let mut empty_model = file.clone();
    empty_model.push(("SOURCE_MODEL_ID".to_string(), String::new()));
    let missing = super::baseline_inputs(&invocation(&[]), &empty_model, &ambient(AMBIENT));
    assert_eq!(
        missing.err().as_deref(),
        Some("FAIL: required env var 'SOURCE_MODEL_ID' not set")
    );
}

#[test]
fn a_key_crossing_the_200_character_cut_leaves_no_prefix() {
    let secrets = vec!["offline-test-key".to_string()];
    let body = format!("{}offline-test-key", "x".repeat(190));
    let detail = baseline::error_detail(500, body.as_bytes(), &secrets);
    assert!(!detail.contains("offline"), "{detail}");
    assert!(detail.starts_with(&"x".repeat(190)), "{detail}");
    // A key across the ERROR_BODY_CAP read leaves nothing either.
    let body = format!(
        r#"{{"error": {{"message": "{}offline-test-key"}}}}"#,
        "y".repeat(baseline::ERROR_BODY_CAP - 30)
    );
    let detail = baseline::error_detail(500, body.as_bytes(), &secrets);
    assert!(!detail.contains("offline"), "{detail}");
    // A labelled credential across the cut is scrubbed by the shared redactor
    // before the cut too.
    let body = format!("{} password=hunter2-shared-secret", "z".repeat(170));
    let detail = baseline::error_detail(500, body.as_bytes(), &[]);
    assert!(!detail.contains("hunter2"), "{detail}");
}

#[test]
fn a_recorded_error_body_never_leaks_a_key_prefix_into_the_row() {
    let case = make_case(
        "OPENAI_API_KEY=offline-test-key\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let recorded = recorded_openai(
        &case,
        &format!(
            r#"{{"url": "{OPENAI_URL}",
                "error": {{"status": 500, "reason": "Internal Server Error",
                          "body": "{}offline-test-key"}}}}"#,
            "x".repeat(190)
        ),
    );
    let (status, _, err) = baseline_cli(&case, &["--recorded", &recorded]);
    assert_eq!(status, 0, "{err}");
    let text = fs::read_to_string(&case.output).expect("rows");
    assert!(!text.contains("offline"), "{text}");
    assert!(
        row_status(&output_rows(&case), 0).starts_with("http_500: Internal Server Error — xxx"),
        "{text}"
    );
}

/// A path whose rendering would expose a userinfo URL credential.
const USERINFO: &str = "https://user:hunter2-userinfo@example.com/x";

#[test]
fn a_failed_read_path_is_redacted_and_bounded_in_the_diagnostic() {
    let dir = TempDir::new("diag-path");
    let missing = format!("{}/missing/{USERINFO}", dir.display());
    let (status, _, err) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0",
        "--recorded",
        &missing,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("cannot read"), "{err}");
    assert!(!err.contains("hunter2"), "{err}");
    // An overlong path is cut, the failure reason stays visible.
    let long = format!("{}/{}", dir.display(), "d/".repeat(400));
    let (status, _, err) = preflight_cli(&[
        "--region",
        REGION,
        "--models",
        "amazon.nova-lite-v1:0",
        "--recorded",
        &format!("{long}absent.json"),
    ]);
    assert_eq!(status, 1);
    // The shared safe_path bound (#4941, round 20): at most 200 characters
    // of the path, then `...`, then the reason.
    let shown = err
        .split_once("cannot read ")
        .and_then(|(_, rest)| rest.split_once("...: "))
        .map(|(path, _)| path)
        .unwrap_or_else(|| panic!("a cut path: {err}"));
    assert_eq!(shown.chars().count(), 200, "{err}");
    assert!(err.chars().count() < 400, "{}", err.chars().count());
    assert!(err.trim_end().ends_with("file does not exist"), "{err}");
}

#[test]
fn source_baseline_paths_never_show_an_env_file_key() {
    let case = make_case(
        "OPENAI_API_KEY=offline-test-key\n",
        &[r#"{"id": "a", "user_prompt": "one"}"#],
    );
    let dataset = case.dir.path("offline-test-key-missing.jsonl");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--source-model-id",
        "m",
        "--dataset",
        &dataset,
        "--output",
        &case.output,
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("cannot read"), "{err}");
    assert!(!err.contains("offline-test-key"), "{err}");
    // The output path: an unwritable one (a directory) names it scrubbed.
    let output = case.dir.path(&format!("out-{}", "offline-test-key"));
    fs::create_dir_all(&output).expect("dir");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "source-baseline",
        &case.env,
        "--source-model-id",
        "m",
        "--dataset",
        &case.dataset,
        "--output",
        &output,
    ]);
    assert_eq!(status, 1, "{err}");
    assert!(!err.contains("offline-test-key"), "{err}");
}

#[test]
fn the_policy_written_diagnostic_is_redacted_and_bounded() {
    let dir = TempDir::new("iam-diag");
    let out_path = format!("{}/{USERINFO}/policy.json", dir.display());
    fs::create_dir_all(Path::new(&out_path).parent().expect("parent")).expect("dirs");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "amazon.nova-lite-v1:0",
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
        "--output",
        &out_path,
    ]);
    assert_eq!(status, 0, "{err}");
    assert!(err.starts_with("Policy written to "), "{err}");
    assert!(!err.contains("hunter2"), "{err}");
    // The file itself is written where the caller asked.
    assert!(Path::new(&out_path).is_file());
    let deep = format!("{}/{}", dir.display(), "e/".repeat(150));
    fs::create_dir_all(&deep).expect("dirs");
    let long_path = format!("{deep}policy.json");
    let (status, _, err) = exec(&[
        "bedrock-probes",
        "iam-policy",
        "--models",
        "amazon.nova-lite-v1:0",
        "--region",
        REGION,
        "--account-id",
        ACCOUNT,
        "--output",
        &long_path,
    ]);
    assert_eq!(status, 0, "{err}");
    // The shared safe_path bound (#4941, round 20): the cut path ends in `...`.
    let shown = err
        .trim_end()
        .strip_prefix("Policy written to ")
        .and_then(|rest| rest.strip_suffix("..."))
        .unwrap_or_else(|| panic!("a cut path: {err}"));
    assert_eq!(shown.chars().count(), 200, "{err}");
    assert!(err.chars().count() < 300, "{}", err.chars().count());
    assert!(Path::new(&long_path).is_file());
}
