//! Offline, deterministic tests for `toolkit bedrock-agent fetch` (#4383).
//!
//! They port the behaviour of `fetch_bedrock_agent.py` over a recorded client:
//! pagination, tolerated and fatal service errors, partial listings, the S3
//! schema inlining, the DRAFT warning and the manifest file. Fixtures are
//! literals and unique temporary directories removed on drop; nothing touches
//! AWS or the network.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_aws::{AwsError, MapEnv};
use toolkit_core::{parse_json, Json};

use super::client::{AgentClient, NoLiveClient, Service};
use super::fetch::{discover, strip_response_metadata, Failure, Params};
use super::recorded::{same_json, RecordedClient};
use super::{fetch_with, region_of};
use crate::{Output, COMMANDS};

const AGENT: &str = "AGENT12345";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-bedrock-agent-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn join_text(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
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

fn json(text: &str) -> Json {
    parse_json(text).unwrap_or_else(|error| panic!("fixture is not JSON ({error:?}): {text}"))
}

/// An ordered list of recorded calls; an earlier entry wins over a later one.
#[derive(Default)]
struct Script {
    calls: Vec<Json>,
    unsupported: Vec<&'static str>,
}

impl Script {
    fn ok(mut self, service: &str, operation: &str, request: &str, response: &str) -> Self {
        self.calls.push(Json::object(vec![
            ("service", Json::string(service)),
            ("operation", Json::string(operation)),
            ("request", json(request)),
            ("response", json(response)),
        ]));
        self
    }

    fn error(
        mut self,
        service: &str,
        operation: &str,
        request: &str,
        code: &str,
        message: &str,
    ) -> Self {
        self.calls.push(Json::object(vec![
            ("service", Json::string(service)),
            ("operation", Json::string(operation)),
            ("request", json(request)),
            (
                "error",
                Json::object(vec![
                    ("code", Json::string(code)),
                    ("message", Json::string(message)),
                ]),
            ),
        ]));
        self
    }

    fn fault(mut self, service: &str, operation: &str, request: &str, kind: &str) -> Self {
        self.calls.push(Json::object(vec![
            ("service", Json::string(service)),
            ("operation", Json::string(operation)),
            ("request", json(request)),
            (
                "error",
                Json::object(vec![
                    ("kind", Json::string(kind)),
                    ("message", Json::string("connection refused")),
                ]),
            ),
        ]));
        self
    }

    fn document(&self) -> Json {
        Json::object(vec![
            ("calls", Json::Array(self.calls.clone())),
            (
                "unsupported",
                Json::Array(self.unsupported.iter().map(|n| Json::string(*n)).collect()),
            ),
        ])
    }

    fn client(&self) -> RecordedClient {
        RecordedClient::from_document(&self.document()).expect("valid script")
    }
}

/// The minimal agent: identity, the agent, an empty alias and version list and
/// nothing attached.
fn minimal() -> Script {
    Script::default()
        .ok(
            "sts",
            "GetCallerIdentity",
            "{}",
            r#"{"Account":"111122223333","Arn":"arn:aws:iam::111122223333:role/Reader","UserId":"U"}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agent":{"agentId":"AGENT12345","agentName":"support"},"ResponseMetadata":{"RequestId":"r1"}}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentAliases",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agentAliasSummaries":[]}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentVersions",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agentVersionSummaries":[]}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentActionGroups",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"actionGroupSummaries":[]}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentKnowledgeBases",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"agentKnowledgeBaseSummaries":[]}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentCollaborators",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"agentCollaboratorSummaries":[]}"#,
        )
}

fn params(version: &str, inline: bool) -> Params {
    Params {
        agent_id: AGENT.to_string(),
        agent_version: version.to_string(),
        agent_alias_id: None,
        region: "us-east-1".to_string(),
        inline_s3_schemas: inline,
    }
}

fn run(script: &Script, version: &str, inline: bool) -> Result<Json, Failure> {
    discover(&mut script.client(), &params(version, inline))
}

fn manifest(script: &Script) -> Json {
    run(script, "DRAFT", false).expect("discovery succeeds")
}

/// `a.b.0.c`: object members and array indexes.
fn at<'a>(value: &'a Json, path: &str) -> &'a Json {
    let mut current = value;
    for step in path.split('.') {
        current = match (current, step.parse::<usize>()) {
            (Json::Array(items), Ok(index)) => items
                .get(index)
                .unwrap_or_else(|| panic!("no index {index} on the way to {path}")),
            (other, _) => other
                .get(step)
                .unwrap_or_else(|| panic!("no member {step} on the way to {path}")),
        };
    }
    current
}

fn text_at<'a>(value: &'a Json, path: &str) -> &'a str {
    at(value, path)
        .as_str()
        .unwrap_or_else(|| panic!("{path} is not a string"))
}

fn len_at(value: &Json, path: &str) -> usize {
    match at(value, path) {
        Json::Array(items) => items.len(),
        other => panic!("{path} is a {}, not an array", other.type_name()),
    }
}

fn fatal_code(result: Result<Json, Failure>) -> &'static str {
    match result {
        Err(Failure::Fatal(error)) => error.code(),
        Err(Failure::Agent(text)) => panic!("agent failure instead of a fatal error: {text}"),
        Ok(_) => panic!("discovery unexpectedly succeeded"),
    }
}

// ---------------------------------------------------------------- discovery

#[test]
fn the_minimal_agent_produces_the_script_manifest() {
    let result = manifest(&minimal());
    let expected = json(
        r#"{
          "discovery": {"account": "111122223333", "region": "us-east-1",
            "callerArn": "arn:aws:iam::111122223333:role/Reader",
            "fetchedAgentVersion": "DRAFT", "fetchedAgentAliasId": null, "warnings": []},
          "agent": {"agentId": "AGENT12345", "agentName": "support"},
          "agentCollaborationMode": "DISABLED",
          "orchestrationType": "DEFAULT",
          "executionRole": {},
          "actionGroups": [],
          "knowledgeBases": [],
          "collaborators": [],
          "aliasesAndVersions": {"aliases": [], "versions": []}
        }"#,
    );
    assert_eq!(result, expected);
    // The key order is the script's, because the file is written in that order.
    let keys: Vec<&str> = match &result {
        Json::Object(members) => members.iter().map(|(key, _)| key.as_str()).collect(),
        _ => panic!("manifest is not an object"),
    };
    assert_eq!(
        keys,
        [
            "discovery",
            "agent",
            "agentCollaborationMode",
            "orchestrationType",
            "executionRole",
            "actionGroups",
            "knowledgeBases",
            "collaborators",
            "aliasesAndVersions"
        ]
    );
}

#[test]
fn the_requested_alias_and_the_agent_settings_are_recorded() {
    let script = Script::default()
        .ok(
            "sts",
            "GetCallerIdentity",
            "{}",
            r#"{"Account":"1","Arn":"a"}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agent":{"agentCollaboration":"SUPERVISOR","orchestrationType":"CUSTOM_ORCHESTRATION"}}"#,
        );
    let merged = Script {
        calls: [script.calls, minimal().calls].concat(),
        unsupported: vec![],
    };
    let mut args = params("DRAFT", false);
    args.agent_alias_id = Some("ALIAS00001".to_string());
    let result = discover(&mut merged.client(), &args).expect("discovery succeeds");
    assert_eq!(text_at(&result, "agentCollaborationMode"), "SUPERVISOR");
    assert_eq!(
        text_at(&result, "orchestrationType"),
        "CUSTOM_ORCHESTRATION"
    );
    assert_eq!(
        text_at(&result, "discovery.fetchedAgentAliasId"),
        "ALIAS00001"
    );
}

