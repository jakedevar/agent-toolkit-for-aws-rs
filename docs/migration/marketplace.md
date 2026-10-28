# marketplace: metering, subscription and cleanup helpers (#4396)

Slice `marketplace`, wave 3. Ports the Python of the `aws-marketplace-metering`
skill (`skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/`):
`metering_core.py`, `discoverer.py`, `aggregator.py`, `cleanup.py`,
`submitter.py`, `expiry.py`, `subscription.py`, `deprovision_cleanup.py`,
`register.py`, `query_metering.py` and `test_event_publisher.py`.

**Prior art checked.** `git log -S` on `metering_core`, `query_metering` and
`test_event_publisher` shows only the skill import and the programme documents;
no part of this slice had landed on the base `e77d1c18`.

## Commands

| Command | Replaces |
| --- | --- |
| `toolkit marketplace handler NAME [--event FILE] [--config FILE] [--recorded FILE] [--now TS] [--seed N]` | the Lambda handlers: `discoverer`, `aggregator`, `cleanup`, `submitter`, `expiry` (all on `metering_core.py`), `subscription`, `deprovision-cleanup`, `register`, `test-event-publisher` |
| `toolkit marketplace query-metering --region R [--days N] [--start-date D] [--end-date D] [--customer-id ID] [--status S] [--dimension D] [--metering-record-id ID] [--product-code C] [--deployment-product-code C] [--query-type TYPE] [--top-n N] [--limit N] [--max-pages N] [--recorded FILE] [--now TS]` | `query_metering.py` (`main`) |
| `toolkit marketplace test-events --product-code C --test-accounts A,B --event-source SRC [--scenario S] [--count N] [--region R] [--event-bus-name B] [--dry-print] [--recorded FILE] [--now TS] [--seed N]` | the command line of `test_event_publisher.py` (`main`) |

Registration is `marketplace::SLICE` (`crates/toolkit-cli/src/marketplace/mod.rs`)
plus one sorted line in `registry.rs`; the module is classified `Recorded` in the
`recorded_contract` inventory.

`handler` runs one Lambda exactly as the script did, with its inputs named:

- `--event FILE`: the Lambda event (an SQS `Records` batch, an API Gateway
  request, the discoverer's `{"mode": ...}` input or the publisher's payload);
  `{}` when absent.
- `--config FILE`: the Lambda environment as a JSON object of strings
  (`PRODUCT_CODE`, `SUBSCRIBERS_TABLE`, `USAGE_TABLE`, `AGGREGATED_USAGE_TABLE`,
  `WORK_QUEUE_URL`, `DEPROVISION_WORK_QUEUE_URL`, `CLEANUP_QUEUE_URL`,
  `METERING_MODE`, `METERING_LOCK_HOURS`, `MAX_RECORDS_PER_RUN`,
  `AGGREGATED_USAGE_TTL_DAYS`, `CLEANUP_MAX_RETRIES`, `CLEANUP_MAX_WORKERS`,
  `METRIC_NAMESPACE`, `CUSTOMER_PROFILE_TABLE`, `PROMOTED_PROFILE_FIELDS`,
  `ALLOWED_REGISTRATION_FIELDS`, `MAX_REGISTRATION_FIELDS`,
  `MAX_FIELD_VALUE_LENGTH`, `AWS_REGION`, `TEST_ACCOUNT_ALLOWLIST`,
  `EVENT_SOURCE`, `EVENT_BUS_NAME`). The process environment is never read.
  A variable the script read with `os.environ[...]` is a failure when absent
  (`SUBSCRIBERS_TABLE` and `PRODUCT_CODE` for the pipeline handlers,
  `AGGREGATED_USAGE_TABLE` for the aggregator, submitter and expiry,
  `CLEANUP_QUEUE_URL` for the aggregator, ...), as the `KeyError` or
  `RuntimeError` was.
- `--recorded FILE`: the AWS responses (below). `--now TS` pins the instant (ISO
  UTC) every window, bucket, stamp and metric timestamp derives from; the
  default is the system clock. `--seed N` seeds the test-event generator.

stdout is the value the Lambda returned (`json.dumps(indent=2)` layout). The log
the Lambda wrote to CloudWatch, `LEVEL text` lines and the EMF metric lines
(`{"_aws": {...}}`, `json.dumps` separators), goes to stderr, so a pipeline can
read the result without parsing the log.

## Recorded input

