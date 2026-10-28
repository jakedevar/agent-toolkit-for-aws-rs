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
10. **Caller paths are redacted before they are shown** (#4907). Every path this
    slice prints (`--recorded`, `--analysis-data`, in the progress line and in
    read or parse errors, and the `Results:` / `Report:` banners under
    `--output`) goes through the shared redactor (`toolkit_aws::redact_text`:
    URL userinfo, labelled credentials, key ids) and then has control,
    bidirectional and invisible characters escaped; paths are never shortened.
    The file reader is `startup::read_text` with that redacted label (the
    shared `read_json_document` escapes but does not redact its label). A path
    without credential-shaped text is shown as given.

## Not in this slice

Tracked in #4886: `graphrag_pipeline.py` and `agentic_memory.py` (helper
libraries), the live AWS / Gremlin / MongoDB bindings, and retiring the Python
files and updating `SKILL.md` / `references/well-architected.md` callers. The
Python stays in place until then: the Rust commands cannot reach AWS or a
database yet, so replacing the callers now would remove a working path.
