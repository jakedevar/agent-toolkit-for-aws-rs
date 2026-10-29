# security-hooks (#4412): pre-review security note

Status: written and committed **before** the Rust implementation. Evidence is the
legacy source at base `98957f87` (`secret-safety.py`, 172 lines, read only) and the
audit `docs/migration/inventory-audit.md` at `71c7d42` (F-10, U-7, U-10). No Python was
executed; every statement about legacy behaviour below comes from reading the source.

## 1. Slice membership (audit U-10)

| Item | Belongs | Evidence |
| --- | --- | --- |
| `plugins/aws-core/com.anthropic.claude-code/hooks/secret-safety.py` | yes | the only `docs/migration/python-inventory.json` row with `.slice == "security-hooks"` (1 of 391 rows); named in the #4412 scope |
| `plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json` | yes | named in the #4412 scope and in audit §8.4; launches the Python file twice (matchers `Bash` and `use_aws\|mcp__aws.*\|mcp__plugin_.*aws-mcp.*`, timeout 5) |
| `plugins/aws-startup-advisor/**/hooks.json` | no | telemetry launchers belong to #4398 (audit F-11) |
| any other file referencing `secret-safety` | none | `git grep secret-safety` finds only `hooks.json`, `issues.json`, `python-inventory.json` |

There is no Python test for the hook and no bundled copy of it.

## 2. What the hook protects

A PreToolUse guard that stops an agent reading Secrets Manager values directly, so the
value never enters model context. The supported route is the
`{{resolve:secretsmanager:...}}` reference used with `asm-exec`. The guard is the control;
it is advisory only in the sense that it inspects text, it cannot see through shell
indirection (see section 6).

Protocol (unchanged): JSON on stdin (`tool_name`, `tool_input`). Allow = no output, exit 0.
Deny = exit 0 and one JSON object on stdout,
`{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": <DENY_MSG>}}`
with Python `json.dump` spacing. The legacy hook has no other exit code on purpose; an
uncaught exception exits 1 with no deny.

## 3. Detection rules to preserve

| ID | Rule | Applies to |
| --- | --- | --- |
| R1 | structured: service (`service_name`/`service`/`serviceName`) equals `secretsmanager` (lower-cased) and operation (`operation_name`/`operation`/`operationName`) normalized (lower-case, `-` and `_` removed) is `getsecretvalue` or `batchgetsecretvalue` | `use_aws`, any `mcp__*` |
| R2 | SDK call shape anywhere in `json.dumps(tool_input)` **and** the text `secretsmanager` anywhere (case-insensitive) | `use_aws`, any `mcp__*` |
| R3 | AWS CLI shape `aws [flags] secretsmanager [flags] [batch-]get-secret-value\b` (case-insensitive, up to 12 flag groups per gap, each flag `-X` with optional non-dash value) in `json.dumps(tool_input)` | `use_aws`, any `mcp__*` |
| R4 | SDK call shape in any top-level string value of `tool_input` (no `secretsmanager` needed) | tool name containing `run_script` (after the MCP check) |
| R5 | AWS CLI shape (R3) in the command | `Bash` |
| R6 | Secrets Manager agent URL `(localhost\|127.0.0.1\|0.0.0.0\|[::1]\|::1):2773/secretsmanager/get` (case-sensitive) in the command | `Bash` |
| R7 | exemption: leading token (after `NAME=value`, `sudo env time nice nohup command builtin` prefixes and a path prefix) in the read-only list and no `; & \| \`` or `$(` in the command: allow, skipping R8 and R9 only (R5 and R6 run first) | `Bash` |
| R8 | inline interpreter `(python[23]?\|python3.N\|node\|ruby\|perl) -...c \| -e ` together with an SDK call shape | `Bash` |
| R9 | SDK call shape and the text `secretsmanager` anywhere in the command (heredoc or pipe to an interpreter) | `Bash` |

SDK call shape: `.[batch(-|_)]get(-|_)secret(-|_)value (` (a dot and optional space before
it) or `[Batch]GetSecretValue[Request|Command] (`, case-insensitive, optional whitespace
before `(`.

## 4. Failure behaviour: legacy and Rust

