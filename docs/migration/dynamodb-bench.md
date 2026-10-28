# Slice `dynamodb-bench` (Issue #4390): DynamoDB benchmark orchestration and performance reporting

Rust replacement for three legacy scripts under
`skills/specialized-skills/database-skills/amazon-dynamodb/scripts/` (read-only
migration evidence; none is edited or deleted here, their callers and
retirement belong to #4415):

| Python source | Rust | Code |
| --- | --- | --- |
| `benchmark_model.py` | `toolkit dynamodb benchmark-model` | `dynamodb_bench/` (`mod.rs`, `run.rs`, `recorded.rs`, `response.rs`, `shape.rs`), `dynamodb/bench.rs`, `dynamodb/bench_rows.rs` |
| `generate_perf_report.py` | `toolkit dynamodb generate-perf-report` | `dynamodb/perf_report.rs`, `dynamodb/perf_render.rs` |
| `benchmark_lambda.py` | **not ported, classified below** (remote workload) | – |
| (shared) | Python-compatible number/JSON formatting, diagnostics rule | `dynamodb/bench_fmt.rs` |

The calculator behind the "expected" numbers and the spend estimate is the
`dynamo-cost` slice (#4389, `dynamodb/cost.rs`); nothing is duplicated.

## Remote benchmark workload classification (AC2)

| Item | Where it runs | Classification | Decision |
| --- | --- | --- | --- |
| Orchestrator `benchmark_model.py` | the user's machine | **Local orchestration, ported.** Budget, split, guardrail, aggregation and the summary are Rust. It issues no DynamoDB data-path call itself. | `benchmark-model` |
| Report generator `generate_perf_report.py` | the user's machine | **Local, offline, ported.** | `generate-perf-report` |
| Handler `benchmark_lambda.py` | **inside the user's AWS account**, in the benchmark Lambda deployed by `deploy-model` | **Remote managed-service Python payload.** It is the load generator: every DynamoDB data-path call (seed, warmup, measurement at up to ~1800 RPS/pattern over 32 threads) happens there so the latency reflects the region, not the user's network. It is uploaded as a zip and run by the managed `python3.12` Lambda runtime. | **Stays a Python payload for now.** It is not executed, packaged, shimmed or imported by any Rust code in this build. Its Rust replacement requires a language/runtime decision (a Rust `provided.al2023` Lambda, built by CI, or a container image) that the manager owns; filed as a follow-up (below). `benchmark_lambda.py` is **not retired** by this slice. |
| Load generation | AWS | **Never from this build.** The build has no AWS client, no network and no subprocess. The only AWS call of the orchestrator (`lambda:Invoke`) goes through `LambdaClient`; the two implementations are `RecordedClient` (replay) and `NoLiveClient` (every call fails with a connect error). Any real run is a live-binding follow-up that must be executed as a daemon job under a spend grant. | – |

What the orchestrator needs from the handler is its **response contract**
(rows, exact tallies, seed verification, coverage, taint map). That contract is
what `response.rs` validates and what the recordings replay, so the Rust side
is complete against any implementation of the handler, Python or Rust.

## CLI contract

```
toolkit dynamodb benchmark-model --model FILE --config FILE --manifest FILE
    --raw-out FILE --summary-out FILE [--allow-spend] [--recorded FILE] [--now TS]
toolkit dynamodb generate-perf-report --model FILE --summary FILE --output FILE
    [--findings-out FILE]
```

The command names and flags are the ones `iterate-design` (#4389) already
builds for its pipeline steps. Rows in the command table carry
`repeatable_flags: &[]` and `passthrough: false`; `strict_values` is on, and
the router's `STDIO_DASH` list is untouched, so a bare `-` is refused as a
value (no path is ever the stream `-`).

| Flag | Python | Rust |
| --- | --- | --- |
| `--model`, `--config`, `--manifest`, `--raw-out`, `--summary-out`, `--allow-spend` | as in the script | unchanged |
| `--recorded FILE` | – | replays recorded `lambda:Invoke` calls (below). Without it the invocation fails with `cannot reach AWS endpoint: ... this build has no live AWS client` after every offline step ran |
| `--now TS` | – | pins `run_started_at` / `benchmark_completed_at` (`YYYY-MM-DDTHH:MM:SSZ`) and the wall time (`0.0`) so fixtures are byte-stable |
| `--model`, `--summary`, `--output`, `--findings-out` (report) | as in the script | unchanged; `--findings-out` defaults to `design_findings.json` |

Exit statuses: `0` success; `1` when the first invocation reported a seed
failure or a handler error (outputs are still written, like the script's
`sys.exit(1)`); `2` for every refusal the script reported through `_die`, for
usage errors and for every rejected input; `3` when the estimated spend exceeds
the guardrail without `--allow-spend`. Diagnostics start `error: ` instead of
`ERROR: `.

## Recorded Lambda invocations (`--recorded`)

```json
{"calls": [
  {"service": "lambda", "operation": "Invoke",
   "request": {"FunctionName": "ddb-skill-bench-abc-bench", "Payload": {"invocation_index": 0}},
   "response": {"StatusCode": 200, "Payload": {"phases_run": ["measure"], "raw_rows": []}}},
  {"service": "lambda", "operation": "Invoke",
   "error": {"kind": "client", "code": "ResourceNotFoundException", "message": "no function"}}
]}
```

Format and rules follow `toolkit_aws::recorded` (#4727): one step per
invocation, replayed in order; absent optional member → default, present
`null` or wrong type → the recording is rejected naming the member, never a
value; the expected `request` of response **and** `error` steps is compared
before the outcome is replayed (`KeyOrder::Contained`: every recorded member
must equal the request made, which may hold more — the payload carries the
whole design); a call with no step left fails; steps the run never used fail
(`the recorded document has N step(s) this run never used`), except after an
early abort. `Payload` is the handler's return value as an object, or as a
string (the raw body, which exercises the non-JSON refusal). `FunctionError`
is a string. The response of the handler is then validated member by member
(`response.rs`): `raw_rows[]` rows (the nullable numbers a row may hold are
`latency_ms`, `consumed_cu`, `vector_search_bytes` and the `gsi_cu` /
`vector_write_bytes` maps), `exact_counts[]`, `seed_verification`,
`measurement_tainted`, `coverage.measured_patterns`, the echoed `mode`,
`key_sampling`, `items_per_partition`, `seed_verification_failed` and
`handler_error`. `tests/recorded_contract.rs` passes for this slice with no
allowlist entry; the two container reads are the marked functions of
`shape.rs`.

## Behavior kept (parity)

- Mode presets `quick` / `representative` (explicit config values win), the
  mode banners, the unknown-mode refusal.
- `_validate` (Mechanics #2 and the structural-reference checks, SearchVectors
  against vector indexes, Query/Scan against GSIs), in the script's order.
- `_compute_split`, including the seed-time estimate, the `0.9` usable
  fraction, `-1` for an impossible budget and Python float floor division.
- `_attach_vector_index_meta`, the per-invocation payload (`phase_plan`,
  `invocation_index`, `invocations_total`, `patterns`, `tables`, `manifest`,
  `config`) and `_bench_rps`.
- `_estimate_bench_spend` with the calculator's per-op CU (`pattern_monthly_cost`),
  the vector-bytes terms, the per-table maximum seed size and the guardrail
  (`cost_guardrail_usd`, default 0.50) with exit status 3.
- `_percentile` (`statistics.quantiles(n=100, method="inclusive")`) and
  `_aggregate`: exact tallies replace row counts; non-throttle errors only;
  `key_distribution` only for zipf sampling; vector blocks only for designs
  with vector indexes; cold-start flag at 2× the steady p99.
- The summary layout and member order (`json.dumps(indent=2)`, ASCII), the raw
  JSONL (`json.dumps` per row), the freshness markers, the coverage warning.
- Report: every section, table, caveat and the `design_findings.json` payload
  (`signals`, finding ids, axioms, severities, `no_significant_findings`), the
  hot-partition / starvation split, driver-saturation `†`, vector capacity
  section, `{:.2g}` rate rendering (so `1500` prints `1.5e+03`, as in Python).
- Sums use CPython ≥ 3.12 `sum()` semantics (Neumaier compensation).

## Intentional changes

1. **No live AWS client.** See the classification. `lambda:Invoke` is replayed
   from `--recorded`; the live binding (SDK client, 900 s read timeout with
   retries off, profile/region session) is a follow-up. `aws_profile` and
   `region` must still be non-empty strings in the config (the script read them
   for the session) but are never echoed or used.
2. **Diagnostics escape text.** Pattern ids, table and index names, config and
   Lambda text go through `bench_fmt::diag`: `toolkit_aws::redact_text` first,
   then truncation (120 characters; 200 for a non-JSON body, 500 for a function
   error), then `toolkit_core` escaping to one quoted line. Quoted values print
   with double quotes where the script printed Python `repr` (`'name'`), and
   lists as `["a", "b"]`. `tests/diag_contract.rs` passes for both commands.
3. **Report text is neutralized.** Names taken from the design or the summary
   (patterns, tables, indexes, error codes, manifest values) pass through
   `neutralize_line` before they enter the Markdown, so a newline or terminal
   control in a name cannot forge a report line. Ordinary text is unchanged.
4. **Wrong-typed input is diagnosed.** Where the scripts crashed with a
   traceback (`KeyError`, `AttributeError`, `TypeError`) the commands refuse
   with a message (exit 2). Python `or` semantics are kept for the design and
   the config (`null`, `0`, `""`, `[]`, `{}` mean "not set"). Patterns must have
   a string `pattern_id` and `operation`.
5. **Response validation.** The Lambda response is validated (see above);
   `null` for a container member the Lambda never emits as `null`
   (`raw_rows`, `exact_counts`, `coverage`, `seed_verification`,
   `measurement_tainted`) is rejected, where the script's `or []` accepted it.
   `Payload` of the `Invoke` response is the parsed document (or a raw string).
6. **Function name checked.** `manifest.lambda.function_name` must be a name or
   ARN of letters, digits, `-`, `_`, `:` (at most 170 characters).
7. **Outputs are published atomically** (review #4954). Each of `--raw-out`,
   `--summary-out`, `--output` and `--findings-out` goes through the private
   writer of `dynamodb-deploy` (`write_private`): a same-directory temporary
   file created with `create_new` and mode 0600, written, flushed and
   `sync_all`-ed, then `rename`d over the destination. A reader sees the old or
   the new complete file; a failed write removes its own temporary file and
   leaves an existing output untouched. A symbolic link or a non-regular file at
   an output path is refused, never followed (the script followed links), and a
   rename replaces a link swapped in later instead of writing through it. The
   files are now owner-only (0600) where the script wrote with the umask.
8. **Calculator failures** (`pattern_monthly_cost` errors) abort the report with
   `the calculator refused the design: ...` (the script crashed); in the spend
   estimate they keep the script's fallback of 1.0 CU per call.
9. **Integer-valued latencies.** Latency and capacity are floats in the summary.
   A hand-fed row that carries an integer latency is written back as a float
   (the Lambda always emits floats).
10. **Floating point.** `sum()` follows CPython 3.12 (compensated); an older
    interpreter can differ in the last bit of a mean.
11. **`--now`, `--recorded`** are new (fixtures); the wall-clock banner and the
    foreground reminder are printed as before.
12. **Mode banners escape caller text.** `quick` and `representative` print
    the config values they show before the config is validated. A number prints
    as before; every other value (a string, list or object) goes through
    `bench_fmt::diag` (redact, bound to 120 characters, escape), so a banner is
    one safe line. The script printed the value as Python `str()`.
13. **Seed verification fails closed** (review #4954). Each table of the
    response's `seed_verification` must carry `passed` (a boolean); `sampled`
    (boolean), `expected` and `actual` (integers) and `seed_shortfall_ratio` (a
    number) are checked when present, and a present `null` or other type is
    refused with a message that names the member only. The script read `passed`
    by truthiness and defaulted a missing `passed` to true, so the string
    `"false"` or a missing member read as `ok`. Only those five members are
    kept in `perf_summary.json`. `generate-perf-report` applies the same rule to
    a hand-fed summary and renders only an explicit `true` as `ok`.

## Security surface

No network, no credential, no environment variable, no subprocess, no
generated code. Inputs are regular files read through the shared path contract
(size-bounded, links refused). The recorded document is data; it cannot make the
command do more than the replay. Caller and recorded text reaches the terminal
only through redaction, truncation and escaping. The guardrail refusal fires
before the first invocation, so no recorded or live call happens past the
configured spend without `--allow-spend`.

## Tests

- Unit: `dynamodb/bench_fmt.rs`, `bench_rows.rs`, `perf_report.rs` and
  `dynamodb_bench/tests.rs` (split arithmetic, validation refusals, spend
  estimate, preset overlay, response validation, recorded client rules, report
  sections with captured summaries).
- Process (`crates/toolkit-cli/tests/dynamodb_bench.rs`): end-to-end runs of
  both commands against recordings captured from the handler contract, the
  guardrail, the abort paths, the report goldens, output safety.

## Follow-ups

- Live binding of `lambda:Invoke` and the handler's language/runtime decision
  (see the Issue filed with this slice's handoff). Load generation happens only
  as a daemon job under a spend grant.
- `iterate-design` still carries its own copy of the mode presets (#4389 noted
  this); it can read `dynamodb::bench::QUICK_PRESET` / `REPRESENTATIVE_PRESET`.
- Retirement of the Python scripts and SKILL.md wiring: #4415.
