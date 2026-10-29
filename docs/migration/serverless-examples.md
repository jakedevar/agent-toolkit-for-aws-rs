# Serverless application examples (#4397, slice `serverless`)

Rust replacements for the three Python application examples that skills ship:

| Python (retired) | Rust |
| --- | --- |
| `skills/core-skills/aws-serverless/assets/powertools-handler.py` (and its `plugins/aws-core` copy) | Cargo project `skills/core-skills/aws-serverless/assets/powertools-handler/` (package `powertools-handler`, binary `powertools-handler`) |
| `skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions/scripts/lambda_function.py` | binary `lambda-processor` of package `s3-upload-processor` (`scripts/src/bin/lambda_processor.rs`, shared code `scripts/src/lib.rs`) |
| `.../processing-s3-uploads-with-step-functions/scripts/fargate_processor.py` | binary `fargate-processor` of the same package (`scripts/src/bin/fargate_processor.rs`) |
| `.../processing-s3-uploads-with-step-functions/scripts/requirements.txt` (hash-pinned boto3 set) | `scripts/Cargo.lock` (checksummed; the Dockerfile builds with `--locked`) |

`scripts/Dockerfile`, `statemachine.asl.json` and the trust policies stay; the
Dockerfile now builds the Rust binary. No Python is left in either skill
(`crates/serverless-recipes/tests/recipes.rs::no_python_remains_for_the_serverless_examples`).

## Layout and where things run

- Both projects are workspace members, so `cargo test` of the repository runs
  their fixtures, but they are **not** part of `toolkit-cli`: no command, no
  `SLICES` entry, no `USAGE` line. The `plugins/` tree is excluded from the
  workspace (`exclude = ["plugins"]`) because the bundled copy of the
  aws-serverless skill carries the same Cargo project; it is byte-identical to
  the canonical copy (`toolkit sync plugin-skills`, and a contract test).
- Each project is self-contained: its manifest has no workspace inheritance and
  its `Cargo.lock` is shipped, so copying the directory out of the skill gives
  a project that builds with `cargo lambda build --locked` (or the Dockerfile).
  The shipped locks were produced from the workspace lock, so they carry the
  versions the tests ran against.
- `crates/serverless-recipes` is a test-only workspace member (no runtime code)
  that pins the repository-level contracts: no Python left, plugin copy
  identical, SKILL text matches the manifests, Dockerfile digests/`--locked`/
  non-root user, lock files match the manifest pins and carry checksums, state
  machine still reaches the container and `TASK_EVENT`.

## Event shapes

| Handler | Input | Output |
| --- | --- | --- |
| `powertools-handler` | any JSON event (API Gateway proxy shape expected: `body` is the idempotency key) | `{"statusCode":200,"headers":{"Content-Type":"application/json","Access-Control-Allow-Origin":"https://your-domain.example"},"body":"{\"message\":\"success\"}"}` |
| `lambda-processor` | EventBridge S3 "Object Created": `detail.bucket.name`, `detail.object.key`, `detail.object.size` | `{"statusCode":200,"body":{"bucket","key","size_bytes","local_path","file_size_on_disk"}}` (`body` is an object); the three log lines `Received event: ...`, `Downloaded s3://B/K to PATH`, `File metadata: ...` |
| `fargate-processor` | env `TASK_EVENT` (the same event as JSON text, default `{}`) | stdout `Downloaded ...`, `File metadata: {bucket,key,local_path,file_size_bytes}`, `Done. File size: N bytes`; exit 0 |

Error behaviour of the processors (fixtures in `tests/processor.rs`):

- Lambda: `Invalid S3 path` (no bucket, no key, key not a string, `/..` in the
  key), `S3 key exceeds maximum length` (more than 1024 characters),
  `File too large for Lambda processing` (size above 512 MiB), checked in that
  order and before any download; a download failure is returned as the error
  and the temporary file is removed on every path.
