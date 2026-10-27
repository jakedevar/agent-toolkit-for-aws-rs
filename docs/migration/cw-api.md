# Slice `cw-api` (Issue #4381): CloudWatch clients, CRUD, instrumentation and trace evaluation

Rust port of the operation layer of the CloudWatch skill
(`skills/core-skills/aws-observability/scripts/cloudwatch/` and
`.../cloudwatch-omni/`, plus the plugin mirrors under `plugins/aws-core/`). The
Python files are read-only migration evidence: none is edited or deleted here,
so the mirrors stay byte-identical to the canonical copies and
`toolkit sync plugin-skills --check` is unaffected. Retirement waits for the
cutover and retire slices (#4414, #4415); the callers they must update are
listed below.

## Existing coverage check (base `a1db6c61`)

`git grep` / `git log -S` for `di_instrumentation`, `di_snapshots`,
`di_gateway`, `di_crud_tools`, `di_status_tools`, `di_snapshot_tools`,
`evaluate_traces` and `capture_dataset_from_traces` over `crates/` found only
the #4380 domain port (`crates/toolkit-cli/src/cloudwatch*`,
`docs/migration/cw-domain.md`), which covers the 13 pure `di_*` modules and
explicitly leaves the ten API files and both Omni helpers to this slice. Reused
unchanged: the origin table (`Origin`, `ArgSpec`, `Args`) and parser of
`cloudwatch/cli.rs`, `pytext` (`safe_text`, `safe_block`, `dumps_indent2`), the
validators, location and capture builders, the renderers, `errors::render_failure`
and `status_assessment::assess`. Ported now (missing before):

| Python | Rust (`crates/toolkit-cli/src/cloudwatch/`) |
| --- | --- |
| `di_app_signals_client.py`, `di_logs_client.py`, `di_session.py` | `aws_client.rs`: `Service`, the `AwsCalls` client trait, `NoLiveClient`, SDK-error mapping onto `AwsFailure` |
| `di_gateway.py` | `gateway.rs`: `AppSignalsOp` (the allowlist as a closed enum), `Gateway`, `render_error` |
| `di_snapshot_queries.py` + the `di_logs_client` allowlist | `logs_query.rs`: `LogsOp`, `Pacer`/`VirtualPacer`, `execute_cloudwatch_query` |
| `di_crud_tools.py` | `crud_tools.rs` |
| `di_status_tools.py` | `status_tools.rs` |
| `di_snapshot_tools.py` | `snapshot_tools.rs` (plus `di_snapshots._is_failure`) |
| `di_instrumentation.py`, `di_snapshots.py` | `ops_cli.rs`: `toolkit cloudwatch instrumentation OP` and `snapshots OP`, `--print-contract`, `--out` |
| `evaluate_traces.py` | `evaluate.rs` and `omni_sql.rs`; `omni_cli.rs`: `toolkit cloudwatch evaluate-traces` |
| `capture_dataset_from_traces.py` | `dataset.rs`; `omni_cli.rs`: `toolkit cloudwatch capture-dataset` |
| `test_evaluate_traces.py` | `tests_omni.rs` (same scenarios, plus writeback, privacy and recorded-input rules) |
| (replay) | `recorded.rs`: the recorded-input client |

Tests: `tests_api.rs` and `tests_omni.rs` (unit, scripted clients) and
`crates/toolkit-cli/tests/cloudwatch_api.rs` (real process spawns).

## Client design

Every `boto3` call is one `Service` and PascalCase operation with the request
the script built as keyword arguments, handed to an `AwsCalls` implementation.
This build wires exactly two: `RecordedClient` (`--recorded FILE`) and
`NoLiveClient`, which sends nothing, reads no credentials and fails every call
with `this build includes no live AWS client; pass --recorded FILE ...`. Tests
use scripted clients; none reaches the network. `--region` and `--profile` are
accepted for compatibility and ignored (`AWS_PROFILE` and the credential chain
are never read); the region command of #4380 is unchanged.

SDK errors are `toolkit_aws::AwsError` and map onto `AwsFailure` (`failure_of`):
a service error becomes the `ClientError` class with the botocore
`str(exc)` text (`An error occurred (Code) when calling the Op operation:
message`), `Connect` the endpoint error, `Timeout`, `NoCredentials` /
`PartialCredentials` the credential classes, anything else `Other`. The
omni pagination uses `toolkit_aws::{collect_pages, next_token_from}` (a
repeated token or a runaway walk is an error); the row cap is the scripts'
`SPAN_ROW_LIMIT + 1` probe.

## Recorded input (`--recorded FILE`)

```json
{"calls": [
  {"service": "application-signals", "operation": "ListInstrumentationConfigurations",
   "request": {"Service": "svc"}, "response": {"Configurations": []}},
  {"service": "logs", "operation": "GetQueryResults",
   "error": {"kind": "client", "code": "ThrottlingException", "message": "slow down"}}
]}
```