#[test]
fn execution_role_is_read_with_attached_and_inline_policies() {
    let script = minimal()
        .ok(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agent":{"agentResourceRoleArn":"arn:aws:iam::111122223333:role/service-role/AgentRole"}}"#,
        )
        .ok(
            "iam",
            "GetRole",
            r#"{"RoleName":"AgentRole"}"#,
            r#"{"Role":{"RoleName":"AgentRole","Arn":"arn:r"},"ResponseMetadata":{"HTTPStatusCode":200}}"#,
        )
        .ok(
            "iam",
            "ListAttachedRolePolicies",
            r#"{"RoleName":"AgentRole"}"#,
            r#"{"AttachedPolicies":[{"PolicyName":"AmazonBedrockFullAccess"}],"IsTruncated":false}"#,
        )
        .ok(
            "iam",
            "ListRolePolicies",
            r#"{"RoleName":"AgentRole"}"#,
            r#"{"PolicyNames":["inline-a","inline-b"]}"#,
        )
        .ok(
            "iam",
            "GetRolePolicy",
            r#"{"RoleName":"AgentRole","PolicyName":"inline-a"}"#,
            r#"{"RoleName":"AgentRole","PolicyName":"inline-a","PolicyDocument":{"Version":"2012-10-17"},"ResponseMetadata":{"RequestId":"x"}}"#,
        )
        .ok(
            "iam",
            "GetRolePolicy",
            r#"{"RoleName":"AgentRole","PolicyName":"inline-b"}"#,
            r#"{"RoleName":"AgentRole","PolicyName":"inline-b","PolicyDocument":{}}"#,
        );
    // The first GetAgent entry is the minimal one: put the role-bearing one first.
    let reordered = Script {
        calls: {
            let mut calls = script.calls.clone();
            let role_agent = calls.remove(7);
            calls.insert(0, role_agent);
            calls
        },
        unsupported: vec![],
    };
    let result = manifest(&reordered);
    assert_eq!(text_at(&result, "executionRole.role.RoleName"), "AgentRole");
    assert_eq!(
        text_at(&result, "executionRole.attachedPolicies.0.PolicyName"),
        "AmazonBedrockFullAccess"
    );
    assert_eq!(len_at(&result, "executionRole.inlinePolicies"), 2);
    assert_eq!(
        text_at(&result, "executionRole.inlinePolicies.0.PolicyName"),
        "inline-a"
    );
    assert!(at(&result, "executionRole.inlinePolicies.0")
        .get("ResponseMetadata")
        .is_none());
}

fn role_agent() -> Script {
    Script::default()
        .ok(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agent":{"agentResourceRoleArn":"arn:aws:iam::1:role/AgentRole"}}"#,
        )
        .ok(
            "iam",
            "GetRole",
            r#"{"RoleName":"AgentRole"}"#,
            r#"{"Role":{"RoleName":"AgentRole"}}"#,
        )
        .ok(
            "iam",
            "ListAttachedRolePolicies",
            r#"{"RoleName":"AgentRole"}"#,
            r#"{"AttachedPolicies":[]}"#,
        )
}

fn with_front(front: Script, back: Script) -> Script {
    Script {
        calls: [front.calls, back.calls].concat(),
        unsupported: back.unsupported,
    }
}

#[test]
fn iam_access_denied_is_recorded_in_place_and_inline_policies_are_skipped() {
    let script = with_front(
        role_agent().error(
            "iam",
            "ListRolePolicies",
            r#"{"RoleName":"AgentRole"}"#,
            "AccessDeniedException",
            "not allowed",
        ),
        minimal(),
    );
    let result = manifest(&script);
    assert_eq!(text_at(&result, "executionRole.role.RoleName"), "AgentRole");
    assert_eq!(len_at(&result, "executionRole.inlinePolicies"), 0);

    let denied = with_front(
        Script::default().error(
            "iam",
            "GetRole",
            r#"{"RoleName":"AgentRole"}"#,
            "ResourceNotFoundException",
            "gone",
        ),
        with_front(
            role_agent().ok(
                "iam",
                "ListRolePolicies",
                r#"{"RoleName":"AgentRole"}"#,
                "{}",
            ),
            minimal(),
        ),
    );
    let result = manifest(&denied);
    assert_eq!(
        text_at(&result, "executionRole.role._error"),
        "ResourceNotFoundException"
    );
    assert_eq!(
        text_at(&result, "executionRole.role._message"),
        "An error occurred (ResourceNotFoundException) when calling the GetRole operation: gone"
    );
}

#[test]
fn a_non_tolerated_iam_error_ends_the_run() {
    let script = with_front(
        role_agent().error(
            "iam",
            "ListRolePolicies",
            r#"{"RoleName":"AgentRole"}"#,
            "ThrottlingException",
            "slow down",
        ),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&script, "DRAFT", false)),
        "aws_service_error"
    );
}

// -------------------------------------------------------------- pagination

fn aliases_two_pages() -> Script {
    Script::default()
        .ok(
            "bedrock-agent",
            "ListAgentAliases",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agentAliasSummaries":[{"agentAliasId":"TSTALIASID"},{"agentAliasId":"PRODALIAS1"}],"nextToken":"t2"}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentAliases",
            r#"{"agentId":"AGENT12345","nextToken":"t2"}"#,
            r#"{"agentAliasSummaries":[{"agentAliasId":"PRODALIAS2"}]}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgentAlias",
            r#"{"agentId":"AGENT12345","agentAliasId":"TSTALIASID"}"#,
            r#"{"agentAlias":{"agentAliasId":"TSTALIASID","agentAliasName":"AgentTestAlias"}}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgentAlias",
            r#"{"agentId":"AGENT12345","agentAliasId":"PRODALIAS1"}"#,
            r#"{"agentAlias":{"agentAliasId":"PRODALIAS1","agentAliasName":"prod","routingConfiguration":[{"agentVersion":"3"}]},"ResponseMetadata":{}}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgentAlias",
            r#"{"agentId":"AGENT12345","agentAliasId":"PRODALIAS2"}"#,
            r#"{"agentAlias":{"agentAliasId":"PRODALIAS2","agentAliasName":"blue","routingConfiguration":[]}}"#,
        )
}

#[test]
fn aliases_are_collected_across_pages_and_response_metadata_is_stripped() {
    let result = manifest(&with_front(aliases_two_pages(), minimal()));
    assert_eq!(len_at(&result, "aliasesAndVersions.aliases"), 3);
    assert_eq!(
        text_at(&result, "aliasesAndVersions.aliases.2.agentAliasId"),
        "PRODALIAS2"
    );
    assert!(at(&result, "aliasesAndVersions.aliases.1")
        .get("ResponseMetadata")
        .is_none());
}