Recorded AWS responses are read only through `toolkit_aws::recorded`
(`step_fields`, `Step`, `StepQueue`, `KeyOrder::Ignored`, the `optional_*` and
`nullable_*` accessors, `list_value`, `object_members`); `tests/recorded_contract.rs`
detects the slice (flag and import signals) and passes with no allow-list entry
and no `recorded-shape:` marker in this slice.

```json
{"calls": [
  {"service": "dynamodb", "operation": "Query",
   "request": {"TableName": "usage", "IndexName": "metering_pending", "...": "..."},
   "response": {"Items": [...], "LastEvaluatedKey": {...}}},
  {"service": "meteringmarketplace", "operation": "BatchMeterUsage",
   "error": {"code": "InvalidUsageDimensionException"}}
]}
```

`service` is `dynamodb`, `sqs`, `meteringmarketplace`, `events` or `cloudtrail`;
`operation` is the API name the script's call maps to: `Query`, `PutItem`,
`UpdateItem`, `SendMessageBatch`, `BatchMeterUsage`, `ResolveCustomer`,
`PutEvents`, `LookupEvents`. Steps of one service and operation replay **in
order**; a call past the last step fails like an unreachable service
(`aws_connect`); the recorded `request` (error steps too) is compared before
the outcome, key order ignored, and a step without a `request` answers any
request; a run that leaves steps unused is rejected. `error.kind` is `client`
(default; `error.code` is the service error code the handler branches on),
`connect` or `no_credentials`. A present member of another type, `null`
included, rejects the recording, naming the entry and the member, never a value.

Requests are the API requests the script built: DynamoDB items and expression
values are plain JSON (the `boto3` resource view), a string set is `{"SS": [..]}`,
a `datetime` is epoch seconds. Responses are read with the shared accessors:
`Items` is a list of objects (a `null` is an invalid response, never an empty
discovery), `LastEvaluatedKey` an object, `UnprocessedRecords` a list or `null`.

## Acceptance criteria

- **AC1 metering idempotency, batching, expiry, subscription state and retries
  are covered by tests.** `marketplace/tests/`:
  - *idempotency*: the aggregator's conditional `PutItem`
    (`attribute_not_exists(licenseArn)`) is the commit point; a
    `ConditionalCheckFailedException` writes nothing and enqueues no cleanup
    (`a_duplicate_visit_writes_nothing_and_enqueues_no_cleanup`); a fresh write
    enqueues cleanup messages carrying the sum and count; a cleanup message
    without a status only clears `meteringPending`;
  - *batching*: 25 records per `BatchMeterUsage` call, Concurrent Agreements
    (`LicenseArn`) and legacy (`ProductCode`) batches apart, deprovisioning
    licenses first, the `MAX_RECORDS_PER_RUN` cap stops reading, 10 entries per
    `SendMessageBatch`, 100 row keys per cleanup message;
  - *retries*: unprocessed records once, then left pending and the run fails
    after the write-back; throttled row updates up to `CLEANUP_MAX_RETRIES`;
    transient codes raise (never bisected); a poison record is isolated by
    bisecting (a full failing batch of 25 takes 49 calls and isolates all 25),
    stamped `Error` with the exception name;
  - *expiry*: raw-row age-out (`AggregationExpired`, shard walk, six-empty-bucket
    stop), aggregated-record expiry (`SubmissionExpired`), the month-end grace
    (03:00 lock drop, 06:00 cutoff, previous-month buckets kept while it is open
    and expired in-window after it closes);
  - *subscription state*: `License Updated` never reactivates a deprovisioning or
    inactive license, `License Deprovisioned` opens the one-hour flush window
    from the event time, `deprovision-cleanup` finalizes only while the marker is
    present, agreement events change `agreementStatus` only, a missing license
    ARN is resolved by `agreementId-index` and a permission error is retried
    rather than read as "not found".
- **AC2 cleanup intent is verified offline.** Every cleanup, reject, age-out,
  expiry and finalize write is asserted by the exact `UpdateExpression`,
  `ConditionExpression` and values it sends (`pipeline_flow.rs`,
  `subscribers_flow.rs`, `core_logic.rs`): attributes are `REMOVE`d and statuses
  stamped; no test and no code path issues `DeleteItem`, `BatchWriteItem`,
  `DeleteMessage` or any resource deletion.
- **AC3 billing, IAM and deletion boundaries are stated before review** (next
  section).

## Boundaries (billing, IAM, deletion)

