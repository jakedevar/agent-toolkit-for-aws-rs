//! Detection rules and the hook input protocol.
//!
//! Rule identifiers (R1-R9) match `docs/migration/security-hooks-review.md`.
//! Anything the hook cannot validate is denied: the hook fails closed.

use toolkit_core::{parse_json, Json};

use crate::decision::{Decision, DenyReason};
use crate::matchers::{
    contains_folded, has_compound_operator, has_inline_interpreter, is_read_only_command,
    matches_agent_url, matches_cli_get_secret_value, matches_sdk_call,
};
use crate::pyjson::python_dumps;

/// Largest hook input accepted; anything larger is denied unread.
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

const DENY_SECRET: Decision = Decision::Deny(DenyReason::SecretFetch);
const DENY_MALFORMED: Decision = Decision::Deny(DenyReason::MalformedInput);

/// Operation names (lower case, no separators) that return secret values.
const GSV_OPERATIONS: [&str; 2] = ["getsecretvalue", "batchgetsecretvalue"];
const SERVICE_KEYS: [&str; 3] = ["service_name", "service", "serviceName"];
const OPERATION_KEYS: [&str; 3] = ["operation_name", "operation", "operationName"];

/// Decides one hook invocation from its raw standard input.
///
/// Never panics on any byte string; any input that is not a valid hook
/// document is denied.
#[must_use]
pub fn evaluate(input: &[u8]) -> Decision {
    if input.len() > MAX_INPUT_BYTES {
        return Decision::Deny(DenyReason::InputTooLarge);
    }
    let Ok(text) = std::str::from_utf8(input) else {
        return DENY_MALFORMED;
    };
    let Ok(document) = parse_json(text) else {
        return DENY_MALFORMED;
    };
    evaluate_document(&document)
}

fn evaluate_document(document: &Json) -> Decision {
    if !matches!(document, Json::Object(_)) {
        return DENY_MALFORMED;
    }
    let tool_name = match document.get("tool_name") {
        None => "",
        Some(Json::String(name)) => name.as_str(),
        Some(_) => return DENY_MALFORMED,
    };
    let empty = Json::Object(Vec::new());
    let tool_input = document.get("tool_input").unwrap_or(&empty);

    if tool_name == "use_aws" || tool_name.starts_with("mcp__") {
        return check_structured_tool(tool_input);
    }
    if tool_name.contains("run_script") {
        return check_run_script(tool_input);
    }
    if tool_name == "Bash" {
        return check_bash(tool_input);
    }
    Decision::Allow
}

/// R1, R2 and R3 for `use_aws` and MCP tools.
fn check_structured_tool(tool_input: &Json) -> Decision {
    if !matches!(tool_input, Json::Object(_)) {
        return DENY_MALFORMED;
    }
    if is_structured_secret_fetch(tool_input) {
        return DENY_SECRET;
    }
    let texts = scan_texts(tool_input);
    let sdk_call = texts.iter().any(|text| matches_sdk_call(text));
    if sdk_call
        && texts
            .iter()
            .any(|text| contains_folded(text, "secretsmanager"))
    {
        return DENY_SECRET;
    }
    if texts.iter().any(|text| matches_cli_get_secret_value(text)) {
        return DENY_SECRET;
    }
    Decision::Allow
}

/// R4: an SDK call in any string value of a `run_script` style tool.
fn check_run_script(tool_input: &Json) -> Decision {
    if !matches!(tool_input, Json::Object(_)) {
        return DENY_MALFORMED;
    }
    let mut values = Vec::new();
    collect_string_values(tool_input, &mut values);
    if values.iter().any(|text| matches_sdk_call(text)) {
        return DENY_SECRET;
    }
    Decision::Allow
}