`service` is `application-signals`, `logs`, `cloudwatchomni`,
`bedrock-agentcore` or `bedrock-agentcore-control`; `operation` is the API
operation name (`StartQuery`, `Evaluate`, ...). Exactly one of `response` and
`error` (both objects) is required; `error.kind` is `client` (default),
`connect`, `timeout` or `no_credentials`. A call matches the entries of its
service and operation whose recorded `request` is *contained in* the actual
request (each recorded member present with an equal value; objects need not be
complete, so a fixture can leave out the span payload of an `Evaluate` call or
a generated `clientToken`). Entries are consumed in file order and the last
matching one repeats, so a polled operation records `Running` then `Complete`;
list the most specific entry first. An unmatched call fails with a connect error
naming the service and operation only, never a request value. The file is read
through the symlink-refusing reader; a missing, non-object or wrong-typed member
(`request`, `error.kind`, `error.code`, `error.message`, `null` included)
rejects the whole recording with the entry index and member name, never the
value (the shared `optional_member` rule, #4712/#4713; the helpers are the
`bedrock_agent` ones, now `pub(crate)`).

Request timestamps (`ExpiresAt`, `StartTime`, `EndTime`) are ISO strings
(`isoformat().replace("+00:00", "Z")`), as the #4380 boundary documents; a
live binding must serialize them to the wire format. `--now ISO` pins the clock
for the `ttl_hours` expiry, the `CreatedAt` fallback of the create report, the
Omni session name and the writeback timestamp.

## Behavior contract

User-visible text is copied from the Python: the `OpResult` texts of every
operation, the identifier-help and `status is required` messages, the failure
templates and their possible-cause / troubleshooting bullets, the snapshot
`ERROR:` lines, the receipts of both Omni helpers, the cap notes and the
`CODE LOCATION TROUBLESHOOTING` trailer. Request members keep the Python order
and the Python omit-when-default rules (`MaxResults` only when not 100, `SyncedAt`
and `NextToken` only when non-empty, `AttributeFilters` only when non-empty).
Exit status: `0` when `OpResult.ok`, `1` otherwise; the snapshot ops exit `1`
for a bare `ERROR` line or an inner `status` of `ERROR`, `TIMEOUT`,
`POLLING TIMEOUT`, `FAILED` or `CANCELLED`; an unusable argument object exits
`2` (`error: invalid arguments for op ...`); the Omni helpers print a failure
receipt as one compact JSON line with exit `1` (`_die`).

| Area | Behavior kept |
| --- | --- |
| Gateway allowlist | Six `application-signals` and two `logs` operations as closed enums; no string dispatch. |
| `create` | Validation order (type, probe, location, capture names, line-level needs `capture_locals`), defaults (`CaptureReturn` true unless line-level, `CaptureStackTrace` true), request member order. |
| Batch delete | `ok` comes from `Errors` in the response, not the text; 1-50 non-blank ARN strings. |
| `check-status` | `created_at` fetch errors keep their three texts; ACTIVE window clamped to `max(created_at, start)`, an empty window skipped with the `Skipped:` note; a failing status query becomes an `API error:` line, not a failure. |
| Logs Insights | Start and poll error texts (`Failed to start query: <str(exc)>`), `Polling Timeout` after `max_timeout` one-second polls, rows as `{field: value}` with a repeated field keeping its first position. |
| Escaping | `service`, `environment` and the hash are escaped by `escape_logs_insights_string`; `custom_filters` pass through raw but an unbalanced quote is rejected; the hash must be 16 lowercase hex. |
| `evaluate-traces` | 50 trace / 10 evaluator / 100 pair caps with notes, the 100-call budget, ground-truth gating, fetch-once, session de-duplication, TOOL_CALL chunks of ten, `pairsScored` / `tracesScored` honesty, explanations off the receipt, writeback (provision, retention, KMS, access-denied skipped silently). |
| `capture-dataset` | Root-span coalesce, ordered collapsed tool trajectory, 25-trace batch, 1 MB example and 1000-example caps, boundary trace flagged `partial`, unreached traces reported. |

## Mutating operations (safety)

Nothing in this slice can reach AWS. The operations that would change a live
resource are gated anyway, so the live binding inherits the rule:

- `instrumentation create`, `delete`, `batch-delete-by-scope` and
  `batch-delete-by-arns` print `ERROR: refusing to run '<op>' ...` and exit `1`
  without `--i-have-confirmed`, before the arguments are read or any client is
  built. The Python had no such flag for these four (see "Intentional changes").
- `capture-dataset` refuses without `--i-have-confirmed` (the Python flag), with
  `confirmationRequired: true`, before any client call; a test proves the
  scripted client sees zero calls.
- `evaluate-traces` writes each score back as a log record by default, like the
  script (`--no-writeback` opts out). Writeback is one `PutLogEvents`, and on a
  missing group `CreateLogGroup` / `PutRetentionPolicy` / `CreateLogStream`.
  The live-binding follow-up must decide whether to keep it default-on.
- No test uses a live client; the only clients are scripted, recorded or
  `NoLiveClient`.

## Intentional changes