| Situation | Legacy Python | Rust (this slice) |
| --- | --- | --- |
| interpreter/binary missing | launcher ends `py -3 "$0"`; exit 127 and no deny (F-10; host handling of 127 unverified, U-7) | **fail closed**: launcher prints the deny JSON (section 5) |
| hook crashes, is killed by signal or exits non-zero | exit 1 or signal, no deny | launcher prints deny JSON |
| stdin empty, not UTF-8, not JSON, not an object | exception, exit 1, no deny | deny |
| `tool_name` present but not a string; `tool_input` not an object for a guarded tool; `command` present but not a string | exception, no deny | deny |
| input larger than the cap (4 MiB) | no cap | deny |
| evaluation exceeds its budget (3 s of the 5 s hook timeout) | no budget; a catastrophic regex would hang past the 5 s host timeout (the source comment records this fail-open risk) | deny |
| panic inside the evaluator | n/a | deny |

Rust matchers are hand written linear-time scanners (no backtracking engine, no regex
dependency), so processing time is `O(input)` plus the 12-group cap, bounded further by the
size cap and a hard deadline thread.

## 5. Launcher contract

`hooks.json` keeps both matchers and `timeout: 5`. The command runs the Rust binary
`${CLAUDE_PLUGIN_ROOT}/com.anthropic.claude-code/hooks/secret-safety` and captures its
output. Exit 0: forward stdout (allow = empty). Any other result (missing file, not
executable, non-zero exit, signal, unset `CLAUDE_PLUGIN_ROOT`): print the deny JSON with a
reason naming the unavailable guard, exit 0. Nothing in the launcher invokes
`python3`, `python` or `py`; `secret-safety.py` therefore becomes **dead** once this lands
and stays in place only as read-only evidence until retirement (#4415).

Consequence to record: until a platform binary is staged at that path, every guarded
call (all `Bash` calls and the AWS tool calls) is denied. That is the manager-ruled
fail-closed behaviour; binary build and staging is a release dependency (see handoff).

## 6. Changes from legacy (all stricter, recorded)

1. Malformed or unexpected input denies (section 4) instead of crashing open.
2. `\n`, `\r`, `<(` and `>(` count as compound operators, so
   `cat f` + newline + `python3 -c '...get_secret_value(...)'` no longer rides the R7
   exemption.
3. R2/R3 also scan every string value and key of `tool_input` as raw text (legacy only
   scanned the ASCII-escaped serialization, in which a newline or non-ASCII space between
   the dot and the method name hid the call), and an array of strings is also scanned
   joined by spaces (an argv-style `["aws","secretsmanager","get-secret-value"]` is not
   visible in the legacy serialization).
4. R1 checks every service/operation key combination, not only the first truthy one; a
   non-string service/operation value is ignored for R1 (legacy crashed) while R2/R3 still
   run.
5. R4 scans nested string values, not only top-level ones.
6. R8 interpreter names accept any `pythonN.M` (legacy `python3.N` only), so
   `python2.7 -c` is covered.
7. `\b` after `get-secret-value` treats only ASCII letters, digits and `_` as word
   characters, so a following non-ASCII letter also denies.
8. Case-insensitive matching folds U+017F, U+0130, U+0131 and U+212A (long s, dotted and dotless i, Kelvin sign) like Python's
   `re.I`.
9. R8 accepts a flag cluster whose last letter is `c` **or `e`** (legacy `-[a-zA-Z]*c`
   or exactly `-e`), so `perl -ne`, `node -pe` and `ruby -ne` are inline execution too.
   Found while hand-checking fixtures after this note was first committed; it is
   stricter, so it is recorded here rather than in the original note.

Nothing is looser. The deny message, protocol bytes and exit codes are unchanged.

## 7. Residual risks (pre-existing, not widened)

- Shell indirection is not interpreted: quoting (`get-secret-"value"`), variables
  (`$OP`), `eval`, base64, `xargs`, or an argv assembled by a script are not detected by
  R3/R5. Only literal command and call shapes are. Follow-up filed for a hardening design
  rather than silently extending this port.
- R7 allows read-only tools that can execute code (`find -exec`, `awk system()`, `git`
  aliases) when no compound operator is present; R5/R6 still run before it.
- `Bash` only: other shell tools are outside the registered matchers.
- Hosts that ignore a hook's stdout deny cannot be defended; the launcher and the binary
  both use the documented deny protocol.
- No defence against a process that replaces the binary at the plugin path; filesystem
  trust of the plugin directory is assumed.

## 8. Test plan (deterministic, offline, synthetic data only)

Unit tests per rule R1-R9 with at least one positive and one negative each, adversarial
flag/case/whitespace/compound/structured fixtures, malformed-input denies, exact protocol
bytes, deadline (slow work denies, panicking work denies), oversize deny, linear-time
inputs. Integration tests run the real binary over stdin, and run the `hooks.json` command
with `sh` for: binary missing, binary exits non-zero, binary killed by signal, unset plugin
root (all deny), empty output (allow), pass-through of the binary's deny, and the real
binary end to end. No cargo runs in the authoring turn; verification is by daemon recipe
jobs on the exact commit.

## 9. Implementation record

Crate `crates/secret-safety-hook` (library `secret_safety_hook`, binary `secret-safety`;
the only dependency is `toolkit-core` for its strict JSON parser). No regex crate: the
patterns are hand written scanners in `src/matchers.rs`.

### 9.1 Rule parity

| Rule | Rust function | Tests (positive and negative) |
| --- | --- | --- |
| R1 structured | `policy::is_structured_secret_fetch` | `r1_structured_operation_spellings_are_denied`, `r1_other_operations_and_services_are_allowed`, `r1_checks_every_spelling_and_ignores_non_strings` |
| R2 SDK call and mention | `policy::check_structured_tool`, `matchers::matches_sdk_call`, `contains_folded` | `r2_sdk_call_with_secretsmanager_is_denied`, `r2_needs_both_the_call_and_the_mention`, `sdk_shape_*` |
| R3 CLI in structured tools | `matchers::matches_cli_get_secret_value` | `r3_*`, `cli_shape_*`, `cli_flag_groups_are_capped_at_twelve`, `cli_value_token_is_tried_both_ways` |
| R4 `run_script` | `policy::check_run_script` | `r4_*`, `unguarded_tools_are_never_inspected` |
| R5 Bash CLI | `policy::check_bash` | `r5_*` |
| R6 agent URL | `matchers::matches_agent_url` | `r6_secrets_manager_agent_urls_are_denied`, `agent_url_hosts` |
| R7 read-only exemption | `matchers::is_read_only_command`, `has_compound_operator` | `r7_*`, `read_only_prefixes`, `compound_operators` |
| R8 inline interpreter | `matchers::has_inline_interpreter` | `r8_*`, `inline_interpreters` |
| R9 heredoc/pipe | `policy::check_bash` | `r9_*` |
| protocol | `Decision::render`, `main` | `secret_fetch_deny_is_byte_identical_to_the_legacy_hook`, `tests/protocol.rs` |
| fail closed (input, size, time) | `policy::evaluate`, `runtime::decide_with_deadline` | `malformed_input_is_denied`, `oversized_*`, `slow_work_is_denied_at_the_deadline`, `panicking_work_is_denied`, `a_stdin_that_never_closes_is_denied`, `very_deep_nesting_*` |
| fail closed (launcher) | `hooks.json` command | `tests/launcher.rs`: `missing_binary_denies`, `unset_plugin_root_denies`, `failing_binary_denies`, `a_directory_at_the_binary_path_denies`, `real_binary_through_the_launcher_allows_and_denies` |

### 9.2 Verification limits

No Python was executed, so there is no differential run against `secret-safety.py`;
expected values in the fixtures were derived by hand from the legacy regexes and that
derivation is what independent review should re-check. The repository-wide verification
gate (`scoped_test`) is blocked on #4425; the recipe jobs (fmt, clippy, workspace-test) on
the exact commit are diagnostic evidence only.

### 9.3 Python file status

`hooks.json` no longer launches `secret-safety.py`, so the Python file is dead. It stays
in place as read-only evidence until retirement (#4415).

### 9.4 Release dependency

The binary is not committed. Until a platform build is staged at
`com.anthropic.claude-code/hooks/secret-safety` in the shipped plugin, every guarded call
is denied by the launcher (the required fail-closed behaviour). Cross-platform build and
staging of this binary needs an owner before this lands in a release.
