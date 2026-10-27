# Bedrock agent discovery (#4383)

Slice `bedrock-fetch`, wave 2 of programme #4369. Base: integration head
`a41cfbb5f5dfbb9272fcb168d797ee4b2bcab961`. No Rust coverage existed at the base
(`git grep fetch_bedrock_agent` finds only Python and prose; there is no
`bedrock_resolve`).

Python source (read-only evidence, **not** deleted or re-wired here):
`skills/core-skills/amazon-bedrock/scripts/fetch_bedrock_agent.py` and its
byte-identical mirror `plugins/aws-core/skills/amazon-bedrock/scripts/fetch_bedrock_agent.py`.
Callers (`SKILL.md`, `references/bedrock-agents-to-agentcore-harness/discovery.md`,
`references/migrate-bedrock-agents-to-agentcore-harness.md`) still run the
Python; see "Retirement and follow-up".

## Command

| Rust | Python original |
| --- | --- |
| `toolkit bedrock-agent fetch --agent-id ID --out FILE [--agent-version V] [--agent-alias-id A] [--region R] [--profile P] [--inline-s3-schemas] [--recorded FILE]` | `fetch_bedrock_agent.py --agent-id ID --out FILE [...]` |

Registered once in `COMMANDS` (`value_flags`, `strict_values: true`,
`passthrough: false`, `max_positionals: 0`; `repeatable_flags` does not exist at
this base). Code: `crates/toolkit-cli/src/bedrock_agent.rs` and
`bedrock_agent/{client,recorded,fetch,tests}.rs`; end-to-end tests in
`crates/toolkit-cli/tests/bedrock_agent.rs`. Reused, not duplicated:
`toolkit_aws::{collect_pages, next_token_from, PageLimits, AwsError, ServiceError,
resolve_region, redact_text}`, `toolkit_core::{read_regular_file via
read_json_document, write_regular_file, quote_untrusted, neutralize_line}`,
`toolkit_msk::pyfmt::{py_str, ascii_escape}`, `manifests::read_failure`.

## Client seam and recorded input

Every `boto3` call is one `(service, Operation, request)` handed to an
`AgentClient`. The command wires `RecordedClient` (`--recorded FILE`) or
`NoLiveClient` (nothing is sent, no credential is read; the first call fails
with a `Connect` error and exit 1). The recorded file is one JSON object:

```json
{"unsupported": ["ListAgentCollaborators"],
 "calls": [
  {"service": "sts", "operation": "GetCallerIdentity", "request": {}, "response": {"Account": "...", "Arn": "..."}},
  {"service": "bedrock-agent", "operation": "GetAgent", "request": {"agentId": "..."},
   "error": {"code": "ResourceNotFoundException", "message": "..."}}]}
```

A call matches the entry with the same service (`sts`, `bedrock-agent`, `iam`,
`s3`), operation and request (objects compare without key order). `error.kind`
is `client` (default; a service error), `connect` or `no_credentials` (transport
failures). An unmatched call is a `Connect` error naming service and operation
only. `unsupported` models an SDK without `list_agent_collaborators`. S3
`GetObject` responses carry `{"Body": "<text>"}`.

Optional recorded members (`request`, `unsupported`, `error.kind`, `error.code`,
`error.message`) are either absent (the default applies) or of the stated type
(a string, an object or an array). A present member of another type, `null`
included, rejects the recording: exit 1, no success line, no manifest written
or overwritten; the reason names the member and its expected type, never its
value.

## Acceptance criteria

- **AC1 (mocked discovery, pagination, errors).** `bedrock_agent/tests.rs`:
  `the_minimal_agent_produces_the_script_manifest` (exact manifest and key order),
  `aliases_are_collected_across_pages_*`, `action_groups_are_paged_*`,
  `knowledge_bases_carry_association_detail_and_paged_data_sources`,
  `a_repeated_pagination_token_ends_the_run`, `an_empty_next_token_ends_the_listing`,
  tolerated errors (`iam_access_denied_*`, `tolerated_knowledge_base_errors_*`,
  `action_groups_are_paged_and_a_denied_detail_*`), fatal errors
  (`an_untolerated_detail_error_ends_the_run`, `a_failing_action_group_listing_*`,
  `transport_and_credential_failures_end_the_run`), partial listings
  (`alias_listing_errors_keep_*`, `a_version_listing_error_*`,
  `a_data_source_service_error_*`, `a_collaborator_listing_error_*`), S3 inlining,
  collaborator SDK absence, DRAFT warnings.
- **AC2 (matching JSON/output contracts).** Manifest members, their order, the
  `_error`/`_message`/`_inlineError` shapes, `str(ClientError)` text, the DRAFT
  warning text, `Wrote FILE` / `  WARNING: ...` on stdout, the two `ERROR:` lines
  on stderr, `indent=2` + `ensure_ascii` output without trailing newline, mode
  `0600`, exit statuses 0/1/2. Tests: `the_manifest_file_is_pretty_ascii_json_*`,
  `the_manifest_is_restricted_to_its_owner`, `tests/bedrock_agent.rs`.
- **AC3 (no Python boto3 helper in the install path).** Partially met, see below.

## Intentional changes

1. **No live client in this build.** `--recorded FILE` replaces the live calls;
   without it the command fails closed (exit 1). Same decision as
   `startup verify-model-path` (#4402).
2. **Output path is not followed through a symbolic link** (`write_regular_file`);
   the Python `open(out, "w")` followed links. A recorded file that is a link is
   refused too.
3. **Errors are redacted and one line.** `ServiceError` redacts credential-looking
   text and flattens the message (bounded), so `_message`, `_error` texts and the
   stderr `ERROR:` line may differ from raw botocore text in those cases. The
   agent id in `ERROR: failed to fetch agent` is quoted with `quote_untrusted`;
   stdout lines (`Wrote`, warnings) use `neutralize_line`. The manifest itself is
   JSON, where controls are escaped by the encoder.
4. **Transport failures end the run with `error: ...`** (shared router format,
   exit 1) instead of a Python traceback.
5. **Region.** `--region`, else `AWS_REGION`, else `AWS_DEFAULT_REGION` (the
   `toolkit-aws` order; boto3 also reads profile config, which is not read here);
   the region check happens before any call instead of after `sts`.
6. **Response shape errors** (an optional list member (every listing and the
   IAM `PolicyNames`) that is present but `null` or not a list, an
   `agentResourceRoleArn` that is not a string, a summary without
   its id, a non-string S3 `Body`) end the run with `aws_invalid_response`; Python
   raised `KeyError`/`TypeError`. A non-object `s3` reference is left unchanged.
7. `datetime` values: recorded responses are JSON, so timestamps are recorded
   strings; Python's `default=str` rendering only applies to live clients.
8. `--profile` is accepted and ignored offline.

## Retirement and follow-up

The Python script and its prose callers are **not** retired: the Rust command has
no live client yet, so replacing the callers now would break the skill. Retiring
the scripts and flipping the three prose callers is part of #4415 once a live
client exists. A bounded follow-up issue covers a `toolkit-aws` live binding
(bedrock-agent REST-JSON, IAM/STS query and S3 GET over `ApiClient` with the
`sdk` feature, behind this same `AgentClient` seam, never run in tests) and
request-id pattern validation for path parameters.
