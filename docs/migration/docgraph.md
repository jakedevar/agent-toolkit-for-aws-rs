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
| `toolkit documentdb normalize-capture --capture FILE --output FILE` | the `--uri` collection of `amazon-documentdb/scripts/wa_review.py` (user-run shell recipe + offline normalizer, #5327) |
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
   The collection itself is replaced by a two-step workflow (S5, #5327, PM-accepted Q1):
   the user runs the read-only commands of
   `amazon-documentdb/references/collection-recipe.md` in their own authenticated
   `mongosh`, assembles a private capture envelope by hand (cost proportional to the
   number of collections), and `toolkit documentdb normalize-capture` validates it
   offline and writes the `--analysis-data` file. See "Collection capture (S5)"
   below.
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

Tracked in #4886: the live AWS / Gremlin bindings (the MongoDB collection is the
user-run recipe of S5, below), and retiring the Python
files and updating `SKILL.md` / `references/well-architected.md` callers. The
Python stays in place until then: the Rust commands cannot reach AWS or a
database yet, so replacing the callers now would remove a working path.

## Collection capture (S5, #5327; R-DG-08)

Delivered: `toolkit documentdb normalize-capture --capture FILE --output FILE`, the
manual recipe `skills/specialized-skills/database-skills/amazon-documentdb/references/collection-recipe.md`
and its allowlist `references/collection-manifest.json`. No database driver, shell
launcher, connection, environment read or socket was added (`docgraph/tests/capture.rs`
scans the module sources and `Cargo.toml` for them). `--uri`, `--tls-ca-file` and
`--tls-allow-invalid-certs` remain unregistered flags on both commands (usage error,
exit 2, value never echoed).

Decisions recorded by this slice:

- **Dialect.** Only canonical Extended JSON from `mongosh` 1.x/2.x
  (`EJSON.stringify(v, { relaxed: false })`) is supported. The legacy `mongo` shell
  and relaxed Extended JSON (bare numbers) are refused with `unsupported_format`, so
  64-bit counts are never rounded through a double. `$numberDecimal` is refused
  (`invalid_number`); the snapshot has no decimal.
- **Envelope.** `toolkit-docdb-collection-capture` version 1: databases and collections
  carry explicit contiguous ordinals; `admin`/`local`/`config` and `system.*` are
  excluded by the normalizer; a per-collection `error` is one of six fixed codes and
  becomes the existing `{"error": code}` skip marker plus a visible
  "N of M collection(s) incomplete" line; a top-level or database `error` exits 1 and
  publishes nothing; a capture with no usable collection is refused (`empty`), a
  deliberate difference from the script, which treated an empty collection as "no
  data".
- **Strictness.** Every level of the envelope is a key allowlist; pasted replies keep
  only the manifest's read fields plus a short list of ignored harmless scalars.
  A secret-shaped key (`password`, `token`, `uri`, `connectionString`, ...) or a
  string value that looks like a connection URI is `secret_field`; any other unknown
  key is `unknown_field`. Error text is a fixed sentence per class and never carries
  a capture value or name.
- **Custody.** The capture must be owner-only before it is parsed (Unix mode `& 0o077
  == 0`; other platforms fail closed, `permissions`). The output is created `0600` with
  `create_new` (no overwrite, no link following). `wa-review` report files are
  unchanged by this slice (see "wa-review report custody" below for #5344).
- **Number fidelity.** Integers stay exact `i64`; unused storage is computed in
  integers when both operands are integers, else as `f64`; the percentage is
  `bytes / storageSize * 100` in `f64` (the script's Python int division is correctly
  rounded and can differ in the last bit above 2^53).
- **Unverified here.** No `mongosh` is installed in the build environment and no
  database is reachable, so the exact shell prompt and `EJSON.stringify` forms are taken
  from the vendor documentation and are not executed. Fixtures are reconstructed from
  documented reply shapes. T-DG-09d (operator live) must confirm both against real
  DocumentDB and MongoDB before the Python `--uri` path is retired (S6).

| Case | Offline tests (`docgraph::tests::capture`) |
| --- | --- |
| T-DG-08a | `t08a_*` (golden snapshot, exclusions, legacy-fixture check parity, shell/engine variants, MongoDB fixture, compound order/zero values/missing usage, compression aliases, exact unused storage, end-to-end `normalize-capture` + `wa-review`) |
| T-DG-08b | `t08b_*` (integer boundaries, non-finite and out-of-range numbers, malformed table, top-level/database failures, per-collection markers, empty capture, arbitrary error text, fixed error text, size/count limits, oversized file) |
| T-DG-08c | `t08c_*` (three refused flags on both commands with credential-shaped and newline values; no shell/driver/socket/env in sources or manifest) |
| T-DG-08d | `t08d_*` (manifest equals code allowlist and schema, read-only single-expression commands, recipe rows equal manifest, no loop/helper/secret form, unsupported dialects) |
| T-DG-08e | `t08e_*` (access policy table, group/world-readable refused before parsing, private 0600 output without overwrite, secret-bearing and unknown fields without echo, transcripts) |

### wa-review report custody (#5344; R-DG-08)

When `documentdb wa-review` is given `--analysis-data`, `wa_review_results.json` and
`wa_review_report.md` are created `0600` from the first instant (mode passed at
creation, so no umask can widen it and nothing is chmod-ed afterwards) with
`create_new`: an existing file or symbolic link (dangling too) at either path is
refused (`<name> already exists; refusing to overwrite it`, exit 1, `docgraph_report`),
never truncated or followed. Both paths are checked before either file is created, so
a refusal leaves no half-written pair. Decisions: the rule is keyed on the flag, since
the analysis data is what carries collection statistics; without `--analysis-data`
the existing overwrite behaviour and messages are unchanged. The `--output` directory
itself is created as before (files inside it are private regardless). Non-Unix
platforms fail closed. Tests: `docgraph::tests::review::r_dg_08_*`.

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
The decision list below records PM workflow/CLI rulings and Q3 acceptance;
the argv-only repair still needs independent re-review. Credentials,
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

Live selection is argv-only, with no environment alias for `--live`. In live
mode, every `--confirm-*` flag (currently `--confirm-wipe`, `--confirm-write`,
`--confirm-delete` and `--confirm-create`) is also argv-only, with no
environment alias. No environment variable, config, recorded response,
analysis/capture envelope,
collection manifest or network-policy file may supply or imply these flags.
Ignore live/consent-shaped data keys as authority; reject them where the
file's schema forbids unknown keys. Check required argv consent before reading
input/policy files, credential resolution, DNS or any network action. The
existing
`NEPTUNE_SEED_CONFIRM_WIPE=1` alias is retained only for non-live replay
compatibility; it is ignored whenever `--live` is present (R-DG-05).

- T-DG-01a: run existing golden review/seed/memory replay fixtures under both
  feature configurations; identical output and exit status, zero provider,
  resolver and socket calls even with hostile AWS configuration present.
- T-DG-01b: forbidden mode combinations and unavailable-feature `--live`
  return usage errors (exit 2) before side effects; feature alone sends nothing.
- T-DG-01c: malformed/null response members, request mismatch, error steps and
  unused seed/memory recordings retain their existing failure contracts.
- T-DG-01d: with the live feature and `NEPTUNE_SEED_CONFIRM_WIPE=1`, a live
  seed lacking argv `--confirm-wipe` is refused with the fixed wipe-refusal
  message and exit 1; provider, resolver and socket counters all remain zero.
  Supplying argv confirmation reaches only an injected adapter; non-live
  replay retains its existing environment-confirmation contract.
- T-DG-01e: table-driven attempts to supply `--live` or each `--confirm-*`
  through environment/config, recorded responses, analysis/capture envelopes,
  collection manifests or network-policy files cannot authorize live work or
  missing consent. Unknown keys are ignored as authority or schema-refused;
  unselected/unconfirmed runs have zero credential, DNS and network actions.

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
through toolkit `Signer`/`Transport`; prove no generated signer is invoked and
no generated credential provider is built (PM ruling 3, #5366).
The bridge is at the generated orchestrator's HTTP-client boundary, not private
modeled serializer APIs. It must remove generated framing headers before the
toolkit signer and sign the final body bytes; the transport regenerates framing
from the same URL/body. The explicit S2 spike exit criteria below must establish
this path, SDK auth/identity suppression and the service-crate feature closure
before live client implementation is enabled.
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
  Assert the signed header set and exact final body bytes; the sent request
  equals the signed request apart from documented transport-generated framing.
- T-DG-04b: modeled successful responses become the same seam objects and
  final reports as existing fixtures, including zero/false versus absent;
  XML/JSON/CBOR truncation, wrong types and service errors cannot become success.
- T-DG-04c: throttling/5xx/connect/timeout recover or exhaust at four sends;
  fresh signatures and exact injected delays prove there is one retry owner.
  AccessDenied, malformed responses and credentials yield one or zero sends.
- T-DG-04d: metadata-only Secrets Manager access never constructs
  `GetSecretValue`; existing repeated-token/page-cap failure behavior survives.
- T-DG-04e: the offline bridge spike exercises generated framing-header
  removal/regeneration and fails if SDK auth, identity resolution or retries
  are constructed/invoked; spies prove only toolkit signing/provider/retry
  paths run. An exact feature/lockfile audit adds no TLS, root or crypto crate.

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
statement/binding order, counts/assertions and exit 1 on verification failure.
For a live seed, only argv `--confirm-wipe` confirms the destructive wipe;
ignore `NEPTUNE_SEED_CONFIRM_WIPE` in live mode. Missing argv confirmation
returns exit 1 with the fixed message: "Refusing to wipe the graph:
g.V().drop() requires explicit confirmation. Re-run with --confirm-wipe."
Do not reuse the replay refusal's suggestion to set an environment variable.
Refuse before reading input/policy files, credential resolution, DNS or network
action. This deliberately tightens the legacy guard: the
environment alias remains valid only for non-live replay parity. Neither a
recording nor any other file can confer consent (R-DG-01).

- T-DG-05a: a local in-memory handshake fixture verifies canonical GET, host
  and port, path, credential scope, date/token, empty payload and fresh signing
  on reconnect; mismatched signature/403 sends no Gremlin frames.
- T-DG-05b: GraphSON v2 fixtures cover seed scalar counts, memory maps/lists,
  typed numeric values, bulk results, empty 204, ordered 206/200 batches,
  unrelated IDs, server failure, invalid types and frame/response limits.
- T-DG-05c: unconfirmed seed sends nothing; confirmed seed preserves every
  statement and verification. Disconnect after a mutation is sent produces
  one send, no retry and no later statement; cleanup runs on every exit.
- T-DG-05d: live seed with environment-only wipe confirmation, including
  `NEPTUNE_SEED_CONFIRM_WIPE=1`, is refused before provider, resolver or socket
  construction/calls. Assert fixed message/exit 1 and zero sends even when
  a policy/capture contains consent-shaped keys; argv confirmation is required.

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
manifest of allowed commands and output fields, consumed as a per-command
manual procedure, not as an executable shell script. Do not imply that the
shell can execute JSON. Document the individual read-only commands, native
canonical-EJSON export steps and manual envelope assembly; no loop/helper JS
is shipped or pasted. This costs manual work proportional to the number of
databases and collections and can be cumbersome for large deployments; record
that workflow cost explicitly. Newly shipped executable normalization,
validation and tests are Rust. The user owns shell installation, authentication
and trusted CA configuration; toolkit never takes its URI. A future automated
shell snippet would be executable shipped text requiring a separate no-write
review and supported-shell contract, not an implicit part of this recipe.

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
store arbitrary driver errors.

Secret custody belongs to the user's authenticated shell and existing secure
credential store, outside toolkit. Prefer interactive authentication: supply
only non-secret host/port/user/authentication-database selectors to the user's
shell and enter the password at its prompt, with no password value in argv.
S5 must verify the exact prompt form for each supported shell/version before
publishing instructions. If established secure tooling uses a connection
string, keep it in that tooling's private credential store and authenticated
shell memory; it never enters toolkit argv, environment, files, manifest,
capture, snapshot, logs or output. Do not put it in shell history, command
arguments or a generated script. The recipe does not retrieve or print it.

Before exporting, the user creates a private directory/file under `umask 077`
(directory 0700, capture 0600 on Unix); toolkit never creates the shell capture.
The normalizer accepts only the versioned allowlisted statistics envelope
(ordinals, ordered index keys, canonical EJSON numeric data and fixed status
codes), never shell transcripts, scripts, connection configuration or raw
driver errors. Reject unknown/connection/credential fields with a fixed error
without echoing their values. Refuse group/world-readable capture files before
parsing on Unix; on other platforms require equivalent owner-only ACL evidence
or fail closed until supported. Normalized files/reports also require private
permissions. No committed live samples or automatic upload. Reject toolkit `--uri`,
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
- T-DG-08d: Rust golden checks tie the documented manual command/export rows
  to the manifest's exact read-only allowlist and envelope schema. Recorded,
  sanitized native outputs for every supported shell/version normalize to the
  same snapshot; unsupported EJSON/export dialects fail explicitly. No shell
  is launched by the test, and the recipe needs no executable loop/helper.
- T-DG-08e: private-permission and ACL fixtures admit owner-only captures and
  refuse group/world-readable or unverifiable files before parsing. Envelopes
  containing URI/password/token/connection fields, transcripts or arbitrary
  errors fail without echoing secret canaries or producing a snapshot.

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
| S1 — shared boundaries | First resolve Q4/Q5 with the offline S2 spike below and record the version/feature matrix before live code. Optional feature/dependency audit, credential/deadline bridge, explicit TLS contexts and narrow database endpoint capability. Keep existing public/payment policy unchanged. | T-DG-01..03, 07 and 09a; S2 spike exit criteria and exact-source security review before enabling live routing. |
| S2 — AWS review client | Offline bridge spike runs in the first implementation slice; live client depends on S1 and spike acceptance. Modeled ten-operation codecs, field map, bridge and one retry owner. | All spike exit criteria below, T-DG-04 including 04e; existing report/error/recorded goldens. |
| S3 — Gremlin | Depends on S1; optional WebSocket/GraphSON adapter, signed upgrade, consent and close/cancellation. Owns the R-DG-07 limits deferred from S1 (PM ruling 3 on #5366): the 1 MiB WebSocket frame limit, GraphSON nesting depth 64 and 100,000 decoded results (bulk expansion), each checked before allocation, plus the Gremlin WebSocket TLS context and wiring `network_policy::parse` after argv consent. | T-DG-05 plus adversarial bounds/TLS cases, including T-DG-07b frame/depth/bulk at the boundary and boundary plus one; seed guard remains enforced before connect. |
| S4 — memory live dispatch | Depends on S1/S3 and integrated item-1 helpers; Analytics, secure gated creation and DynamoDB conversions. | T-DG-06 and all ordered replay/expansion fixtures; no unsupported partial-live plan. |
| S5 — collection recipe | DELIVERED in #5327 (see "Collection capture (S5)"). Q1 accepted; per-command manual manifest/docs, private capture custody and Rust offline normalization. Can proceed independently of network adapters. | T-DG-08 including 08d/e; explicit manual-work cost and supported engine/shell/export matrix. |
| S6 — acceptance/callers/retirement | Depends on S2..S5, exact-source review and the relevant operator grants. | T-DG-09, recorded evidence of unverified live cases, caller/mirror verification before Python retirement. |

**S2 offline bridge spike exit criteria (#5325).** Complete and record these
in the first implementation slice before enabling live client code; this is
not authorization for a network experiment:

1. Pin the orchestrator HTTP-client boundary for each selected service crate.
   Record how generated `host`, `content-length`, `transfer-encoding`, `te`,
   `trailer`, `connection`, `keep-alive`, `proxy-connection` and `upgrade`
   headers (the current `FRAMING_HEADERS` set) are removed before
   `SigV4Signer`; retain semantic headers and let toolkit transport regenerate
   authority/framing from the unchanged URL and final body bytes. T-DG-04a/e
   must prove signed-versus-sent header/body equivalence and reject unsupported
   streaming/framing rather than silently changing payload semantics.
2. Demonstrate the exact SDK auth/identity suppression mechanism (for example,
   a proven anonymous/no-credentials scheme) and one-attempt configuration.
   A no-credentials option alone is not evidence: construction/call spies must
   prove no generated signer/provider or SDK retry path runs, with toolkit as
   the sole signer, credential resolver and retry owner (T-DG-04c/e).
3. Record exact pins and `default-features=false` plus the explicit enabled
   features for every service crate, including Analytics/DynamoDB if selected.
   Audit the resolved feature graph and lockfile delta against the base: no
   new TLS, root or crypto crates, no ring/webpki-roots or implicit native-root
   path, and no default SDK HTTP connector. Share rustls/aws-lc and R-ROOTS;
   record licence, MSRV and dependency-size costs (T-DG-04e).
4. Record the supported endpoint, engine, GraphSON and shell/export versions
   justified by offline fixtures (Q5); operator versions remain live-run input.
   If any bridge criterion fails, stop live enablement and revise the design;
   do not bypass signer checks, add a second stack or invent protocol codecs.

#### S1 spike record, 2026-10-29 (#5366): gate not passed

At `baee1137478b1b19eb099ce52fac31bfc1359f0b`, offline inspection did not
establish the required bridge. The local Cargo registry contained
`aws-sdk-dynamodb` 1.134.0 but no `aws-sdk-docdb`, `aws-sdk-cloudwatch`,
`aws-sdk-ec2` or `aws-sdk-secretsmanager` source. No service dependencies were
added, no service versions or features were selected, and no generated client
was constructed. Consequently the HTTP boundary, auth/identity and retry spies,
service feature closure, lockfile delta, licence/MSRV and dependency-size costs
remain unverified. The absence of local sources is an evidence gap, not proof
that a suitable bridge is impossible. Q4 remains open; the spike gate fails
until those demonstrations exist. Follow-up: #5367.

The current `FRAMING_HEADERS` list is host, content-length, transfer-encoding,
te, trailer, connection, keep-alive, proxy-connection and upgrade. The existing
signer refuses those headers and signs `request.body` as bytes; this source
inspection does not prove generated-versus-sent equivalence or streaming safety.
No signer check was changed. The base lockfile already contains `ring` 0.17.14:
`cargo tree --offline --locked -i ring` traces it through example packages and
the old rustls 0.21 stack. The scoped `cargo tree --offline --locked -p
toolkit-cli --features aws-sdk -i ring` reports no dependency. An eventual audit
must check the actual new closure and lockfile delta against this baseline; it
cannot truthfully assert that the entire existing workspace lockfile is ring-free.

The synthetic capture fixtures identify DocumentDB 5.0 with mongosh 2.3.1 and
MongoDB 7.0 with mongosh 1.10.6, using canonical Extended JSON and capture
envelope version 1. They are reconstructed, not live shell exports, as noted
in S5. The design proposes GraphSON v2, but this inspection establishes no live
endpoint/engine interoperability or GraphSON wire fixture matrix. Q5 remains
open; these fixtures alone do not satisfy the support-matrix exit criterion.
No network, DNS, credentials, live database or SDK bridge test was used for
this record. S1 implementation must not proceed on the strength of it.

#### Spike continuation: resolved dependency graph fails the stated gate

The PM clarified that fetching crates for resolution is permitted; offline
means no AWS, DNS or database work by the spike code. A temporary, uncommitted
`docgraph-live` feature implying `aws-sdk` and four optional service crates
resolved successfully with defaults disabled and no explicit service features:
docdb 1.126.0, cloudwatch 1.138.0, ec2 1.271.0, secretsmanager 1.123.0.
Each reports Apache-2.0 and Rust 1.94.1. The newer attempted cloudwatch 1.139.0
requires smithy-runtime-api 1.19.2 and conflicts with the shared exact 1.19.1
pin; the selected versions resolve with the current shared pins. These are
exploratory pins, not a supported service matrix or committed dependencies.

`cargo tree -p toolkit-cli --features docgraph-live -i ring` reported no
dependency; `-i webpki-roots` reported no matching package. The lockfile delta
added the four SDK crates, aws-smithy-cbor, aws-smithy-compression, bigdecimal,
flate2, libm, minicbor, miniz_oxide 0.9.1, simd-adler32 and zlib-rs; no new
TLS, root or cryptography package was identified. However the explicitly
required empty `-i hyper-rustls` check failed: it reports hyper-rustls 0.27.10.
The feature tree also includes `aws-smithy-runtime/default-https-client`.
Both occur already in `--features aws-sdk`: `toolkit-aws` enables
`aws-smithy-http-client/rustls-aws-lc`, and its `aws-config/rustls` enables
`client-hyper` and `default-https-client`. Graph presence alone does not prove
a default connector is constructed; no construction spy was run.

The stated feature-graph gate therefore conflicts with the requirement that
`docgraph-live` imply the existing `aws-sdk` feature. The temporary manifest
and lockfile changes were discarded; no service client or bridge was added.
The spike and Q4/Q5 remain unresolved, including the auth, framing, retry and
endpoint/version demonstrations. Stop S1 pending resolution of #5368; do not
weaken the signer or infer approval to change shared transport policy.

#### Spike continuation: corrected delta and signer construction

PM ruling 2 supersedes the absolute hyper-rustls graph exclusion above:
compare `docgraph-live` against `aws-sdk`, and distinguish dependency presence
from construction. With the four exact pins now in the manifest, the sorted
`cargo tree -p toolkit-cli -e normal --prefix none` set difference (after
removing Cargo's `(*)` duplicate marker) contains the four SDKs,
aws-smithy-cbor, aws-smithy-compression, bigdecimal, crc32fast, flate2,
futures-macro, half, http 0.2, http-body 0.4, libm, minicbor, miniz_oxide
0.9, simd-adler32 and zerocopy-derive. None is a TLS, root, crypto or HTTP
connector crate. The lockfile adds the 13 packages listed in the preceding
record and no TLS/root/crypto/connector crate. The docgraph graph has no ring;
webpki-roots is absent from the resolved lockfile. The existing hyper-rustls
and default-https-client feature are baseline, not additions. This passes the
corrected graph-delta criterion, not the connector construction criterion.

The SDK source establishes a separate failure of the design's requirement to
prove that no generated signer is **built**. Each of the four pinned service
clients' `Client::from_conf` calls `base_client_runtime_plugins`; its generated
`ServiceRuntimePlugin::new` unconditionally creates
`aws_runtime::auth::sigv4::SigV4AuthScheme::new()`. In aws-runtime 1.10.2 that
scheme contains a `SigV4Signer`, constructed by `Default`. Configured plugins
are appended afterwards; selecting a different auth scheme can prevent use but
does not undo construction. This is source evidence, not a construction/call
spy and not proof that an SDK signer signs a request. Neither explicit
connector injection, absence of default connector construction/calls, nor sole
toolkit signing, credential resolution and retry ownership has been proven.
PM ruling 3 on #5366 resolves the wording: constructing this stateless scheme
is permitted, while invoking the SDK signer or constructing an SDK credential
provider is not. The required spies remain outstanding; source inspection is
not a substitute. No signer guard or shared transport was changed.

The offline experiment `docgraph_bridge_spike` (job
`5ba8f79f-1fdd-4dcf-9c2a-81d26c8b0a7d`) constructed each pinned service
client with `allow_no_auth`, disabled SDK retries and an injected in-memory
connector. One operation per client reached the connector once on a 503;
the generated request had no authorization, date, security-token or content
hash signature headers. The experiment strips the toolkit framing-header
set and applies the real toolkit SigV4 signer to the buffered bytes using
synthetic credentials, without sending a request. It confirms the body remains
unchanged and no framing headers reach the signer. This is a limited spike,
not proof of signed-versus-sent equivalence, every operation, SDK signer call
instrumentation, provider construction, explicit production connector or
toolkit retry ownership. A non-buffered body is refused by the experiment.

Source inspection also shows Smithy runtime 1.16.1's `default_plugins`
unconditionally constructs a lazy identity cache during generated client
construction. The config getter's `None` only describes the supplied config;
it does not prove the default plugin was not constructed. The SDK also builds
a default HTTP client object; its connector is constructed lazily on selection.
The injected connector spy shows the chosen connector handled these calls but
does not instrument construction of default objects. Neither fact proves
credential resolution or a network call; the invariant requires demonstrating
their absence, rather than treating config getters as such proof. The PM's
separate no-identity-cache-construction requirement is not yet demonstrated.
No live routing is enabled by this experiment; the remaining spike evidence
and security tests still gate the slice.

The expanded offline experiment (job `2f57fc64-0956-4bdb-b392-f1a203dbf5e7`)
prefers `noAuth` explicitly. In a negative-control configuration only, an
injected credential provider panics if called; none of the four sampled calls
invoked it. Removing that preference caused the first such call to invoke the
trap (red job `81b9fb56-7ee2-464f-a45f-474c86b3e5e4`). The preference was
restored, and the SDK client without an injected provider remains tested too.
An injected toolkit `SdkTransport` connector then observed the same body and
all signed headers after framing removal for each sampled request, with a
synthetic public peer and no socket. This does not prove all operations,
default connector construction, production connector selection, provider
construction or absence of the SDK's default identity cache. In particular,
`allow_no_auth` alone is insufficient in the presence of a provider: the
explicit preference and the absence of SDK credential sources are both
necessary to the tested design. S2 must fail closed if no-auth is not selected.

The successor's offline spike wraps the SDK's actual `SigV4AuthScheme` with a
call counter and installs it under the same scheme ID for all four sampled
clients. With `noAuth` preferred, the counter remains zero; a separate
DocumentDB control with synthetic credentials and no preference invokes the
wrapped SDK signer exactly once (job `8ca768d7-9fbd-4aed-86ed-142f1dcab142`).
This observes the configured replacement scheme, not the construction or
invocation of the original generated scheme. The expanded experiment passed
all ten allowed operations with and without a forbidden credential provider,
each reaching the injected connector once on a 503 (job
`75cca6f3-5cd1-4f42-87e7-40ecefa73df2`); the signed toolkit headers and
buffered body were then observed by an injected toolkit transport connector.
This is not a production bridge or proof of every input and response shape.
The direct, optional `aws-runtime` pin exposes this already-resolved
crate for the spy; it adds no lockfile package. No live adapter uses it.
The spike also supplies a real file-backed streaming SDK body and verifies that
the in-memory bridge rejects it before toolkit signing (job
`bd037b0e-54e3-43a3-b545-74239d319bd0`); the bridge is test code only. The
`aws-smithy-types/rt-tokio` feature enables that fixture. A renewed offline
normal-dependency set comparison against `aws-sdk` has the same 19-entry delta
listed above, and the lockfile has no added package from this feature.

The memory preflight's current mutating-operation table is covered by a test
that checks argv consent before input or policy file access. Removing the
`ingest-chunk` guard produced red job `16c931f6-28fc-46db-bbb0-05c11f6a1e2d`;
the restored guard passed job `10249051-918b-488b-88a9-1cbe16e70d86`. This
does not establish full T-DG-01e file-source or zero-socket instrumentation.
With and without the feature, environment-shaped live selection produced the
same offline outputs as a cleared environment (job
`9c8e3962-624d-4fc4-914e-84d3c3c0b5cf`); temporarily treating the environment
variable as live selection failed the default-feature check (red job
`9c5f305b-763c-42ec-ab92-a5d90ebaa670`).

As a partial R-DG-02 boundary, `GuardedCredentials::resolve_required` converts
unsigned and source failures to fixed credential errors and rebuilds once when
the first result is already expired; it samples the clock after resolution and
again before returning. Its `CredentialsProvider` implementation connects
this policy to the existing toolkit signer. The expiry mutation job
`65de334b-11c0-4364-8bf8-4f4c17958ea3` failed as intended; the initial green
job was `dbbf7a33-6c8c-4d5a-b4e3-a8314c115516`. The caller must check validity
at signing as well. This synchronous source is not cancellable and the method
is not wired to a live adapter: R-DG-07's provider deadline and real
guarded-source expiry/recovery tests remain open. A real guarded-source test
with injected HTTP covers absent and invalid sources, failing process stderr
and network error canaries, and fallback refusal (green job
`c8984212-48f8-4d11-87d1-a52cc8fcab57`). A temporary fixed-diagnostic mutation
that emitted the same synthetic canary failed the assertion (red job
`8e2f6ec8-c73d-41b5-a5d0-f2c5824d2c50`). This does not yet cover service
response messages, reports, or a live adapter's diagnostics.
Configured-source recovery through the `CredentialsProvider` interface is
also exercised with an injected provider HTTP client (job
`eb71f797-9868-4a9c-b4b2-74aae78e4271`).

The existing `SdkTransport::call` awaits `connector.call(request)` before
`check_peer`, so the peer check is post-send, not a pre-send admission control
(#5387). The public DNS guard remains in place, but this ordering cannot meet
R-DG-03 for private database targets. S1 must add a connector-level check
before signed HTTP bytes are written, using the validated DNS answer set; it
must not rely on the current post-response check or an unguarded connector.

#### S1 continuation: database endpoint capability and bounds (#5387)

PM ruling 4 on #5366 permits an additive connector-level pre-send peer
check in toolkit-aws for the database capability only. It is delivered as
`toolkit_aws::sdk::database` (feature `sdk`):

- `DatabaseEndpoint::new(host, port, region, private_ranges)` is the narrow
  capability. The host must be a lowercase standard-partition Neptune
  (`<…>.<region>.neptune.amazonaws.com`) or Neptune Analytics
  (`<…>.<region>.neptune-graph.amazonaws.com`) name; wildcards, trailing
  dots, userinfo/ports in the host, FIPS, `.amazonaws.com.cn` and custom
  domains are refused until reviewed. The region must be a standard name and
  equal the host's region label; the port is fixed and non-zero. Private
  ranges must be canonical and lie inside RFC 1918 with a `/16` or longer
  prefix, or inside `fc00::/7` with a `/48` or longer prefix; at most 16;
  and share no address with the forbidden blocks below (#5428).
  `admits` judges IPv4-mapped IPv6 as IPv4, then refuses the forbidden
  blocks **first**, before any listed range: IPv4 `0.0.0.0/8`, loopback,
  link-local (instance metadata `169.254.169.254`, container credentials
  `169.254.170.2`), multicast and broadcast; IPv6 `::`, `::1`, `fe80::/10`,
  `ff00::/8` and the AWS instance service block `fd00:ec2::/64` (metadata
  `fd00:ec2::254`). Only then does it accept any public address (the shared
  public predicate) or a listed private one; CGNAT, NAT64 and every other
  non-public block cannot be listed and are refused.
- `toolkit-cli` `docgraph::network_policy::parse` (feature `docgraph-live`)
  reads the `--network-policy` JSON schema: exactly `host`, `port`, `region`
  and optional `private_ranges`. Unknown keys, including `live` and every
  `confirm_*` shape, are refused rather than ignored (T-DG-01e for policy
  files). It is not yet called: `--live` is still refused before any file.
- `SdkTransport::for_database(runtime, roots, endpoint, bounds)` is a new
  constructor. Its connector owns the TCP dial: it checks the URL (https, no
  userinfo, exact policy host and port, no framing header, buffered body
  only), resolves the host afresh for every request, refuses empty, more
  than 16, or **any** non-admitted answer (mixed sets are not filtered),
  dials only validated answers and then compares the socket's actual peer
  with the dialed answer and the policy **before writing any byte**, not
  even a TLS ClientHello. TLS uses rustls with aws-lc, Smithy's cipher-suite
  list, safe default protocol versions, exactly the `ValidatedRoots`
  certificates and the original host name for SNI and verification. One
  HTTP/1.1 exchange runs per connection (no pool), so a reconnect is always
  revalidated. Redirects are returned, never followed; no environment
  variable (proxy or endpoint override) is read.
- `SdkTransport::new`, `with_resolver`, `with_connector`, the public DNS
  guard, the post-response `check_peer` and `PaymentRoots` are unchanged;
  their existing tests still pass. The only shared edit is a
  `GuardRejection::carrying` constructor so the database verdicts travel the
  existing `map_connector_error` path unchanged.

Dependency decision: the connector uses hyper's HTTP/1 client connection,
hyper-util `TokioIo`, tokio-rustls, http, http-body, http-body-util and bytes,
all already resolved in the `aws-sdk` graph through aws-smithy-http-client.
They are pinned to the resolved versions with default features disabled
(tokio-rustls without `logging`, so R-LOG holds). The lockfile change is six
dependency edges on `toolkit-aws` and no new package; no TLS, root or crypto
crate is added. Smithy's TCP connector hook is crate-private in
aws-smithy-http-client 1.5.1, so wrapping Smithy's connector could not run a
check between connect and the first write; owning the dial was the only
additive way to meet ruling 4.

R-DG-07 decisions: `DatabaseBounds::default()` is the design's 10 s
connect-phase budget (DNS, TCP connect and TLS/HTTP handshake each), 30 s
idle (response head and between body frames), 60 s per operation and
16 MiB per response. Every phase runs inside one absolute deadline: the
earlier of the operation budget and a caller deadline (the streaming
`StreamLimits` deadline). All waits are async and dropped at the deadline,
and the HTTP connection is never spawned: it is owned and driven by the
exchange and then the response body, so dropping either drops the socket
synchronously (#5429). A streamed read that ends the response (end of body,
any terminal error or limit refusal) releases the socket before it returns,
even while the caller keeps the stream (#5465). A declared `content-length` above the limit fails before any body
byte is read; undeclared bodies fail at the first frame that would exceed the
remaining allowance. A deadline after the request was handed to the
connection is a non-retryable `Timeout(Deadline)` that says the outcome is
unknown; a missing response head after sending is a `Timeout(Read)` with the
same wording; neither claims a rollback. `server_query_timeout` derives a
server-side query timeout from the remaining budget, a return margin and a
cap (55 s for Analytics), refusing to send when nothing useful remains.
Tests stall the injected resolver, dialer and in-memory TLS server with
Tokio's real timer and short budgets; Tokio's paused clock (`test-util`) was
not added. Residual: the production resolver is the operating system's on
Tokio's blocking pool. The deadline abandons the lookup and discards its
answer, but that `getaddrinfo` thread ends only when the system resolver
returns, and dropping the runtime waits for it. No asynchronous DNS crate was
added. Superseded: the residual is accepted and capped (PM ruling 2), see
"S1 continuation: credential deadline, DNS cap and remaining evidence".

Not delivered in S1, by decision: WebSocket frame, GraphSON depth and bulk
limits (T-DG-07b) need the S3 codecs that do not exist yet; the constants are
recorded above and S3 must test them at the boundary and boundary plus one.
Credential-provider deadlines (T-DG-07a provider phase) remain open:
`GuardedCredentials::resolve_required` is synchronous and its SDK providers
are driven by `SdkRuntime::block_on`, so an outer async deadline cannot drop
them without a provider-level change. S2 must not wire live signing before
that bound exists. Superseded: the bound now exists
(`GuardedCredentials::resolve_required_until`, next section).

T-DG-09a: `docgraph_live::test_inventories_hold_no_unignored_live_entry`
parses every test in toolkit-cli and toolkit-aws with `syn` and requires any
test naming `for_database`, `SystemDatabaseResolver`, `TcpDialer` or the
exact `TOOLKIT_DOCGRAPH_LIVE_TESTS` variable to be both `#[ignore]` and
`cfg(feature = "docgraph-live")`; a fixture test proves the rule flags an
unguarded test. `ordinary_and_live_shaped_runs_never_reach_the_network`
runs default and live-shaped invocations with every proxy, endpoint and
metadata variable aimed at a loopback listener that only counts connection
attempts, with canary credentials and the acceptance flag set; it asserts
zero connections, no canary in output and no live success. Both run under
the default and `docgraph-live` features. No live test exists yet.

Endpoint-admission red/green jobs (mutations uncommitted, restored with
`git show HEAD:<path>`): removing the pre-send peer check made
`a_forbidden_connected_peer_receives_zero_bytes` fail because the connector
began TLS to the forbidden peer (red `8e4940b3-aae6-4853-92fe-da856707555b`,
green `ac5a5ddc-46a5-46ae-ba90-85e2c3cd2ed4`). Admitting every DNS answer made
`forbidden_or_mixed_answers_are_never_dialed` fail on the metadata address
(red `878a82dc-f8ae-4da3-9ad6-387c766e13d0`, green
`e604fbfa-6b5f-4def-a939-427147c44110`). The full connector module passed
in job `86de3b8b-0eec-453c-81e8-50e2fdfa18c8` (25 tests), and the
`docgraph_live` suite with the policy parser passed under both feature sets
in job `fcf49e10-7784-45dc-b7a8-7c6198a68500`.

The `.rsi/jobs.toml` additions in this slice declare bounded daemon recipes
for the offline spike, consent, environment-selection, credential and
feature-specific clippy checks. Their Makefile targets use the daemon-stamped
Cargo target and sccache; they do not enable a live test or a network call.

The initial S1 CLI preflight recognizes `--live` but deliberately returns a
usage error after validation because no live adapter exists in this slice.
It rejects recorded/dry-run conflicts before file reads, ignores the legacy
wipe environment variable in live mode, requires an explicit seed endpoint,
and checks operation-level memory consent before reading its input. These are
partial T-DG-01 checks, not evidence that the remaining S1 boundaries or all
file-origin consent cases have been implemented. The ambient-wipe mutation
red/green jobs were `ff393657-dff0-4d7f-afeb-7a3c2ef620ef` and
`44ba8156-f0fc-42fa-bb2d-fae216004c7c`; the memory consent guard mutation
red/green jobs were `08718c7a-172c-42cc-8aac-cf5862bb9659` and
`77a0bafe-4692-47bb-b041-565ed9ccf080`. Mutations were not committed.

#### S1 continuation: credential deadline, DNS cap and remaining evidence

PM rulings on #5366 (second batch) for the items the previous checkpoint left
open, and how they are met. Public and payment transports are unchanged;
no crate is added (the explicit `sync` feature on toolkit-aws's `tokio` was
already enabled in the resolved graph, so `Cargo.lock` does not change).

**Credential deadline (ruling 1, T-DG-07a provider phase).**
`GuardedCredentials::resolve_required_until(deadline)` and its
`CredentialsProvider` view `GuardedCredentials::until(deadline)` bound
credential resolution by the operation's absolute deadline, further capped
at `CREDENTIAL_BUDGET` (10 s) from the start of resolution. Each provider
load, including the D1 rebuild, runs as a future inside `SdkRuntime` under
`tokio::time::timeout_at` and is dropped when the bound fires, so its STS,
SSO, Sign-in, ECS or IMDS request is cancelled with it; a deadline already
past starts no provider request. A timeout is the fixed
`Timeout(Deadline)` "credential resolution did not finish within its time
budget; nothing was sent", returned at once: no rebuild, no fallback source,
no unsigned request. The proxy's `resolve`/`resolve_required` paths are
unchanged. Live signing must use `until` with the same deadline given to the
database transport (`StreamLimits` deadline), so credentials, signing and the
exchange share one monotonic deadline.

Decision: aws-config 1.13.0 runs `credential_process` with
`tokio::process::Command::output()` and no `kill_on_drop`, so a dropped
load would leave the child running past the deadline (the design forbids
detached process work). The bounded path therefore refuses, before building
it, any selected profile or default-chain source whose profile can reach a
`credential_process`, with the fixed message `UNBOUNDED_PROCESS_REFUSAL`.
Product consequence: live docgraph cannot use `credential_process` profiles
until a kill-on-deadline process runner exists (follow-up #5415). Other sources (environment, static profiles, SSO/login,
assume-role, web identity, ECS, IMDS) are HTTP or file based and bounded.
`ProviderHttp::validated_bounded` builds the provider-plane client with the
gated system resolver below; live wiring must use it with `until`.

**DNS residual (ruling 2): accepted and capped.** `sdk::lookup` runs every
production system lookup of the bounded paths (`SystemDatabaseResolver` and
`ProviderHttp::validated_bounded`) through one process-wide `LookupGate`:
`getaddrinfo` runs on a detached `std` thread (not Tokio's blocking pool, so
dropping the runtime never waits for it) and the caller waits
asynchronously, so the deadline drops the wait. The gate admits one blocking
lookup at a time (others queue under their own deadlines). While a lookup the
deadline abandoned is still running, a new lookup fails closed at once with
`ABANDONED_LOOKUP_REFUSAL` and starts no thread; the thread clears the flag
when the system resolver returns. Accepted residual: one abandoned
`getaddrinfo` thread per process may outlive its deadline until the system
resolver returns; it holds no socket of ours and its answer is discarded.

**Close budget (T-DG-07c).** The database connector closes by dropping the
socket synchronously when the response or a failed call is dropped (see
"S1 repair 1" below: the connection is never a spawned task); no graceful
TLS close is awaited. `CLOSE_BUDGET` (2 s) records the design bound; the
tests hold both a completed exchange and a body-limit failure against a
server that never closes to it.

**Deferred to S3 (ruling 3):** WebSocket frame (1 MiB) and GraphSON depth
(64) and bulk (100,000) limits, listed in the S3 row of "Delivery slices and
gates".

**Spike exit criteria.** PM ruling 3 permits constructing SDK defaults and
forbids using them. `docgraph_bridge_spike::the_default_connector_identity_cache_and_sdk_retries_are_never_used`
configures all four pinned clients as S2 will: injected HTTP client,
pass-through (non-caching) identity cache, `noAuth` preferred, SDK retries and
timeouts disabled, and a credential provider that panics if called, with
every client's endpoint set to a loopback listener that counts connections.
One operation per client: the injected connector sees exactly one call per
operation on a 503 (the SDK retries nothing, so the toolkit `RetryPolicy` is
the sole retry owner, as T-DG-04c will prove end to end), the listener
accepts zero connections (the SDK default connector is never used), and every
identity goes through the installed cache with none carrying AWS credentials
(the default lazy identity cache is never consulted and no credential identity
is resolved). Together with the earlier spike records (no SDK signer call,
injected connector only, buffered-body bridge), criteria 1-3 are met for the
sampled operations; S2 must keep this configuration and fail closed if
`noAuth` is not selected.

**Q5 support matrix (criterion 4), from offline fixtures.**

| Area | Supported (fixture or test evidence) | Everything else |
|---|---|---|
| Database endpoint | Standard-partition Neptune `<id>.<region>.neptune.amazonaws.com` and Neptune Analytics `<id>.<region>.neptune-graph.amazonaws.com`, one fixed port, region equal to the host label (`sdk::database` tests) | FIPS, other partitions, custom domains, wildcards: refused |
| Gremlin wire | GraphSON v2 (codec and T-DG-05b fixtures delivered in S3, #5515) | Other GraphSON/GraphBinary versions fail closed in S3 |
| DocumentDB / MongoDB capture | DocumentDB 5.0 with mongosh 2.3.1; MongoDB 7.0 with mongosh 1.10.6; canonical Extended JSON; capture envelope version 1 (`capture_fixtures`) | Legacy shell `tojson`, relaxed EJSON and other dialects: `unsupported_format` (T-DG-08d) |

Fixtures are reconstructed, not live exports; operator deployment versions
remain live-run input (O4), not guesses.

**Remaining T-case coverage added.** T-DG-01a:
`docgraph_live::golden_replays_are_unchanged_by_hostile_configuration` runs
the seed dry run and replay (recording rebuilt from the dry-run statements),
memory dry run and the review fallback with a hostile shared AWS
configuration (a marker-writing `credential_process`, forbidden endpoint,
consent-shaped keys), proxy/endpoint variables at a counting listener, canary
credentials and `RUST_LOG=trace`; output and status equal a cleared
environment's, the golden lines hold, zero sockets and no provider ran. The
suite runs it with and without `docgraph-live`, and also runs every docgraph
unit test (including the review/seed/memory golden replays) under
`docgraph-live`, while scoped_test runs them under the default features.
T-DG-01e: `consent_carriers_never_authorize_live_work_or_missing_consent`
tables recorded responses, memory inputs, analysis envelopes, collection
capture (no live mode), network-policy files, the shared AWS configuration
and the environment as carriers. T-DG-02a:
`bounded_resolution_refreshes_expired_credentials_and_fails_closed` (real
guarded source, injected Sign-in HTTP). T-DG-02b:
`network_policy::signing_region` (shared precedence; profile region ignored;
mismatch refused before signing, no echo) and
`credential_arguments_are_rejected_without_echo`. T-DG-02c: `RUST_LOG=trace`
in the hostile runs and the stalled-provider test, canaries absent.

Red/green jobs (mutations uncommitted, restored with `git show HEAD:<path>`):

| Rule | Mutation | Red (fails as intended) | Green |
|---|---|---|---|
| Credential deadline | `load_until` deadline pushed out by an hour | `1df3764e-4950-42e1-be4b-6ef448a1c7eb` (stalled STS took 13,890 ms) | `b9078c88-1aea-43c9-9a18-50a35fd5f0e2` |
| `credential_process` refusal | refusal disabled | `acf6a742-b7c8-4eba-9480-2cb92448a58a` (process ran, `aws_timeout` instead of the refusal) | `b9078c88-1aea-43c9-9a18-50a35fd5f0e2` |
| Abandoned-lookup cap | abandoned check removed | `4c81459c-1012-4c6e-88e3-89a60623a840` (second lookup waited and timed out instead of failing closed) | `b9078c88-1aea-43c9-9a18-50a35fd5f0e2` |

The green job ran `docgraph-bounded` (bounded credential tests, `sdk::lookup`
and all `sdk::database` tests) at `f637417f`; the final verification batch
reruns each rule's recipe at the final commit.

#### S1 repair 1: security review #5425 blockers #5428 and #5429

**#5428, metadata admission.** `admits` used to OR the shared non-public
check with listed-range membership, so a policy listing `fd00:ec2::/48` (or
the exact `/128`) admitted the IPv6 instance metadata address `fd00:ec2::254`
at both the DNS-answer and connected-peer checks. Per the PM ruling, an
unconditional forbidden-address check now runs first, after IPv4-mapped
canonicalisation and before any listed exception, on every DNS answer and
the connected peer (`DatabaseEndpoint::check_peer`, moved from the connector
unchanged so it can be tested directly). `DatabaseEndpoint::new` also refuses
a listed range that overlaps a forbidden block (contains it or lies inside
it), without echoing it. Decision: the forbidden IPv6 metadata entry is the
whole `fd00:ec2::/64`, which holds every documented AWS instance service
address (metadata `::254`, DNS `::253`, time `::123`, pod identity `::23`),
not only `::254`; neighbouring ULA ranges such as `fd00:ec2:0:1::/64` stay
listable. The shared public predicate, the public guard and the payment
plane are unchanged.

**#5429, socket release.** `AbortOnDrop` only aborted the spawned HTTP
connection task; on the current-thread `SdkRuntime` an aborted task keeps its
stream until the runtime is next polled, so `Transport::send` could return
with the socket still open indefinitely. The connection is now never
spawned: a `Driver` owns hyper's connection future (and so the TLS stream
and socket); the exchange drives it while waiting for the response head,
and the `BoundedBody` owns and drives it while the body is read. Dropping
the body, or the call future on any error or timeout, drops the socket
synchronously, before the synchronous API returns and with no later runtime
poll. The open-failure paths (refused peer, TLS failure, failure before the
head, deadline in `open`) already dropped their stream with the failing
future, and now hold no task at all. `SdkTransport::send`/`open` and the
public and payment transports are unchanged.

Regression tests (`sdk::database::tests`):
`a_policy_cannot_list_a_range_holding_a_forbidden_address`,
`forbidden_addresses_are_refused_even_inside_a_listed_range` (policies built
without `new`, `/48` and exact `/128`, admission and peer path, every
forbidden kind incl. IPv4-mapped forms),
`forbidden_answers_inside_a_listed_range_are_never_dialed` (DNS path and
mixed answers: zero dials, zero bytes),
`every_send_releases_its_socket_before_it_returns` (success, declared and
mid-body limit failures, timeout after send, TLS failure, refused peer) and
`a_streamed_response_releases_its_socket_when_dropped_or_failed` (a dropped
stream without a deadline, and a deadline failure in `open`). The release
tests read a drop counter on the client stream right after the call returns
and never drive the runtime again.

| Blocker | Red (tests at `e0f24e41`, fix absent) | Green |
|---|---|---|
| #5428 | `08f160f7-a385-4fde-8810-bff71ea15f7e`: `fd00:ec2::/48` accepted by `new`; `fd00:ec2::254` admitted; dialed and reached TLS | `78567375-fcaf-426b-9f89-d46d32da4a44` (fix `35126dc5`: `sdk::database` 32 passed, 0 failed) |
| #5429 | `08f160f7-a385-4fde-8810-bff71ea15f7e`: "success left the socket open", "dropping the stream left the socket open" | `78567375-fcaf-426b-9f89-d46d32da4a44` (fix `35126dc5`: `sdk::database` 32 passed, 0 failed) |

#### S1 repair 2: re-review #5458 blocker #5465

**#5465, a terminal streamed read kept its socket.** Repair 1 made the body
own the connection, but `BoundedBody` returned its deadline, byte-limit and
idle errors with the `Driver` still in place, and `SdkStream::next_chunk`
keeps its body after an error, so a caller that retained the stream after a
failed read held the socket until it dropped the stream. Two layers now
release it before the read returns:

- `BoundedBody` releases its `Driver` (connection, TLS stream, socket) at
  the end of the body and on every terminal failure (operation deadline,
  byte limit, idle time, connection error), and repeats the failure on any
  later poll; a failed body never reports a clean end.
- The database transport wraps its `SdkStream` in `ReleasingStream`, which
  drops the whole stream the moment `next_chunk` ends the response: end of
  body, or any error from the database body or the outer `StreamLimits`
  (deadline, idle time, byte cap, a caller deadline that had already passed
  before the body was read). A `narrow_body_limit` that refuses the stream
  releases it too. Later reads repeat the outcome (`Ok(None)` after the end,
  the same error after a failure). Decision: on the database plane every
  `StreamError`, `IdleTimeout` included, is terminal.

`SdkStream` itself is unchanged; only the database transport sets the new
`release_ended_streams` flag, so the public and payment streams behave as
before. Successful end of body (audit): the red run already released the
socket when the body ended, because hyper's connection finishes once the
body is complete and its sender is gone; both layers now release it
explicitly at that point as well.

Regression tests (`sdk::database::tests`), each with the stream retained and
the client-stream drop counter read right after the call returns, with no
runtime poll in between:
`a_terminal_database_body_error_releases_the_socket_before_it_returns`
(the reviewer's idle, operation-deadline and 64-byte-cap cases),
`a_terminal_stream_limit_refusal_releases_the_socket_before_it_returns`
(stream byte cap, already-expired caller deadline, caller deadline while
reading, refusing `narrow_body_limit`),
`a_completed_stream_releases_its_socket_at_the_end_of_the_body`, and, with
the fix, `the_releasing_stream_drops_its_inner_stream_on_every_terminal_read`
(a scripted inner stream: every `StreamError` kind, end of body, narrowing).

| Blocker | Red (tests at `3379cc20`, fix absent) | Green (fix `60499087`) |
|---|---|---|
| #5465 | `4e9a4a29-3486-474c-988e-b16535d0bdef`: "idle: the read returned Failed(.. stalled ..) with the socket still open"; "stream cap: the read returned TooLarge with the socket still open" (33 passed, 2 failed) | `be0e45aa-b245-4a41-b49d-5eb009458202` (`sdk::database` 36 passed, 0 failed) |


#### S2: AWS review client (#5489)

`toolkit_cli::docgraph::live` (feature `docgraph-live`) adds
`LiveAwsClient`, the R-DG-04 implementation of the review's `AwsClient` seam
for exactly the ten operations of the table above (`OPERATIONS`). Any other
pair, any request member the field map does not cover, or a member of the
wrong type fails with a fixed `InvalidConfig` before anything is encoded,
signed or sent. `--live` is still refused at the CLI: this slice adds the
client, not its routing, which waits for the exact-source security review.

**Bridge (spike criteria 1-2, T-DG-04e).** The pinned service crates encode
the typed input (Query, AWS JSON, RPC-v2 CBOR) and decode the response; they
never sign, resolve credentials, retry or open a socket. Each client is built
per attempt with `noAuth` preferred, no credentials provider, an identity
cache that refuses any AWS credential identity, SDK retries, timeouts and
stalled-stream protection disabled, and one HTTP client: the in-process
bridge connector. The SDK future is polled on the caller's thread with no
async runtime: the connector parks the generated request and returns
`Pending`, the driver runs the synchronous toolkit path and fills the
response slot, and polling resumes. Nothing is nested in another runtime,
and an SDK wait on anything but the bridge fails closed. The toolkit path:

1. the generated URL must equal the operation's own standard-partition
   endpoint (`https://{rds|monitoring|ec2|secretsmanager}.{region}.amazonaws.com`
   plus the modeled path), the method POST and the body buffered; otherwise
   refused;
2. any SDK-prepared signature header (`authorization`, `x-amz-date`,
   `x-amz-security-token`, `x-amz-content-sha256`) means `noAuth` was not
   selected: refused (fail closed);
3. the generated framing headers (`FRAMING_HEADERS`, including the
   `content-length` every operation generates) are removed, all other
   headers and the final body bytes kept unchanged;
4. the toolkit signer signs with the operation's scope (`rds`, `monitoring`,
   `ec2`, `secretsmanager`) and credentials bounded by the operation
   deadline (`DeadlineSigner`; production: `GuardedCredentials::until`);
5. the toolkit `StreamingTransport` sends exactly the signed request under
   the same deadline and a 16 MiB response cap, regenerating authority and
   framing from the unchanged URL and body;
6. any response carrying a document type declaration is refused explicitly
   as `InvalidResponse`, for every operation and HTTP status (repair #5544).
   A 2xx with an empty body, or a Query/XML 2xx that is not one
   well-formed XML document rooted at the operation's `{Operation}Response`
   (repair #5521, below), is also `InvalidResponse`; everything else, non-2xx
   included, goes to the modeled decoder.

**One retry owner (T-DG-04c).** `RetryPolicy::default()` (three retries,
four sends, full jitter, 20 s cap) wraps whole attempts; each attempt
encodes and signs afresh. The backoff never sleeps past the operation
deadline (60 s from the start of the call, covering credentials, signing,
DNS, connect, TLS, every attempt, backoff and body reads), and an attempt
that would start after it fails `Timeout(Deadline)` without sending. Errors:
a service error keeps only its HTTP status and sanitized code (message and
request id discarded) and is retried only when throttling or transient;
transport and credential failures keep their toolkit classes; a decode
failure is a non-retried `InvalidResponse`.

Decisions (engineering, recorded for review):

- The SDK's Query and JSON decoders report an undecodable 2xx as an
  unhandled *service* error with status 200; such an error is classified
  as `InvalidResponse`, never a service verdict (found by T-DG-04b).
- aws-smithy-xml stops silently at the end of input and reads only the
  elements it looks for, so a document cut at an element boundary, a
  mismatched close, a missing inner close or a second root could decode as
  a success. A closing-root suffix check was not enough (#5516 review,
  BLOCKER #5521). The bridge therefore requires the whole body to parse as
  one well-formed document (`well_formed_xml`, below).
- The client is given its service-plane transport by reference. Live wiring
  (#5507) must pass `SdkTransport::for_public_service`, never
  `SdkTransport::new`: the ordinary public transport resolves DNS
  synchronously and cannot meet the operation deadline (#5522, below).
- The field map does not emit instance-level `IsClusterWriter`: the
  DocumentDB model's `DBInstance` has no such member, exactly as boto3
  returned none to `wa_review.py`, so the writer-only check (REL9) is
  skipped on live data as it was in Python. Recorded fixtures may still
  carry it. Deriving the writer from `DBClusterMembers` is a product change
  left to a follow-up.
- Regions outside the standard partition (`cn-`, `us-gov-`, `us-iso*`,
  `eu-isoe-`) are refused up front; the bridge's exact-URL check would
  refuse their endpoints anyway.
- The client takes its signer, transport, sleeper and jitter by reference,
  so the CLI wiring (follow-up) supplies the guarded credentials, the
  R-ROOTS bounded service-plane `SdkTransport::for_public_service`,
  `ThreadSleeper` and `FullJitter`.

**S2 field map (SDK output to seam).** Only members the checks read; an
absent value is omitted, never `null`; list order is kept; `0` and `false`
stay distinct from absent.

| Operation | Seam members emitted |
|---|---|
| DescribeDBClusters | `DBClusters[]`: `BackupRetentionPeriod`, `DeletionProtection`, `EngineVersion`, `DBClusterParameterGroup`, `VpcSecurityGroups[].VpcSecurityGroupId`, `StorageEncrypted`, `EnabledCloudwatchLogsExports[]`, `DBSubnetGroup`, `PreferredMaintenanceWindow`, `DBClusterArn`, `StorageType` |
| DescribeDBInstances | `DBInstances[]`: `DBInstanceIdentifier`, `DBInstanceClass`, `AvailabilityZone` |
| DescribeEvents | `Events[]`: `Message`, `EventCategories[]`; `Marker` |
| DescribeDBClusterParameters | `Parameters[]`: `ParameterName`, `ParameterValue` |
| DescribeDBSubnetGroups | `DBSubnetGroups[].Subnets[].SubnetAvailabilityZone.Name` |
| ListTagsForResource | `TagList[]`: `Key`, `Value` |
| GetMetricStatistics | `Datapoints[]`: `SampleCount`, `Average`, `Sum`, `Minimum`, `Maximum` (finite only) |
| DescribeAlarms | `MetricAlarms[].AlarmName` |
| DescribeSecurityGroups | `SecurityGroups[].IpPermissions[]`: `IpRanges[].CidrIp`, `Ipv6Ranges[].CidrIpv6` |
| ListSecrets | `SecretList[]`: `Name`, `Description`; `NextToken` |

Request members accepted: exactly those the checks build (for example
`Filters[{Name, Values[]}]`, `Dimensions[{Name, Value}]`, ISO `StartTime`/
`EndTime`, integer `Period`, `Marker`, `NextToken`). Secrets Manager is
`ListSecrets` only; `GetSecretValue`, `BatchGetSecretValue` and
`DescribeSecret` are refused before any request is built (T-DG-04d).

**Dependency audit (spike criterion 3).** Original S2 and repair 1 changed
no manifest or lockfile. Repair 2 (#5544) adds an optional direct
`xmlparser = "=0.13.6"` dependency to toolkit-cli's `docgraph-live` feature
and its toolkit-cli dependency edge in Cargo.lock. This is the already
locked tokenizer used by roxmltree and aws-smithy-xml; it adds no package
or version to the lockfile and no default-build dependency.
The four service crates, `aws-credential-types`, `aws-smithy-runtime-api`
and `aws-smithy-types` are the existing optional `docgraph-live` pins, so
no TLS, root, crypto or connector crate is added and no SDK HTTP connector
is used (the bridge is the only HTTP client the SDK is given).

**Tests (`docgraph::tests::live`, recipe `docgraph-review-client`).**

| T-case | Test(s) |
|---|---|
| 04a | `t_dg_04a_all_ten_encodings_reach_the_injected_transport_signed` (URL, method, content type, exact final body bytes incl. RPC-v2 CBOR, `x-amz-target`, signing scope and signed header set, sent == signed, no framing, one shared deadline, 16 MiB cap); `t_dg_04a_unknown_pairs_and_unmapped_request_members_fail_before_the_transport` |
| 04b | `t_dg_04b_a_live_review_equals_the_seam_fixture_review` (whole review over the wire equals the seam fixture review, call by call and check by check); `..._zero_and_false_stay_distinct_from_absent`; `..._pagination_tokens_pass_through_one_page_per_call`; `..._truncation_wrong_types_and_service_errors_never_succeed` (XML, JSON, CBOR; message canary never kept) |
| 04c | `t_dg_04c_transient_failures_recover_or_exhaust_at_four_sends` (throttling, 5xx, connect, read timeout, CBOR throttling; four fresh signatures; sleeps 2/4/8 s); `..._denial_malformed_and_credential_failures_send_once_or_never`; `..._the_operation_deadline_stops_retries` |
| 04d | `t_dg_04d_a_live_review_only_lists_secret_metadata`; `..._repeated_tokens_and_the_page_cap_still_fail_the_check` |
| 04e | `t_dg_04e_generated_framing_is_removed_and_the_url_and_body_kept` (real SDK-generated requests); `..._the_bridge_refuses_sdk_signing_streaming_bodies_and_foreign_urls`; `..._no_sdk_credential_identity_is_ever_accepted`; `..._only_toolkit_signing_and_retries_run_for_every_operation` |

Red at `521a92b1` (tests only): job `825fac9c-f92a-4e32-b990-c8b71780a1d5`
fails to compile (`unresolved import crate::docgraph::live`). Existing
review/error/recorded goldens are unchanged and still run under both
feature sets (`docgraph-live-suite`, scoped_test).

**#5484 (re-review #5482 follow-up).** `sdk::database::tests` gains
`a_bounded_body_releases_its_socket_on_every_terminal_failure` (idle,
operation deadline, byte limit, connection cut mid-body) and
`a_bounded_body_releases_its_socket_at_its_end`: they take the connector's
response body directly (no `SdkStream`, no `ReleasingStream`), read the
client socket's drop counter right after the terminal poll, before the body
is dropped or the runtime driven again, and check that a later poll repeats
a failure, never a clean end. With `self.driver.release()` removed from
`BoundedBody::end` only, job `d965d330-5ce7-4c25-96af-e929036c646c` fails
("idle: the body poll failed (.. stalled ..) with the socket still open";
37 passed, 1 failed); with it restored, job
`d643b659-e2cf-4cbe-aae4-db3ebb0d8ce9` passes (38 passed). Public, payment
and offline fixtures are unchanged.

#### S2 repair 1: review #5516 blockers (#5521, #5522)

Exact-source review #5516 of `817eb1ee` returned CHANGES_REQUESTED with two
BLOCKERs. Both are repaired here and the reviewer's probes are now
permanent tests.

**#5521: whole-document XML validation.** The closing-root suffix check
admitted malformed documents that the SDK decoder then read as success:
a mismatched close, a second root, or a missing inner close, each still
ending in `</{Operation}Response>`. `well_formed_xml` (in `live.rs`) now
requires every successful Query/XML body to meet all of these:

- valid UTF-8;
- one well-formed XML document: matched nesting, a single root, only
  comments, processing instructions and whitespace outside the root, and
  declared namespace prefixes;
- no document type declaration (completed by repair 2's explicit token
  check below; repair 1's parser-default assumption was incomplete);
- a root element whose local name is the operation's `{Operation}Response`,
  in any namespace.

Anything else is a non-retried `InvalidResponse` after one send.

- **Parser:** `roxmltree` 0.14.1 with its default options. Those options
  refuse internal-subset DTDs but accept no-subset declarations (#5544);
  the explicit token check in repair 2 completes the refusal. It is
  already a direct toolkit-cli dependency, used by the payment
  plane, and is built on `xmlparser` 0.13.6, the same tokenizer the SDK's
  `aws-smithy-xml` 0.64.0 uses. Repair 1 changed no manifest or lockfile.
- **Open elements at end of input:** roxmltree 0.14.1 refuses a mismatched
  close, but it accepts end of input while elements are still open. The
  first fix commit, `63e9edb5`, missed this: job
  `dca1ed93-0346-40ae-9e23-8d181c083b52` failed the existing truncation
  case 1, where the body is cut after `</DBCluster>` and decoded with
  `BackupRetentionPeriod` 7. `element_closed` now also requires every
  element to be closed.
  - roxmltree spans an open element up to the end of its start tag, and a
    start tag cannot contain `<`: xmlparser refuses it even in attribute
    values.
  - So a closed element's span ends with `/>` or contains `</`.
  - DocumentDB and EC2 cases cut at an element boundary pin this.
- **Decoding:** the parser only validates. The SDK decoder still reads the
  unchanged bytes, so a document that passes decodes exactly as before.
- **Valid forms the tests pin as decoding unchanged:** the XML declaration,
  default `xmlns` namespaces, `ResponseMetadata`, comments and processing
  instructions inside or after the root, and empty lists in both the
  self-closing and the open/close form.

**#5522: a bounded public service-plane transport.**
`SdkTransport::new`/`with_resolver` resolve DNS synchronously inside the
connector poll, so a stuck lookup held the caller past the operation
deadline. The reviewer's probe returned after 401 ms against a 30 ms
deadline. The fix is a separate construction, `SdkTransport::for_public_service`
(`crates/toolkit-aws/src/sdk/service.rs`):

1. The URL and header guards are unchanged.
2. The lookup runs on S1's process-wide `LookupGate` (`sdk::lookup`, shared
   with the database and bounded provider planes): it runs on a detached
   thread and is awaited asynchronously, so the deadline drops the wait.
3. The answers pass the same public-address admission
   (`guarded_addresses`). A failed or empty lookup, or any non-public
   answer, is refused before a dial; mixed answers are refused, never
   filtered.
4. The actual-peer check is unchanged.

The residual is the same one the database plane accepts: the blocked
`getaddrinfo` thread runs until the system resolver returns. While it does,
every new lookup on the process gate fails closed at once with
`ABANDONED_LOOKUP_REFUSAL`, so at most one abandoned lookup is outstanding
per process.

`SdkTransport::new`, `with_resolver` and `with_connector`, the payment
transports and their tests are unchanged. CLI routing stays refused (#5507),
and that wiring must use this constructor.

| Blocker | Tests | Red (`ff114353`, tests plus a skeleton delegating to the ordinary composition) |
|---|---|---|
| #5521 | `docgraph::tests::live::t_dg_04b_malformed_xml_with_a_closing_root_never_succeeds` (DocumentDB and EC2: mismatched close, second root, missing inner close, crossed closes, trailing text, DTD, undeclared prefix, unclosed item, cut at an element boundary, root never closed); `t_dg_04b_valid_empty_namespaced_and_commented_xml_decodes_unchanged` | `122b243e-0a23-47ca-8263-4bc9cd2f08db`: both fail ("docdb mismatched close: malformed XML became a success … BackupRetentionPeriod 7"; a trailing comment was refused); the other 15 S2 tests pass |
| #5522 | `sdk::service::tests::a_blocked_service_lookup_returns_control_at_the_operation_deadline`; `abandoned_service_lookups_are_capped_at_one_and_fail_closed`; `the_service_transport_keeps_the_public_and_private_endpoint_guards`; `the_production_service_transport_queues_on_the_process_wide_gate` (recipe `docgraph-service-dns`) | `ad59b164-5a65-4e34-874e-a88498a1bb86`: the deadline and cap tests fail ("held the caller past its deadline: 5.0013s" against a 100 ms deadline); the guard and process-gate tests pass |


#### S2 repair 2: explicit document type refusal (#5544)

Re-review #5516 found that roxmltree's default DTD refusal did not cover
`xmlparser::Token::EmptyDtd`: plain, SYSTEM and PUBLIC declarations without
an internal subset still decoded as successful review data. The bridge now
checks the pinned tokenizer's structural stream before any modeled decoder,
refusing both `DtdStart` and `EmptyDtd` explicitly as a non-retried
`InvalidResponse` after exactly one send. This applies to all ten operations
and every HTTP status, including non-2xx service-error bodies. No DTD URL is
resolved or fetched. Comments, CDATA and processing instructions are distinct
tokens: their text is never searched for markup. Non-XML bodies continue to
the same operation validation/decoder as before; valid bytes are unchanged.

Five separate permanent tests cover the review's DocumentDB plain/SYSTEM/
PUBLIC and EC2 plain/SYSTEM no-subset probes. The all-operation matrix adds
empty and populated internal subsets, prolog declaration/comment handling,
and HTTP 200/400/429/500, asserting `InvalidResponse`, non-retryability,
exactly one send and no backoff. Existing malformed nesting, partial,
multiple-root and valid-response equality tests remain. The equality tests
also cover DOCTYPE-looking text in comments and CDATA for both services.

Red source: tests-only `54697ae9af9e6d5a49dea5ad6f118cd3e9423055`, based on
merge `9001128661f3783d2ad90f84c7ec91459fb9968c` (repair-1 `87f1c3fa` plus
rolling `921aa719719982c1cb4c533880b5a562cd537fcf`). Daemon job
`4c7d5e13-1e8a-4a8d-84bb-5af9fdfbf3b8` ran 23 S2 tests: all five required
probes decoded as success and the status matrix failed on a 400 service
error; the other 17 tests passed, including valid comment/CDATA equality.
The final green and gate job IDs are recorded in the #5489 handoff.

The merge keeps both feature-gated modules (`live` for S2 and `live_seed`
for S3), all S3 Gremlin CLI wiring, and both migration-document sections.
The accepted #5522 DNS construction and guards are untouched by repair 2.
The review CLI still refuses `--live`; no live AWS run is authorized here.

#### S3 Gremlin adapter (#5515; R-DG-05, R-DG-07 limits)

**Dependency decision.** `tokio-tungstenite` `=0.29.0` with default features
off and only `handshake` enabled. It is optional, behind a new toolkit-aws
feature `gremlin` (`sdk` plus the WebSocket crate). Only toolkit-cli
`docgraph-live` enables `gremlin`, so the default build and the plain `sdk`
build compile no WebSocket crate. This is the maintained RFC 6455 codec the
design asks for, and the smallest option that keeps a reviewed client
handshake: it checks `101`, `Upgrade`, `Connection` and `Sec-WebSocket-Accept`.
Hand-rolling the framing was rejected ("do not invent codecs"). `handshake`
uses only crates already in the lock (http, httparse, sha1, data-encoding).
No TLS, connect, URL or root feature is enabled. The connection is the
database capability's rustls/aws-lc stream, so no second TLS or root stack is
added.

The lockfile adds five packages: tokio-tungstenite 0.29.0, tungstenite
0.29.0, and rand 0.9.5, rand_chacha 0.9.0 and rand_core 0.9.5. tungstenite
depends on rand unconditionally to generate masking keys and the
`Sec-WebSocket-Key`. None of the five is a TLS, root or certificate crate, and
neither ring nor webpki-roots appears. rand_chacha is a ChaCha CSPRNG used for
nothing but that masking. The sync job's log shows exactly these five
`Adding` lines. Gremlin request ids come from the aws-lc `SecureRandom` that
rustls already carries. Licences are MIT/Apache-2.0; MSRV is 1.71 for
tungstenite and 1.63 for tokio-tungstenite. `futures-util` (already resolved)
is a direct optional dependency, used for `StreamExt`/`SinkExt`.

**Shared boundary.** `sdk::database::SecureOpener` (feature `gremlin`) runs
the database connector's steps 2-4 unchanged. These are fresh resolution,
validation of every answer and of the actual peer before any byte, and TLS
that trusts exactly the `ValidatedRoots` and checks the original host name.
The HTTP exchange now calls the same `secure_stream`, which is a pure
extraction with no behaviour change. So the Gremlin plane cannot send a signed
byte before the peer check (#5387). Verdicts keep their fixed `GuardRejection`
text. Any other dial or TLS failure becomes one fixed message, "the TLS
connection to the database could not be established", and rustls text is
never forwarded.

**`sdk::gremlin::GremlinConnection`:**
- The upgrade `GET https://HOST:8182/gremlin` has an empty body. It is signed
  with the toolkit `SigV4Signer` (service `neptune-db`) before any DNS or
  socket work. Missing credentials, an unsigned result, a port other than 8182
  or a region other than the policy's each send nothing.
- The handshake carries the signed `host` (with its port), `x-amz-date`,
  `x-amz-security-token` and `authorization` verbatim. The handshake layer
  then adds `Connection`, `Upgrade` and `Sec-WebSocket-*`, which are therefore
  not signed fields.
- A non-101 answer is reported as a status number only.
- Each `submit` sends one GraphSON v2 `eval` as a binary frame with the
  drivers' MIME prefix: traversal source `g`, bindings typed as data, and a
  fresh v4 request id. It collects 206 batches through the final 200/204 for
  that id.
- A foreign id, a Gremlin error status, an unsupported type, malformed JSON, a
  premature close, idle silence or the deadline fails the statement. Server
  text is never forwarded.
- Any failure spends the connection: the socket is dropped at once, later
  calls fail without sending, and nothing is reconnected or retried. A failure
  after send says the outcome is unknown.
- `close` sends a close frame within `CLOSE_BUDGET`, capped by the invocation
  deadline. Dropping the connection drops the socket synchronously.

**R-DG-07 limits (PM ruling 3 on #5366):**
- The WebSocket frame limit is 1 MiB (`MAX_FRAME_BYTES`). tungstenite checks
  the declared length in the frame header before it reserves the payload, so a
  tiny frame declaring 2^40 bytes fails at once.
- Messages and the bytes of one operation are capped at 16 MiB.
- `GraphSON` depth is at most 64 enclosing containers (`MAX_DEPTH`). A typed
  list counts as one container; a typed scalar wrapper does not. A raw JSON
  nesting scan runs before parsing, so no tree is built past the limit.
- At most 100,000 decoded results per operation (`MAX_RESULTS`), counting
  every bulk copy. A bulk is checked before its value is decoded or copied.
- Decision: bulk copies are also capped at 16 MiB of compact JSON
  (`MAX_EXPANDED_BYTES`), so a small frame cannot expand to gigabytes within
  the count limit.
- The upgrade and each statement share one absolute deadline: 60 s per
  operation, capped by the 15-minute invocation deadline, with the 30 s idle
  bound between frames.

**CLI.** `toolkit neptune seed --live --confirm-wipe --endpoint HOST
--network-policy FILE [--region R]` (feature `docgraph-live`) checks
everything below in this order, before roots, credentials, DNS or any socket:
1. Argv consent and `--endpoint`, as in S1. `NEPTUNE_SEED_CONFIRM_WIPE` is
   ignored in live mode.
2. The policy file is read with its 16 KiB bound and parsed with
   `network_policy::parse`. Consent-shaped keys are refused, and the file path
   is never echoed.
3. `--endpoint` must equal the policy host, the policy port must be 8182, and
   the signing region must equal the policy region.

After those checks, credentials use `GuardedCredentials::until` with the same
deadline. The connection opens on the first statement, so the output order is
the script's. The S1 T-DG-01e carrier row for this invocation now expects the
policy's unknown-key refusal (exit 1) instead of "not available". `memory` and
`wa-review` stay unavailable in live mode (S4/S2).

Not changed, by decision: the `neptune` help text still says `--live` is
reserved. Editing it would move the pinned usage hash in `diag_contract.rs`,
a shared file S2 also touches. Follow-up: S6 caller/help update.

**Tests.** `sdk::gremlin::tests` (17) and `docgraph_live` (2 new):

| Case | Tests |
|---|---|
| T-DG-05a | `the_upgrade_is_signed_for_the_exact_authority_path_and_empty_body`: the received head is re-signed independently. `every_connection_is_signed_afresh`. `a_refused_upgrade_sends_no_statement`: a wrong secret gives a 403 and no frame. `missing_credentials_or_a_mismatched_scope_send_nothing`. `a_forbidden_answer_or_peer_never_receives_the_signed_upgrade`. `untrusted_and_wrong_host_certificates_get_no_upgrade`: untrusted, wrong-host and expired certificates. |
| T-DG-05b | `the_request_is_a_graphson_v2_eval_with_bound_values_and_unique_ids`. `partial_batches_are_collected_in_order_through_the_final_status`: 206/206/200 with bulk, plus 204. `foreign_ids_server_errors_and_invalid_types_fail_and_spend_the_connection`. `graphson_v2_typed_values_decode_and_others_fail_closed`. |
| T-DG-05c | `a_drop_after_the_statement_is_sent_is_one_send_no_retry_and_no_later_statement`. `a_silent_server_stops_at_the_idle_bound_with_an_unknown_outcome`. `close_is_bounded_against_a_server_that_never_answers_it`. |
| T-DG-05d | `live_seed_consent_shaped_policy_keys_and_ambient_wipe_confirm_nothing`. `live_seed_refuses_policy_mismatches_before_credentials_or_dns`. Also the S1 `live_wipe_ignores_ambient_confirmation` and the carrier and hostile-network tests. |
| T-DG-07b | `a_frame_at_the_limit_is_read_and_one_byte_more_is_refused`: exactly 1 MiB, then 1 MiB + 1. `a_tiny_frame_declaring_a_huge_length_fails_at_its_header`. `graphson_depth_is_admitted_at_64_and_refused_at_65`: plain lists, typed lists, maps, and 10,000 raw levels. `bulk_expansion_is_admitted_at_100000_results_and_refused_at_100001`: one past, `i64::MAX`, plain plus bulk, across batches, and the byte cap. |

T-DG-09a: `connect_live`, the production constructor (system resolver and
TCP dialer), joins the live identifiers. No test names it; the tests use the
injected `SecureOpener::new`. Recipes: `gremlin-sync` (fmt plus an offline
lock sync), `docgraph-gremlin` and `clippy-gremlin`.

Implementation slices remain Rust, use scoped optional service dependencies,
daemon-owned jobs and independent exact-SHA review. Each owns a requirement/test
matrix and deliberate CLI/output changes; no worker may treat this proposal as
authorization for live AWS calls. This design's validation is build-free:
source/requirement review and a docs-only diff check.

### Open decisions and operator gates

| ID | Owner/type | Recommendation, assumption and required ruling/grant |
| --- | --- | --- |
| Q1 | PM product choice — ACCEPT | PM ff294c79 accepted (b) on #5318: two-step user-run declarative collection plus Rust normalization, no MongoDB driver and continued URI/TLS-flag refusal. This repair specifies a per-command manual procedure and its scale cost; automation would require a separate design/review. |
| Q2 | PM CLI compatibility choice — provisionally ACCEPT | PM ff294c79 accepted the proposed flags subject to pre-review tightening. #5322 tightens R-DG-01/05 to argv-only live/consent flags, ignoring environment wipe confirmation for live runs while preserving replay parity. Other proposed region/network-policy, startup-config and timeout changes remain as reviewed. |
| Q3 | Security pre-review — ACCEPT | Reviewer 887003f3 ruled ACCEPT in #5321: exact standard AWS database host/port/region and operator-listed RFC1918/ULA CIDRs; every DNS answer and connected peer checked before signed bytes, with original hostname for TLS/signing and reconnect revalidation. R-ROOTS infrastructure reuse is correct; PaymentRoots remains payment-only. No global/provider/payment admission change. VPC-interface-endpoint review API use and non-RFC1918/ULA VPC CIDRs fail closed pending review. S1 must move this capability's peer check before transmission; O1 remains required. |
| Q4 | Engineering choice for implementation review | Resolve in the first implementation slice via the explicit offline S2 spike exit criteria, before live code. Optional official service codecs/WebSocket codec share toolkit signer, credentials, TLS, retries and deadlines; prove framing handling, auth/identity suppression, R-LOG and audited exact feature/lockfile closure. |
| Q5 | Engineering support matrix | Record fixture-supported standard endpoints, GraphSON v2 and exact DocumentDB/MongoDB/shell/export versions in the first implementation slice before live code, as an S2 spike exit criterion. Unsupported forms fail closed. Operator deployment versions are required for live acceptance, not guessed. Recorded for S1 in "S1 continuation: credential deadline, DNS cap and remaining evidence"; GraphSON v2 fixtures follow in S3. |
| O1 | Operator gate: credentials/access | Supply an approved account/role, target IDs/hosts, signing region, network CIDRs and connectivity. Authorize metadata access and narrowly scoped database actions; do not send secrets in Issues. Credentials are resolved locally through existing approved sources. |
| O2 | Operator gate: spend | Approve resources/cost ceiling and run duration for live Analytics/Neptune/DynamoDB work, and any graph creation separately. Prefer existing disposable resources; neither `--live` nor a test flag authorizes spending or deployment. |
| O3 | Operator gate: real database/user data | Approve read-only statistics access and capture custody. Separately identify a disposable graph/table and authorize specific writes/wipe/consolidation; no destructive test on real user data. Operator owns final cleanup/snapshot retention. |
| O4 | Operator gate: live acceptance scope | Choose which T-DG-09 live cases may run and the approved engine versions. Deferred paid/destructive cases remain explicit evidence gaps; offline passage alone cannot retire the corresponding live legacy path. |

Only O1..O4 require real-resource authorization and gate live runs, not offline
slices; the PM will consolidate those grants in one operator to-do after the
offline slices land. Q1 and Q3 are accepted, Q2 is provisionally accepted with
the argv-only tightening above, and #5322's repair still needs independent
re-review before implementation. Q4/Q5 are engineering gates resolved with
recorded evidence in the first implementation slice, before live code.

## GraphRAG / agentic memory (#4886, item 1)

The Rust surface is `docgraph::memory`, with `toolkit neptune memory
--operation OP --input FILE [--dry-run | --recorded FILE]`. The input is a JSON
object of operation arguments. Dry run prints a JSON array of ordered request
objects; replay uses the `calls` format and strict ordered matching like the seed
session. A retrieval dry run cannot predict
response-dependent expansion, so it prints the initial request; replay expands
from the returned chunks. No live network client is provided.

Requirements: **M1** execute-query validates graph IDs and binds openCypher
parameters; **M2** escaping, Unicode character chunk boundaries and SHA-256
entity IDs; **M3** graph creation preserves secure defaults and tags; **M4**
GraphRAG document/chunk/entity/link ingestion, embedding and vector search;
**M5** vector search followed by entity, relation and neighbor expansion;
**M6** short-term message requests preserve timestamp, TTL, ordering and limit;
**M7** Gremlin entity, relationship and recall builders bind values safely;
**M8** analytics memory embedding, semantic and hybrid recall; **M9**
consolidation decays facts older than 90 days by 0.9 and deletes below 0.1;
**M10** deterministic dry-run and recorded replay, including malformed results.
Invalid chunk sizes and overlaps greater than or equal to the size are rejected
to avoid the original infinite loop. Compared with the Python helpers, graph IDs
are validated for every openCypher request, dynamic Gremlin property keys are
bound rather than interpolated, and vertex_filter JSON is compact rather than
using json.dumps spacing.
Live AWS, DynamoDB and Gremlin clients, driver selection and Python retirement
are outside this item; the legacy helpers remain until the live follow-up.

### Memory command inputs and recordings

`--operation` and `--input` are required; the latter is a JSON object. Exactly
one of `--dry-run` and `--recorded` is needed. Request arrays contain the AWS
API arguments, Gremlin `gremlin`/`bindings`, or DynamoDB operation and arguments.
`--recorded` accepts `{"calls":[{"request":{...},"response":{"results":[]}}]}`:
each step requires a request object, matched in order ignoring object key order,
including on error steps. Unused or exhausted calls and malformed typed members fail. A
create response uses `{"id":"g-..."}`, a DynamoDB query response uses
`{"Items":[...]}`, and query/Gremlin responses use `{"results":[...]}`.
`retrieve` returns the expanded context array; `recent-messages` returns items
in chronological order; `hybrid-recall` returns `graph_recall` and
`semantic_recall`. Other requests return an array of result arrays, except graph
creation returns its id. A missing `results` list is empty; a present null or
non-list is invalid. Recorded errors are reported without service messages.

| Operation | JSON inputs (optional inputs in parentheses) |
| --- | --- |
| `create-graphrag`, `create-memory` | `graph_name` (optional for memory), (`memory_gb`, `embedding_dimension`, `generation_model`) |
| `query` | `graph_id`, `query`, (`parameters` object) |
| `vector-search`, `retrieve`, `semantic-recall` | `graph_id`, `embedding`, (`top_k`); `vector-search` also accepts `vertex_label` |
| `ingest-document` | `graph_id`, `doc_id`, `title`, `source` |
| `ingest-chunk` | `graph_id`, `chunk_id`, `doc_id`, `text`, `sequence`, `embedding`, (`prev_chunk_id`) |
| `ingest-entity` | `graph_id`, `name`, `entity_type`, `description`, `embedding` |
| `store-embedding` | `graph_id`, `vertex_id`, `embedding` |
| `link-chunk-entity` | `graph_id`, `chunk_id`, `entity_name` |
| `link-entities` | `graph_id`, `source`, `target`, `rel_type` |
| `store-message` | `session_id`, `role`, `content`, `now_ms`, (`table`) |
| `recent-messages` | `session_id`, (`limit`, `table`) |
| `remember-entity` | `entity_name`, `entity_type`, (`properties` object) |
| `remember-relationship` | `entity_a`, `entity_b`, `rel_type`, (`confidence`) |
| `recall-entity` | `entity_name` |
| `remember-embedding` | `graph_id`, `entity_name`, `description`, `embedding` |
| `hybrid-recall` | `graph_id`, `entity_name`, `embedding` |
| `consolidate` | `graph_id` |

The public `docgraph::memory` module also provides `chunk_document`,
`escape_cypher_string`, `entity_id`, request builders and a `MemoryClient`
trait for injected clients. Escape preserves newlines as the source helper did;
parameterized queries are preferred. `store-message` uses a single injected
millisecond instant instead of two independent clock reads. Recorded DynamoDB
requests include `operation` and `table` for deterministic matching. A dry run
of `retrieve` prints the initial vector search only because later requests
require response IDs; replay executes the expansions. The dry-run and replay
commands cannot perform live writes or destructive consolidation.