1. **No live client** (above). `--region` / `--profile` are accepted and unused;
   the SDK-version guard (`MIN_BOTO3_VERSION`, the missing-operation upgrade
   message) has no equivalent and is dropped. `--print-contract` still reports
   `api_version` `2024-04-15`.
2. **`--i-have-confirmed` on the four mutating instrumentation ops** (the manager
   brief requires an explicit flag; the Python required one only for the dataset
   write).
3. **No stdin arguments.** `--json -` / `--json-file -` are refused by the shared
   router (`strict_values: true` rejects a value starting with `-`); use
   `--json-file`.
4. **Caller text is escaped before it reaches a request.** Every `Caller`
   argument is made single-line (`safe_text`) at parse time (the #4380 origin
   table), so a control or bidi character in a service name or path is sent
   escaped, not raw. `custom_filters` keep their text (JSON-encoded on output).
5. **Typed arguments.** An unknown, missing or mistyped argument exits `2`
   (the legacy `TypeError` case); `ttl_hours`, `max_timeout` and `max_results` are integers (Python also accepted floats); an explicit `null`
   reads as absent, except `limit` (`null` -> `ERROR: limit must be an integer`).
   `limit` is coerced like `int()`: integers, booleans, truncated floats and
   decimal strings.
6. **Recorded responses are validated, not trusted.** A wrong-typed response
   member (`Events`, `evaluationResults`, `rows`, `datasets`), a result row of an
   unexpected shape, or a missing `sessionId` / `queryId` becomes a reported
   error (`API error:`, `Error` query status, `evaluate failed:`, a receipt
   `error`) where Python raised `AttributeError` / `KeyError`.
7. **`check-status` and `CreatedAt`.** A `CreatedAt` that is neither an ISO string
   nor epoch seconds, or a non-object `Configuration`, is an `ERROR: Failed to
   fetch created_at: ...` instead of an exception.
8. **Dates.** `ttl_hours` that leaves the supported date range, or a search window
   outside years 1-9999, is an `ERROR` (Python raised `OverflowError`).
   `datetime.fromisoformat` is the #4380 subset.
9. **Session ids.** `^[0-9A-Za-z._:\-]{1,128}$` is matched strictly: Python's `$`
   also accepted a trailing newline, which would have reached the SQL text.
10. **Span `kind` digits.** Only ASCII digits read as the numeric enum; other
    Unicode decimal digits map to `UNSPECIFIED` (Python's `int()` accepted them).
11. **Time.** Replays never sleep: `VirtualPacer` advances its own clock, so
    `Polling Timeout` and the Omni poll waits cost no wall time. The live binding
    needs a real pacer.
12. **Client token.** The dataset `clientToken` is a version-4 UUID from the
    standard library's randomly keyed hasher (no `uuid` dependency); it is an
    idempotency token, not a secret.
13. **Contract text.** `--print-contract` `encoding` and the `service` notes name
    the Rust commands instead of `python3 scripts/...`.
14. **File output.** `snapshots --out` creates the file `0600` with `create_new`,
    narrows an existing file to `0600` before writing, and refuses a symbolic
    link or non-regular file (Python opened with `O_NOFOLLOW`).

## Python callers left for #4415

Nothing is removed or rewritten here. The cutover must repoint these at the
Rust commands (`toolkit cloudwatch instrumentation|snapshots|evaluate-traces|
capture-dataset`) before the scripts are retired:

- `skills/core-skills/aws-observability/SKILL.md` and its plugin mirror
  (`di_instrumentation.py`, `di_snapshots.py` run instructions).
- `references/cloudwatch/dynamic-instrumentation.md` (about 37 references),
  `dynamic-instrumentation/breakpoint-creation.md`,
  `dynamic-instrumentation/snapshot-parsing.md`.
- `references/cloudwatch-omni/agent-evaluation.md` (`evaluate_traces.py` and
  `capture_dataset_from_traces.py` usage, 12 references).
- The scripts themselves: `scripts/cloudwatch/di_instrumentation.py`,
  `di_snapshots.py`, `di_app_signals_client.py`, `di_logs_client.py`,
  `di_session.py`, `di_gateway.py`, `di_snapshot_queries.py`, `di_crud_tools.py`,
  `di_status_tools.py`, `di_snapshot_tools.py` and `scripts/cloudwatch-omni/
  evaluate_traces.py`, `capture_dataset_from_traces.py`,
  `test_evaluate_traces.py`, each with its `plugins/aws-core/` mirror, and the
  remaining #4380 modules they import (`di_region.py`, `di_result.py`, ...).
- `docs/migration/python-inventory.json`, `inventory-audit.md`,
  `aws-core-contracts.md` and `aws-core-security-review.md` mention the scripts.

## Follow-ups (filed as Issues, out of scope here)

- #4734, the live binding: an Application Signals, Logs, Omni and AgentCore client
  behind the `toolkit-aws` transport and signer seams, a real `Pacer`, and the
  decision on default-on writeback.
- Retirement of the Python scripts and the skill/reference rewrite: #4414, #4415.