**Billing.** `BatchMeterUsage` (the only call that bills a buyer) is reached only
in `submitter` and only when `METERING_MODE` is not `dry-run`, through the client
seam: here the seam is a recording or the no-live client, so this build cannot
bill. In `dry-run` the submitter runs the whole pipeline (reads, batches,
deprovisioning-first order) but never calls the API; it logs the masked record,
emits `DryRunSubmitted` and stamps `DryRunSubmitted`. `test-event-publisher` and
`test-events` refuse anything but a `dry-run` stage (the command takes the stage
from the caller's `--event-source`, which must be stage scoped; an `aws.` source
is rejected), draw the buyer account only from the explicit allow-list and
publish only simulated events, which confer no entitlement.
`register` calls `ResolveCustomer` (read only). The submit idempotency keys are
unchanged: first write wins per account, license, dimension and hour.

**IAM.** No command reads a credential or opens a socket. The calls each handler
makes, which a live binding must be granted and nothing else (the CloudFormation
templates `assets/template-main.yaml` and `template-events.yaml` stay as they
are; see "Not in this slice"):

| Handler | Calls |
| --- | --- |
| discoverer | `dynamodb:Query` (usage `metering_pending`, subscribers `deprovisioning-pending-index`), `dynamodb:Query` + `dynamodb:UpdateItem` (age-out), `sqs:SendMessageBatch` (work queues) |
| aggregator | `dynamodb:Query`, `dynamodb:PutItem` (aggregated usage, conditional), `dynamodb:UpdateItem` (usage rows), `sqs:SendMessageBatch` (cleanup queue) |
| cleanup | `dynamodb:UpdateItem` (usage rows) |
| submitter | `dynamodb:Query` (aggregated usage, subscribers), `dynamodb:UpdateItem` (aggregated usage), `meteringmarketplace:BatchMeterUsage` (live mode only) |
| expiry | `dynamodb:Query`, `dynamodb:UpdateItem` (aggregated usage) |
| subscription | `dynamodb:UpdateItem`, `dynamodb:Query` (`agreementId-index`) on the subscribers table |
| deprovision-cleanup | `dynamodb:Query`, `dynamodb:UpdateItem` on the subscribers table |
| register | `meteringmarketplace:ResolveCustomer`, `dynamodb:UpdateItem` (profile and subscribers tables) |
| publisher | `events:PutEvents` |
| query-metering | `cloudtrail:LookupEvents` |

**Deletion.** Nothing deletes a resource or an item. The "cleanup" handlers
`REMOVE` the `meteringPending` marker (and the two deprovisioning markers) from
rows and stamp a status; the rows are kept for audit. The Python Lambdas ask for
no confirmation (they are scheduled, idempotent attribute updates); here the
equivalent guard is stricter: every write goes through the client seam, which is
a recording (a replayed run changes nothing) or the no-live client (the run fails
`aws_connect` before any write), and `--recorded` runs reject unused steps. A
live binding (follow-up) must keep these updates conditional exactly as written
(`attribute_exists(deprovisioningPendingFlag)`,
`attribute_not_exists(licenseArn)`, the `subscriptionStatus` guard) and needs its
own review before it may run against a real table.

## Intentional changes from the Python

1. **Offline only.** There is no live AWS client in this build (as for
   `docgraph`, `bedrock-agent`, `elasticache`). A run without `--recorded` sends
   nothing, prints a stderr note and fails with the class `aws_connect` (exit 1).
   Live bindings are the follow-up; the Python stays in place until then.
2. **Lambdas are one `handler` command.** The module-level `boto3` clients and
   `os.environ` reads become the `--recorded` client and `--config`; the process
   environment is not consulted. The Lambda `context` argument is unused by
   every script and has no flag.
3. **Streams.** The returned value is on stdout; the log and the EMF lines are on
   stderr (the Lambda runtime put both in CloudWatch Logs). `print` of the EMF
   line is the same `json.dumps` text.
4. **Determinism.** `--now` pins the clock. The jittered back-off sleep after a
   throttled write is not performed (nothing is sent); the retry count is
   unchanged. The bounded thread pool of `update_rows_bounded` is a sequential
   loop (same retries, every row tried, failures counted and reported together,
   in the order of the keys; `CLEANUP_MAX_WORKERS` is validated, not used). A
   Python `set` of licenses (iterated in hash order) is iterated in query order.
   The `expiry` and `deprovision-cleanup` pagination reads every page, then
   updates, instead of interleaving the two.
