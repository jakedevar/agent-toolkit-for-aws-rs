//! End-to-end tests of `toolkit bedrock-agent fetch`: real process spawns, real
//! exit codes, separated stdout/stderr and a real manifest file. Responses are
//! replayed from a recorded file; no network or AWS call is involved.

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

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

fn temp_root(label: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "toolkit-bedrock-agent-cli-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

/// Spawns the binary with no AWS environment at all.
fn toolkit(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(BIN);
    command.args(args).stdin(Stdio::null());
    for name in [
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_PROFILE",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    ] {
        command.env_remove(name);
    }
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().expect("spawn toolkit")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

const RECORDED: &str = r#"{
  "calls": [
    {"service": "sts", "operation": "GetCallerIdentity", "request": {},
     "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:role/Reader"}},
    {"service": "bedrock-agent", "operation": "GetAgent", "request": {"agentId": "AGENT12345"},
     "response": {"agent": {"agentId": "AGENT12345", "agentName": "support"},
                  "ResponseMetadata": {"RequestId": "r"}}},
    {"service": "bedrock-agent", "operation": "ListAgentAliases", "request": {"agentId": "AGENT12345"},
     "response": {"agentAliasSummaries": [{"agentAliasId": "PRODALIAS1"}]}},
    {"service": "bedrock-agent", "operation": "GetAgentAlias",
     "request": {"agentId": "AGENT12345", "agentAliasId": "PRODALIAS1"},
     "response": {"agentAlias": {"agentAliasId": "PRODALIAS1", "agentAliasName": "prod",
                                 "routingConfiguration": [{"agentVersion": "2"}]}}},
    {"service": "bedrock-agent", "operation": "ListAgentVersions", "request": {"agentId": "AGENT12345"},
     "response": {"agentVersionSummaries": [{"agentVersion": "DRAFT"}, {"agentVersion": "1"}]}},
    {"service": "bedrock-agent", "operation": "ListAgentActionGroups",
     "request": {"agentId": "AGENT12345", "agentVersion": "DRAFT"},
     "response": {"actionGroupSummaries": []}},
    {"service": "bedrock-agent", "operation": "ListAgentKnowledgeBases",
     "request": {"agentId": "AGENT12345", "agentVersion": "DRAFT"},
     "response": {"agentKnowledgeBaseSummaries": []}},
    {"service": "bedrock-agent", "operation": "ListAgentCollaborators",
     "request": {"agentId": "AGENT12345", "agentVersion": "DRAFT"},
     "response": {"agentCollaboratorSummaries": []}}
  ]
}"#;

fn write_recorded(dir: &Path, content: &str) -> String {
    let path = dir.join("recorded.json");
    fs::write(&path, content).expect("write recorded");
    path.to_string_lossy().into_owned()
}

#[test]
fn a_recorded_run_writes_the_manifest_and_reports_the_draft_warning() {
    let dir = temp_root("run");
    let recorded = write_recorded(&dir, RECORDED);
    let out = dir.join("out").join("manifest.json");
    let out_text = out.to_string_lossy().into_owned();
    let run = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id",
            "AGENT12345",
            "--region",
            "us-east-1",
            "--out",
            &out_text,
            "--recorded",
            &recorded,
        ],
        &[],
    );
    assert_eq!(status(&run), 0, "{}", text(&run.stderr));
    assert_eq!(text(&run.stderr), "");
    assert_eq!(
        text(&run.stdout),
        format!(
            "Wrote {out_text}\n  WARNING: Fetched DRAFT but non-DRAFT aliases exist \
             (prod->v2). DRAFT may diverge from production.\n"
        )
    );
    let manifest = fs::read_to_string(&out).expect("manifest");
    assert!(manifest.contains("\"callerArn\": \"arn:aws:iam::111122223333:role/Reader\""));
    assert!(manifest.contains("\"region\": \"us-east-1\""));
    assert!(!manifest.contains("ResponseMetadata"));
    assert!(!manifest.ends_with('\n'));
}