#[test]
fn a_page_without_the_recorded_token_request_fails_the_replay() {
    // Page two is only recorded for token t2; a client that asked with another
    // token (or none) gets no response, and the walk is reported as an error
    // entry rather than silently ending after page one.
    let script = with_front(
        Script::default().ok(
            "bedrock-agent",
            "ListAgentAliases",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agentAliasSummaries":[],"nextToken":"unrecorded"}"#,
        ),
        minimal(),
    );
    assert_eq!(fatal_code(run(&script, "DRAFT", false)), "aws_connect");
}

#[test]
fn a_repeated_pagination_token_ends_the_run() {
    let script = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentActionGroups",
                r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
                r#"{"actionGroupSummaries":[],"nextToken":"loop"}"#,
            )
            .ok(
                "bedrock-agent",
                "ListAgentActionGroups",
                r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","nextToken":"loop"}"#,
                r#"{"actionGroupSummaries":[],"nextToken":"loop"}"#,
            ),
        minimal(),
    );
    assert_eq!(fatal_code(run(&script, "DRAFT", false)), "aws_pagination");
}

#[test]
fn an_empty_next_token_ends_the_listing() {
    let script = with_front(
        Script::default().ok(
            "bedrock-agent",
            "ListAgentVersions",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agentVersionSummaries":[{"agentVersion":"1"}],"nextToken":""}"#,
        ),
        minimal(),
    );
    let result = manifest(&script);
    assert_eq!(len_at(&result, "aliasesAndVersions.versions"), 1);
    assert_eq!(
        text_at(&result, "aliasesAndVersions.versions.0.agentVersion"),
        "1"
    );
}

#[test]
fn a_listing_that_is_not_a_list_is_a_response_error() {
    let script = with_front(
        Script::default().ok(
            "bedrock-agent",
            "ListAgentActionGroups",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"actionGroupSummaries":"nope"}"#,
        ),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&script, "DRAFT", false)),
        "aws_invalid_response"
    );
    let script = with_front(
        Script::default().ok(
            "bedrock-agent",
            "ListAgentActionGroups",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"actionGroupSummaries":[{"name":"no id"}]}"#,
        ),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&script, "DRAFT", false)),
        "aws_invalid_response"
    );
}

// ----------------------------------------------------------- action groups

fn action_groups(schema: &str) -> Script {
    Script::default()
        .ok(
            "bedrock-agent",
            "ListAgentActionGroups",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"actionGroupSummaries":[{"actionGroupId":"AG00000001"},{"actionGroupId":"AG00000002"}],"nextToken":"p2"}"#,
        )
        .ok(
            "bedrock-agent",
            "ListAgentActionGroups",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","nextToken":"p2"}"#,
            r#"{"actionGroupSummaries":[{"actionGroupId":"AG00000003"}]}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgentActionGroup",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","actionGroupId":"AG00000001"}"#,
            &format!(
                r#"{{"agentActionGroup":{{"actionGroupId":"AG00000001","apiSchema":{schema}}},"ResponseMetadata":{{"RequestId":"q"}}}}"#
            ),
        )
        .error(
            "bedrock-agent",
            "GetAgentActionGroup",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","actionGroupId":"AG00000002"}"#,
            "AccessDeniedException",
            "User is not authorized",
        )
        .ok(
            "bedrock-agent",
            "GetAgentActionGroup",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","actionGroupId":"AG00000003"}"#,
            r#"{"agentActionGroup":{"actionGroupId":"AG00000003"}}"#,
        )
}

#[test]
fn action_groups_are_paged_and_a_denied_detail_is_recorded_in_place() {
    let result = manifest(&with_front(action_groups(r#"{"payload":"{}"}"#), minimal()));
    assert_eq!(len_at(&result, "actionGroups"), 3);
    assert_eq!(
        text_at(&result, "actionGroups.0.actionGroupId"),
        "AG00000001"
    );
    assert_eq!(
        text_at(&result, "actionGroups.1._error"),
        "AccessDeniedException"
    );
    assert_eq!(
        text_at(&result, "actionGroups.1._message"),
        "An error occurred (AccessDeniedException) when calling the GetAgentActionGroup \
         operation: User is not authorized"
    );
    assert_eq!(
        text_at(&result, "actionGroups.2.actionGroupId"),
        "AG00000003"
    );
    assert!(at(&result, "actionGroups.0")
        .get("ResponseMetadata")
        .is_none());
}

#[test]
fn an_untolerated_detail_error_ends_the_run() {
    let script = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentActionGroups",
                r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
                r#"{"actionGroupSummaries":[{"actionGroupId":"AG00000001"}]}"#,
            )
            .error(
                "bedrock-agent",
                "GetAgentActionGroup",
                r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","actionGroupId":"AG00000001"}"#,
                "ThrottlingException",
                "Rate exceeded",
            ),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&script, "DRAFT", false)),
        "aws_service_error"
    );
}

#[test]
fn a_failing_action_group_listing_ends_the_run() {
    let script = with_front(
        Script::default().error(
            "bedrock-agent",
            "ListAgentActionGroups",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            "AccessDeniedException",
            "no",
        ),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&script, "DRAFT", false)),
        "aws_service_error"
    );
}

const S3_SCHEMA: &str = r#"{"s3":{"s3BucketName":"schemas","s3ObjectKey":"api/openapi.json"}}"#;