/// R5 to R9 for `Bash`.
fn check_bash(tool_input: &Json) -> Decision {
    if !matches!(tool_input, Json::Object(_)) {
        return DENY_MALFORMED;
    }
    let command = match tool_input.get("command") {
        None => "",
        Some(Json::String(command)) => command.as_str(),
        Some(_) => return DENY_MALFORMED,
    };
    if matches_cli_get_secret_value(command) || matches_agent_url(command) {
        return DENY_SECRET;
    }
    if is_read_only_command(command) && !has_compound_operator(command) {
        return Decision::Allow;
    }
    if has_inline_interpreter(command) && matches_sdk_call(command) {
        return DENY_SECRET;
    }
    if matches_sdk_call(command) && contains_folded(command, "secretsmanager") {
        return DENY_SECRET;
    }
    Decision::Allow
}

/// R1. Every service and operation spelling present is considered, not only
/// the first truthy one; non-string values are ignored here.
fn is_structured_secret_fetch(tool_input: &Json) -> bool {
    let is_service = SERVICE_KEYS.iter().any(|key| {
        tool_input
            .get(key)
            .and_then(Json::as_str)
            .is_some_and(|service| service.eq_ignore_ascii_case("secretsmanager"))
    });
    is_service
        && OPERATION_KEYS.iter().any(|key| {
            tool_input
                .get(key)
                .and_then(Json::as_str)
                .is_some_and(|operation| {
                    GSV_OPERATIONS.contains(&normalize_operation(operation).as_str())
                })
        })
}

/// Collapses casing and `-`/`_` separators so `GetSecretValue` equals
/// `get-secret-value`.
fn normalize_operation(operation: &str) -> String {
    operation.to_lowercase().replace(['-', '_'], "")
}

/// Texts the SDK and CLI rules scan for an AWS tool call: the exact text the
/// legacy hook scanned, plus every key, every string value and every array of
/// strings joined with spaces.
fn scan_texts(tool_input: &Json) -> Vec<String> {
    let mut texts = vec![python_dumps(tool_input)];
    collect_texts(tool_input, &mut texts);
    texts
}

fn collect_texts(value: &Json, texts: &mut Vec<String>) {
    match value {
        Json::String(text) => texts.push(text.clone()),
        Json::Array(items) => {
            let strings: Vec<&str> = items.iter().filter_map(Json::as_str).collect();
            if !strings.is_empty() && strings.len() == items.len() {
                texts.push(strings.join(" "));
            }
            for item in items {
                collect_texts(item, texts);
            }
        }
        Json::Object(pairs) => {
            for (key, item) in pairs {
                texts.push(key.clone());
                collect_texts(item, texts);
            }
        }
        Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) => {}
    }
}