#[test]
fn the_region_can_come_from_the_environment_and_a_numbered_version_never_warns() {
    let dir = temp_root("env");
    // Version 2 requests: rewrite the recorded versions.
    let recorded = write_recorded(&dir, &RECORDED.replace("DRAFT", "2"));
    let out = dir.join("manifest.json");
    let out_text = out.to_string_lossy().into_owned();
    let run = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id=AGENT12345",
            "--agent-version=2",
            "--out",
            &out_text,
            "--recorded",
            &recorded,
        ],
        &[("AWS_DEFAULT_REGION", "eu-west-1")],
    );
    assert_eq!(status(&run), 0, "{}", text(&run.stderr));
    assert_eq!(text(&run.stdout), format!("Wrote {out_text}\n"));
    assert!(fs::read_to_string(&out)
        .expect("manifest")
        .contains("\"region\": \"eu-west-1\""));
}

#[test]
fn missing_required_flags_are_usage_errors() {
    let missing_out = toolkit(&["bedrock-agent", "fetch", "--agent-id", "AGENT12345"], &[]);
    assert_eq!(status(&missing_out), 2);
    assert!(text(&missing_out.stderr).contains("usage: toolkit bedrock-agent fetch"));
    let missing_id = toolkit(&["bedrock-agent", "fetch", "--out", "m.json"], &[]);
    assert_eq!(status(&missing_id), 2);
    let unknown = toolkit(&["bedrock-agent", "fetch", "--secret-key", "x"], &[]);
    assert_eq!(status(&unknown), 2);
    let positional = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id",
            "A",
            "--out",
            "m",
            "extra",
        ],
        &[],
    );
    assert_eq!(status(&positional), 2);
}

#[test]
fn no_region_prints_the_script_error_and_exits_two() {
    let dir = temp_root("noregion");
    let out = dir.join("manifest.json");
    let run = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id",
            "AGENT12345",
            "--out",
            &out.to_string_lossy(),
        ],
        &[],
    );
    assert_eq!(status(&run), 2);
    assert_eq!(
        text(&run.stderr),
        "ERROR: no region resolved from credentials. Pass --region or set AWS_DEFAULT_REGION.\n"
    );
    assert_eq!(text(&run.stdout), "");
    assert!(!out.exists());
}

#[test]
fn a_missing_agent_prints_the_script_error_and_exits_one_without_a_manifest() {
    let dir = temp_root("noagent");
    let recorded = write_recorded(
        &dir,
        r#"{"calls": [
          {"service": "sts", "operation": "GetCallerIdentity", "response": {"Account": "1"}},
          {"service": "bedrock-agent", "operation": "GetAgent", "request": {"agentId": "NOSUCHAGNT"},
           "error": {"code": "ResourceNotFoundException", "message": "Agent not found"}}
        ]}"#,
    );
    let out = dir.join("manifest.json");
    let run = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id",
            "NOSUCHAGNT",
            "--region",
            "us-east-1",
            "--out",
            &out.to_string_lossy(),
            "--recorded",
            &recorded,
        ],
        &[],
    );
    assert_eq!(status(&run), 1);
    assert_eq!(text(&run.stdout), "");
    assert_eq!(
        text(&run.stderr),
        "ERROR: failed to fetch agent \"NOSUCHAGNT\": An error occurred \
         (ResourceNotFoundException) when calling the GetAgent operation: Agent not found\n"
    );
    assert!(!out.exists());
}

#[test]
fn without_a_recorded_file_no_live_client_exists_and_nothing_is_written() {
    let dir = temp_root("nolive");
    let out = dir.join("manifest.json");
    let run = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id",
            "AGENT12345",
            "--region",
            "us-east-1",
            "--profile",
            "dev",
            "--out",
            &out.to_string_lossy(),
        ],
        &[],
    );
    assert_eq!(status(&run), 1);
    assert_eq!(text(&run.stdout), "");
    assert!(
        text(&run.stderr).starts_with("error: "),
        "{}",
        text(&run.stderr)
    );
    assert!(text(&run.stderr).contains("--recorded"));
    assert!(!out.exists());
}