#[test]
fn s3_schemas_are_inlined_only_with_the_flag() {
    let s3_object = Script::default().ok(
        "s3",
        "GetObject",
        r#"{"Bucket":"schemas","Key":"api/openapi.json"}"#,
        r#"{"Body":"{\"openapi\":\"3.0.0\"}"}"#,
    );
    let script = with_front(s3_object, with_front(action_groups(S3_SCHEMA), minimal()));
    let plain = run(&script, "DRAFT", false).expect("discovery");
    assert!(at(&plain, "actionGroups.0.apiSchema")
        .get("_inlinedPayload")
        .is_none());
    let inlined = run(&script, "DRAFT", true).expect("discovery");
    let schema = at(&inlined, "actionGroups.0.apiSchema");
    assert_eq!(text_at(schema, "_inlinedPayload"), r#"{"openapi":"3.0.0"}"#);
    assert_eq!(
        text_at(schema, "_inlinedSource"),
        "s3://schemas/api/openapi.json"
    );
    // The original reference stays first, the additions follow in order.
    let keys: Vec<&str> = match schema {
        Json::Object(members) => members.iter().map(|(key, _)| key.as_str()).collect(),
        _ => panic!("apiSchema is not an object"),
    };
    assert_eq!(keys, ["s3", "_inlinedPayload", "_inlinedSource"]);
}

#[test]
fn an_s3_error_is_recorded_as_inline_error_and_other_schemas_are_left_alone() {
    let failing = Script::default().error(
        "s3",
        "GetObject",
        r#"{"Bucket":"schemas","Key":"api/openapi.json"}"#,
        "NoSuchKey",
        "missing",
    );
    let script = with_front(failing, with_front(action_groups(S3_SCHEMA), minimal()));
    let result = run(&script, "DRAFT", true).expect("discovery");
    assert_eq!(
        text_at(&result, "actionGroups.0.apiSchema._inlineError"),
        "An error occurred (NoSuchKey) when calling the GetObject operation: missing"
    );
    for untouched in [
        r#"{"payload":"inline"}"#,
        r#"{"s3":{"s3BucketName":"only-bucket"}}"#,
        "{}",
        "null",
    ] {
        let script = with_front(action_groups(untouched), minimal());
        let result = run(&script, "DRAFT", true).expect("discovery");
        assert!(same_json(
            at(&result, "actionGroups.0.apiSchema"),
            &json(untouched)
        ));
    }
}

#[test]
fn a_connection_failure_while_reading_s3_ends_the_run() {
    let script = with_front(
        Script::default().fault(
            "s3",
            "GetObject",
            r#"{"Bucket":"schemas","Key":"api/openapi.json"}"#,
            "connect",
        ),
        with_front(action_groups(S3_SCHEMA), minimal()),
    );
    assert_eq!(fatal_code(run(&script, "DRAFT", true)), "aws_connect");
}

// ---------------------------------------------------------- knowledge bases

fn knowledge_base() -> Script {
    Script::default()
        .ok(
            "bedrock-agent",
            "ListAgentKnowledgeBases",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"agentKnowledgeBaseSummaries":[{"knowledgeBaseId":"KB00000001"}]}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgentKnowledgeBase",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","knowledgeBaseId":"KB00000001"}"#,
            r#"{"agentKnowledgeBase":{"knowledgeBaseId":"KB00000001","knowledgeBaseState":"ENABLED"},"ResponseMetadata":{}}"#,
        )
        .ok(
            "bedrock-agent",
            "GetKnowledgeBase",
            r#"{"knowledgeBaseId":"KB00000001"}"#,
            r#"{"knowledgeBase":{"knowledgeBaseId":"KB00000001","name":"docs"}}"#,
        )
        .ok(
            "bedrock-agent",
            "ListDataSources",
            r#"{"knowledgeBaseId":"KB00000001"}"#,
            r#"{"dataSourceSummaries":[{"dataSourceId":"DS00000001"}],"nextToken":"d2"}"#,
        )
        .ok(
            "bedrock-agent",
            "ListDataSources",
            r#"{"knowledgeBaseId":"KB00000001","nextToken":"d2"}"#,
            r#"{"dataSourceSummaries":[{"dataSourceId":"DS00000002"}]}"#,
        )
        .ok(
            "bedrock-agent",
            "GetDataSource",
            r#"{"knowledgeBaseId":"KB00000001","dataSourceId":"DS00000001"}"#,
            r#"{"dataSource":{"dataSourceId":"DS00000001"},"ResponseMetadata":{"RequestId":"z"}}"#,
        )
}

#[test]
fn knowledge_bases_carry_association_detail_and_paged_data_sources() {
    let script = with_front(
        knowledge_base().ok(
            "bedrock-agent",
            "GetDataSource",
            r#"{"knowledgeBaseId":"KB00000001","dataSourceId":"DS00000002"}"#,
            r#"{"dataSource":{"dataSourceId":"DS00000002"}}"#,
        ),
        minimal(),
    );
    let result = manifest(&script);
    assert_eq!(len_at(&result, "knowledgeBases"), 1);
    assert_eq!(
        text_at(&result, "knowledgeBases.0.association.knowledgeBaseState"),
        "ENABLED"
    );
    assert_eq!(
        text_at(&result, "knowledgeBases.0.knowledgeBase.name"),
        "docs"
    );
    // The data-source entries keep the `dataSource` wrapper, like the script.
    assert_eq!(len_at(&result, "knowledgeBases.0.dataSources"), 2);
    assert_eq!(
        text_at(
            &result,
            "knowledgeBases.0.dataSources.1.dataSource.dataSourceId"
        ),
        "DS00000002"
    );
    assert!(at(&result, "knowledgeBases.0.dataSources.0")
        .get("ResponseMetadata")
        .is_none());
}

#[test]
fn a_data_source_service_error_keeps_the_earlier_sources_and_adds_an_error_entry() {
    let script = with_front(
        knowledge_base().error(
            "bedrock-agent",
            "GetDataSource",
            r#"{"knowledgeBaseId":"KB00000001","dataSourceId":"DS00000002"}"#,
            "ThrottlingException",
            "Rate exceeded",
        ),
        minimal(),
    );
    let result = manifest(&script);
    assert_eq!(len_at(&result, "knowledgeBases.0.dataSources"), 2);
    assert_eq!(
        text_at(
            &result,
            "knowledgeBases.0.dataSources.0.dataSource.dataSourceId"
        ),
        "DS00000001"
    );
    assert_eq!(
        text_at(&result, "knowledgeBases.0.dataSources.1._error"),
        "An error occurred (ThrottlingException) when calling the GetDataSource operation: \
         Rate exceeded"
    );
}

#[test]
fn tolerated_knowledge_base_errors_are_recorded_per_call() {
    let script = with_front(
        Script::default()
            .error(
                "bedrock-agent",
                "GetKnowledgeBase",
                r#"{"knowledgeBaseId":"KB00000001"}"#,
                "ValidationException",
                "bad id",
            )
            .error(
                "bedrock-agent",
                "ListDataSources",
                r#"{"knowledgeBaseId":"KB00000001"}"#,
                "AccessDeniedException",
                "denied",
            ),
        with_front(knowledge_base(), minimal()),
    );
    let result = manifest(&script);
    assert_eq!(
        text_at(&result, "knowledgeBases.0.knowledgeBase._error"),
        "ValidationException"
    );
    // A listing error is not "tolerated": it becomes the `_error` entry.
    assert_eq!(len_at(&result, "knowledgeBases.0.dataSources"), 1);
    assert!(text_at(&result, "knowledgeBases.0.dataSources.0._error")
        .starts_with("An error occurred (AccessDeniedException)"));
}

// ---------------------------------------------------- aliases and versions

#[test]
fn alias_listing_errors_keep_the_collected_aliases_and_add_an_error_entry() {
    let script = with_front(
        Script::default().error(
            "bedrock-agent",
            "ListAgentAliases",
            r#"{"agentId":"AGENT12345","nextToken":"t2"}"#,
            "ThrottlingException",
            "Rate exceeded",
        ),
        with_front(aliases_two_pages(), minimal()),
    );
    let result = manifest(&script);
    assert_eq!(len_at(&result, "aliasesAndVersions.aliases"), 3);
    assert_eq!(
        text_at(&result, "aliasesAndVersions.aliases.2._error"),
        "An error occurred (ThrottlingException) when calling the ListAgentAliases operation: \
         Rate exceeded"
    );
    // The error entry has no alias id, so it never counts as a production alias.
    assert_eq!(len_at(&result, "discovery.warnings"), 1);
}

#[test]
fn a_version_listing_error_is_recorded_after_the_collected_versions() {
    let script = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentVersions",
                r#"{"agentId":"AGENT12345"}"#,
                r#"{"agentVersionSummaries":[{"agentVersion":"DRAFT"}],"nextToken":"v2"}"#,
            )
            .error(
                "bedrock-agent",
                "ListAgentVersions",
                r#"{"agentId":"AGENT12345","nextToken":"v2"}"#,
                "AccessDeniedException",
                "denied",
            ),
        minimal(),
    );
    let result = manifest(&script);
    assert_eq!(len_at(&result, "aliasesAndVersions.versions"), 2);
    assert_eq!(
        text_at(&result, "aliasesAndVersions.versions.0.agentVersion"),
        "DRAFT"
    );
    assert!(text_at(&result, "aliasesAndVersions.versions.1._error").contains("ListAgentVersions"));
}