5. **Quantities are 64-bit.** An integer beyond `i64`, or a group sum that
   overflows, is rejected client-side with the new reason `QuantityOutOfRange`
   (Python had unbounded integers and `BatchMeterUsage` rejects such a value
   anyway). Every other reason code and the order they are decided in are
   unchanged.
6. **Wrong-typed members are invalid, not passed through.** An item member, a
   queue message field or an event section of another type (a number where a
   string is expected, a list where an object is) is an invalid response or a
   failed message (`batchItemFailures`) instead of a value carried into a
   request; a `null` list in a response is never an empty list. An SQS message
   that is valid JSON but not an object is "Unknown message format" in
   `subscription`; a `rowKeys` entry that is truthy and not a string fails the
   cleanup message.
7. **Failure text.** A failed call names its class and service code
   (`aws_service_error (ConditionalCheckFailedException)`, `aws_connect`), never
   the service message or any response value (`type(e).__name__: {e}` before).
   `Discoverer invoked without a valid mode/offset` no longer echoes the event.
   A failed `SendMessageBatch` entry list shows its codes, a failed `PutEvents`
   its count, not the response (the scripts printed the whole object). Every
   value interpolated into a log line (dimension, hour bucket, reasons, product
   code, region, masked identifiers) is redacted by the shared
   `toolkit_aws::redact_text`, bounded to 120 characters and escaped on one line
   (`toolkit_core::escape_untrusted` / `neutralize_line`); a caller path is
   redacted and escaped but never shortened.
8. **Timestamps on the wire.** A `datetime` in a request (`Timestamp`,
   `StartTime`, `EndTime`) is epoch seconds, as the JSON protocols send it. A
   recorded response timestamp may be epoch seconds or ISO text; a usage record
   is matched to its result by account, license (or customer identifier),
   dimension and the first 13 characters / UTC hour of the timestamp, as
   `_ts_iso` did. A CloudTrail `EventTime` number is shown as `str(datetime)`.
9. **`register`.** `urllib.parse.parse_qs` (blank values dropped, `+` as space,
   invalid UTF-8 replaced) and `base64.b64decode` (non-alphabet characters
   discarded, bad padding an error) are reimplemented; a registration field
   named in `ALLOWED_REGISTRATION_FIELDS` is truncated by characters, as
   `value[:N]`. The subscriber row's `registeredRegions` set is the request
   value `{"SS": ["<region>"]}` with `ADD`.
10. **`test-events` / publisher.** `uuid4` and `random` are a seeded generator
    (`--seed`, default from the clock); the values stay well formed (12-digit
    allow-listed account, `agmt-` + 20 hex, `l-` + 16 hex). `--region` must look
    like `us-east-1` and `--event-bus-name` is limited to letters, digits and
    `/ . - _`, both usage errors (exit 2), so a hostile value never reaches a
    request. `--dry-print` is `json.dumps(indent=2)` with non-ASCII escaped. The
    `aws.` source error no longer echoes the rejected value.
11. **`query-metering`.** Option values may not start with `-` (the router's
    strict values), so `--limit -1` is refused (the aggregation keeps Python's
    negative slice for callers of the library functions and is tested); an
    invalid `--start-date` / `--end-date` is exit 1 with `--start-date must be
    YYYY-MM-DD` (a traceback before) without echoing the value; `--region` is
    validated; a quantity that is not a number, a `CloudTrailEvent` that is not a
    string, or a usage record that is not an object is an invalid response
    (Python crashed); an event body that is not JSON or an event without
    `EventTime` is skipped, as before. `--recorded` and `--now` are new.
12. **Numbers.** `json.dump` of a float prints its shortest form, and a summed
    quantity stays an integer unless a float was seen, as `sum()` did.

## Not in this slice

- `scripts/deploy.sh`, `assets/template-main.yaml`, `assets/template-events.yaml`
  and `assets/requirements.txt`: infrastructure and packaging of the skill, not
  Python (shell, CloudFormation, a dependency list). They stay as they are; the
  Lambda code they deploy is still the Python until the live bindings and the
  retirement (#4415) land.
- Live AWS bindings for the five services, and replacing the callers (the skill's
  `SKILL.md`, `references/*.md`, `deploy.sh`) with `toolkit marketplace ...`:
  the follow-up filed from this slice (see the handoff on #4396). The Python is
  not retired here.
