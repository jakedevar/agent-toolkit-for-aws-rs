# Slice `ai-models` (Issue #4384): deployable-model discovery, filtering and diagnostics

Rust replacement for six legacy scripts of the `aws-ai-ml` skill (read-only
migration evidence; none is deleted or edited here, and the skill text that
calls them is retired by a later slice after this one is verified, as in
`dynamo-cost`). The `skills/core-skills` and `plugins/aws-core` copies are
byte-identical.

| Python source (under `references/`) | Rust command | Code (`crates/toolkit-cli/src/ai_models/`) |
| --- | --- | --- |
| `model-selection/scripts/get_deployable_models.py` | `toolkit ai-models list-models` | `hub.rs`, `normalize.rs`, `snapshot.rs`, `mod.rs` |
| `model-selection/scripts/filter_deployable_models.py` | `toolkit ai-models filter` | `filter.rs`, `mod.rs` |
| `model-selection/scripts/search_deployable_models.py` | `toolkit ai-models search` | `search.rs`, `mod.rs` |
| `model-selection/scripts/get_model_names.py` | `toolkit ai-models model-names` | `hub.rs` |
| `finetuning-technique/scripts/get_recipes.py` | `toolkit ai-models recipes` | `hub.rs` |
| `endpoint-diagnostics/scripts/collect_diagnostics.py` | `toolkit ai-models endpoint-diagnostics` | `diagnostics.rs` |

There are no Python tests for these scripts. Every expectation in
`ai_models/tests/` is derived by hand from the Python source (arithmetic and
orderings are commented) and from the AWS API reference quoted below.

## The client seam

`client.rs` defines `SageMakerClient`: one `call(Operation, &Json request)`
returning the response object or a `toolkit_aws::AwsError`. The six operations
are `ListHubContents`, `DescribeHubContent`, `Search` (`Resource=HubContent`),
`DescribeEndpoint`, CloudWatch `GetMetricData` and Logs `FilterLogEvents`, each
with the request the Python code passed as keyword arguments.