// ----------------------------------------------------------- collaborators

fn collaborators() -> Script {
    Script::default()
        .ok(
            "bedrock-agent",
            "ListAgentCollaborators",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            r#"{"agentCollaboratorSummaries":[{"collaboratorId":"COLLAB0001"},{"collaboratorId":"COLLAB0002"}]}"#,
        )
        .ok(
            "bedrock-agent",
            "GetAgentCollaborator",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","collaboratorId":"COLLAB0001"}"#,
            r#"{"agentCollaborator":{"collaboratorId":"COLLAB0001","collaboratorName":"billing"}}"#,
        )
        .error(
            "bedrock-agent",
            "GetAgentCollaborator",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT","collaboratorId":"COLLAB0002"}"#,
            "ResourceNotFoundException",
            "deleted",
        )
}

#[test]
fn collaborators_are_listed_and_a_missing_detail_is_recorded_in_place() {
    let result = manifest(&with_front(collaborators(), minimal()));
    assert_eq!(len_at(&result, "collaborators"), 2);
    assert_eq!(
        text_at(&result, "collaborators.0.collaboratorName"),
        "billing"
    );
    assert_eq!(
        text_at(&result, "collaborators.1._error"),
        "ResourceNotFoundException"
    );
}

#[test]
fn an_sdk_without_collaboration_support_leaves_the_list_empty() {
    let mut script = with_front(collaborators(), minimal());
    script.unsupported = vec!["ListAgentCollaborators"];
    let result = manifest(&script);
    assert_eq!(len_at(&result, "collaborators"), 0);
}

#[test]
fn a_collaborator_listing_error_becomes_an_error_entry() {
    let script = with_front(
        Script::default().error(
            "bedrock-agent",
            "ListAgentCollaborators",
            r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
            "AccessDeniedException",
            "denied",
        ),
        minimal(),
    );
    let result = manifest(&script);
    assert_eq!(len_at(&result, "collaborators"), 1);
    assert_eq!(
        text_at(&result, "collaborators.0._error"),
        "An error occurred (AccessDeniedException) when calling the ListAgentCollaborators \
         operation: denied"
    );
}

// ------------------------------------------------------------ agent errors

#[test]
fn a_get_agent_service_error_is_an_agent_failure_not_a_manifest() {
    let script = with_front(
        Script::default().error(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            "ResourceNotFoundException",
            "Agent not found",
        ),
        minimal(),
    );
    match run(&script, "DRAFT", false) {
        Err(Failure::Agent(text)) => assert_eq!(
            text,
            "An error occurred (ResourceNotFoundException) when calling the GetAgent \
             operation: Agent not found"
        ),
        other => panic!("expected an agent failure, got {other:?}"),
    }
}

#[test]
fn transport_and_credential_failures_end_the_run() {
    let connect = with_front(
        Script::default().fault(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            "connect",
        ),
        minimal(),
    );
    assert_eq!(fatal_code(run(&connect, "DRAFT", false)), "aws_connect");
    let no_credentials = with_front(
        Script::default().fault("sts", "GetCallerIdentity", "{}", "no_credentials"),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&no_credentials, "DRAFT", false)),
        "aws_no_credentials"
    );
    // Identity failures, including service errors, are not tolerated.
    let denied = with_front(
        Script::default().error("sts", "GetCallerIdentity", "{}", "AccessDenied", "no"),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&denied, "DRAFT", false)),
        "aws_service_error"
    );
}

#[test]
fn a_call_with_no_recorded_response_names_only_the_operation() {
    let mut client = Script::default().client();
    let error = client
        .call(
            Service::BedrockAgent,
            "GetAgent",
            &json(r#"{"agentId":"SECRET-ID-123"}"#),
        )
        .expect_err("unrecorded");
    let text = error.to_string();
    assert!(text.contains("bedrock-agent GetAgent"), "{text}");
    assert!(!text.contains("SECRET-ID-123"), "{text}");
}

#[test]
fn the_default_client_sends_nothing_and_says_so() {
    let result = discover(&mut NoLiveClient, &params("DRAFT", false));
    match result {
        Err(Failure::Fatal(AwsError::Connect { message })) => {
            assert!(message.contains("--recorded"), "{message}");
        }
        other => panic!("expected a connect failure, got {other:?}"),
    }
}

// ---------------------------------------------------------- DRAFT warnings

#[test]
fn draft_with_production_aliases_warns_and_the_test_alias_does_not() {
    let result = manifest(&with_front(aliases_two_pages(), minimal()));
    assert_eq!(len_at(&result, "discovery.warnings"), 1);
    assert_eq!(
        text_at(&result, "discovery.warnings.0"),
        "Fetched DRAFT but non-DRAFT aliases exist (prod->v3, blue->v?). \
         DRAFT may diverge from production."
    );
}

#[test]
fn only_the_test_alias_gives_no_warning_and_a_numbered_version_never_warns() {
    let only_test = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentAliases",
                r#"{"agentId":"AGENT12345"}"#,
                r#"{"agentAliasSummaries":[{"agentAliasId":"TSTALIASID"}]}"#,
            )
            .ok(
                "bedrock-agent",
                "GetAgentAlias",
                r#"{"agentId":"AGENT12345","agentAliasId":"TSTALIASID"}"#,
                r#"{"agentAlias":{"agentAliasId":"TSTALIASID"}}"#,
            ),
        minimal(),
    );
    assert_eq!(len_at(&manifest(&only_test), "discovery.warnings"), 0);

    // Version 3: every call that carries a version records it.
    let versioned = Script {
        calls: minimal()
            .calls
            .iter()
            .map(|call| {
                let text = call.to_compact_string().replace("DRAFT", "3");
                json(&text)
            })
            .collect(),
        unsupported: vec![],
    };
    let script = with_front(aliases_two_pages(), versioned);
    let result = discover(&mut script.client(), &params("3", false)).expect("discovery");
    assert_eq!(len_at(&result, "discovery.warnings"), 0);
    assert_eq!(text_at(&result, "discovery.fetchedAgentVersion"), "3");
}

#[test]
fn an_alias_without_a_name_or_routing_uses_question_marks() {
    let script = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentAliases",
                r#"{"agentId":"AGENT12345"}"#,
                r#"{"agentAliasSummaries":[{"agentAliasId":"PRODALIAS1"}]}"#,
            )
            .ok(
                "bedrock-agent",
                "GetAgentAlias",
                r#"{"agentId":"AGENT12345","agentAliasId":"PRODALIAS1"}"#,
                r#"{"agentAlias":{"agentAliasId":"PRODALIAS1"}}"#,
            ),
        minimal(),
    );
    assert_eq!(
        text_at(&manifest(&script), "discovery.warnings.0"),
        "Fetched DRAFT but non-DRAFT aliases exist (?->v?). DRAFT may diverge from production."
    );
}

