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
   "request": {"Service": "svc"}, "response": {"LatestConfigurations": []}},
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
value (#4712/#4713). Since repair round 1 every step is parsed by the shared
`toolkit_aws::recorded::step_fields`, stored as a shared `Step` (expected
request and outcome, success and error steps alike) and compared with the
shared `KeyOrder::Contained` rule before either outcome is replayed (#4727,
#4726 class); the optional members are read with the shared `optional_text`.
`KeyOrder::Contained` is an additive variant of the #4727 module: this slice's
fixtures leave generated members out of the recorded request.

**Responses.** Every response member a command reads is checked before any
result is formed (`response_shape.rs`, over the shared accessors): absent takes
its default; a present `null` or wrong type is an invalid response that names
only the member path and both types. Per operation:

| Operation | Members checked |
| --- | --- |
| `CreateInstrumentationConfiguration` | `LocationHash`, `ARN` (string); `CreatedAt`, `ExpiresAt` (timestamp) |
| `ListInstrumentationConfigurations` | `LatestConfigurations` (list of configuration objects, every member below); `SyncedAt`; `NextToken` (string or `null`) |
| `GetInstrumentationConfiguration` | `Configuration` (object: `InstrumentationType`, `SignalType`, `LocationHash`, `Description`, `ARN` strings; `Location`, `CaptureConfiguration` objects with their code members; `AttributeFilters` list of objects; `CreatedAt`, `ExpiresAt`) |
| `BatchDeleteInstrumentationConfigurations` | `DeletedCount` (integer); `SuccessfulDeletions`, `Errors` (lists of objects with string members) |
| `GetInstrumentationConfigurationStatus` | `Events` (list of objects: `Time`, `Status`, `ErrorCause`); `Service`, `Environment`, `SignalType`, `Status`, `LocationHash` (string); `Location` (object); `NextToken` (string or `null`) |
| `StartQuery` / `GetQueryResults` | `queryId`, `status` (string); `results`, `messages` (list); each row a list of objects whose `field` / `value` are strings |
| Omni `StartTelemetryQuerySession` / `StartTelemetryQuery` / `GetTelemetryQueryResults` | `sessionId`, `queryId`, `status` (string); `rows` (list of objects); `nextToken` (`next_token_from`) |
| `Evaluate` | the response is an object; `evaluationResults` (list of objects: `value` number or `null`, `evaluatorId`, `label`, `explanation`, `errorCode`, `errorMessage` strings) |
| `ListDatasets` / `GetDataset` | `datasets` (list of objects: `datasetId`, `datasetName` strings); `nextToken` (string or `null`); `exampleCount` (integer) |
| `CreateDataset` / `AddDatasetExamples` | `datasetId` (required non-empty string), `status` (string); `exampleIds` (list of strings) |

The only nullable members are explicit opt-ins: a display-only `NextToken`
(`null` = no further page) and an `Evaluate` `value` (`null` = an error row
without a score). Members a command never reads are not checked (a recording
may carry them; unknown members stay forward-compatible, as in Python).
The #4380 renderers stay tolerant; they only ever see a checked response.

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
3. **Bounded stdin arguments (#4780).** `--json -` and `--json-file -` read the
   JSON object from stdin like the Python originals (`STDIO_DASH`; at most 1 MiB,
   UTF-8; larger, invalid or empty input exits 2). `./-` names a file called `-`.
   Every other bare `-` is refused by the router (`docs/migration/cli-bare-dash.md`).
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
   member (every member of the table above), a result row of an unexpected
   shape, or a missing `sessionId` / `queryId` becomes a reported error
   (`API error:`, `Error` query status, `evaluate failed:`, a receipt `error`)
   where Python raised `AttributeError` / `KeyError`. See items 15-20.
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
14. **File output (repair round 1, #4757).** `snapshots --out` never opens the
    final path. The text is written to a new private file
    `.<name>.<pid>.<n>.tmp` in the same directory (`create_new`, i.e.
    `O_CREAT|O_EXCL`, mode `0600`, which follows no link and reuses no entry),
    synced, then renamed over the final path. `rename` replaces the directory
    entry, so a symbolic link swapped in after the check is replaced, never
    followed: its target is not opened, truncated, chmodded or written. A
    pre-existing symbolic link or non-regular file at the final path is still
    refused before any file is created (Python opened with `O_NOFOLLOW`). An
    existing regular file is replaced by a new inode (Python truncated it in
    place), so its hard links and its old mode are not carried over; the result
    is always `0600`. No parent directory is created; the parent components
    are the caller's (as with Python's `O_NOFOLLOW`, which guards the last
    component only). A failed write removes its temporary file. Platforms
    without POSIX `rename` semantics over an existing entry are not supported
    targets of this slice.
15. **Malformed responses fail closed (repair round 1, #4749/#4750/#4755/#4758).**
    Per the table above, a present `null` or wrong-typed member is an invalid
    response: the instrumentation ops print `Failed to <action>` /
    `Unexpected error: the <Operation> response is invalid: ...` and exit 1
    (never `No active ... found`, `Events Returned: 0` or `BATCH DELETE
    COMPLETED`); the snapshot tools return query status `Error` (exit 1, never
    `NO_SNAPSHOTS_FOUND`); `capture-dataset` returns a failure receipt that
    keeps `conversionErrors` (never `examplesWritten`); `evaluate-traces`
    records a `scoreErrors` entry for that evaluator call (never a scored pair,
    a `tracesScored` count or a writeback record). Python crashed or, for the
    tolerant `.get(..., [])` reads, silently showed an empty result.
16. **`evaluationResults` must be a list when present.** Python treated `null`
    and other falsy values as no results; they are now a score error. A score
    counts only when `value` is a number (zero included) and the row has no
    `errorCode`.
17. **`CreateDataset` needs its `datasetId`.** An absent or empty id is an
    invalid response (Python printed `"datasetId": null` with a write count).
    `AddDatasetExamples` without `exampleIds` (or with `[]`) keeps the script's
    fallback to the number of examples sent.
18. **`GetDataset` before an add.** A failed call still does not block the
    write (the service enforces the cap); a response that is not an object or
    whose `exampleCount` is present and not an integer stops the write.
19. **`Configuration: null`** from `GetInstrumentationConfiguration` is an invalid
    response for `get` and `check-status` (it read as "No instrumentation
    found"); an absent or empty `Configuration` still reads as not found.
20. **Log query rows.** A row `field` or `value` that is present and not a
    string is an invalid row (Python copied any value).
21. **Value-free diagnostics (repair round 2, #4784).** A diagnostic about a
    response or a recording names only fixed member names, positions and JSON
    type labels, never a value (the `toolkit_aws::recorded` rule). The changes
    from Python:
    - An absent or empty `StartQuery` `queryId` is still `start_query did not
      return a queryId`, but without the `(response: {...})` dump Python
      appended.
    - A `GetInstrumentationConfiguration` `CreatedAt` string that is not ISO 8601
      no longer quotes the value. The error now reads `CreatedAt is not a
      timestamp: expected an ISO 8601 string or epoch seconds, got a string that
      is not ISO 8601`.
    - The Omni incomplete-query error names a known status (`Scheduled`,
      `Running`, `Complete`, `Failed`, `Cancelled`, `Timeout`) or `None`. Any
      other status text shows as `<unrecognised>`.

    Three kinds of text are still shown, all escaped:
    - The service error itself (`An error occurred (<Code>) when calling the
      <Operation> operation: <Message>`, and the transport errors' text). That
      text is the error, as in Python.
    - The caller's own arguments (`ATTEMPTED PARAMETERS:`, an invalid trace id,
      `dataset '<name>' not found`).
    - The data a command exists to show (snapshot bodies, rendered
      configurations, receipts).
22. **Dataset resolution keeps `conversionErrors` (repair round 2, #4785).** In
    add mode, every `ListDatasets` failure keeps the `conversionErrors` already
    collected and never reports `examplesWritten`. That covers a service error,
    a malformed first or later page, a wrong-typed `nextToken`, a non-object
    response and `dataset '<name>' not found`. The receipt has the same shape as
    the create, get and add failures: `{"error": ..., "conversionErrors":
    [...]}`, naming the listing failure and every conversion error. Item 15's
    guarantee now holds on every `capture-dataset` path that runs after
    conversion.
23. **Response-mutation harness and its fixes (repair round 3, #4816, #4817,
    #4818).** `tests/cloudwatch_api.rs::every_recorded_response_member_fails_closed_or_is_documented`
    replays every recording the command tests use, plus rich fixtures that
    hold every response member the slice reads. For each response step and
    member path it reruns the command three times: once with a unique
    sentinel string, once with `null` and once with a value of another type.
    A run must exit non-zero, print exactly what the unmutated run printed,
    or match the commented `HARNESS_ALLOWED` table. It must never print the
    sentinel on stderr or on the stdout of a failing run, and it must never
    report a new non-terminal state (PENDING, Running) with exit 0. Changes
    made in this round:
    - **#4816:** the dataset-cap refusal no longer names the resolved dataset
      id or its stored count. Both are response values, and a name-only add
      never supplied the id. The refusal names only this run's example
      count and the service cap.
    - **#4817:** the writeback notes say "the results log group". They no
      longer give the group name, which comes from the span's `svc_name`.
      The caller's `--kms-key-id`, the retention days and the service error
      text are still shown, escaped. A retention warning shared by several
      traces is now shown once.
    - **#4818:** `check-status` fails on an invalid
      `GetInstrumentationConfigurationStatus` response. The message is
      "ERROR: Failed to check <STATUS> status: <value-free shape error>",
      and the command exits 1. No further status query is sent, and no
      ACTIVE, READY, ERROR or PENDING verdict is drawn. `GatewayError`
      carries `invalid_response` to tell this case apart. A service error
      stays the best-effort "API error:" line inside the verdict, as in
      Python.
    - Found by the harness:
      - A timestamp member (`CreatedAt`, `ExpiresAt`, `SyncedAt`, `Events[].Time`)
        must be an ISO 8601 string or epoch seconds in range. Any other
        string used to pass the shape check and be printed as a time; it is
        now an invalid response.
      - Each `AttributeFilters` group must be an object of strings (the
        Python type).
      - Each Logs Insights `messages` item must be an object whose `queryId`
        and `text` are strings when present.
    - **Allowlisted, with the reason in each entry:** values the command
      exists to print. These are:
      - rendered configurations (`list`, `get`), the create receipt, the
        `get-status` report header, its events' `ErrorCause`, and the
        check-status ERROR cause;
      - snapshot records, field names and query messages;
      - the query receipt's `queryId`;
      - evaluation rows (`evaluatorId`, `label`, and the service's per-row
        `errorCode`/`errorMessage`);
      - `--include-explanations` explanations;
      - dataset receipts, including the resolved `datasetId` of a
        *successful* add.

      Also allowlisted, as documented absent-defaults: epoch-second
      timestamps, a `null` `NextToken` (no further page), and a `null` score
      (an unscored row; the receipt counts `pairsScored: 0`, as in Python).
      The batch-delete report is the only output allowed on a failing run
      (exit 1 when any item failed). It lists the deleted and failed ARNs
      with the service's own per-item code and message. The query receipt
      also keeps its `queryId` on failure, as in Python.
24. **Harness coverage by construction (repair round 4, #4837, #4838).** The
    harness of item 23 mutated only members a fixture already held. Its
    coverage now comes from the read sites' own tables:
    - **The read set.** `cloudwatch::response_reads` lists every response
      each command reads, by command, service and operation. Each entry
      names the table its read site uses:
      - `AppSignalsOp::response_fields` and `LogsOp::response_fields`;
      - `EVALUATE_RESPONSE` and the dataset `control_fields`, which now
        include `GetDataset.exampleCount`;
      - the Omni envelope keys of `omni_sql`;
      - the span-row tables `evaluate::SPAN_ROW` and `dataset::SPAN_ROW`,
        built at compile time from each reader's base columns and its
        `ATTR_COLS`.

      `response_shape::member_paths` derives the member paths from these
      tables: 241 today.
    - **New shapes.** Three shapes make this possible, and none changes
      what a response check accepts:
      - `Shape::Union` (wrapper or flat members, wrapper first) replaces
        the hand-written `Location`/`CaptureConfiguration` tables.
      - `Shape::Rows` names the Logs Insights `results[*][*].field/value`
        members that `result_row` checks.
      - `Shape::Data` marks an Omni span column, which is passed on
        unchecked as in Python.
    - **Reviewed exclusions** (listed in the module): the JSON inside an
      `@message` string; the data keys of an `AttributeFilters` group; the
      `error` of a recorded failure step; and the responses no member of
      which is read (Logs writeback calls, `DeleteInstrumentationConfiguration`,
      `StopTelemetryQuerySession`).
    - **Synthesised variants.** Every written scenario gets two variants,
      `complete` and `flat`:
      - Each derived member the fixture lacks is added with a near-absent
        value: empty text, `0`, `false`, or `null` where null is valid. An
        absent list gets one item.
      - In the `flat` variant each union's wrapper is removed and its
        members are written flat.
      - A variant must replay with its scenario's declared exit, and is then
        mutated like any other fixture.
    - **Hand-written fallback rows.** These cover the `gen_ai.*` input and
      output fallback and the `TOOL` kind and name, for evaluation (TRACE and
      TOOL_CALL) and for capture.
    - **The coverage test.** `every_derived_response_member_path_is_mutated`
      fails when any derived (command, service, operation, path) is mutated
      by no scenario.

    The expanded harness, 9504 mutated runs, found no value echo, false
    success or null-default defect. It added two allowlist entries, both
    Python behaviour:
    - A Failed Logs Insights query's `messages` are part of its result, as
      in `di_snapshot_queries.py`'s terminal-status dict (`on_failure`).
    - `hasExplanation` is `bool(explanation)`, so changing an empty
      explanation to text flips that flag. The explanation text itself is
      never printed without `--include-explanations`.

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