#[test]
fn an_unreadable_malformed_or_linked_recorded_file_is_refused() {
    let dir = temp_root("badrecorded");
    let out = dir.join("manifest.json");
    let out_text = out.to_string_lossy().into_owned();
    let fetch = |recorded: &str| {
        toolkit(
            &[
                "bedrock-agent",
                "fetch",
                "--agent-id",
                "AGENT12345",
                "--region",
                "us-east-1",
                "--out",
                &out_text,
                "--recorded",
                recorded,
            ],
            &[],
        )
    };
    let missing = fetch(&dir.join("absent.json").to_string_lossy());
    assert_eq!(status(&missing), 1);
    assert!(text(&missing.stderr).contains("file does not exist"));

    let malformed = write_recorded(&dir, "{not json SECRET-VALUE");
    let run = fetch(&malformed);
    assert_eq!(status(&run), 1);
    assert!(
        !text(&run.stderr).contains("SECRET-VALUE"),
        "{}",
        text(&run.stderr)
    );

    let bad_shape = write_recorded(&dir, r#"{"calls": [{"service": "ec2"}]}"#);
    let run = fetch(&bad_shape);
    assert_eq!(status(&run), 1);
    assert!(text(&run.stderr).contains("recorded call 0"));

    #[cfg(unix)]
    {
        let real = write_recorded(&dir, RECORDED);
        let link = dir.join("linked.json");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let run = fetch(&link.to_string_lossy());
        assert_eq!(status(&run), 1);
        assert!(text(&run.stderr).contains("symbolic links are not followed"));
    }
    assert!(!out.exists());
}

#[cfg(unix)]
#[test]
fn the_manifest_is_owner_only_and_a_link_at_the_output_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_root("perm");
    let recorded = write_recorded(&dir, RECORDED);
    let out = dir.join("manifest.json");
    let out_text = out.to_string_lossy().into_owned();
    let args = [
        "bedrock-agent",
        "fetch",
        "--agent-id",
        "AGENT12345",
        "--region",
        "us-east-1",
        "--out",
        &out_text,
        "--recorded",
        &recorded,
    ];
    let run = toolkit(&args, &[]);
    assert_eq!(status(&run), 0, "{}", text(&run.stderr));
    let mode = fs::metadata(&out).expect("metadata").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    let target = dir.join("target.txt");
    fs::write(&target, "keep").expect("write target");
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let link_text = link.to_string_lossy().into_owned();
    let mut linked: Vec<&str> = args.to_vec();
    let position = linked.iter().position(|arg| *arg == out_text).expect("out");
    linked[position] = &link_text;
    let run = toolkit(&linked, &[]);
    assert_eq!(status(&run), 1);
    assert!(text(&run.stderr).contains("symbolic links are not followed"));
    assert_eq!(fs::read_to_string(&target).expect("read"), "keep");
}

#[test]
fn hostile_argument_values_never_reach_the_output_raw() {
    let dir = temp_root("hostile");
    let recorded = write_recorded(
        &dir,
        r#"{"calls": [
          {"service": "sts", "operation": "GetCallerIdentity", "response": {}},
          {"service": "bedrock-agent", "operation": "GetAgent", "request": {"agentId": "x"},
           "error": {"code": "ValidationException", "message": "bad"}}
        ]}"#,
    );
    let hostile = "bad\u{1b}[31m\u{202e}\nERROR: forged";
    let run = toolkit(
        &[
            "bedrock-agent",
            "fetch",
            "--agent-id",
            hostile,
            "--region",
            "us-east-1",
            "--out",
            &dir.join("m.json").to_string_lossy(),
            "--recorded",
            &recorded,
        ],
        &[],
    );
    let stderr = text(&run.stderr);
    assert!(
        !stderr.chars().any(|c| c.is_control() && c != '\n'),
        "{stderr:?}"
    );
    assert!(!stderr.contains('\u{202e}'));
    assert!(!stderr.lines().any(|line| line.starts_with("ERROR: forged")));
}

