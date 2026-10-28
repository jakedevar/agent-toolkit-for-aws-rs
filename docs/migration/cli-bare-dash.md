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
| `bedrock-report render` | `--date-suffix` | `render_report.py:113` | `bedrock_report/render.rs:50,125`: a JSON string field of the payload |
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
FNV-1a hash of `USAGE` to the base `f597a058`, next to
`every_command_help_is_the_global_usage_text`.

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