- Fargate: `Error: No bucket or key in event` and `Error: Invalid S3 key` on
  stdout with exit status 1; any other failure exits 1 with `Error: ...` on
  stderr.

## Behaviour changes

Every intentional difference from the Python, with the reason.

| # | Change | Why |
| --- | --- | --- |
| 1 | JSON text (`body` of the Powertools response, the `File metadata:` log lines, `Received event:`) is compact UTF-8 (`{"message":"success"}`), not Python's `json.dumps` spacing and `\u` escapes. Object keys of the logged metadata are alphabetical, not insertion order. | Equivalent JSON; serde_json does not reproduce Python's formatting. Consumers parse it. |
| 2 | Powertools logger: `location` is `file:line` of the call (not `function:line`); `timestamp` is RFC 3339 UTC (`2023-11-14T22:13:20.123Z`), the Powertools `use_rfc3339` form, not `2023-11-14 22:13:20,123+0000`. Unknown `POWERTOOLS_LOG_LEVEL` falls back to `INFO` instead of raising. | No Rust edition of Powertools exists; the field names, order and values (`cold_start`, `function_*`, `xray_trace_id`) are kept. |
| 3 | Tracer: `## handler` and `### process` subsegments are sent to the X-Ray daemon over UDP with the `ColdStart` and `Service` annotations; exceptions become `fault` + `cause`, but no stack trace and no `response` metadata are captured. | The Python `aws-xray-sdk` is not available; a full OpenTelemetry/ADOT setup is the documented alternative for richer traces. |
| 4 | Idempotency key: `<function>.handler#<SHA-256 hex of the canonical JSON of the key value>`; Python used MD5 over `json.dumps(sort_keys=True)`. The event key is a dot-separated field path (`body`, `requestContext.requestId`), the JMESPath subset the template needs. | MD5 is avoided; full JMESPath is a large dependency for one expression. Records written by the Python version under the old key format are simply not matched (they expire by TTL). |
| 5 | The table name is `IDEMPOTENCY_TABLE`, default `IdempotencyTable` (the Python hard-coded it). | Same default; configurable without editing code. |
| 6 | A key value that is JSON `null`, an empty string, or an array/object holding only nulls counts as missing (Python: library rule for "no key"); the handler then logs the Powertools warning and runs without idempotency. | Assumption: reproduced from the Powertools v2/v3 behaviour as documented; the Python library is not installed here, so it was not re-run. |
| 7 | Metrics: `ColdStart` is its own EMF line with dimensions `function_name`, `service` before the handler runs; `RequestsProcessed` is flushed afterwards, also when the invocation failed. With no namespace the cold start metric is skipped silently and `flush` returns `MetricsError::MissingNamespace` (Python: `SchemaValidationError`). Namespace is `MyApp` in code (as in the Python), `POWERTOOLS_METRICS_NAMESPACE` is available through `metrics::namespace_from_env()`. | Rust has no decorators; same EMF layout. |
| 8 | Lambda processor: a key whose last path segment is empty, `.` or `..` (`folder/`, `..`, `a/.`) is refused up front as `Invalid S3 path`. Python downloaded the whole object and then failed renaming onto a directory. | Same observable result (an error, no file) without the transfer. |
| 9 | Lambda processor: a bucket that is present but not a string, and any field of the wrong JSON type (`detail`, `detail.bucket`, `detail.object` not objects, `size` not a number or boolean) are errors with a `Malformed event: ...` or `Invalid S3 path` message. Python raised `AttributeError`/`TypeError`/boto3 `ParamValidationError`. | Error type names differ (Rust errors carry no Python class name); an error either way, before any download. |
| 10 | Fargate processor: a TASK_EVENT that is not valid JSON, not an object, or has a non-object `detail`, `bucket` or `object` prints `Error: ...` to stderr and exits 1 (Python: traceback, exit 1). A non-string bucket and a key naming no file do the same. | Same exit status; no traceback. |
| 11 | S3 download streams to the destination and removes a partial file on failure. Errors read `Failed to download s3://B/K: <SDK error chain>`. | boto3's `download_file` cleaned up its own temporary file; messages differ. |
| 12 | Runtime and packaging: Lambda `provided.al2023` + `bootstrap` built with cargo-lambda (was `python3.12`, zip of one `.py`); Fargate image is `rust:1.94.1-slim-bookworm` (build) + `gcr.io/distroless/cc-debian12:nonroot` (run), both digest-pinned, user `65532` (was `python:3.12-slim` digest-pinned, user `appuser`). | The point of the conversion. |