- `NoLiveClient` (the default, SDK-free build's client) sends nothing and reads no credential;
  every call fails with `invalid client: this build has no live AWS client and
  sent nothing: built without ai-models-live; replay recorded responses with
  --recorded FILE` (exit 1). The staged plugin build enables `ai-models-live`;
  see [shipped toolkit](shipped-toolkit.md).
- `RecordedClient` (`--recorded FILE`, module `recorded.rs`) replays
  `{"calls": {"<Operation>": [{"request": {...}?, "response": {...}} |
  {"request": {...}?, "error": {"kind": "service"|"no_credentials"|"connect",
  "code", "message", "status"}}]}}` in order per operation. A step's `request`,
  when present, must equal the request made, and that check runs **before
  either outcome is replayed**: a mismatching error step is the same runtime
  data error (exit 1) as a mismatching response step, never the recorded
  service error. Every optional member (`request`, `error.kind`, `error.code`,
  `error.message`, `error.status`) is validated for every step and error kind
  at load: absent takes its default, a present one of another type, `null`
  included, rejects the whole recording (exit 1, no success output). A
  recording that runs out, an unknown key or a malformed step is an error
  naming the position (`calls.Search[0].error.code`) and the rule, never a
  value. The file is read with the symlink-refusing, size-bounded reader. The
  accessors and the step replay are the shared `toolkit_aws::recorded` module
  (#4727), also used by the response sites.
- AWS response members follow the same rule through the same accessors
  (`optional_list`, `optional_object_list`, `optional_string_list`):
  `HubContentSummaries`, `Results`, `HubContentSearchKeywords`,
  `ProductionVariants`, `MetricDataResults` (and each result's `Values`) and
  `events` are absent (default empty) or of their type; `null` or any other
  type is a malformed response. `Search` then falls back visibly to the
  complete catalog (`SearchResponseError`); the listing and diagnostics
  commands fail with exit 1 and no success output, also on a later page.
  Nullable scalars (`FailureReason`, `OriginalCreationTime`) and a null or
  empty terminal `NextToken` stay accepted.
- Pagination reuses `toolkit_aws::collect_pages` / `next_token_from` (page and
  item caps, repeated-token detection, tokens never echoed); errors are
  `toolkit_aws::AwsError`, with `ServiceError` carrying the redacted code and
  message.

Issue #4719 adds the third implementation, `live::LiveClient`, enabled with
`cargo build -p toolkit-cli --features ai-models-live --locked`. The feature
enables `toolkit-aws/sdk` and only these service SDKs: SageMaker 1.246.0,
CloudWatch 1.138.0 (already used by docgraph), Logs 1.156.0. Service SDKs have
default features disabled. Default builds gain no SDK dependencies.
`--recorded FILE` always wins, including a missing/malformed file; it never
constructs live clients or falls through to live AWS. Construction is lazy.
Explicit REGION / `--region` passes through; recipes uses the shared region
resolution (environment, then us-east-1).

The shared `toolkit-aws::sdk::service_bridge::ServiceBridge` follows the docgraph S1
experiment: generated SDKs serialize/deserialize with an injected HTTP client,
`noAuth`, no identity cache and SDK retries/timeouts disabled. The toolkit's
`ClientConfig`, `GuardedCredentials` with bounded provider HTTP and null stdin,
`SigV4Signer`, `SdkTransport::for_public_service` with validated roots, gated
public DNS and peer checks,
and `run_with_retry` remain the sole foundations. Each operation has a
60-second deadline across credentials, retries and streamed reads, and a
16 MiB response cap. Credential/transport errors retain their typed classes;
SDK service metadata enters the existing redacting `ServiceError`. Codec and
construction errors use fixed text, never SDK source-chain text. Generated
SDKs and blocking toolkit adapters use separate runtimes to avoid nesting.
The shared endpoint resolver knows SageMaker's `api.sagemaker` control plane;
its signing scope remains `sagemaker`.

`check_search_support` checks the linked SDK's modeled `ResourceType` values
for `HubContent` before Search; the existing visible fallback stays in charge.
The pinned SDK also models `SearchRecord.HubContent`.

## Rust SDK / HTTP support (AC2), checked against primary documentation

Checked on 2026-10-27 against the AWS documentation:

| Fact | Source |
| --- | --- |
| `Search` accepts `Resource` = `HubContent` (the list ends `... PipelineVersion \| Job \| HubContent`); `MaxResults` 1-100; `SortBy` 1-255 chars; `SortOrder` `Ascending\|Descending`; `NextToken` up to 8192 chars; the response is `NextToken` + `Results[]` of `SearchRecord`, with `HubContent` for this resource | <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_Search.html> |
| `SearchExpression`: `Filters`, `NestedFilters`, `SubExpressions` each 1-20 items, `Operator` `And\|Or` | <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_SearchExpression.html> |
| `Filter`: `Name` 1-255, `Operator` one of `Equals, NotEquals, GreaterThan, GreaterThanOrEqualTo, LessThan, LessThanOrEqualTo, Contains, Exists, NotExists, In`, `Value` 1-1024; `In` is a comma-delimited list and `Contains` is limited to once per expression (the reason `search.rs` uses at most one `Contains`, for a single `name`) | <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_Filter.html> |
| `ListHubContents`: `HubName` pattern `(arn:...:hub/)?[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}`, `HubContentType` includes `Model`, `MaxResults` 1-100, `NextToken` up to 8192; the response carries `HubContentSummaries[]` (`HubContentName`, `HubContentSearchKeywords`, `OriginalCreationTime`) and `NextToken` | <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_ListHubContents.html> |
| `DescribeHubContent`: `HubContentName` pattern `[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}`, response `HubContentSearchKeywords` (at most 50, hence the document fallback), `HubContentDocument`; error `ResourceNotFound` (HTTP 400) | <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_DescribeHubContent.html> |
| `DescribeEndpoint`: `EndpointName` pattern `[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}`; response `EndpointStatus`, `FailureReason`, `CreationTime`, `LastModifiedTime`, `ProductionVariants[]` (`VariantName`, `CurrentInstanceCount`, `CurrentWeight`) | <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_DescribeEndpoint.html> |
| The Rust `aws-sdk-sagemaker` client (crate version shown 1.246.0) has `search`, `list_hub_contents`, `describe_hub_content` and `describe_endpoint` | <https://docs.rs/aws-sdk-sagemaker/latest/aws_sdk_sagemaker/client/struct.Client.html> |

Consequences: the identifier checks in `hub.rs` (`1-63` characters, letters,
digits and hyphens, optional Hub ARN) are the documented patterns, so a name
the service would reject is refused before any request, and an endpoint name
can never inject a log-group path.

Additional primary documentation checked for #4719 on 2026-10-29:

| Contract | Source / live test |
| --- | --- |
| GetMetricData takes MetricDataQueries (metric, dimensions, period, statistic), inclusive StartTime and exclusive EndTime. Results include Id, Values, Timestamps, StatusCode and optional NextToken. The adapter preserves the legacy one-page read and exposes Id/Values to diagnostics. | [API reference](https://docs.aws.amazon.com/AmazonCloudWatch/latest/APIReference/API_GetMetricData.html); `diagnostics_maps_endpoint_metrics_and_logs_without_extra_pages` |
| FilterLogEvents uses logGroupName, startTime/endTime in epoch milliseconds and limit (1–10000); events include message/timestamp. An empty page may have a nextToken. The legacy one-page, limit-100 behavior remains deliberate. No unmask/filter/sort flag is added. | [API reference](https://docs.aws.amazon.com/AmazonCloudWatchLogs/latest/APIReference/API_FilterLogEvents.html); same diagnostics test |
| SageMaker uses api.sagemaker endpoints; CloudWatch uses monitoring. | [SageMaker endpoints](https://docs.aws.amazon.com/general/latest/gr/sagemaker.html), [CloudWatch endpoints](https://docs.aws.amazon.com/general/latest/gr/cw_region.html); endpoint and wire tests |

The pinned CloudWatch SDK uses Smithy rpc-v2-cbor at
`/service/GraniteServiceVersion20100801/operation/GetMetricData` (CBOR tag-1
timestamps in seconds); SageMaker and Logs use AWS JSON 1.1. Verified against
generated SDK operation serializers and independent offline wire fixtures.
The test-only `minicbor` dependency (2.3.1, already in the SDK dependency tree)
encodes/decodes the independent CBOR fixtures. `make ai-models-test` runs the
unchanged default/offline assertions; `make ai-models-sdk` runs the feature's
139 unit tests (including 12 new wire/selection/error contracts) and three
recorded process tests, without AWS or real credentials.

## Parity table (Python behavior -> Rust)

| Area | Python behavior kept | Rust location / evidence |
| --- | --- | --- |
| Size buckets | `<=10b`, `11b-70b`, `71b-100b`, `>100b`, `unknown`; ranges classified by upper bound, `>Nb` is the largest, `rstrip("b")`, unparseable is `unknown` | `normalize_size_bucket`; 29-row table test |
| Normalized shape | keyword prefixes -> fields in dict insertion order, lists append, scalars replace in place, `bedrock_eligible` / `customization_eligible` always present | `normalize_hub_content`; exact `json.dumps` golden |
| Keyword cap | a keyword is authoritative; `ModelSize` / `ContextWindow` of `HubContentDocument` (string, or first non-empty string of a list, lower-cased) fill what the 50-keyword cap dropped | `document_scalar`; 4 tests |
| Creation time | `OriginalCreationTime` kept as text; epoch seconds -> `isoformat()` UTC | `original_creation_time` |
| Pagination | `ListHubContents` with `MaxResults` 100, `NextToken` until empty | `list_hub_contents` (shared `collect_pages`); request-checked 2-page test |
| Values / snapshot | 11 sorted distinct-value lists; snapshot members and order, `raw_sizes_by_bucket`, `original_creation_times`; every `validate_catalog_snapshot` rule and message; mode 0600 even for an existing file; temp file `sherpa-model-catalog.*` removed on a failed write | `snapshot.rs`; 14 message cases |
| Filter semantics | substring (lower-cased) for task/data_type/modalities/license/language/provider/name, exact for size/context_window/model_type, last value wins for the two booleans, missing metadata excludes, OR within a key, AND across keys | `filter.rs` |
| Order | dated models by `original_creation_time` descending, ties and undated by lower-cased name; the input is not mutated | `sort_models_newest_first`; 7-model test with a prefix-length trap |
| Constraint words | `key:value` split on the first colon, key trimmed, value trimmed and lower-cased, de-duplicated, 13 supported keys, unsupported key / empty value / missing colon refused | `parse_constraints`; refusal tests |
| Search expression | HubName + HubContentType filters; metadata filters with all casings of free-text values and one upper-case enum; keyword tags resolved against the catalog (both `Apache 2.0` and `Apache-2.0`); size bucket -> every raw label; capability `true` only; name as one `Contains` with the shared catalog casing, else exact names | `build_search_expression`; golden JSON |
| Limits | `In` values packed to 1024 characters, a value with a comma gets its own `Equals`, more than 20 alternatives nest `Or` trees of at most 20 | 250-value test over the documented limits; validator tests |
| Search paging | `MaxResults` 100, `SortBy` `HubContentName` ascending, a repeated name kept once, a repeated token is an error | `search_deployable_models` |
| Date restore | Search `CreationTime` dropped, the snapshot's `OriginalCreationTime` attached, search-only models undated and warned about with the same sentence | `restore_catalog_order` |
| Fallback | unsupported SDK / service / transport / malformed response -> complete local filter with `retrieval_backend: "list_fallback"`, `fallback_reason {type, message}`, the same warning sentence and the snapshot timestamp source; an empty successful Search is authoritative; a constraint or snapshot error is never hidden | `select_public_hub_models`; 12 tests |
| Output | `total_models`, `filters_applied`, `matched`, `catalog_source`, then the selection members in dataclass order (`fallback_reason` last, omitted when absent) | `search_command`; order assertion |
| Model names | customization-capable only, sorted case-insensitively, a missing name sorts as `""` | `customization_model_names` |
| Recipes | `@recipe:finetuning_<technique>_...` -> `sft`, `dpo`, `rlvr`, `rlaif`, sorted, unique | `finetuning_techniques` |
| Diagnostics | the 7 invocation metrics (60 s periods, `Sum`/`Average`/`p99`), the 4 instance metrics only when the variant is known, `AllTraffic` default, the three warning sentences, one `GetMetricData` call, 15-minute trailing log window (absolute windows capped), one `FilterLogEvents` page of 100, `str(datetime)` timestamps (`None` when missing), a service error of one step is listed and the next step runs | `diagnostics.rs`; request-level assertions |

## Intentional changes (recorded per the worker contract)

1. **Command shape.** Each script is a `toolkit ai-models` subcommand with the
   same arguments. Python accepted `--snapshot-file` / `--list-values` /
   `--create-snapshot` anywhere in the word list; the router does the same. The
   router rejects unknown flags and any value that is empty or starts with `-`
   as a usage error (exit 2) where Python printed its usage line and exited 1.
   `collect_diagnostics` had no command line (it was imported); its parameters
   are the positionals ENDPOINT REGION and the options below.
2. **Opt-in live client; `--recorded FILE` added.** Every AWS call goes through the
   client seam; the default build sends nothing. Without `--recorded` a default-build call fails
   with exit 1, so the commands that need only a snapshot (`filter`, and
   `search` with `--snapshot-file`, which falls back visibly) work offline.
   With `ai-models-live`, an unrecorded command uses AWS. NoLiveClient and
   RecordedClient keep their contracts. Default-build tests run in that build;
   recorded tests run in both. SDK response normalization applies only to live
   responses (modeled absent/null lists become empty); strict raw JSON recording
   validation is unchanged. The bounded credential/deadline/size policy above
   replaces Python tracebacks and unbounded response buffering on the live path.
3. **Error text.** Messages follow the shared `error: <message>` contract on
   stderr. AWS failures keep the Python wording (`AWS API call failed (CODE):
   MESSAGE`, `No AWS credentials found. Configure credentials via 'aws
   configure' or environment variables.`, `Could not connect to SageMaker
   endpoint: ...`) with the AWS-derived parts escaped onto one line. Scripts
   that crashed with a traceback on an AWS error (`get_model_names`,
   `get_recipes`) now report the same wording and exit 1; `get_recipes` on an
   unknown model reports the `ResourceNotFound` service error.
4. **Fail closed.** (a) A `bedrock` / `customization` filter value other than
   `true` / `false` is an error (Python ignored it and kept every model). (b)
   Hub, model and endpoint names are checked against the documented patterns
   before any request and are never echoed. (c) A mistyped member of a model
   (a string where a list belongs, a non-boolean eligibility flag, a
   non-string creation time) is an error naming the model's position, where
   Python raised or iterated characters. (d) `--minutes` must be 1-525600; a
   window must start before it ends; `--minutes` cannot be combined with
   `--start`. (e) A models file must be a JSON list of objects; duplicate
   object keys in any JSON input are an error (Python kept the last).
5. **Diagnostics stream.** Warnings of `filter` quote the offending word
   (`Warning: Ignoring malformed constraint (no ':' separator): "x"`). Every
   JSON value leaves as ASCII `\uXXXX` escapes (`json.dumps` default), so a
   Hub value or log line cannot drive a terminal.
6. **SDK version gate.** Python refused boto3/botocore older than 1.43.100 for
   `Search(Resource="HubContent")`. Rust has no such split: the client reports
   through `check_search_support`, and a client that cannot issue HubContent
   Search produces the same `UnsupportedSdkError` fallback. The live binding
   decides what it returns.
7. **Time.** `endpoint-diagnostics` computes the window in whole microseconds
   from `--now` (default: the system clock) so a test can pin it; `--start` /
   `--end` take ISO 8601 text (naive times are UTC). Snapshot `fetched_at`
   comes from the system clock (`datetime.now(timezone.utc).isoformat()` form).
8. **Deterministic ties.** `raw_sizes_by_bucket` sorts case-insensitively with
   plain order as the tie-break (Python's order of `7B` / `7b` came from set
   iteration). Page and item caps of `collect_pages` (1000 pages, 100000 items)
   bound a runaway listing.
9. **Documents.** `HubContentDocument` is parsed with the strict shared JSON
   parser; a document with duplicate keys or other invalid JSON is ignored,
   like any unparseable document.
10. **Paths.** The models file, snapshot and recording are read with the
    symlink-refusing bounded reader (64 MiB); a snapshot is written to a
    regular file only (a link, even a dangling one, is refused) with owner-only
    permissions before any content is written. A snapshot path is caller-owned
    and kept; a `--create-snapshot` file is created in the temporary directory
    and its path printed, as before.
11. **Model identity.** The slice handles SageMaker Hub content names, not
    Bedrock model identifiers; no `bedrock_resolve` module exists at this base,
    so there is no Bedrock identity to reuse. An unknown or ambiguous name is
    never guessed: it is refused (`name`, `task`, ... constraints that match
    nothing in the catalog), or reported by the service (`recipes`).

## Verification

Recipe jobs (`fmt`, `clippy`, `workspace-test`) are diagnostic only. Trusted
candidate evidence is the committed-source `scoped_test` owned by the
manager/harness. `crates/toolkit-cli/tests/diag_contract.rs` runs every
registered command with hostile arguments and payloads and covers all six
commands without change: none uses `repeatable_flags` (the Python scripts take
positional words, not `action="append"` flags).

## Follow-ups

- #4719 supplies the opt-in live binding. Security review and exact-SHA
  integration are required before delivery; no live AWS probes are test gates.
- Retire the six Python scripts and update the skill text that runs them
  (`model-selection/overview.md`, `references/select-for-deployment.md`,
  `finetuning-technique/overview.md`, `endpoint-diagnostics/overview.md`, in
  both trees) to `toolkit ai-models ...` after review of this slice.