/// Runs the command on `recorded` against a fresh output and an existing one;
/// both runs must fail with exit 1, print no success line and leave the output
/// paths as they were.
fn assert_refused_and_nothing_written(recorded: &str, label: &str) {
    let dir = temp_root("refused");
    let recorded = write_recorded(&dir, recorded);
    let fresh = dir.join("fresh").join("manifest.json");
    let existing = dir.join("existing.json");
    fs::write(&existing, "previous manifest").expect("seed");
    for out in [&fresh, &existing] {
        let out_text = out.to_string_lossy().into_owned();
        let run = toolkit(
            &[
                "bedrock-agent",
                "fetch",
                "--agent-id",
                "AGENT12345",
                "--region",
                "us-east-1",
                "--out",
                &out_text,
                "--recorded",
                &recorded,
            ],
            &[],
        );
        assert_eq!(status(&run), 1, "{label}: {}", text(&run.stderr));
        assert!(!text(&run.stdout).contains("Wrote"), "{label}");
        assert_eq!(text(&run.stdout), "", "{label}");
        assert!(text(&run.stderr).contains("error"), "{label}");
    }
    assert!(!fresh.exists(), "{label}: a manifest was written");
    assert_eq!(
        fs::read_to_string(&existing).expect("read"),
        "previous manifest",
        "{label}"
    );
}

#[test]
fn a_wrong_typed_recorded_error_member_fails_closed_without_a_manifest() {
    for member in ["kind", "code", "message"] {
        for wrong in ["null", "1", "true", "[]", "{}"] {
            let entry = format!(
                r#"{{"service": "bedrock-agent", "operation": "ListAgentCollaborators",
                    "request": {{"agentId": "AGENT12345", "agentVersion": "DRAFT"}},
                    "error": {{"{member}": {wrong}}}}}"#
            );
            let recorded = RECORDED.replace(r#""calls": ["#, &format!(r#""calls": [{entry},"#));
            assert_refused_and_nothing_written(&recorded, &format!("{member}={wrong}"));
        }
    }
}

#[test]
fn a_null_or_wrong_typed_listing_member_fails_closed_without_a_manifest() {
    for (member, wrong) in [
        ("actionGroupSummaries", "null"),
        ("actionGroupSummaries", "{}"),
        ("agentKnowledgeBaseSummaries", "null"),
        ("agentCollaboratorSummaries", "7"),
        ("agentAliasSummaries", "null"),
        ("agentVersionSummaries", "\"no\""),
    ] {
        let recorded = if member == "agentAliasSummaries" {
            RECORDED.replace(
                r#""agentAliasSummaries": [{"agentAliasId": "PRODALIAS1"}]"#,
                &format!(r#""agentAliasSummaries": {wrong}"#),
            )
        } else if member == "agentVersionSummaries" {
            RECORDED.replace(
                r#""agentVersionSummaries": [{"agentVersion": "DRAFT"}, {"agentVersion": "1"}]"#,
                &format!(r#""agentVersionSummaries": {wrong}"#),
            )
        } else {
            RECORDED.replace(
                &format!(r#""{member}": []"#),
                &format!(r#""{member}": {wrong}"#),
            )
        };
        assert_ne!(recorded, RECORDED, "{member}: the fixture was not edited");
        assert_refused_and_nothing_written(&recorded, &format!("{member}={wrong}"));
    }
}

#[test]
fn an_omitted_or_empty_listing_member_still_succeeds() {
    for replacement in [r#""agentCollaboratorSummaries": []"#, "\"unrelated\": 1"] {
        let dir = temp_root("omitted");
        let recorded = write_recorded(
            &dir,
            &RECORDED.replace(r#""agentCollaboratorSummaries": []"#, replacement),
        );
        let out = dir.join("manifest.json");
        let out_text = out.to_string_lossy().into_owned();
        let run = toolkit(
            &[
                "bedrock-agent",
                "fetch",
                "--agent-id",
                "AGENT12345",
                "--region",
                "us-east-1",
                "--out",
                &out_text,
                "--recorded",
                &recorded,
            ],
            &[],
        );
        assert_eq!(status(&run), 0, "{}", text(&run.stderr));
        assert!(text(&run.stdout).starts_with(&format!("Wrote {out_text}\n")));
        assert!(out.exists());
    }
}
