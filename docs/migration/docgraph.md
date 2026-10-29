# docgraph: DocumentDB and Neptune analysis and data utilities (#4395)

Slice `docgraph`, wave 3. Ports the scripts of `amazon-documentdb` and
`amazon-neptune` (`skills/specialized-skills/database-skills/`).

**Prior art checked.** `git log -S` on `wa_review`, `input_validator`,
`docgraph` and the Neptune script names shows only the original skill imports
(#97, #219) and the programme documents (#4369, #4371); no part of this slice had
landed on `origin/rolling` (`b8204e9d`).

## Commands

| Command | Replaces |
| --- | --- |
| `toolkit documentdb wa-review --cluster-id ID --region R [--recorded FILE] [--analysis-data FILE] [--output DIR] [--now TS]` | `amazon-documentdb/scripts/wa_review.py` |
| `toolkit neptune validate-input [KEY=VALUE...]` | `amazon-neptune/scripts/input_validator.py` |
| `toolkit neptune seed [--confirm-wipe] [--endpoint HOST] [--recorded FILE \| --dry-run]` | `amazon-neptune/scripts/seed_test_data.py` |
| `neptune_test_env.cfn.json` (declarative, no command) | `amazon-neptune/scripts/cdk_test_env.py` |

Each command is one registration line in `lib.rs` (`docgraph::WA_REVIEW`,
`NEPTUNE_VALIDATE_INPUT`, `NEPTUNE_SEED`, defined in `docgraph/mod.rs`) plus its
help text.

No command opens a socket or a database connection, reads a credential, runs
Python or creates an environment. Every AWS or database interaction is a trait
(`docgraph::client::AwsClient`, `docgraph::seed::GremlinSession`) with a recorded
implementation (`--recorded FILE`) and a "no live client" implementation that
sends nothing.

## Recorded input

Recorded AWS and Gremlin responses are read only through `toolkit_aws::recorded`
(`step_fields`, `Step`, `StepQueue`, `KeyOrder::Ignored`, the `optional_*`
accessors); `tests/recorded_contract.rs` detects the slice (flag and import
signals) and passes. The two helpers that read a snapshot file's list or object
(`snapshot::list`, `snapshot::members`) carry a reasoned `recorded-shape:`
marker; the recording and response members are never read by shape.

`wa-review --recorded`: `{"calls": [{"service": "docdb|cloudwatch|ec2|secretsmanager",
"operation": "DescribeDBClusters", "request"?: {...}, "response": {...} | "error":
{"code"?, "message"?, "kind"?: "client|connect|no_credentials"}}]}`. A call
matches the first entry with the same service and operation whose recorded
`request` equals it (key order ignored); an entry without a `request` answers any
request of its operation. Metric requests are fully specified (`Namespace`,
`MetricName`, `Dimensions`, `StartTime`, `EndTime`, `Period`, `Statistics`), so
pin the instant with `--now`. A call with no matching entry fails like an
unreachable service (`aws_connect`) and the check that made it becomes a warning,
exactly as a failed boto3 call did. A present member of another type, `null`
included, rejects the recording; the reason names the entry index and the member,
never a value.

`seed --recorded`: `{"calls": [{"request"?: {"gremlin", "bindings"?}, "response":
{"results": [...]} | "error": {"message"?}}]}`, one entry per statement in the
order sent. The request is compared before the outcome (a failure entry too), a
statement past the last entry fails, and a recording with unused entries is
rejected after the run.

`wa-review --analysis-data`: the JSON the script read (`{database: {collection:
{count, size, avgObjSize, indexes: [...], compression, unusedStorageSize}}}`).
Members absent take the script's default; a present member of another type is
rejected with ordinals (`database 1, collection 2`) and the member name, never a
name or value of the file.

## Acceptance criteria

- **AC1 Well-Architected outputs and graph input validation preserve fixture
  behavior.** `docgraph/tests/review.rs`: the check ids in the script's order
  (healthy cluster, degraded cluster, thresholds at both sides, P95 index,
  idle reader, pagination, unknown instance class), the 11 database checks
  (`database_checks_follow_the_script`, five-item cap), exact `wa_review_results.json`
  layout (`json.dump(indent=2)`) and Markdown layout (pillar order, `|` -> `/`,
  `Other` counted but not listed), the printed summary lines. The class tables are
  compared with the script's literals. `docgraph/tests/neptune.rs`: every
  validator pattern with boundary and hostile values (63/64, 255/256 characters,
  9/10 characters, trailing newline, ARN partitions, S3 bucket 2/3/63/64), the
  script's messages, last-duplicate-wins and stop-at-first-bad-argument.
- **AC2 graph seeding and CDK environments receive reviewed replacements.**
  Seeding is `toolkit neptune seed` (same statements, bindings, wipe guard,
  verification and assertions; `--dry-run` prints the statement plan). The CDK
  stack is `skills/.../amazon-neptune/scripts/neptune_test_env.cfn.json`, a
  CloudFormation template (VPC with two private and two public subnets, one NAT,
  security group open on 8182 to the VPC CIDR only, a Neptune Serverless
  cluster 1-4 NCU with encryption at rest and IAM authentication, deletion-safe
  defaults (decision 9), the `created_by` / `generation_model` tags, the seed
  Lambda role with explicit `neptune-db` actions, the five outputs).
  `docgraph/tests/template.rs` asserts those properties from the file, and that
  no `Action` anywhere holds a wildcard. Nothing deploys it.
- **AC3 no paid environment creation in default tests.** No test (and no
  command) creates, deploys or contacts anything; the template is only parsed.

## Intentional changes from the Python

1. **Offline only.** No live AWS client or Gremlin driver exists in this build
   (as for `bedrock-agent`, `ai-models`, `elasticache`). Without `--recorded`,
   `wa-review` reports `Cannot describe cluster: aws_connect` (a fail check, exit
   0 like the script) and a stderr note; `seed` fails with `no live Gremlin
   client` after the wipe guard. Live bindings are the follow-up.
2. **`--uri`, `--tls-ca-file`, `--tls-allow-invalid-certs` are not accepted.** They
   belong to the `pymongo` collection of `collStats`/`$indexStats`; a
   secret-bearing option that does nothing would only mislead. Use
   `--analysis-data`. The script's text `(no --uri or --analysis-data)` is kept.
3. **Failure text.** A failed AWS call names its class, `aws_service_error
   (AccessDenied)` or `aws_connect`, never the service's message or any response
   value (the script printed `{e}`). A response member of the wrong type fails
   the check that reads it (`invalid response (X must be a list when present (it
   is null))`) instead of raising a traceback or defaulting. A malformed
   instance or cluster entry that the script would have crashed on is a
   `fail`/`warn` check.
4. **Text from the account is escaped and redacted.** Response and snapshot text
   that reaches a label or detail (instance ids, engine version, event message,
   collection names) goes through the shared redactor and has control,
   bidirectional and invisible characters escaped. The failover event message is
   still limited to 120 characters, but the whole message is redacted first,
   then cut, then escaped (#4908): userinfo whose `@host` lies past the bound,
   or an access-key id the bound would cut in half, is still recognized. A
   message without credential-shaped text shows the same 120-character prefix
   as the script.
5. **Arguments are validated.** `--cluster-id` must be a DocumentDB identifier
   (starts with a letter, letters/digits/hyphens, 63 at most) and `--region` the
   shared region shape, both usage errors (exit 2) before any call, so a hostile
   value never reaches a file or a terminal. `--output` is created as needed, as
   `mkdir(parents=True)` did. `--now TS` (ISO UTC) is new: it pins the review
   instant (and with it the metric windows and the report timestamp) so a recording
   is deterministic; the default is the system clock.
6. **`validate-input`** shows a rejected value (and an unknown key or a
   malformed argument) redacted in full, then bounded and escaped (the script
   printed it raw; #4908), and matches the whole value (the script's `$`
   also matched before a trailing newline; `\d` meant any Unicode digit, here
   ASCII). The region rule is the shared `is_valid_region` (it also bounds the
   length at 32). `+12` is shown as `12` in the range message as before.
7. **`seed`** takes the endpoint from `--endpoint HOST` (default `localhost`,
   host-name characters only) instead of `NEPTUNE_ENDPOINT`; the confirmation
   stays `--confirm-wipe` or `NEPTUNE_SEED_CONFIRM_WIPE=1`. The guard is checked
   after the `Connecting...` line and before any statement, as `clear_graph`
   raised before any query. The `time.sleep(1)` after the wipe is dropped (no
   session to wait on). Results echoed by the verification are escaped.
   A failed verification is an `error:` line and exit 1 (the script's
   `AssertionError` traceback). `--dry-run` is new.
8. **`json.dump` parity.** Non-ASCII text in a label is written as UTF-8, not
   `\uXXXX` (`ensure_ascii`), and a float prints as Rust's shortest form.
   Neither occurs in the check texts; stated for completeness.
9. **`cdk_test_env.py`.** The CDK stack was a throwaway (deletion protection
   off, destroy on removal). Review #4904 requires deletion-safe defaults
   (#4906), so the template now defaults to them: the cluster has
   `DeletionPolicy` and `UpdateReplacePolicy` `Snapshot` and
   `DeletionProtection` from the `DeletionProtection` parameter (allowed
   `true`/`false`, default `true`); the instance (no storage of its own, and
   `Snapshot` is not a policy `AWS::Neptune::DBInstance` supports) is
   `Retain` by default. Throwaway teardown is only the explicit non-default
   `DeletionProtection=false`: it lifts protection and switches the instance
   policies to `Delete` through the `TeardownAllowed` condition (the
   `AWS::LanguageExtensions` transform, which allows `Fn::If` in those
   attributes); stack deletion still leaves a final cluster snapshot, to be
   deleted by hand. `grant_connect` (#4905) becomes an explicit list scoped to
   this cluster's resource id, with no wildcard:
   - `neptune-db:connect`: open the Gremlin WebSocket (IAM auth);
   - `neptune-db:ReadDataViaQuery`: the verification reads (`g.V().count()`,
     `has(...)` traversals);
   - `neptune-db:WriteDataViaQuery`: `addV` / `addE` / `property` seeding;
   - `neptune-db:DeleteDataViaQuery`: the guarded wipe `g.V().drop()`.

   `neptune-db:GetEngineStatus` and `neptune-db:GetQueryStatus` are left out:
   neither the seed nor the validation flow reads engine or query status
   (`validate-input` makes no call at all); a live binding that needs them
   (#4886) adds them by name.

   Fine-grained actions need engine 1.2.0.0 or later (Neptune Serverless
   already requires it); on older engines `neptune-db:connect` alone granted
   all data access. The attached managed policy
   `AWSLambdaVPCAccessExecutionRole` grants only CloudWatch Logs writes and the
   EC2 network-interface calls a VPC Lambda needs, no Neptune data action.
   `template.rs` fails on any `Action` (anywhere in the template, string or
   list) holding `*` or `?` (the bare `*`, `svc:*`, `svc:Get*`), on any
   non-string action and on any `NotAction`, with a negative test of each
   shape. The CDK `generation_model` stack argument becomes the
   `GenerationModel` parameter. The template has not been deployed in this
   slice (no paid environment); a reviewer can lint it with `cfn-lint` or
   `aws cloudformation validate-template`.
10. **Caller paths are redacted before they are shown** (#4907, #4941). Every
    path this slice prints (`--recorded`, `--analysis-data`, in the progress
    line and in read or parse errors, and the `Results:` / `Report:` banners
    under `--output`) goes through the shared `safe_text::safe_path`: redacted
    in full (`toolkit_aws::redact_text`: URL userinfo, labelled credentials,
    key ids), then bounded to 200 characters with `...`, then escaped. Response
    text in labels uses the same helper, validator values `safe_text::safe_text`
    and the failover message `safe_text::safe_prefix` (120 characters, no mark).
    The slice-local `clean`, `clean_prefix`, `first_chars`, `shown_path` and
    `neptune_input::shown` helpers were removed for these shared ones (#4941).
    The file reader is `startup::read_text` with that redacted label (the
    shared `read_json_document` escapes but does not redact its label). A path
    without credential-shaped text and of at most 200 characters is shown as
    given.

## Not in this slice

Tracked in #4886: `graphrag_pipeline.py` and `agentic_memory.py` (helper
libraries), the live AWS / Gremlin / MongoDB bindings, and retiring the Python
files and updating `SKILL.md` / `references/well-architected.md` callers. The
Python stays in place until then: the Rust commands cannot reach AWS or a
database yet, so replacing the callers now would remove a working path.

## Live adapter design (#5318; #4886 item 2)

Design proposal, 2026-10-29. This section changes no executable behavior,
dependencies or permissions. The offline-only statements above still describe
this checkout. A manager-owned cross-family pre-review must accept this design
before implementation exposes networks or credentials; exact-source review and
integration remain separate gates. No live run is authorized by this document.

### Evidence and recommended decisions

The source baseline is `f3ab564ce8b2ba77e198b014e7d1e1836c2372d9`.
Read `docgraph/{mod,client,infra,seed,snapshot,dbchecks}.rs`, the original
`wa_review.py`, `seed_test_data.py`, `graphrag_pipeline.py` and
`agentic_memory.py`, and `toolkit-aws`'s `sdk`, `sdk/source`, `sdk/roots`,
`sdk/roots_payment`, `transport`, `client`, `config`, `binding`, `retry` and
`stream` modules. P-ROOTS has implementation in `sdk/roots_payment.rs` and
contract tests in `tests/payment_roots.rs`. The accepted item-1 helper source
is inspected at `990c9008a8a9d171e484f8954ef43e322db75ac4` with `git show`:
`docgraph::memory::{MemoryClient,RecordedMemory,plan,retrieve,execute}` and
`NEPTUNE_MEMORY` registration. It is absent from this checkout; implementation
must start after its integrated equivalent, without copying a second builder.

Recommend optional live adapters over the existing traits, explicit `--live`
selection, mandatory IAM signing, one shared AWS credential implementation,
bounded requests, and a declarative user-run shell collection recipe feeding
the existing analysis snapshot. Keep all three legacy URI/TLS flags refused.
Retain Python until replacements, callers and contract tests are verified.

The following are engineering recommendations for pre-review, not requests
for operator decisions: adapter structure, retry limits, timeout defaults,
root-policy separation, and a narrowly scoped private-endpoint capability.
The PM must approve the collection workflow and CLI divergences. Credentials,
account access, spending and permission to touch a real database remain operator
gates even after design acceptance.

### Requirements and contract tests

Each `T-DG-*` below is a required future Rust behavioral test, except the
explicitly labelled operator live cases. These are a test plan, not claims
that tests were added or run in this design-only change.

**R-DG-01 — Selection and offline compatibility.** Add non-default
`docgraph-live` support implying the existing CLI `aws-sdk` feature; default
workspace builds remain SDK-free. Merely compiling the feature never contacts
anything. `--recorded`, `--analysis-data` and `--dry-run` retain current schemas,
output, exit statuses and redaction. A proposed `--live` chooses adapters;
reject `--live` with `--recorded` or `--dry-run` before credentials, DNS or
output-file creation. Live infrastructure plus `--analysis-data` is valid.
Without explicit live selection, preserve the present no-live behavior.

- T-DG-01a: run existing golden review/seed/memory replay fixtures under both
  feature configurations; identical output and exit status, zero provider,
  resolver and socket calls even with hostile AWS configuration present.
- T-DG-01b: forbidden mode combinations and unavailable-feature `--live`
  return usage errors (exit 2) before side effects; feature alone sends nothing.
- T-DG-01c: malformed/null response members, request mismatch, error steps and
  unused seed/memory recordings retain their existing failure contracts.

**R-DG-02 — Credentials, region and logging.** Keep `wa-review --region`
required. Add optional `--region` for live seed/memory, resolved by the shared
`resolve_region`: explicit, `AWS_REGION`, `AWS_DEFAULT_REGION`, `us-east-1`.
`AWS_PROFILE` selects credentials only; a profile region never changes signing
region. Reject a target/region mismatch before signing. Do not add credential
arguments or read secret values from Secrets Manager. Use
`sdk::source::GuardedCredentials` and `ProviderHttp::validated`, preserving
R-ABS/R-SRC source precedence, configured-source failure terminality, process
stdin isolation and fixed credential-error classes. All live calls require
credentials: genuine absence is an error, never an unsigned fallback. The
existing sources include env, profiles, SSO/login, process, assume-role, web
identity and workload/instance roles; reuse their builders solely in
`sdk/source.rs`. Refresh expired credentials before a new request/upgrade;
never replay a submitted mutation to repair authentication.

Keep R-LOG's absence of a tracing subscriber/logger and allowlisted diagnostics.
Never format SDK, driver, WebSocket or provider errors with Debug or forward
their arbitrary messages. Classify them into fixed errors, then apply existing
safe-text helpers to permitted labels. No authorization headers, tokens,
queries, bindings, URI or response-body logging; no automatic live recordings.

- T-DG-02a: synthetic providers/configurations cover absence, partial env,
  invalid selected profile beside a valid fallback, SSO/login/process failure,
  expiry, refresh failure and recovery; no request on failure or identity
  fallback. Exercise the real guarded source with injected provider HTTP.
- T-DG-02b: region precedence, explicit mismatch, valid profile with a different
  region and credential-argument rejection pin signing scope and zero-send errors.
- T-DG-02c: bare secret canaries in provider stderr, network errors and service
  messages never appear in stdout/stderr, reports, Debug or diagnostics, including
  attempts to enable logging through `RUST_LOG`.

**R-DG-03 — TLS and endpoint admission.** Reuse the P-ROOTS infrastructure
and its separation of trust policies, without reusing its payment-only policy
as a database policy. In the landed code `PaymentRoots` deliberately excludes
caller root overrides; AWS/provider traffic uses `roots::ValidatedRoots`
(R-ROOTS), which validates configured `SSL_CERT_FILE`/`SSL_CERT_DIR` once,
fails closed on broken configured roots and disables implicit native-root
fallback. Use that AWS policy for the provider, service and Gremlin planes,
passing one immutable validated root set to explicit rustls/aws-lc contexts.
Use the shared DER certificate access for the WebSocket context; do not add
another root loader or silently convert a configured root set to PaymentRoots.
`AWS_CA_BUNDLE`, proxies, redirects, plaintext, URI userinfo and disabling
certificate/hostname verification remain unsupported. A private CA is installed
through R-ROOTS configuration, not a new `--tls-ca-file` flag.

`SdkTransport::new` currently guards public DNS answers, literal hosts and
socket peers. `EndpointPolicy::without_dns_check` does **not** disable those
connect-time checks. Neptune DB and private Analytics need a reviewed extension,
not an unguarded injected connector. Propose a capability constructed from an
explicit `--network-policy FILE`: exact AWS database host, fixed port, region,
and narrowly listed RFC1918/IPv6 ULA CIDRs supplied by the operator. Validate
every DNS answer and actual connected peer against it before transmitting signed
headers; connect using only the validated answers, retain the original hostname for SNI/verification/signing,
and revalidate on reconnect. Refuse loopback, link-local/metadata, unspecified,
multicast, mixed permitted/forbidden answers, arbitrary CNAME redirection to
unpermitted peers, custom URL paths and wildcard host rules. This capability
is limited to the selected database target; it never changes provider HTTP,
payment roots or the default public guard. Generic endpoint environment
overrides cannot grant private access. Start with standard AWS partition
endpoint forms; unsupported/custom/FIPS forms fail explicitly until reviewed.

- T-DG-03a: reuse R-ROOTS fixtures for configured, platform, broken, empty and
  mixed root locations; real in-memory TLS handshakes accept the intended CA
  and reject untrusted, expired and wrong-host certificates for each connector.
- T-DG-03b: P-ROOTS tests still prove payment roots ignore overrides; database
  configuration has no effect on payment trust. Both AWS planes receive the
  intended immutable R-ROOTS set; no implicit root loader is constructed.
- T-DG-03c: injected resolution/connection covers public and approved private
  targets, rebinding, mixed answers, IPv4-mapped IPv6, forbidden peers, malformed
  policy and region/host/port mismatch; rejected targets receive no signed bytes.
- T-DG-03d: wrong scheme, userinfo, proxy env, redirect and generic endpoint
  override attempts cannot widen admission or forward credentials.

**R-DG-04 — DocumentDB AWS operations and response mapping.** Implement one
`LiveAwsClient: docgraph::client::AwsClient`. Keep `infra.rs` check orchestration
and pagination ownership. Allow only these service/operation pairs:

| Seam service | Allowed operations | Wire/signing responsibility |
| --- | --- | --- |
| `docdb` | `DescribeDBClusters`, `DescribeDBInstances`, `DescribeEvents`, `DescribeDBClusterParameters`, `DescribeDBSubnetGroups`, `ListTagsForResource` | DocumentDB Query protocol, endpoint prefix/signing name `rds` |
| `cloudwatch` | `GetMetricStatistics`, `DescribeAlarms` | Pinned SDK's modeled protocol (current model prefers RPC-v2 CBOR and also lists JSON/Query), endpoint prefix/signing name `monitoring` |
| `ec2` | `DescribeSecurityGroups` | EC2 Query protocol, signing name `ec2` |
| `secretsmanager` | `ListSecrets` | AWS JSON protocol, signing name `secretsmanager`; metadata only |

Protocol/scope evidence: the official
[DocumentDB model](https://github.com/boto/botocore/blob/develop/botocore/data/docdb/2014-10-31/service-2.json)
and [CloudWatch model](https://github.com/boto/botocore/blob/develop/botocore/data/cloudwatch/2010-08-01/service-2.json).
Do not assume every service speaks JSON or that its CLI name is its signing name.

Recommend optional, individually scoped official service SDK crates behind the
live feature for modeled Query/XML, JSON and CBOR conversion, with the existing
Signer/Transport used through a shared Smithy HTTP bridge. Pin compatible
versions during implementation, documenting licence, MSRV and lockfile delta.
The SDK request serializer/endpoint rules are useful; a second credentials,
TLS, logging or retry stack is not. Configure generated clients to one attempt
with toolkit signing as the sole signer, and bridge generated HTTP requests
through toolkit `Signer`/`Transport`; prove no generated signer/provider is built.
Return bounded raw HTTP responses, including non-2xx responses, to the modeled
decoder. The JSON-only `ApiClient::execute_json` and its generic error parser
cannot decode Query/XML or CBOR. At the typed-call boundary use the shared
`run_with_retry`/`RetryPolicy` primitives after modeled error classification,
not an additional `ApiClient` retry layer. Each retry reconstructs and signs
one modeled request. This bridge needs explicit cancellation, size limits and
connect-time admission from S1; these are not claimed as existing SDK behavior.
If that integration cannot meet the gates, revise the design before adding
clients; do not hand-roll four XML protocols as an unreviewed fallback.

At the seam, preserve PascalCase JSON names, scalar kinds, list order, UTC
timestamps and `Marker`/`NextToken` behavior used by fixtures. Omit absent
optional values rather than manufacturing null; an explicitly malformed wire
response is `InvalidResponse`, not an empty successful result. Map only fields
consumed by the checks, with a source-to-seam field table and fixture for each
operation. Discard request metadata and arbitrary error messages. Service
errors become `AwsError::Service` with safe code/status, transport/credential
failures keep their typed classes; existing `Fail::describe` controls check
labels and exit-0 review results. Invalid startup config is an error before
the review, rather than a misleading report of successful collection.

Use shared `RetryPolicy` as the sole retry owner for these read-only calls:
three retries maximum (four attempts), fresh signature per attempt, full jitter
and the current 20-second backoff cap. No retry of access denial, bad config,
credential refusal or invalid responses. Keep each request's existing metric
window and one page per seam call; generated paginators must not swallow tokens.

- T-DG-04a: all ten operation encodings are captured by an injected HTTP
  transport: correct protocol, endpoint, signature scope, filters, timestamps,
  list ordering and pagination tokens; unknown pairs fail before transport.
- T-DG-04b: modeled successful responses become the same seam objects and
  final reports as existing fixtures, including zero/false versus absent;
  XML/JSON/CBOR truncation, wrong types and service errors cannot become success.
- T-DG-04c: throttling/5xx/connect/timeout recover or exhaust at four sends;
  fresh signatures and exact injected delays prove there is one retry owner.
  AccessDenied, malformed responses and credentials yield one or zero sends.
- T-DG-04d: metadata-only Secrets Manager access never constructs
  `GetSecretValue`; existing repeated-token/page-cap failure behavior survives.

**R-DG-05 — Gremlin WebSocket adapter.** Implement a live
`seed::GremlinSession` and reuse it for the memory Gremlin request kind. The
legacy serializers are GraphSON v2; begin with that wire contract and a small,
tested typed-value codec over an optional maintained Rust WebSocket crate,
using the workspace rustls/aws-lc stack. Dependency selection/pins require
pre-review; do not add a second general graph-query builder.

Connect to `wss://HOST:8182/gremlin`, traversal source `g`. Construct the HTTP
GET upgrade for that same authority/path, sign its HTTPS representation with
`SigV4Signer`, service `neptune-db`, selected region and empty body. Preserve
the authority including port, `x-amz-date`, temporary security token and signed
header values in the WebSocket handshake. Let the handshake layer generate
Upgrade/Connection/Sec-WebSocket headers after signing: the shared signer
intentionally refuses caller framing headers. Test that these additions do
not alter signed fields. Do not pass a `wss` URL to `SdkTransport` or assume
its request/response interface can expose an upgraded socket. AWS documents
the signed Gremlin upgrade and credential refresh in its
[IAM connection example](https://docs.aws.amazon.com/neptune/latest/userguide/gremlin-go-iam-auth.html).

Use unique request IDs, bound values, one in-flight request and ordered
submission. Validate the 101 handshake, request-ID association and Gremlin
status; collect all partial (206) result batches through terminal success
(200/204), with bounded typed GraphSON decoding, including bulk counts and
maps required by memory. Unsupported types, foreign IDs, malformed/oversized
frames, premature close and server errors fail the operation. Close on every
exit and bound close time. Re-sign every new connection; refresh before expiry
between operations, never reconnect/resubmit an in-flight statement. A drop
after send has an unknown outcome and terminates the remaining plan. Preserve
the seed wipe guard, statement/binding order, counts/assertions and exit 1 on
verification failure. Check confirmation before provider/DNS/socket work.

- T-DG-05a: a local in-memory handshake fixture verifies canonical GET, host
  and port, path, credential scope, date/token, empty payload and fresh signing
  on reconnect; mismatched signature/403 sends no Gremlin frames.
- T-DG-05b: GraphSON v2 fixtures cover seed scalar counts, memory maps/lists,
  typed numeric values, bulk results, empty 204, ordered 206/200 batches,
  unrelated IDs, server failure, invalid types and frame/response limits.
- T-DG-05c: unconfirmed seed sends nothing; confirmed seed preserves every
  statement and verification. Disconnect after a mutation is sent produces
  one send, no retry and no later statement; cleanup runs on every exit.

**R-DG-06 — Analytics and memory dispatch.** Implement a dispatcher behind
`MemoryClient::submit`, consuming the existing planned requests unchanged.
`graphIdentifier` requests use Neptune Analytics, never Gremlin/port 8182:
modeled `ExecuteQuery` is signed as `neptune-graph`, POST `/queries` at the
selected graph endpoint. Preserve `OPEN_CYPHER`, `queryString` and typed
`parameters`; read the payload with byte/deadline limits and return the parsed
`{"results":[...]}` object to existing response handling. Keep graph-ID
validation before endpoint construction. The
[API reference](https://docs.aws.amazon.com/neptune-analytics/latest/apiref/API_ExecuteQuery.html)
defines the body and read/write/delete IAM actions; the
[access guide](https://docs.aws.amazon.com/neptune-analytics/latest/userguide/gettingStarted-accessing.html)
distinguishes the graph endpoint from the control-plane endpoint.

Graph creation uses a separately gated control-plane client and normalizes
its response to `{"id":"g-..."}`. Preserve `deletionProtection=true`,
`publicConnectivity=false`, vector configuration and required tags. Never
create a graph implicitly to make a query succeed. Gremlin request kinds use
R-DG-05; DynamoDB message kinds use existing DynamoDB transport conventions,
with typed AttributeValue encoding/decoding at this adapter only, preserving
the injected timestamp/TTL, descending limit and final chronological order.
This is required to avoid presenting a partly live memory command as complete.
Do not silently skip an unsupported backend; reject before executing the plan.

All memory backend calls, including graph creation, DynamoDB operations,
Analytics nominal reads and arbitrary `query`, have one attempt, as do
Gremlin statements. AWS explicitly recommends one attempt for
[ExecuteQuery](https://docs.aws.amazon.com/neptune-analytics/latest/userguide/query-APIs-execute-query.html).
After ambiguous failure, report a fixed unknown-outcome error, stop subsequent
steps, and do not claim rollback or atomicity. Preserve two-phase retrieval
and sequential consolidation; no automatic compensation/deletion. A live
invocation requires an operation-level consent check: writes require proposed
`--confirm-write`, consolidation requires `--confirm-delete`, creation requires
`--confirm-create`. Arbitrary query requires both write/delete confirmation
because lexical inspection cannot prove read-only behavior. These flags
acknowledge intent; they do not replace operator access/spend grants.

- T-DG-06a: capture Analytics wire requests and normalize success, payload
  truncation/invalid JSON/wrong-type results and errors; confirm graph endpoint,
  scope, typed parameters and no query interpolation.
- T-DG-06b: run existing retrieve/hybrid/consolidate and creation fixtures
  through the live dispatcher with injected transports; equal seam results,
  ordered expansions and secure creation fields/tags, no implicit provisioning.
- T-DG-06c: DynamoDB typed conversions preserve integers/TTL/order, reject
  unsupported values and keep request-plan objects unchanged for replay.
- T-DG-06d: every consent failure and unsupported-backend plan has zero sends;
  failures after a committed first step stop the plan and never repeat it.

**R-DG-07 — Deadlines and resource bounds.** Proposed defaults: 10-second
credential resolution and connect/TLS/upgrade budgets; 30-second response idle
limit; 60-second absolute deadline per operation including credentials,
signing, DNS, retries/backoff and body/frame reads; 15-minute whole invocation
deadline; 2-second close budget within the remaining invocation budget.
Use one monotonic deadline, injected in tests, not fresh timeouts per frame or
page. Preserve shared pagination caps and add a total response limit of 16 MiB
per operation, a 1 MiB WebSocket frame limit and bounded GraphSON nesting/bulk
expansion (depth 64, 100,000 decoded results). Never allocate from untrusted
lengths before checking limits. Analytics sends a server query timeout shorter
than the remaining client budget (initially 55 seconds); client timeout does
not establish that a mutation was cancelled on the server.

These bounds are new implementation work: current `ApiClient` has retries
and per-I/O timeouts, not an invocation deadline; `StreamingTransport` has
`StreamLimits`, but blocking system DNS may overrun by one lookup. Use a
cancellable bounded resolver/async bridge for the new live adapters, without
nesting `SdkRuntime::block_on` inside another runtime. Credential resolution,
including process handling, must obey the outer deadline too. Do not claim a
hard bound by merely checking time after a blocking call or detaching an
unbounded worker. If existing seams cannot enforce it, S1 below must extend
them and their tests before enabling live selection.

- T-DG-07a: injected clock/provider/resolver/transport tests stall each phase;
  deadlines stop admission, retries and later steps. A slow drip cannot extend
  total time; cancellation leaves no detached resolver/process/socket work.
- T-DG-07b: boundary and boundary-plus-one byte/frame/depth/bulk fixtures prove
  bounded reads/allocations; tiny frames with huge declared lengths fail early.
- T-DG-07c: server query timeout fits remaining budget, close is bounded and
  timeout after send is reported as uncertain, never an assertion of rollback.

**R-DG-08 — Database statistics collection decision.** Prefer a declarative
collection recipe executed by the user's existing mongo shell, followed by a
Rust offline normalizer and `wa-review --analysis-data`. No toolkit database
connection, subprocess shell launcher, new JavaScript helper, Python runtime
or MongoDB dependency is proposed. The recipe is documentation plus a JSON
manifest of allowed commands and output fields; newly shipped executable
normalization/validation and tests are Rust. The user owns shell installation,
authentication and trusted CA configuration; toolkit never takes its URI.

| Option | Supply chain, TLS, licence, maintenance and MSRV | Compatibility and decision |
| --- | --- | --- |
| (a) Optional official `mongodb` Rust driver | Adds BSON/serde, async pooling/authentication and resolver dependencies. Upstream inspected manifest labels itself 3.9.1; defaults include DNS resolution and ring-based rustls, while an aws-lc TLS feature exists. Apache-2.0 driver licence; every transitive licence/feature still needs review. Upstream workspace declares Rust 1.88, below this fork's pinned 1.94.1, but this is not a verified lockfile or compatibility test. Exact new package/size delta requires a pinned candidate dependency audit. | Best one-command URI parity, but introduces credential parsing, driver logging/errors, independent TLS/root/DNS policy and DocumentDB engine compatibility obligations for a small metadata collector. Keep as an alternative if PM requires one-command collection; do not add it now. |
| (b) User-run declarative shell recipe + Rust normalization | Zero new toolkit network-driver dependencies or TLS stack. Existing Apache-2.0 project docs/normalizer retain provenance. User shell has its own version/licence/support obligations and is neither bundled nor invoked. Offline Rust code stays on the workspace toolchain; manifest/schema maintenance is small but required. | Preserves collection capability through an explicit two-step workflow and existing snapshot schema. Recommended, subject to PM acceptance of the workflow divergence. |
| (c) Drop URI collection with no replacement | Smallest maintenance/dependency burden; only existing snapshot ingestion remains. | Removes the documented way to obtain statistics; violates complete conversion intent unless operator explicitly reduces scope. Reject as the default recommendation. |

Driver facts above come from its
[manifest](https://github.com/mongodb/mongo-rust-driver/blob/main/driver/Cargo.toml),
[workspace manifest](https://github.com/mongodb/mongo-rust-driver/blob/main/Cargo.toml)
and [licence](https://github.com/mongodb/mongo-rust-driver/blob/main/LICENSE),
inspected for this design. These mutable references are comparison evidence,
not a dependency pin or a claim that this version is the latest release.

The collection manifest must cover the original full sequence: list databases
excluding `admin`, `local`, `config`; list collections excluding `system.*`;
run `collStats`, aggregate `[{$indexStats:{}}]`, and list indexes per collection.
Retain index field order separately from the key object, index sizes, access
counts, optional TTL (including zero), `potential_unused = ops == 0 && name !=
"_id_"`, `cardinality.is_low=false`, compression `enabled || enable`, and
`max(0, storageSize-size)` / percentage with a zero-storage denominator guard.
Output the existing collection fields including `storageSize` and
`totalIndexSize`, even where today's checks do not consume them. No sampling
of user documents, index changes, writes or environment creation is needed.

Use versioned raw capture envelopes with explicit database/collection ordinals,
ordered index keys, sanitized success/error codes and canonical Extended JSON
numbers. Rust normalization must decode finite numeric values without silently
rounding 64-bit counts through JavaScript doubles, reject values outside the
snapshot's representable range, and feed `snapshot::read_analysis`. Test both
DocumentDB and supported MongoDB outputs: compatibility of these commands is
an engine/version question, not guaranteed by choosing a MongoDB client.
Per-collection failures retain the existing `error` skip marker with a fixed
code and a visible incomplete-collection summary; top-level connection/list
failure exits nonzero and publishes no apparently complete snapshot. Never
store arbitrary driver errors. Capture files/reports are user data: private
permissions, no committed live samples, no automatic upload, no URI/credentials
in envelopes or terminal output. Shell connection instructions use interactive
authentication or the user's established secure tooling, never passwords in
arguments, shell history or a generated script. Reject toolkit `--uri`,
`--tls-ca-file` and `--tls-allow-invalid-certs` before any value is echoed.
Even if (a) is chosen later, acceptance needs a separately reviewed secret
input channel and fixed-error/TLS path; parsing a password URI from argv is
not that path, and insecure certificate validation remains refused.

- T-DG-08a: Rust fixture normalization covers every command/result above,
  exclusions, compound index order, zero TTL/count/storage, missing usage,
  compression aliases and exact unused-storage calculation; then golden
  database checks match legacy analysis fixtures.
- T-DG-08b: canonical EJSON integer boundaries, nonfinite/out-of-range numbers,
  malformed capture and per-collection/top-level failures cannot silently
  become an empty healthy report. No secret or arbitrary error text is echoed.
- T-DG-08c: all three refused flags, including credential-shaped values and
  newlines, fail safely; no shell process or database driver is constructed.

**R-DG-09 — Live acceptance and retirement.** Ordinary `cargo test`, including
all-feature runs, must not use credentials, external DNS, live sockets or paid
resources. Offline transport/codec/root tests use injected clients or in-memory
TLS; any loopback integration fixture is explicitly local and credential-free.
Live acceptance tests are Rust, `#[ignore]`, feature-gated and require exact
`TOOLKIT_DOCGRAPH_LIVE_TESTS=1` as a second gate before any provider/DNS action.
Setting the flag alone must not activate default tests. Target/region and a
network policy are explicit. No automatic provisioning/teardown or real user
data deletion occurs in a test. Mutating tests additionally require a specific
operator-approved disposable target and operation consent; the env flag
does not grant either. No run is part of this design change.

- T-DG-09a: default and all-feature test inventories contain no unignored
  live entry; ignored tests without the exact flag stop before credentials or
  DNS. A forbidden-network fake proves ordinary tests never reach adapters.
- T-DG-09b (operator live): on approved existing resources, verify all four
  AWS service mappings/permissions and paginated metadata, cluster review
  output, expired credentials/access denial, TLS trust and IAM upgrade success
  and failure. Supply failures by restricted identity/fixtures, not by changing
  security settings on the database.
- T-DG-09c (operator live): verify approved disposable Gremlin wipe/seed counts
  and bindings, memory read/write round trip, Analytics parameterized query,
  two-phase retrieval and DynamoDB TTL/order. Consolidation/delete and graph
  creation require separate explicit spend/data-deletion grants; absent grants,
  record those cases as unverified, not passed. Never delete a graph/snapshot
  or weaken deletion protection as implicit cleanup.
- T-DG-09d (operator live): run the declarative collection workflow with trusted
  TLS on the agreed DocumentDB/MongoDB versions; validate snapshot/check parity,
  denied collection permissions and no secrets in local output. Operator owns
  credentials, connectivity, costs, capture custody and resource cleanup.
- T-DG-09e: after parity acceptance, scan/update canonical SKILL/reference
  callers and synchronized mirrors to Rust/recipe commands, verify offline
  contracts and then retire each covered Python artifact. No extension-only
  deletion or premature claim of full #4886 completion.

### Delivery slices and gates

| Slice | Deliverable and dependency | Required acceptance evidence |
| --- | --- | --- |
| S0 — this design | PM resolves Q1/Q2/Q3 below; manager obtains cross-family pre-review of the committed document. No dependency/code changes. | R-DG-01..09 and T lists reviewed; publication alone is not pre-review acceptance. |
| S1 — shared boundaries | Optional feature/dependency audit, credential/deadline bridge, explicit TLS contexts and narrow database endpoint capability. Keep existing public/payment policy unchanged. | T-DG-01..03, 07 and 09a; exact-source security review before enabling live routing. |
| S2 — AWS review client | Depends on S1; modeled ten-operation codecs, field map, bridge and one retry owner. | T-DG-04; existing report/error/recorded goldens. |
| S3 — Gremlin | Depends on S1; optional WebSocket/GraphSON adapter, signed upgrade, consent and close/cancellation. | T-DG-05 plus adversarial bounds/TLS cases; seed guard remains enforced before connect. |
| S4 — memory live dispatch | Depends on S1/S3 and integrated item-1 helpers; Analytics, secure gated creation and DynamoDB conversions. | T-DG-06 and all ordered replay/expansion fixtures; no unsupported partial-live plan. |
| S5 — collection recipe | Depends on Q1 acceptance; declarative manifest/docs and Rust offline normalization. Can proceed independently of network adapters. | T-DG-08; explicit workflow divergence and supported engine/shell matrix. |
| S6 — acceptance/callers/retirement | Depends on S2..S5, exact-source review and the relevant operator grants. | T-DG-09, recorded evidence of unverified live cases, caller/mirror verification before Python retirement. |

Implementation slices remain Rust, use scoped optional service dependencies,
daemon-owned jobs and independent exact-SHA review. Each owns a requirement/test
matrix and deliberate CLI/output changes; no worker may treat this proposal as
authorization for live AWS calls. This design's validation is build-free:
source/requirement review and a docs-only diff check.

### Open decisions and operator gates

| ID | Owner/type | Recommendation, assumption and required ruling/grant |
| --- | --- | --- |
| Q1 | PM product choice | Accept (b), the explicit user-run collection plus Rust snapshot workflow, and continued refusal of legacy URI/TLS flags. Assume two steps are acceptable if collection fidelity is preserved. If one-command parity is required, select (a) and commission a pinned driver/secret-input design; do not silently choose (c). |
| Q2 | PM CLI compatibility choice | Accept explicit `--live`, proposed region/network-policy and operation consent flags, stronger pre-connect seed guard, startup-config failure and bounded timeouts. Existing recorded/dry-run outputs remain the contract. Assume live behavior can be additive rather than activating when a recording is absent. |
| Q3 | PM/security pre-review | Accept narrowly scoped private-target capability and R-ROOTS reuse with P-ROOTS policy separation. Recommend it because current public guards cannot reach normal VPC databases. Require tests before relaxing any target admission; no global bypass. |
| Q4 | Engineering choice for implementation review | Recommend optional official AWS service codecs through a toolkit bridge and optional WebSocket/GraphSON codec, sharing signer, TLS, retry and deadline policy. Pin only after demonstrating the bridge can suppress duplicate signer/retries and satisfy R-LOG; exact licence/size/MSRV audit is still required. No operator ruling needed on routine crate mechanics. |
| Q5 | Engineering support matrix | Recommend initial standard AWS endpoint forms, GraphSON v2 and an explicitly tested DocumentDB/MongoDB/mongo-shell version matrix. Record unsupported engines/partitions/custom endpoints explicitly; derive broader support from fixture evidence. Initial deployment versions must be supplied with the operator target, not guessed. |
| O1 | Operator gate: credentials/access | Supply an approved account/role, target IDs/hosts, signing region, network CIDRs and connectivity. Authorize metadata access and narrowly scoped database actions; do not send secrets in Issues. Credentials are resolved locally through existing approved sources. |
| O2 | Operator gate: spend | Approve resources/cost ceiling and run duration for live Analytics/Neptune/DynamoDB work, and any graph creation separately. Prefer existing disposable resources; neither `--live` nor a test flag authorizes spending or deployment. |
| O3 | Operator gate: real database/user data | Approve read-only statistics access and capture custody. Separately identify a disposable graph/table and authorize specific writes/wipe/consolidation; no destructive test on real user data. Operator owns final cleanup/snapshot retention. |
| O4 | Operator gate: live acceptance scope | Choose which T-DG-09 live cases may run and the approved engine versions. Deferred paid/destructive cases remain explicit evidence gaps; offline passage alone cannot retire the corresponding live legacy path. |

The engineering defaults above are recommendations, not unresolved requests to
the operator. Only O1..O4 require real-resource authorization; Q1..Q3 require PM
acceptance before dependent slices, and Q4/Q5 are resolved by implementers with
recorded evidence during review.