Unchanged on purpose: the log-event default (`log_event=False`: the Powertools
handler never logs the event; the S3 Lambda still prints `Received event: ...`
exactly as the Python did), the CORS placeholder
`https://your-domain.example`, the 512 MiB / 1024-character limits, the
`/..` substring rule (stricter checks would reject keys the Python accepted),
the Fargate task keeping the downloaded file, the 6 MB routing threshold in
the state machine, and the IAM/ECS recipes.

## Dependencies

Exact versions (`=`) in both manifests; `Cargo.lock` pins the rest. The AWS
SDK crates were chosen at the newest versions compatible with the
`aws-smithy-runtime-api = "=1.19.1"` pin of `toolkit-aws` (the workspace lock
must resolve once): `aws-sdk-s3 1.155.0`, `aws-sdk-dynamodb 1.134.0`.

| Crate | Used by | Why |
| --- | --- | --- |
| `lambda_runtime 1.5.0` | both Lambda binaries (optional `lambda` feature in the processor, so the container build skips it) | The AWS Lambda Rust runtime named by the Issue. |
| `aws-sdk-s3 1.155.0` | processor | `GetObject` streaming (replaces boto3 `download_file`). |
| `aws-sdk-dynamodb 1.134.0` | Powertools handler | Idempotency record `PutItem`/`GetItem`/`UpdateItem`/`DeleteItem`. |
| `aws-config 1.13.0` | both | Already used by `toolkit-aws`; default credential/region chain. |
| `tokio 1.54.1` | both | Already used by `toolkit-aws`; the runtime of `lambda_runtime` and the SDK. |
| `serde_json 1.0.154` | both | Already in the workspace. |
| `sha2 0.11.1` | Powertools handler | Idempotency key hash; already in the workspace lock. |
| dev: `aws-smithy-http-client 1.5.1` (`test-util`), `aws-smithy-types 1.9.0`, `http 1.5.0` | both | `StaticReplayClient`: the real SDK clients run against recorded responses, so request shapes (condition expression, record layout, key encoding) and error mapping are asserted offline. Already in the lock. |

Not added: Powertools for Rust (does not exist), `tracing`/`tracing-subscriber`
(the logger is 150 lines and keeps the Powertools field order), a JMESPath
engine, `chrono`/`time` (the timestamp is formatted by hand and tested at the
epoch, leap days and 2099), `rand` (segment ids use the standard library's
randomly keyed hasher).

## Verification

- Fixtures, all offline (`StaticReplayClient`, in-memory stores, temp
  directories; no AWS, no Docker): `powertools-handler/tests/handler.rs`
  (valid and keyless events, duplicate, in-flight, timed-out and expired
  records, failing function/store, log/EMF/X-Ray output schema),
  `tests/dynamodb.rs` (request bodies and error mapping),
  `scripts/tests/processor.rs` (valid, malformed, boundary and error events for
  both processors, output schema), `scripts/tests/s3.rs` (request, streaming,
  404, unwritable destination), and `crates/serverless-recipes/tests/recipes.rs`.
- Not exercised here and therefore unverified: `cargo lambda build`, the
  `docker build` itself (base-image digests were read from the registries on
  2026-10-29; the Dockerfile's properties are asserted statically), a real
  deployment, a live X-Ray daemon and real DynamoDB/S3 behaviour. No deployment
  or paid call was made.
