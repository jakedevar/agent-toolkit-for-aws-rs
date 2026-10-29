# The bare `-` argument (Issue #4780)

argparse passes a lone `-` as an ordinary value, and a few Python originals use
it to mean standard input. Almost every slice turns a path argument straight into
a filesystem path, where an accepted `-` reads or overwrites a file literally
named `-`. The `toolkit` router therefore applies one policy, before any handler
and so before any filesystem access, to every registered value flag and
positional argument:

- The exact token `-` is a usage error (exit 2, `error: invalid arguments: <flag>
  does not accept - (...)`, one escaped line naming the flag, then the usage text)
  in both spellings (`--flag -` and `--flag=-`), for plain and repeatable flags,
  on `strict_values` and non-strict commands alike. A positional `-` is refused
  the same way. Nothing is read, written or created.
- `./-` stays a legal explicit literal path: `--output ./-`, `skills list ./-`.
- `-x`, `--` and other dash-prefixed words keep their previous behavior
  (unknown flags, or on `strict_values` commands refused values; a non-strict
  command still takes `-x` as a value).
- The policy lives in the router (`value_allowed` in
  `crates/toolkit-cli/src/lib.rs`), not in the slices, so a new slice cannot
  bypass it and core regular-file helpers are unchanged. Two explicit allowlists,
  `STDIO_DASH` and `LITERAL_DASH`, open exactly the audited pairs; a pair is in at
  most one of them.
- The contract test `every_value_flag_and_positional_dash_is_refused_or_in_exactly_one_list`
  (`crates/toolkit-cli/tests/diag_contract.rs`) enumerates every registered
  command's value flags and positionals from `COMMANDS` and fails if any of them
  routes a `-` that neither list names, if a listed pair is refused or in both
  lists, or if a `LITERAL_DASH` flag does not receive exactly `-`.

## `STDIO_DASH`: parameters that read standard input