fn collect_string_values<'a>(value: &'a Json, values: &mut Vec<&'a str>) {
    match value {
        Json::String(text) => values.push(text.as_str()),
        Json::Array(items) => {
            for item in items {
                collect_string_values(item, values);
            }
        }
        Json::Object(pairs) => {
            for (_, item) in pairs {
                collect_string_values(item, values);
            }
        }
        Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(tool: &str, input: Json) -> String {
        Json::object(vec![
            ("tool_name", Json::string(tool)),
            ("tool_input", input),
        ])
        .to_compact_string()
    }

    fn decide(tool: &str, input: Json) -> Decision {
        evaluate(payload(tool, input).as_bytes())
    }

    fn fields(pairs: &[(&str, &str)]) -> Json {
        Json::object(
            pairs
                .iter()
                .map(|(key, value)| (*key, Json::string(*value)))
                .collect(),
        )
    }

    fn bash(command: &str) -> Decision {
        decide("Bash", fields(&[("command", command)]))
    }

    fn denies_secret(decision: Decision) -> bool {
        decision == DENY_SECRET
    }

    // ---- R1 structured -------------------------------------------------

    #[test]
    fn r1_structured_operation_spellings_are_denied() {
        let key_pairs = [
            ("service_name", "operation_name"),
            ("service", "operation"),
            ("serviceName", "operationName"),
        ];
        let operations = [
            "get-secret-value",
            "GetSecretValue",
            "get_secret_value",
            "GETSECRETVALUE",
            "batch-get-secret-value",
            "BatchGetSecretValue",
            "batch_get_secret_value",
        ];
        for tool in ["use_aws", "mcp__aws__call", "mcp__plugin_x_aws-mcp__run"] {
            for (service_key, operation_key) in key_pairs {
                for service in ["secretsmanager", "SecretsManager"] {
                    for operation in operations {
                        let input = fields(&[(service_key, service), (operation_key, operation)]);
                        assert!(
                            denies_secret(decide(tool, input)),
                            "{tool} {service_key}={service} {operation_key}={operation}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn r1_other_operations_and_services_are_allowed() {
        for operation in [
            "list-secrets",
            "describe-secret",
            "get-resource-policy",
            "put-secret-value",
            "get-random-password",
            "GetSecretValues",
        ] {
            let input = fields(&[
                ("service_name", "secretsmanager"),
                ("operation_name", operation),
            ]);
            assert_eq!(decide("use_aws", input), Decision::Allow, "{operation}");
        }
        let input = fields(&[
            ("service_name", "ssm"),
            ("operation_name", "get-secret-value"),
        ]);
        assert_eq!(decide("use_aws", input), Decision::Allow);
        let input = fields(&[("service_name", "secretsmanager")]);
        assert_eq!(decide("use_aws", input), Decision::Allow);
    }

    #[test]
    fn r1_checks_every_spelling_and_ignores_non_strings() {
        let mixed = fields(&[
            ("service_name", "ec2"),
            ("service", "secretsmanager"),
            ("operation", "get-secret-value"),
        ]);
        assert!(denies_secret(decide("use_aws", mixed)));
        let non_string = Json::object(vec![
            ("service_name", Json::Int(5)),
            ("service", Json::object(vec![("a", Json::Int(1))])),
            ("operation", Json::string("GetSecretValue")),
        ]);
        assert_eq!(decide("use_aws", non_string), Decision::Allow);
    }

    // ---- R2 SDK call plus secretsmanager --------------------------------

    #[test]
    fn r2_sdk_call_with_secretsmanager_is_denied() {
        for code in [
            "import boto3; boto3.client('secretsmanager').get_secret_value(SecretId='x')",
            "new GetSecretValueCommand({}) // secretsmanager",
            "client.\nget_secret_value(x)\n# SecretsManager",
            "client.batch_get_secret_value(x) SECRETSMANAGER",
        ] {
            let input = fields(&[("code", code)]);
            assert!(denies_secret(decide("mcp__aws__run", input)), "{code:?}");
        }
        // Mention and call in different fields, and the mention in a key.
        let split = fields(&[("a", "client.get_secret_value(x)"), ("b", "secretsmanager")]);
        assert!(denies_secret(decide("use_aws", split)));
        let key = fields(&[("secretsmanager", "client.get_secret_value(x)")]);
        assert!(denies_secret(decide("use_aws", key)));
    }

    #[test]
    fn r2_needs_both_the_call_and_the_mention() {
        for code in [
            "s3.get_secret_value(x)",
            "docs about secretsmanager get_secret_value usage",
            "client.get_secret_values(secretsmanager)",
        ] {
            let input = fields(&[("code", code)]);
            assert_eq!(decide("use_aws", input), Decision::Allow, "{code:?}");
        }
    }

    // ---- R3 CLI in structured tools --------------------------------------

    #[test]
    fn r3_cli_command_strings_and_argv_arrays_are_denied() {
        for command in [
            "aws secretsmanager get-secret-value --secret-id x",
            "aws --region us-east-1 secretsmanager get-secret-value",
            "AWS SecretsManager Batch-Get-Secret-Value",
        ] {
            let input = fields(&[("cli_command", command)]);
            assert!(
                denies_secret(decide("mcp__aws__call_aws", input)),
                "{command:?}"
            );
        }
        let argv = Json::object(vec![(
            "args",
            Json::Array(vec![
                Json::string("aws"),
                Json::string("secretsmanager"),
                Json::string("get-secret-value"),
            ]),
        )]);
        assert!(denies_secret(decide("mcp__aws__call_aws", argv)));
    }

    #[test]
    fn r3_other_cli_commands_are_allowed() {
        for command in [
            "aws s3 ls",
            "aws secretsmanager list-secrets",
            "aws secretsmanager describe-secret --secret-id x",
        ] {
            let input = fields(&[("cli_command", command)]);
            assert_eq!(
                decide("mcp__aws__call_aws", input),
                Decision::Allow,
                "{command:?}"
            );
        }
        let argv = Json::object(vec![(
            "args",
            Json::Array(vec![
                Json::string("aws"),
                Json::string("s3"),
                Json::string("ls"),
            ]),
        )]);
        assert_eq!(decide("mcp__aws__call_aws", argv), Decision::Allow);
    }

    #[test]
    fn unguarded_tools_are_never_inspected() {
        let input = fields(&[("cli_command", "aws secretsmanager get-secret-value")]);
        assert_eq!(decide("Read", input.clone()), Decision::Allow);
        assert_eq!(decide("Edit", input), Decision::Allow);
        assert_eq!(
            evaluate(br#"{"tool_input": {"command": "aws secretsmanager get-secret-value"}}"#),
            Decision::Allow,
            "a missing tool_name selects no rule"
        );
        assert_eq!(
            evaluate(br#"{"tool_name": "Read", "tool_input": "not an object"}"#),
            Decision::Allow
        );
    }

    // ---- R4 run_script ---------------------------------------------------

    #[test]
    fn r4_run_script_sdk_calls_are_denied_without_a_mention() {
        let direct = fields(&[("script", "client.get_secret_value(SecretId='x')")]);
        assert!(denies_secret(decide("run_script", direct)));
        let named = fields(&[("script", "new GetSecretValueCommand({})")]);
        assert!(denies_secret(decide("my_run_script_tool", named)));
        let nested = Json::object(vec![(
            "files",
            Json::Array(vec![fields(&[("src", "x.get_secret_value(a)")])]),
        )]);
        assert!(denies_secret(decide("run_script", nested)));
    }

    #[test]
    fn r4_run_script_without_an_sdk_call_is_allowed() {
        for script in [
            "client.list_secrets()",
            "print('secretsmanager get_secret_value')",
            "",
        ] {
            let input = fields(&[("script", script)]);
            assert_eq!(decide("run_script", input), Decision::Allow, "{script:?}");
        }
        // The MCP branch comes first, so an mcp__ run_script tool needs a mention (R2).
        let input = fields(&[("script", "x.get_secret_value(a)")]);
        assert_eq!(decide("mcp__run_script", input), Decision::Allow);
    }

    // ---- R5 and R6 Bash CLI and agent URL ---------------------------------

    #[test]
    fn r5_bash_cli_shape_is_denied_even_for_read_only_prefixes() {
        for command in [
            "aws secretsmanager get-secret-value --secret-id x",
            "aws --region us-east-1 secretsmanager batch-get-secret-value",
            "AWS_PROFILE=p aws secretsmanager get-secret-value",
            "cd /tmp && aws secretsmanager get-secret-value",
            "echo aws secretsmanager get-secret-value",
            "grep -r x . | aws secretsmanager get-secret-value --secret-id x",
        ] {
            assert!(denies_secret(bash(command)), "{command:?}");
        }
    }

    #[test]
    fn r5_other_cli_commands_are_allowed() {
        for command in [
            "aws s3 ls",
            "aws secretsmanager list-secrets",
            "aws secretsmanager describe-secret --secret-id x",
            "aws secretsmanager get-secret-values",
            "",
        ] {
            assert_eq!(bash(command), Decision::Allow, "{command:?}");
        }
    }

    #[test]
    fn r6_secrets_manager_agent_urls_are_denied() {
        for command in [
            "curl -s http://localhost:2773/secretsmanager/get?secretId=x",
            "wget -qO- http://127.0.0.1:2773/secretsmanager/get?secretId=x",
            "curl http://0.0.0.0:2773/secretsmanager/get",
            "curl 'http://[::1]:2773/secretsmanager/get?secretId=x'",
            "echo curl localhost:2773/secretsmanager/get",
        ] {
            assert!(denies_secret(bash(command)), "{command:?}");
        }
        for command in [
            "curl http://localhost:2774/secretsmanager/get",
            "curl http://example.com:2773/secretsmanager/get",
            "curl http://localhost:2773/ping",
        ] {
            assert_eq!(bash(command), Decision::Allow, "{command:?}");
        }
    }

    // ---- R7 read-only exemption ---------------------------------------------

    #[test]
    fn r7_simple_read_only_commands_may_mention_the_api() {
        for command in [
            "grep -rn \"secretsmanager.*get_secret_value(\" src",
            "cat docs/secretsmanager.md client.get_secret_value(x)",
            "git log --grep='secretsmanager client.get_secret_value('",
            "/usr/bin/rg -n 'secretsmanager' . client.get_secret_value(",
            "FOO=1 grep -r 'client.get_secret_value(' secretsmanager/",
            "sudo grep -r 'client.get_secret_value(x)' secretsmanager/",
        ] {
            assert_eq!(bash(command), Decision::Allow, "{command:?}");
        }
    }

    #[test]
    fn r7_compound_commands_lose_the_exemption() {
        for command in [
            "grep -r x . ; python3 -c \"import boto3; boto3.client('secretsmanager').get_secret_value(SecretId='x')\"",
            "grep -r x . && node -e \"client.getSecretValue(x)\"",
            "cat notes.txt\npython3 -c \"client.get_secret_value(x)\"",
            "cat x | python3 -c \"c.get_secret_value(1)\"",
            "cat <(python3 -c \"c.get_secret_value(1)\")",
            "echo `python3 -c \"c.get_secret_value(1)\"`",
            "echo $(python3 -c \"c.get_secret_value(1)\")",
        ] {
            assert!(denies_secret(bash(command)), "{command:?}");
        }
    }

    // ---- R8 inline interpreter ----------------------------------------------

    #[test]
    fn r8_inline_interpreter_with_sdk_call_is_denied_without_a_mention() {
        for command in [
            "python3 -c \"import boto3; boto3.client('secretsmanager').get_secret_value(SecretId='x')\"",
            "python -c 'c.get_secret_value(1)'",
            "python3.11 -c 'c.get_secret_value(1)'",
            "python2.7 -c 'c.get_secret_value(1)'",
            "node -e \"client.send(new GetSecretValueCommand({}))\"",
            "node -pe 'c.getSecretValue(1)'",
            "ruby -e 'c.get_secret_value(id)'",
            "sudo env X=1 python3 -c 'c.batch_get_secret_value(1)'",
        ] {
            assert!(denies_secret(bash(command)), "{command:?}");
        }
    }

    #[test]
    fn r8_inline_interpreter_without_an_sdk_call_is_allowed() {
        for command in [
            "python3 -c \"print(1)\"",
            "python3 -c \"client.list_secrets()\"",
            "python3 script.py",
            "node -e \"console.log(1)\"",
        ] {
            assert_eq!(bash(command), Decision::Allow, "{command:?}");
        }
    }

    // ---- R9 heredoc and pipe -----------------------------------------------------

    #[test]
    fn r9_sdk_call_with_mention_is_denied() {
        for command in [
            "python3 <<'EOF'\nimport boto3\nboto3.client('secretsmanager').get_secret_value(SecretId='x')\nEOF",
            "python3 script.py # client.get_secret_value(x) uses SecretsManager",
            "cat <<EOF | python3\nimport boto3\nboto3.client(\"secretsmanager\").get_secret_value(SecretId=\"x\")\nEOF",
        ] {
            assert!(denies_secret(bash(command)), "{command:?}");
        }
    }

    #[test]
    fn r9_needs_both_the_call_and_the_mention() {
        for command in [
            "python3 <<EOF\nclient.get_secret_value(x)\nEOF",
            "python3 <<EOF\nimport boto3\nclient = boto3.client('secretsmanager')\nEOF",
            "make test",
        ] {
            assert_eq!(bash(command), Decision::Allow, "{command:?}");
        }
    }

    // ---- fail-closed input handling -------------------------------------------

    #[test]
    fn malformed_input_is_denied() {
        let cases: [&[u8]; 14] = [
            b"",
            b"   ",
            b"not json",
            b"[]",
            b"null",
            b"\"Bash\"",
            b"{\"tool_name\": 5}",
            b"{\"tool_name\": null}",
            b"{\"tool_name\": \"Bash\", \"tool_input\": \"x\"}",
            b"{\"tool_name\": \"Bash\", \"tool_input\": {\"command\": 5}}",
            b"{\"tool_name\": \"Bash\", \"tool_input\": {\"command\": null}}",
            b"{\"tool_name\": \"run_script\", \"tool_input\": []}",
            b"{\"tool_name\": \"use_aws\", \"tool_input\": null}",
            b"{\"tool_name\": \"Bash\", \"tool_name\": \"Read\"}",
        ];
        for case in cases {
            assert_eq!(
                evaluate(case),
                DENY_MALFORMED,
                "{}",
                String::from_utf8_lossy(case)
            );
        }
        assert_eq!(
            evaluate(&[0xff, 0xfe, 0x7b]),
            DENY_MALFORMED,
            "invalid UTF-8"
        );
        assert_eq!(
            evaluate(b"\xef\xbb\xbf{}"),
            DENY_MALFORMED,
            "byte order mark"
        );
    }

    #[test]
    fn missing_tool_input_is_an_empty_object() {
        assert_eq!(evaluate(br#"{"tool_name": "use_aws"}"#), Decision::Allow);
        assert_eq!(evaluate(br#"{"tool_name": "Bash"}"#), Decision::Allow);
        assert_eq!(evaluate(b"{}"), Decision::Allow);
    }

    #[test]
    fn oversized_input_is_denied_unread() {
        let big = vec![b' '; MAX_INPUT_BYTES + 1];
        assert_eq!(evaluate(&big), Decision::Deny(DenyReason::InputTooLarge));
        let exact = vec![b' '; MAX_INPUT_BYTES];
        assert_eq!(
            evaluate(&exact),
            DENY_MALFORMED,
            "at the cap it is parsed (and empty)"
        );
    }

    #[test]
    fn very_deep_nesting_is_denied_not_a_stack_overflow() {
        let deep = format!(
            "{{\"tool_name\": \"use_aws\", \"tool_input\": {}1{}}}",
            "[".repeat(5000),
            "]".repeat(5000)
        );
        assert_eq!(evaluate(deep.as_bytes()), DENY_MALFORMED);
    }

    #[test]
    fn long_adversarial_commands_finish_and_are_judged() {
        let flags = "-a v ".repeat(100_000);
        let near_miss = format!("aws {flags}secretsmanager get-secret-valu");
        assert_eq!(bash(&near_miss), Decision::Allow);
        let hit = format!("aws {}secretsmanager get-secret-value", "-a ".repeat(12));
        assert!(denies_secret(bash(&hit)));
        let spaces = format!("x.{}y", " ".repeat(500_000));
        assert_eq!(bash(&spaces), Decision::Allow);
        let dots = ". ".repeat(250_000);
        assert_eq!(bash(&dots), Decision::Allow);
    }
}
