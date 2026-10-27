# `toolkit asm exec` — Rust port of the `asm-exec` helper (#4451)

Slice `asm-exec`, wave 2 of programme #4369. Python source (read-only migration
evidence, **not** deleted or re-wired here; retirement belongs to #4415):
`plugins/aws-core/skills/aws-secrets-manager/references/asm-exec`, an
extensionless `#!/usr/bin/env python3` executable that the `*.py` inventory
missed (audit #4371, F-8). Stacked on the accepted #4379 (`toolkit-aws`,
`5112a8b`) and on the #4378 integration head with the canonical router
(`35640f1`); the merge needed one conflict resolution, `Cargo.lock`, kept both
sides (the `toolkit-aws` workspace entry from #4379 and the dependency set from
#4378).

## Command

```text
toolkit asm exec [--] COMMAND [ARG...]
```

Replaces every `{{resolve:secretsmanager:ID[:SecretString[:KEY[:STAGE]]]}}`
reference in `COMMAND`, `ARG...` and the **exported environment** with the
secret value, then runs `COMMAND` with the result and exits with its status.
One optional leading `--` is dropped. The launcher that maps the old name
`asm-exec` onto `toolkit asm exec` is #4415's job; until then the Python helper
stays the shipped `asm-exec`.

Code: `crates/toolkit-cli/src/asm_exec.rs` (+ `asm_exec/{reference,sma,loopback,
mcp,credentials,process}.rs`), registered in the canonical `COMMANDS` table of
`crates/toolkit-cli/src/lib.rs`. No parallel router.

### Router changes (canonical, shared)

| Change | Why |
| --- | --- |
| `CommandSpec::passthrough` | `asm exec ls -la --help` must hand every word, dash-prefixed ones and `--` included, to the command. With the flag set, all words after the command path are positionals and `-h`/`--help` are not interpreted. All other commands keep `passthrough: false` and behave as before. |
| `Output::set_exit_status` / `exit_status`, used by `run` when the handler returns `Ok` | The child's exit status (0-255) must be the process status without an `error:` report, and the Python's own failures print their own `asm-exec: ...` lines. `ToolkitError` can only express 1 and 2. |

## Secret handling

- A resolved value is a `toolkit_aws::Secret` (fixed `Debug`) and is copied
  only into the **child's argument vector and environment**, exactly where the
  Python put it. It is never written to stdout, stderr or a file and is not
  passed on stdin (the Python did not use stdin either). While the child runs,
  an argument is visible to anyone who can read the child's process table; that
  is inherent to the documented design and is why the skill prefers environment
  variables where the tool allows it. The value is not zeroized in memory.
- `asm exec` writes **nothing to stdout**. Every stderr line goes through
  `toolkit_aws::redact_text` in one place (`asm_exec::emit`), and control
  characters are flattened afterwards so remote text cannot forge lines or
  terminal escapes.
- Resolution is single-pass: a replacement is never scanned again, so a secret
  containing `{{resolve:...}}` cannot trigger a second lookup.
- The agent session token is sent only to a loopback address (see below).
- Nothing is sent unsigned or over plain HTTP to a non-loopback host; the MCP
  endpoint must pass `toolkit_aws::validate_endpoint` (https, AWS domains only,
  every DNS answer public).
- The command that is started is never named after resolution: a launch
  failure prints the **unresolved** first word.
- Resolution failures stop everything before the command starts: arguments are
  resolved first, then the environment (the Python order); the first failure
  exits 1 with nothing launched.

## Collaborators and tests

Everything with a side effect is injected (`asm_exec::Deps`): environment,
child-environment snapshot, program launcher, agent transport, MCP transport,
MCP signer, DNS. Tests use `toolkit_aws::testing` (`MockTransport`,
`FakeSigner`, `FixedResolver`, `MapEnv`) and scripted fakes; no test reaches
AWS or resolves a name. The only sockets are listeners on `127.0.0.1` inside the
test process (the loopback transport tests and the end-to-end test in
`tests/asm_exec.rs`), and the only programs started are `/bin/sh`, `true`
and the test binary itself.

## Parity with the Python

| Behavior | Python | Rust | Evidence |
| --- | --- | --- | --- |
| No command / only `--` | `Usage: asm-exec <command> [args...]`, exit 1 | same text, exit 1 | `no_command_prints_the_usage_line_and_exits_one`, `a_missing_command_is_the_python_usage_error` |
| Leading `--` | dropped once | dropped once, later `--` kept | `only_one_leading_double_dash_is_dropped...` |
| Exit status | `sys.exit(returncode)`; signal N becomes 256-N | same | `exit_codes_are_forwarded_and_signals_fold_like_sys_exit`, e2e `kill -9` = 247 |
| Reference regex `\{\{resolve:secretsmanager:([^}]+)\}\}` | `re.sub` | hand scanner with the same leftmost/greedy semantics, linear time | `reference::tests::*` incl. `{{{...}}`, `a}b}}`, newline in reference, 20,000 unterminated openers |
| Single pass | `re.sub` with callable | same | `a_resolved_value_is_never_scanned_for_references` |
| Scope | argv words and every exported variable (values only) | same; values may be non-UTF-8 bytes | `environment_values_are_resolved_after_arguments...`, `non_utf8_environment_values...` |
| Reference parts | `id[:type[:key[:stage]]]`, ARN-aware (first 7 segments), empty key = whole value, `split(':', 3)` | same | `reference::tests::{plain_references,arns}...` |
| Field type | only `SecretString`; else `asm-exec: ERROR: Only SecretString is supported, got: X`, exit 1 | same, nothing contacted | `only_secret_string_is_supported...` |
| Region | ARN region, else `AWS_REGION`, else `AWS_DEFAULT_REGION` (empty = unset) | same; passed as `region_name` to `call_boto3` only when set | `the_secrets_region_comes_from_the_arn_then_the_environment` |
| Agent | `GET /ping` 1 s once; `GET /secretsmanager/get?...` 5 s; token header = `AWS_SESSION_TOKEN`, else `AWS_TOKEN` (an empty `AWS_SESSION_TOKEN` does not fall back); any failure falls through | same | `sma::tests::*`, `the_agent_endpoint_and_token_follow_the_python_settings` |
| Fallback | MCP: `initialize`, `notifications/initialized`, `tools/call aws___run_script` with `call_boto3('secretsmanager','GetSecretValue',...)`; `Mcp-Session-Id` carried; envelope search `SecretString`, `return_value`, `result`, `results`, `output`, `stdout` (JSON-text values parsed); distinct causes (rejected, HTTP status, timeout, unreachable, run_script error, no SecretString) on `asm-exec: MCP resolution failed: <cause>` | same messages and causes | `mcp::tests::*` |
| Script literals | `%r` of id, stage and region | the canonical `manifests::py_repr_str` (same escaping rules), tested with hostile ids | `hostile_ids_stay_inside_their_python_string_literals` |
| Signing scope | `service.region.api.aws` -> (service, region); `bedrock-agentcore` special case; else first label + `AWS_REGION`/`AWS_DEFAULT_REGION`/`us-east-1` | same | `signing_service_and_region_come_from_the_endpoint_host` |
| Credentials for signing | env, then `aws configure export-credentials --format env`, then `aws configure get` (5 s each) | same order as `toolkit-aws` providers over a `CommandRunner` seam | `credentials::tests::*` |
| `MCP timeout` | `ASM_EXEC_MCP_TIMEOUT` seconds, default 30 | same; see differences for invalid values | `the_mcp_endpoint_and_timeout_settings_apply...` |
| JSON key | `json.loads(v)[key]`; missing key or non-JSON: `asm-exec: ERROR: JSON key 'k' not found in: <id>`, exit 1; non-strings via `json.dumps` | same text; `json.dumps` formatting reproduced (`, ` / `: ` separators, ASCII, float `repr`) | `json_keys_select_members_and_non_strings_are_dumped_like_python`, `reference::tests::json_dumps...` |
| Empty value | treated as unresolved (`if not value`) | same | `an_empty_secret_does_not_resolve` |
| Unresolvable | `asm-exec: ERROR: Failed to resolve: <ref>`, exit 1, command not started | same line, scrubbed | `an_unresolvable_reference_stops_everything...` |
| Launch failure | uncaught `FileNotFoundError` traceback, exit 1 | `asm-exec: ERROR: Failed to run <unresolved word>: <os error>`, exit 1 | `a_launch_failure_exits_one...` |
| Child I/O | inherits stdin/stdout/stderr | same | e2e `the_command_keeps_the_standard_streams` |

## Intentional differences (recorded)

| Change | Reason |
| --- | --- |
| **MCP signing and HTTPS are not available in this build** (follow-up #4582) | `toolkit-aws` is std-only; TLS and SigV4 arrive with the SDK adapter (#4463). The whole MCP flow is ported and tested behind the `Signer`/`Transport` seams; production wiring uses `UnavailableSigner` (loads credentials first, so a machine without any reports the Python's `no AWS credentials available for MCP signing`, then fails closed with `AWS MCP request signing and the HTTPS transport are not available in this build`) and `UnavailableTransport`. Without a running Secrets Manager Agent, resolution therefore fails closed. **#4415 must not retire the Python helper until #4582 lands.** |
| The agent endpoint (`AWS_SECRETS_MANAGER_AGENT_ENDPOINT`) must be `http://` + `localhost`, `127.0.0.0/8` or `[::1]` with no path; anything else is skipped with one stderr notice and the MCP path is used | The Python sent the session token header to whatever URL the variable named, in clear text. Every resolved address is checked again before connecting; redirects are not followed |
| The agent's version stage is percent-encoded | The Python interpolated it raw, so `&`/`#` could add query parameters. Identical for normal stages |
| The MCP endpoint (`ASM_EXEC_MCP_ENDPOINT`) is validated with `toolkit_aws::validate_endpoint` (https, `.api.aws`/`.amazonaws.com` only, public DNS answers, no userinfo) | The Python signed and sent credentials to any URL. The default endpoint passes. The signing region must look like a region name |
| `ASM_EXEC_MCP_TIMEOUT` must be a finite number of seconds in (0, 3600]; anything else exits 1 before anything runs | Python raised a traceback at import for non-numbers and accepted 0, negative and infinite values |
| Diagnostics are scrubbed by `redact_text`, so a reference such as `db:SecretString:password` prints as `db:SecretString:[REDACTED]` in `Failed to resolve` lines (accepted over-redaction of the shared scrubber, label `secret` in `SecretString`) and control characters are flattened | The Python printed raw text. No diagnostic may carry a secret |
| Non-string agent members, non-object agent replies and non-string `SecretString` are "not resolved here" | Python would crash or pass a non-string on |
| JSON secrets use the strict `toolkit-core` parser: duplicate keys are an error (`JSON key not found`), integers beyond `i64` become floats | The shared parser is strict by design; failure is closed. Python kept the last duplicate |
| Arguments must be valid UTF-8 (the router reports exit 2 otherwise); environment values may be any bytes | Canonical router contract. Python allowed undecodable arguments via surrogateescape |
| Reference text between the delimiters must be UTF-8 | A non-UTF-8 reference is `asm-exec: ERROR: A reference is not valid UTF-8`, exit 1 |
| `Failed to run` instead of a traceback; `--help` after `exec` is a command name | Both exit 1 as before; no Python traceback |
| MCP HTTP statuses 300-399 are errors (no redirect following) and the reason phrase is a fixed table | Credentials must not follow redirects |

## Missing-binary behavior (AC3)

Nothing in this change alters how the shipped `asm-exec` finds its
implementation: the Python file and its launcher wiring are untouched. The
launcher change (a wrapper that execs `toolkit asm exec "$@"` and **fails
closed with exit 127 and a message when `toolkit` is missing instead of running
the command unresolved**) is part of the retirement in #4415, and is blocked on
#4582.

## Acceptance mapping

- **AC1** CLI, exit codes, secret handling, offline tests with mocked Secrets
  Manager: implemented and tested for the Secrets Manager Agent path; the MCP
  path is ported and tested with fakes but cannot run in this build (#4582).
- **AC2** skill docs/launchers updated and Python retired: deliberately
  deferred to #4415 (operator instruction on #4451); this page and
  `crates/toolkit-cli/README.md` document the Rust command.
- **AC3** missing-binary behavior: specified above, implemented with the
  launcher in #4415.