`STDIO_DASH` ships **empty**. The Python originals of `cloudwatch instrumentation`
and `cloudwatch snapshots` read `--json -` and `--json-file -` from stdin
(`di_instrumentation.py:212,217`, `di_snapshots.py:239,244`), but the Rust port
deliberately does not (the accepted #4381 divergence recorded in
`docs/migration/cw-api.md`, change 3): a read to EOF has no finite wait on a TTY or
a held-open pipe, so stdin is not an argument source (#4935). The four parameters
are refused like every other path parameter; use `--json-file PATH` or `./-`.
No parameter writes standard output for `-`.

A slice that wants stdin support adds its pair here in the same change that makes
its handler read stdin deliberately: bounded, UTF-8 checked, a finite wait, escaped
single-line diagnostics, and no fallback to a file named `-`.

## `LITERAL_DASH`: non-path values where `-` is an ordinary string

Some value flags are selectors, ids, names or text. Their Python original takes an
arbitrary string with no `-` sentinel (argparse hands the lone `-` through as an
ordinary value, and the original selects, compares or sends it), and the Rust
handler uses the value only as a key, a request field or output text, never as a
path. For these the router passes `-` through unchanged in both spellings
(#4934). The reviewer's reproduction is `startup verify-model-path --workload -`:
the recommendation schema allows a workload named `-` (`verify_model_path.py:284`,
selected by exact key at `:246-257`); `./-` would be a different workload ID.

**Path parameters are never listed.** Every flag that names a file or directory
(`--output`, `--out`, `--json-file`, `--recorded`, `--price-list`, `--mappings`,
`--snapshot-file`, `--plugin-root`, ...) keeps the refusal, and so does every
positional. `diag_contract.rs` fails a `LITERAL_DASH` entry whose flag name has a
path word (a floor, not the proof), requires every entry to name a real value flag
of a real command, and runs each pair against a FIFO named `-` in the working
directory (`a_literal_dash_flag_never_reaches_the_filesystem`): any open of `./-`
would block, so a handler that treated the value as a path fails the test.

| Command | Flag | Python original | Rust consumer (never a path) |
| --- | --- | --- | --- |
| `startup verify-model-path` | `--workload` | `verify_model_path.py:284` | `model_path.rs:215` `selected_workloads`: key into the recommendation's `workloads` |
| `bedrock-agent fetch` | `--agent-id` | `fetch_bedrock_agent.py:233` | `bedrock_agent.rs:206`: `Params.agent_id`, a request field |
| `bedrock-agent fetch` | `--agent-version` | `fetch_bedrock_agent.py:235` | `bedrock_agent.rs:220`: request field |
| `bedrock-agent fetch` | `--agent-alias-id` | `fetch_bedrock_agent.py:240` | `bedrock_agent.rs:223`: manifest text |
| `bedrock-agent fetch` | `--region` | `fetch_bedrock_agent.py:243` | `bedrock_agent.rs:210` `region_of` (`resolve_region` validates it with its own error) |
| `bedrock-agent fetch` | `--profile` | `fetch_bedrock_agent.py:246` | accepted, not consumed (no live client in this build) |
| `bedrock-resolve identity` | `--provider` | `model_identity.py:51` | `bedrock_resolve/mod.rs:187` into `identity::same_model`: string comparison |
| `bedrock-resolve identity` | `--source` | `model_identity.py:52` | same |
| `bedrock-resolve identity` | `--target` | `model_identity.py:53` | same |
| `bedrock-resolve pricing` | `--region` | `bedrock_pricing.py:341` | `bedrock_resolve/mod.rs:350`: price lookup key |
| `bedrock-resolve pricing` | `--models` | `bedrock_pricing.py:342` | `bedrock_resolve/mod.rs:351`: split on `,`, each id a lookup key |
| `cloudwatch instrumentation` | `--region` | `di_instrumentation.py:223` | accepted, not consumed (recorded client only) |
| `cloudwatch instrumentation` | `--profile` | `di_instrumentation.py:229` | accepted, not consumed |
| `cloudwatch snapshots` | `--region` | `di_snapshots.py:255` | accepted, not consumed |
| `cloudwatch snapshots` | `--profile` | `di_snapshots.py:261` | accepted, not consumed |
| `cloudwatch evaluate-traces` | `--trace-id` | `evaluate_traces.py:930` | `cloudwatch/omni_cli.rs` `owned(...)`: request field |
| `cloudwatch evaluate-traces` | `--trace-ids` | `evaluate_traces.py:933` | same |
| `cloudwatch evaluate-traces` | `--session-id` | `evaluate_traces.py:934` | same |
| `cloudwatch evaluate-traces` | `--evaluator-id` | `evaluate_traces.py:938` | same |
| `cloudwatch evaluate-traces` | `--evaluator-ids` | `evaluate_traces.py:939` | same |
| `cloudwatch evaluate-traces` | `--region` | `evaluate_traces.py:944` | accepted, not consumed |
| `cloudwatch evaluate-traces` | `--kms-key-id` | `evaluate_traces.py:968` | `omni_cli.rs` `owned(...)`: request field |
| `cloudwatch capture-dataset` | `--trace-ids` | `capture_dataset_from_traces.py:310` | `omni_cli.rs`: request field |
| `cloudwatch capture-dataset` | `--dataset-name` | `capture_dataset_from_traces.py:312` | same |
| `cloudwatch capture-dataset` | `--dataset-id` | `capture_dataset_from_traces.py:313` | same |
| `cloudwatch capture-dataset` | `--description` | `capture_dataset_from_traces.py:314` | same |
| `cloudwatch capture-dataset` | `--region` | `capture_dataset_from_traces.py:315` | accepted, not consumed |
| `msk size` | `--region` | `msk_sizing.py:1363` | `msk.rs:326`: price-table lookup key |
| `ai-models search` | `--region` | `search_deployable_models.py:839` | `ai_models/mod.rs:471`: `check_region` validates it, then it is compared with a snapshot's region field (the snapshot path is `--snapshot-file`, not listed) |

### Re-audit of every entry for path composition (#4955)

The first audit followed each flag to the field or request it ends in. That is not
enough: a value can also become a path component (a file name, a suffix, a
directory, a `join`, a `format!` into a path). `--date-suffix` did exactly that:
`bedrock_report/render.rs` stores it in `reportDateSuffix` (`:125`), then
`find_report_path` (`:146-149`) builds `repo/MIGRATION_REPORT_<suffix>.md`, tests it
with `fs::symlink_metadata` (`:133-134`) and `copy_report` reads and copies it
(`:64-66`, `:187`, `:200-201`). `--date-suffix -` would therefore select
`MIGRATION_REPORT_-.md`. Every remaining entry was re-read to the end of its
dataflow for that, in the Rust handler and in every module it hands the value to
(the whole of `bedrock_agent/`, `bedrock_resolve/`, `cloudwatch/`, `toolkit-msk`,
`toolkit-aws`):

| Pair | Where the value ends | Path composition? |
| --- | --- | --- |
| `startup verify-model-path --workload` | `model_path.rs:215` `selected_workloads`, then `verify.rs:398-425`: compared with the recommendation's `workloads` object keys; the output path is `--output` or `VERIFICATION_FILENAME` next to the input, never the workload id | none |
| `bedrock-agent fetch --agent-id` / `--agent-version` / `--agent-alias-id` | `Params` (`bedrock_agent.rs:218-223`), request fields of `fetch.rs` (`agentId`, `agentVersion`, alias selection) and the manifest text; the only paths are `--out` and `--recorded` | none |
| `bedrock-agent fetch --region` / `--profile` | `region_of` -> `resolve_region` (validated name, `toolkit-aws` has no file access); `--profile` is never read | none |
| `bedrock-resolve identity --provider` / `--source` / `--target` | `bedrock_resolve/mod.rs:187-188` -> `identity::same_model`: string comparison; paths are `--mappings` only | none |
| `bedrock-resolve pricing --region` / `--models` | `mod.rs:350-351`, `pricing.rs` lookup keys (`words.join(" ")` is text); the only path is `--price-list` | none |
| `cloudwatch instrumentation` / `snapshots --region` / `--profile` | accepted, never read; `client_of` opens only `--recorded`, `snapshots` writes only `--out` | none |
| `cloudwatch evaluate-traces --trace-id` / `--trace-ids` / `--session-id` / `--evaluator-id` / `--evaluator-ids` / `--kms-key-id` / `--region` | `omni_cli.rs` `owned(...)` request fields of `evaluate.rs` (error text, `join(", ")` of ids); `omni_cli.rs` and `evaluate.rs` use no file API | none |
| `cloudwatch capture-dataset --trace-ids` / `--dataset-name` / `--dataset-id` / `--description` / `--region` | `omni_cli.rs` request fields of `dataset.rs` (service request and text); no file API | none |
| `msk size --region` | `msk.rs:325` `SizeOptions.region` -> `toolkit-msk` price-table lookup and report text (`size_report.rs:486-533`); `msk.rs`'s file calls belong to other commands and `size` opens no file (`--emit-static-pricing` is a boolean flag that prints) | none |
| `ai-models search --region` | `ai_models/mod.rs:471`: `check_region` (`is_valid_region`), then compared with a snapshot's region field (`snapshot.rs:174`); the snapshot path is `--snapshot-file` | none |

Result: `--date-suffix` is the only entry that composes a path, and it is removed.
No other entry was removed. The check is kept in the tests, not only here:
`a_literal_flag_in_a_filesystem_bearing_file_is_audited_per_flag`
(`tests/diag_contract.rs`) scans the non-test sources for every `LITERAL_DASH`
flag name that shares a file with filesystem code (`fs::`, `Path`, the
regular-file helpers) and fails for a `(file, flag)` that is not recorded in
`AUDITED_FILESYSTEM_CONSUMERS` (and for a stale row), so a new entry, or a flag
that moves into a file with filesystem code, forces a reading of the whole
dataflow. `the_filesystem_scan_catches_the_removed_date_suffix_flow` shows that the
scan finds `--date-suffix` in `bedrock_report/render.rs`. The path-word tripwire
also names `suffix`, `prefix`, `stem`, `extension`, `filename` and `basename`. The
FIFO canary alone cannot see a composed name (a FIFO called `./-` is not
`MIGRATION_REPORT_-.md`, and without its companions the handler returns before the
value is used), which is why `a_dash_report_date_suffix_is_refused_and_selects_no_report`
runs the complete offline command with a repository holding `MIGRATION_REPORT_-.md`.

Flag-name-only rows added at the round-16 integration (#4960). Three slices
registered after `f597a058` spell a `LITERAL_DASH` flag name in a file that holds
filesystem code. None of their commands has a `LITERAL_DASH` entry, so the router
refuses `-` for these flags (exit 2, `--F does not accept -`, no `./-` created).
The scan lists them only because it matches by flag name:

| File | Flag | Why it is listed |
| --- | --- | --- |
| `bedrock_probes/mod.rs` | `--models` | `bedrock-probes iam-policy` / `preflight` / `api-path` / `classify --models -` are refused by the router; the only entry is `bedrock-resolve pricing --models` |
| `bedrock_probes/mod.rs` | `--region` | `bedrock-probes iam-policy` / `preflight` / `classify --region -` are refused; the entries belong to other slices |
| `docgraph/mod.rs` | `--region` | `documentdb wa-review --region -` is refused; the entries belong to other slices |

Round-18 integration (#5006), `marketplace` (#4396). Its three commands (`handler`,
`query-metering`, `test-events`) are `strict_values`, not passthrough, and have no
`STDIO_DASH` or `LITERAL_DASH` entry, so the router refuses a bare `-` for every value
flag in both spellings and for the `handler NAME` positional, before any file is read
(`--event`, `--config` and `--recorded` are paths; `./-` names a file called `-`).
`query_metering.py` and `test_event_publisher.py` take their options as argparse
strings with no `-` sentinel, so `--customer-id -` or `--product-code -` were accepted
there; the slice already refuses every dash-prefixed option value (`strict_values`,
divergence 11 of `docs/migration/marketplace.md` for `query-metering`), and `-` is
not a valid region, product code, account, event source or bus name. Left refused; one flag-name-only row:

| File | Flag | Why it is listed |
| --- | --- | --- |
| `marketplace/mod.rs` | `--region` | `marketplace query-metering` / `test-events --region -` are refused by the router; the entries belong to other slices |

Round-19 integration (#5015), `elasticache-tunnel` (#4393). Its three commands
(`elasticache find-tunnel-host`, `start-tunnel`, `test-connection`) are `strict_values`,
not passthrough, and have no `STDIO_DASH` or `LITERAL_DASH` entry, so the router refuses
a bare `-` for every value flag in both spellings and for the `test-connection HOST`
positional, before the `--recorded` file is read (`./-` names a file called `-`);
`a_bare_dash_is_refused_for_every_value_flag_and_the_positional` pins it.
`find_tunnel_host.py`, `start_tunnel.py` and `test_connection.py` read no `-` sentinel
for any argument (divergence 3 of `docs/migration/elasticache-tunnel.md`), and `-` is
not a valid VPC id, instance id, host, port, region or profile. Left refused. No row is
added: the tunnel sources spell `--region`, `--profile` and `--target` (the SSM
session argument), but none of them holds filesystem code (the recording is read
through `elasticache::read_json_file`), so the scan above does not list them.

`telemetry` is a passthrough command that parses its own words, so the router policy
and the scans above never see its path flags. It declares them (`--telemetry-dir`,
`--plugin-root`) in `value_flags`, which the router ignores for a passthrough command,
and applies the policy itself (#4961):

| Entry point | `--telemetry-dir -` / `--telemetry-dir=-` and `--plugin-root -` / `=-` |
| --- | --- |
| interactive: `status`, `reconcile`, `consent show\|status\|accept\|opt-out`, `bundles`, and an unknown word | exit 2, the router's flag-naming message, a bare `-` word refused as a positional, no path read or written |
| hooks: `skill-invoked`, `migration`, `consent session-start` | **divergence**: the flag is treated as absent. Exit 0, output identical to the hook without the flag, nothing emitted, no file touched |

The hook contract ("hook subcommands never fail") is why the hooks do not refuse; their
fail-closed result is `pluginVersion: null` and no event. `./-` stays an explicit literal path.
The contract test `passthrough_commands_declare_their_value_flags_and_refuse_dash_unless_a_hook`
(`tests/diag_contract.rs`) covers every passthrough command except `asm exec` (which forwards
argv): each must declare value flags and have a probe row, and the real binary is spawned with
both spellings against a `./-` sentinel that must stay unchanged. `tests/telemetry.rs` covers each
subcommand with a `./-` canary directory.

### Divergence: `bedrock-report render --date-suffix -` is refused (#4955)

`render_report.py:113` takes `--date-suffix` as an arbitrary string and `:74-75`
builds the report name from it, so the Python original accepts `-` and would select
`MIGRATION_REPORT_-.md`. The Rust port refuses the bare `-` for this flag, in both
spellings, like every flag that is composed into a path (PM ruling): exit 2, the
flag-naming diagnostic, no report looked up, nothing copied. Every other suffix
behaves as before (`2026-02-02`, `-x`, `--date-suffix=-,`), and the fallback (the last
`MIGRATION_REPORT_*.md` in name order) is unchanged. Only a request to select a report
named exactly `MIGRATION_REPORT_-.md` by its suffix is affected; omit the flag to get
the name-order fallback.

### Audited and left refused

- **Validated in the Python original, so `-` is invalid there too:**
  `--mode`/`--level`/`--format`/`--pricing-source`/`--broker-classes`/`--skill`
  (argparse `choices` or a parser), every numeric flag (`type=int`/`float`, `int("-")`
  fails), `msk sim --instance-type` (`simulation_load_test_config.py:74` rejects an
  unknown type) and `--kafka-version` (rejected unless supported). The router's
  refusal and the original's rejection agree (exit 2).
- **No Python original takes the flag** (new surface of the Rust port, or a Rust-only
  command): `aurora`, `elasticache`, `relational` (`--tenant`, `--cluster`,
  `--instance`, `--engine`, `--serverless`, ...), `dynamodb` / `dynamodb-deploy`,
  `ai-data transform` (`--execution-role`, `--base-job-name`, ...),
  `bedrock-resolve source-model` (`--plan-model-id`, `--source-provider`) and `image`
  (`--emit`, `--detail`), `cloudwatch region`. There is no original that accepts `-`,
  so `-` stays refused; add a pair to `LITERAL_DASH` with its audit row if a
  legitimate `-` value ever appears.
- **Composed into a path:** `bedrock-report render --date-suffix` (#4955, see the
  divergence above).
- **Paths** (listed above) and **positionals** (`skills list DIR`, `msk compat
  CLUSTER_CONFIG`, `ai-data detect-format FILE_PATH`, ...): never literal.

The assumption behind the earlier all-flags ruling stands corrected: the claim that
no registered non-path flag has a Python original accepting `-` was false
(`--workload`, reported in #4934); the list above replaces it.

## Global help and usage bytes

The global usage text is printed for every command's `--help` and appended to every
usage error, so a slice's description edit would change the help bytes of every
other command (#4936). The #4780 change adds no help text: the CloudWatch
description keeps the base wording (`stdin is not accepted`), and
`the_global_usage_text_is_byte_identical_to_the_base` pins the length and the
FNV-1a hash of `USAGE`, next to `every_command_help_is_the_global_usage_text`.
At the base `f597a058` the pin was 35200 bytes, `0xa95b3df510f48191`. The round-16
integration (#4960) moves it to 39862 bytes, `0x586799b8d88de137`. The new text is
the base text plus the synopsis and description of bedrock-probes (#4404),
docgraph (#4395), dynamodb-bench (#4390) and telemetry (#4398). Deleting exactly
those lines gives back
the base length and hash. The round-18 integration (#5006) moves it to 41167 bytes,
`0x614d170cb18738c2`: the round-16 text plus the marketplace (#4396) synopsis (650
bytes, 9 lines) and description (655 bytes, 8 lines), 39862 + 650 + 655 = 41167.
marketplace has empty exit notes, and the merge changes no other slice's help text,
`USAGE_HEAD` or the assembly in `registry.rs`. The round-19 integration (#5015) moves
it to 42243 bytes, `0x874fc84f07b07a26`: the round-18 text plus the elasticache-tunnel
(#4393) synopsis (512 bytes, 8 lines) and description (564 bytes, 8 lines),
41167 + 512 + 564 = 42243. The tunnel has empty exit notes, and the merge changes no
other slice's help text, `USAGE_HEAD` or the assembly in `registry.rs`. Round 22
(#5147) adds mcp-proxy and pins 43490 bytes, `0xe8bae3964772eac2`. Round 23
(#5194) also adds the ai-payloads synopsis and description (#5143, S1 of #4386);
the merged binary's usage is 44013 bytes, `0x169fada3871a54b6`. ai-payloads has
empty exit notes. The `ai-payloads templates --root` path flag is a
directory and has no `LITERAL_DASH` or `STDIO_DASH` entry: `-` and `--root=-`
are refused with exit 2 by the router (`ap_cli_03_exit_codes_and_usage_errors`).

## The four reproductions

| Reproduction | Before | Now |
| --- | --- | --- |
| `elasticache dashboard --serverless demo --output=-` | truncated `./-` | exit 2, `./-` untouched or not created |
| `startup terraform-policy terraform --json -` | overwrote `./-` | exit 2 |
| `cloudwatch validate instrumentation-type --json-file -` | read `./-` | exit 2 (refused earlier by `strict_values` since #4743) |
| `skills list -` | enumerated `./-/...` | exit 2 (positional `-` refused since #4743) |

`tests/diag_contract.rs` (`the_4780_reproductions_refuse_dash_and_leave_a_file_named_dash_alone`,
`an_explicit_dot_slash_dash_is_still_a_literal_path`) runs the real binary with
complete valid companion arguments, with `./-` present (preserved byte for byte)
and absent (not created), in both spellings, and checks that `./-` still works as
an explicit literal. `tests/cloudwatch_api.rs`
(`a_bare_dash_argument_is_refused_and_stdin_is_never_read`) checks that
`cloudwatch instrumentation` and `snapshots` refuse `--json -` / `--json-file -`
and never read stdin.

## `mcp-proxy` (#5079)

`toolkit mcp-proxy` (#4413 S3) lists nothing in either list. Its positional is
the endpoint URL, and every value flag (`--service`, `--region`, `--profile`,
`--metadata`, `--header`, `--retries`, the five timeouts, `--log-level`) is a
name, number or `KEY=VALUE` pair that never names a file, so the exact value
`-` is refused (exit 2) by the router before the handler runs. The command
reads only its own stdin stream (the MCP client) and writes only its own
stdout; neither is selected by an argument.

The `launch auth-start` and `launch auth-wait` `--region` value is passed to
`resolve_region` and validated against the supported region names; it is never
used to construct a filesystem path. The router refuses a bare `-` for both
commands (#5216).

The reward checker (#5214) adds `--format`, `--event`, and `--response` value
flags. The router refuses bare `-` and `--flag=-` for each; no stdin or literal
file named `-` is read. Its two synopsis lines change global usage to 44155
bytes, FNV-1a `0xea11e5a1e031b87f` before the launch routes were added. With
the launch routes and reward checker together, usage is 44502 bytes, FNV-1a
`0x95a0edb8063410e1`.

The launch snapshot routes (#5317) add `launch download-snapshot LAUNCH_ID
[--output DIR] [--region REGION]` and `launch apply-snapshot SNAPSHOT_DIR
[--into DIR]`. `--region` goes only to `resolve_region` (a supported-region
name, never a path). `--output`, `--into` and the snapshot directory are paths;
the router refuses a bare `-` and `--flag=-` for each value flag, and the
positionals refuse a bare `-`, so `./-` is the way to name a literal
directory `-`. Neither command reads stdin. Global usage is 45014 bytes, FNV-1a
`0xcf2cb8463038df2b`.