// ------------------------------------------------------------------ helpers

#[test]
fn response_metadata_is_stripped_at_every_depth_and_nothing_else() {
    let stripped = strip_response_metadata(&json(
        r#"{"a":{"ResponseMetadata":1,"b":[{"ResponseMetadata":2,"c":3}]},"ResponseMetadata":4}"#,
    ));
    assert_eq!(stripped, json(r#"{"a":{"b":[{"c":3}]}}"#));
}

#[test]
fn recorded_documents_are_validated_without_echoing_values() {
    for (document, needle) in [
        (r"{}", "calls"),
        (
            r#"{"calls":[{"service":"ec2","operation":"X","response":{}}]}"#,
            "service",
        ),
        (
            r#"{"calls":[{"service":"sts","operation":"","response":{}}]}"#,
            "operation",
        ),
        (
            r#"{"calls":[{"service":"sts","operation":"X"}]}"#,
            "exactly one",
        ),
        (
            r#"{"calls":[{"service":"sts","operation":"X","response":{},"error":{}}]}"#,
            "exactly one",
        ),
        (
            r#"{"calls":[{"service":"sts","operation":"X","request":3,"response":{}}]}"#,
            "request",
        ),
        (
            r#"{"calls":[{"service":"sts","operation":"X","error":{"kind":"boom"}}]}"#,
            "error.kind",
        ),
        (r#"{"calls":[],"unsupported":3}"#, "unsupported"),
        (r#"{"calls":[],"unsupported":[1]}"#, "unsupported"),
    ] {
        let reason = RecordedClient::from_document(&json(document))
            .map(|_| ())
            .expect_err(document);
        assert!(reason.contains(needle), "{document}: {reason}");
    }
}

#[test]
fn recorded_requests_match_without_regard_to_key_order() {
    let mut client = Script::default()
        .ok(
            "bedrock-agent",
            "GetAgentActionGroup",
            r#"{"agentId":"A","agentVersion":"V","actionGroupId":"G"}"#,
            r#"{"ok":true}"#,
        )
        .client();
    let response = client.call(
        Service::BedrockAgent,
        "GetAgentActionGroup",
        &json(r#"{"actionGroupId":"G","agentVersion":"V","agentId":"A"}"#),
    );
    assert_eq!(response.expect("matches"), json(r#"{"ok":true}"#));
}

// ----------------------------------------------------------------- command

fn env(pairs: &[(&str, &str)]) -> MapEnv {
    pairs
        .iter()
        .fold(MapEnv::new(), |env, (k, v)| env.with(k, v))
}

#[test]
fn the_region_comes_from_the_flag_then_the_environment_and_has_no_default() {
    assert_eq!(region_of(None, &env(&[])).expect("ok"), None);
    assert_eq!(region_of(Some(""), &env(&[])).expect("ok"), None);
    assert_eq!(
        region_of(Some("eu-west-1"), &env(&[("AWS_REGION", "us-west-2")]))
            .expect("ok")
            .as_deref(),
        Some("eu-west-1")
    );
    assert_eq!(
        region_of(None, &env(&[("AWS_DEFAULT_REGION", "ap-south-1")]))
            .expect("ok")
            .as_deref(),
        Some("ap-south-1")
    );
    let error = region_of(Some("not a region\u{1b}"), &env(&[])).expect_err("invalid");
    assert!(!error.to_string().contains('\u{1b}'));
}

fn run_fetch(
    client: &mut dyn AgentClient,
    version: &str,
    out: &str,
) -> (Result<(), toolkit_core::ToolkitError>, String, String, u8) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let (result, status) = {
        let mut output = Output::with_diagnostics(&mut stdout, &mut stderr);
        let result = fetch_with(client, &params(version, false), out, &mut output);
        (result, output.exit_status())
    };
    (
        result,
        String::from_utf8(stdout).expect("utf-8"),
        String::from_utf8(stderr).expect("utf-8"),
        status,
    )
}

#[test]
fn the_manifest_file_is_pretty_ascii_json_without_a_trailing_newline() {
    let dir = TempDir::new("file");
    let out = dir.join_text("nested/deeper/manifest.json");
    let script = with_front(
        Script::default().ok(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            r#"{"agent":{"agentId":"AGENT12345","description":"café 😀 \"q\""}}"#,
        ),
        minimal(),
    );
    let (result, stdout, stderr, status) = run_fetch(&mut script.client(), "DRAFT", &out);
    result.expect("fetch succeeds");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(stdout, format!("Wrote {out}\n"));
    let written = fs::read_to_string(&out).expect("read manifest");
    assert!(written.is_ascii(), "{written}");
    assert!(!written.ends_with('\n'));
    assert!(written.starts_with("{\n  \"discovery\": {\n    \"account\": \"111122223333\","));
    // ensure_ascii: non-ASCII becomes `\uXXXX` (a surrogate pair above the BMP).
    let bs = '\\';
    let expected = format!("\"description\": \"caf{bs}u00e9 {bs}ud83d{bs}ude00 {bs}\"q{bs}\"\"");
    assert!(written.contains(&expected), "{written}");
    assert!(written.contains("\"executionRole\": {},"));
    assert!(written.contains("\"warnings\": []"));
    assert_eq!(parse_json(&written).expect("round trip"), manifest(&script));
}

#[cfg(unix)]
#[test]
fn the_manifest_is_restricted_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("mode");
    let out = dir.join_text("manifest.json");
    let (result, ..) = run_fetch(&mut minimal().client(), "DRAFT", &out);
    result.expect("fetch succeeds");
    let mode = fs::metadata(&out).expect("metadata").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // A pre-existing, world-readable file is tightened too.
    fs::set_permissions(&out, fs::Permissions::from_mode(0o644)).expect("chmod");
    let (result, ..) = run_fetch(&mut minimal().client(), "DRAFT", &out);
    result.expect("fetch succeeds");
    let mode = fs::metadata(&out).expect("metadata").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_link_at_the_output_path_is_refused_and_its_target_is_untouched() {
    let dir = TempDir::new("link");
    let target = dir.join("target.json");
    fs::write(&target, "precious").expect("write target");
    let link = dir.join("manifest.json");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let out = link.to_string_lossy().into_owned();
    let (result, stdout, ..) = run_fetch(&mut minimal().client(), "DRAFT", &out);
    let error = result.expect_err("link refused");
    assert!(
        error
            .to_string()
            .contains("symbolic links are not followed"),
        "{error}"
    );
    assert_eq!(stdout, "");
    assert_eq!(fs::read_to_string(&target).expect("read"), "precious");
}

#[test]
fn an_unwritable_output_is_a_runtime_error_and_prints_no_success_line() {
    let dir = TempDir::new("unwritable");
    // The output path is a directory.
    let out = dir.join_text("sub");
    fs::create_dir(dir.join("sub")).expect("mkdir");
    let (result, stdout, ..) = run_fetch(&mut minimal().client(), "DRAFT", &out);
    assert!(result.is_err());
    assert_eq!(stdout, "");
}

#[test]
fn draft_warnings_print_after_the_wrote_line_with_the_script_prefix() {
    let dir = TempDir::new("warn");
    let out = dir.join_text("m.json");
    let script = with_front(aliases_two_pages(), minimal());
    let (result, stdout, ..) = run_fetch(&mut script.client(), "DRAFT", &out);
    result.expect("fetch succeeds");
    assert_eq!(
        stdout,
        format!(
            "Wrote {out}\n  WARNING: Fetched DRAFT but non-DRAFT aliases exist \
             (prod->v3, blue->v?). DRAFT may diverge from production.\n"
        )
    );
}

#[test]
fn hostile_aws_text_cannot_forge_lines_on_either_stream() {
    let dir = TempDir::new("hostile");
    // A hostile alias name reaches the stdout warning.
    let hostile_alias = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentAliases",
                r#"{"agentId":"AGENT12345"}"#,
                r#"{"agentAliasSummaries":[{"agentAliasId":"PRODALIAS1"}]}"#,
            )
            .ok(
                "bedrock-agent",
                "GetAgentAlias",
                r#"{"agentId":"AGENT12345","agentAliasId":"PRODALIAS1"}"#,
                r#"{"agentAlias":{"agentAliasId":"PRODALIAS1","agentAliasName":"x\u001b[31m\nerror: forged\u202e","routingConfiguration":[{"agentVersion":"1\r2"}]}}"#,
            ),
        minimal(),
    );
    let out = dir.join_text("m.json");
    let (result, stdout, ..) = run_fetch(&mut hostile_alias.client(), "DRAFT", &out);
    result.expect("fetch succeeds");
    assert_eq!(stdout.lines().count(), 2, "{stdout:?}");
    assert!(
        !stdout.chars().any(|c| c.is_control() && c != '\n'),
        "{stdout:?}"
    );
    assert!(!stdout.contains('\u{202e}'));
    assert!(!stdout.lines().any(|line| line.starts_with("error:")));
    // The manifest keeps the value, JSON-escaped.
    let written = fs::read_to_string(&out).expect("read");
    assert!(
        written.contains(r"\u001b[31m\nerror: forged\u202e"),
        "{written}"
    );

    // A hostile service message reaches the stderr ERROR line.
    let hostile_error = with_front(
        Script::default().error(
            "bedrock-agent",
            "GetAgent",
            r#"{"agentId":"AGENT12345"}"#,
            "ResourceNotFoundException",
            "gone\u{1b}[2J\nERROR: forged\u{202e}",
        ),
        minimal(),
    );
    let (result, stdout, stderr, status) = run_fetch(
        &mut hostile_error.client(),
        "DRAFT",
        &dir.join_text("n.json"),
    );
    result.expect("an agent failure is reported, not returned");
    assert_eq!((stdout.as_str(), status), ("", 1));
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    assert!(stderr.starts_with("ERROR: failed to fetch agent \"AGENT12345\": "));
    assert!(!stderr.chars().any(|c| c.is_control() && c != '\n'));
    assert!(!stderr.contains('\u{202e}'));
    assert!(
        !dir.join("n.json").exists(),
        "no manifest on an agent failure"
    );
}

#[test]
fn a_hostile_agent_id_is_quoted_in_the_error_line() {
    let dir = TempDir::new("agent-id");
    let mut args = params("DRAFT", false);
    args.agent_id = "bad\u{1b}[31m\nERROR: forged".to_string();
    let mut client = Script::default()
        .ok("sts", "GetCallerIdentity", "{}", "{}")
        .client();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut output = Output::with_diagnostics(&mut stdout, &mut stderr);
    // The agent call is unrecorded: a transport failure, not an agent failure.
    let result = fetch_with(&mut client, &args, &dir.join_text("m.json"), &mut output);
    let error = result.expect_err("unrecorded call");
    assert!(error.to_string().contains("no recorded response"));
    assert!(!error.to_string().contains('\u{1b}'));
}

// ---------------------------------------------------------- registration

#[test]
fn the_command_is_registered_with_the_strict_router_contract() {
    let spec = COMMANDS
        .iter()
        .find(|spec| spec.path == ["bedrock-agent", "fetch"])
        .expect("registered");
    assert!(spec.strict_values);
    assert!(!spec.passthrough);
    assert_eq!(spec.max_positionals, 0);
    assert_eq!(spec.flags, ["--inline-s3-schemas"]);
    for flag in [
        "--agent-id",
        "--agent-version",
        "--agent-alias-id",
        "--region",
        "--profile",
        "--out",
        "--recorded",
    ] {
        assert!(spec.value_flags.contains(&flag), "{flag}");
    }
}

// ------------------------------------------- optional members: absent vs invalid

/// Every JSON value that is neither absent nor of the expected type.
const WRONG_TYPES: [&str; 6] = ["null", "1", "true", "\"text\"", "{}", "{\"a\":1}"];

/// One recorded call whose response is a listing, with the script that makes
/// the rest of the discovery succeed around it.
struct ListingSite {
    label: &'static str,
    service: &'static str,
    operation: &'static str,
    request: &'static str,
    key: &'static str,
    scaffold: fn() -> Script,
}

fn plain() -> Script {
    minimal()
}

fn with_knowledge_base() -> Script {
    with_front(knowledge_base(), minimal())
}

fn with_role() -> Script {
    with_front(role_agent(), minimal())
}

const LISTING_SITES: [ListingSite; 7] = [
    ListingSite {
        label: "aliases",
        service: "bedrock-agent",
        operation: "ListAgentAliases",
        request: r#"{"agentId":"AGENT12345"}"#,
        key: "agentAliasSummaries",
        scaffold: plain,
    },
    ListingSite {
        label: "versions",
        service: "bedrock-agent",
        operation: "ListAgentVersions",
        request: r#"{"agentId":"AGENT12345"}"#,
        key: "agentVersionSummaries",
        scaffold: plain,
    },
    ListingSite {
        label: "action groups",
        service: "bedrock-agent",
        operation: "ListAgentActionGroups",
        request: r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
        key: "actionGroupSummaries",
        scaffold: plain,
    },
    ListingSite {
        label: "knowledge bases",
        service: "bedrock-agent",
        operation: "ListAgentKnowledgeBases",
        request: r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
        key: "agentKnowledgeBaseSummaries",
        scaffold: plain,
    },
    ListingSite {
        label: "collaborators",
        service: "bedrock-agent",
        operation: "ListAgentCollaborators",
        request: r#"{"agentId":"AGENT12345","agentVersion":"DRAFT"}"#,
        key: "agentCollaboratorSummaries",
        scaffold: plain,
    },
    ListingSite {
        label: "data sources",
        service: "bedrock-agent",
        operation: "ListDataSources",
        request: r#"{"knowledgeBaseId":"KB00000001"}"#,
        key: "dataSourceSummaries",
        scaffold: with_knowledge_base,
    },
    ListingSite {
        label: "inline policy names",
        service: "iam",
        operation: "ListRolePolicies",
        request: r#"{"RoleName":"AgentRole"}"#,
        key: "PolicyNames",
        scaffold: with_role,
    },
];

impl ListingSite {
    /// The discovery script in which this site answers `response`.
    fn script(&self, response: &str) -> Script {
        with_front(
            Script::default().ok(self.service, self.operation, self.request, response),
            (self.scaffold)(),
        )
    }
}

#[test]
fn every_listing_member_rejects_null_and_wrong_types_as_invalid_responses() {
    for site in &LISTING_SITES {
        for wrong in WRONG_TYPES {
            let response = format!("{{\"{}\":{wrong}}}", site.key);
            assert_eq!(
                fatal_code(run(&site.script(&response), "DRAFT", false)),
                "aws_invalid_response",
                "{}: {response}",
                site.label
            );
        }
    }
}

#[test]
fn every_listing_member_still_accepts_an_absent_member_and_an_empty_list() {
    for site in &LISTING_SITES {
        for response in ["{}".to_string(), format!("{{\"{}\":[]}}", site.key)] {
            let result = run(&site.script(&response), "DRAFT", false);
            assert!(result.is_ok(), "{}: {response}", site.label);
        }
    }
}

#[test]
fn an_invalid_listing_is_never_recorded_as_an_error_entry_or_tolerated() {
    // The sites that record a service error in place must not record an
    // invalid response the same way: the run ends instead.
    for label in ["aliases", "versions", "collaborators", "data sources"] {
        let site = LISTING_SITES
            .iter()
            .find(|site| site.label == label)
            .expect("site");
        let response = format!("{{\"{}\":null}}", site.key);
        assert_eq!(
            fatal_code(run(&site.script(&response), "DRAFT", false)),
            "aws_invalid_response",
            "{label}"
        );
    }
}

#[test]
fn a_null_listing_on_a_later_page_ends_the_run_and_keeps_no_manifest() {
    let script = with_front(
        Script::default()
            .ok(
                "bedrock-agent",
                "ListAgentAliases",
                r#"{"agentId":"AGENT12345"}"#,
                r#"{"agentAliasSummaries":[],"nextToken":"p2"}"#,
            )
            .ok(
                "bedrock-agent",
                "ListAgentAliases",
                r#"{"agentId":"AGENT12345","nextToken":"p2"}"#,
                r#"{"agentAliasSummaries":null}"#,
            ),
        minimal(),
    );
    assert_eq!(
        fatal_code(run(&script, "DRAFT", false)),
        "aws_invalid_response"
    );
}

#[test]
fn the_role_arn_is_absent_or_a_string_and_nothing_else() {
    let agent = |agent: &str| {
        with_front(
            Script::default().ok(
                "bedrock-agent",
                "GetAgent",
                r#"{"agentId":"AGENT12345"}"#,
                &format!("{{\"agent\":{agent}}}"),
            ),
            minimal(),
        )
    };
    assert!(run(&agent(r#"{"agentName":"a"}"#), "DRAFT", false).is_ok());
    for wrong in ["null", "1", "true", "[]", "{}"] {
        let script = agent(&format!("{{\"agentResourceRoleArn\":{wrong}}}"));
        assert_eq!(
            fatal_code(run(&script, "DRAFT", false)),
            "aws_invalid_response",
            "{wrong}"
        );
    }
}

#[test]
fn an_invalid_listing_writes_no_manifest_and_leaves_an_existing_one_alone() {
    let dir = TempDir::new("invalid-listing");
    let fresh = dir.join_text("fresh/manifest.json");
    let existing = dir.join_text("existing.json");
    fs::write(&existing, "previous manifest").expect("seed");
    for site in &LISTING_SITES {
        let response = format!("{{\"{}\":null}}", site.key);
        for out in [&fresh, &existing] {
            let (result, stdout, ..) =
                run_fetch(&mut site.script(&response).client(), "DRAFT", out);
            assert!(result.is_err(), "{}", site.label);
            assert_eq!(stdout, "", "{}: no success line", site.label);
        }
    }
    assert!(!Path::new(&fresh).exists());
    assert_eq!(
        fs::read_to_string(&existing).expect("read"),
        "previous manifest"
    );
}

/// A recorded error entry with one member replaced by `member_json`.
fn recorded_error(member: &str, member_json: &str) -> String {
    let mut parts = vec![
        r#""kind":"client""#.to_string(),
        r#""code":"ThrottlingException""#.to_string(),
        r#""message":"slow""#.to_string(),
    ];
    parts.retain(|part| !part.starts_with(&format!("\"{member}\"")));
    parts.push(format!("\"{member}\":{member_json}"));
    format!(
        r#"{{"calls":[{{"service":"sts","operation":"X","error":{{{}}}}}]}}"#,
        parts.join(",")
    )
}

#[test]
fn a_present_but_wrong_typed_recorded_error_member_is_rejected() {
    for member in ["kind", "code", "message"] {
        for wrong in WRONG_TYPES.into_iter().filter(|wrong| *wrong != "\"text\"") {
            let document = recorded_error(member, wrong);
            let reason = RecordedClient::from_document(&json(&document))
                .map(|_| ())
                .expect_err(&document);
            assert!(reason.contains(member), "{document}: {reason}");
            assert!(reason.contains("recorded call 0"), "{reason}");
        }
    }
}

#[test]
fn recorded_error_members_are_validated_whatever_the_kind() {
    for kind in ["client", "connect", "no_credentials"] {
        for member in ["code", "message"] {
            let document = format!(
                r#"{{"calls":[{{"service":"sts","operation":"X","error":{{"kind":"{kind}","{member}":null}}}}]}}"#
            );
            assert!(
                RecordedClient::from_document(&json(&document)).is_err(),
                "{document}"
            );
        }
    }
}

#[test]
fn absent_recorded_error_members_keep_their_defaults() {
    for document in [
        r#"{"calls":[{"service":"sts","operation":"X","error":{}}]}"#,
        r#"{"calls":[{"service":"sts","operation":"X","error":{"kind":"connect"}}]}"#,
        r#"{"calls":[{"service":"sts","operation":"X","error":{"kind":"no_credentials"}}]}"#,
        r#"{"calls":[{"service":"sts","operation":"X","error":{"code":"E","message":"m"}}]}"#,
    ] {
        assert!(
            RecordedClient::from_document(&json(document)).is_ok(),
            "{document}"
        );
    }
}

#[test]
fn every_optional_recorded_member_rejects_null_and_wrong_types() {
    for (member, wrongs) in [
        ("request", &["null", "1", "[]", "\"x\""][..]),
        ("unsupported", &["null", "1", "{}", "\"x\""][..]),
    ] {
        for wrong in wrongs {
            let document = if member == "request" {
                format!(
                    r#"{{"calls":[{{"service":"sts","operation":"X","request":{wrong},"response":{{}}}}]}}"#
                )
            } else {
                format!(r#"{{"calls":[],"unsupported":{wrong}}}"#)
            };
            let reason = RecordedClient::from_document(&json(&document))
                .map(|_| ())
                .expect_err(&document);
            assert!(reason.contains(member), "{document}: {reason}");
        }
    }
}

#[test]
fn recorded_validation_reasons_never_echo_the_wrong_value() {
    let document = recorded_error("message", r#"{"secret":"SENSITIVE-VALUE-123"}"#);
    let reason = RecordedClient::from_document(&json(&document))
        .map(|_| ())
        .expect_err("rejected");
    assert!(!reason.contains("SENSITIVE"), "{reason}");
}
