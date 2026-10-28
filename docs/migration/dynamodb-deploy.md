# Slice `dynamo-deploy` (Issue #4391): DynamoDB deployment and teardown plans

Rust replacement for two legacy scripts under
`skills/specialized-skills/database-skills/amazon-dynamodb/scripts/` (read-only
migration evidence; neither is edited or deleted here, their callers and
retirement belong to #4415):

| Python source | Rust command | Code |
| --- | --- | --- |
| `deploy_model.py` | `toolkit dynamodb deploy-model` | `crates/toolkit-cli/src/dynamodb_deploy/` (`mod.rs`, `design.rs`, `plan.rs`, `policy.rs`, `identity.rs`, `recorded.rs`) |
| `generate_teardown.py` | `toolkit dynamodb generate-teardown` | `dynamodb_deploy/teardown.rs`, `teardown_template.txt` |
| (shared) | private atomic output writer | `dynamodb_deploy/write.rs` |

## Scope ruling: offline plan generators

The manager's ruling for this slice is that **no live AWS client exists**.
Both commands only generate a document; nothing is created, changed or
deleted, no script is run, and tests never touch AWS.

- `deploy-model` validates the design and config, asks a client for the caller
  identity (the only AWS state the plan needs), refuses a production identity
  and writes the **deployment plan**: the exact `CreateTable`, `CreateRole`,
  `PutRolePolicy` and `CreateFunction` requests a live deploy would send.
- `generate-teardown` renders `teardown.sh` from a run manifest and writes it
  only with `--yes-generate-teardown`.
- The live execution path is Issue #4801. Until then `--manifest-out` is accepted
  (caller contract) but **never written**: no resource exists, so there is no
  created-resources manifest, and the command prints that.

## CLI contract

```
toolkit dynamodb deploy-model --model FILE --config FILE --manifest-out FILE --yes-deploy
    [--recorded FILE --plan-out FILE] [--created-at TS] [--run-id ID]
toolkit dynamodb generate-teardown --manifest FILE --out FILE --yes-generate-teardown
```

Rows in `COMMANDS` carry `repeatable_flags: &[]` (neither script has an
`action=append` flag) and `passthrough: false`.

| Flag | Python | Rust |
| --- | --- | --- |
| `--model`, `--config`, `--manifest-out`, `--yes-deploy` | required | unchanged; `--yes-deploy` is checked first (exit 2) |
| `--recorded FILE` | – | recorded caller identity (below) |
| `--plan-out FILE` | – | where the plan is written; required with `--recorded` |
| `--created-at TS`, `--run-id ID` | – | deterministic clock/id for fixtures (`YYYY-MM-DDTHH:MM:SSZ`, 32 lowercase hex); defaults are the system clock and a random id |
| `--manifest`, `--out` (teardown) | required | unchanged |
| `--yes-generate-teardown` | – | **new, required** consent flag |

Exit statuses: `0` success; `2` for every refusal the script reported through
`_die` (design, config, identity, consent, unwritable output) and for usage
errors. Diagnostics start `error: ` (the toolkit's form) instead of `ERROR: `.

## Recorded caller identity (`--recorded`)

The Python read two things from AWS: `sts get-caller-identity` and
`iam list-account-aliases`. They go through the `IdentityClient` trait. This
build has two implementations: `RecordedClient` (replay) and `NoLiveClient`
(every call fails with a connect error, so `deploy-model` without `--recorded`
validates the design and config and then stops with
`error: cannot reach AWS endpoint: ... this build has no live AWS client`).

Format and rules follow `toolkit_aws::recorded` (#4727, README rule):

```json
{"calls": [
  {"service": "sts", "operation": "GetCallerIdentity", "request": {},
   "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}},
  {"service": "iam", "operation": "ListAccountAliases", "request": {},
   "response": {"AccountAliases": ["acme-sandbox"]}}
]}
```

- Absent optional member → default; present `null` or wrong type → the
  recording is rejected, naming the step and member, never a value.
- The expected `request` of every step, response **and** `error` steps, is
  compared before the outcome is replayed. A call with no step left fails; a
  step the run never used fails (`the recorded document has N step(s) this run
  never used`).
- `error` steps replay `client` (default), `connect` or `no_credentials`
  failures with `code` and `message`.
- Response reads use the shared accessors: `Arn` and `Account` are
  `optional_text` (null → `sts get-caller-identity returned an invalid
  response`), `AccountAliases` is `optional_string_list` (null, a string, a
  number list → invalid response, **not** "no alias"). **Every** replay or
  service problem on the STS or the IAM step stops the run (exit 2, no plan):
  a request that differs from the recorded one, a call with no step left (a
  recording without an IAM step), a recorded `error` step and a response of
  the wrong shape. See intentional changes 10–12.
- Every remaining JSON shape read in the module is either through these
  accessors or marked `// recorded-shape:` (design/config reads, which are
  caller input, not responses). `tests/recorded_contract.rs` passes for this
  slice.

## What `deploy-model` writes (the plan)

`--plan-out` (owner-only `0600`, ASCII-escaped JSON, `plan_version: 1`,
`executed: false`): `account`, `alias`, `aws_profile`, `region`, `run_id`,
`created_at`, `prefix`, `tags`, then

- `tables[]`: `{name, original_name, create_table}` where `create_table` is the
  `CreateTable` request of `_build_create_kwargs` (same member order):
  `TableName`, sorted `AttributeDefinitions`, `KeySchema`, `BillingMode`
  (`PAY_PER_REQUEST`, or `PROVISIONED` with `ProvisionedThroughput` on the
  table and each GSI), `DeletionProtectionEnabled: false`, `Tags`, then
  `GlobalSecondaryIndexes`, `VectorIndexes`, `StreamSpecification`.
- `iam.create_role` and `iam.put_role_policy`: the policy documents as the
  exact `json.dumps` text the API receives.
- `lambda.create_function` and `lambda.code_package` (module `benchmark.py`
  from `scripts/benchmark_lambda.py`, `bundle_vector_sdk`, minimum botocore
  `1.43.64` for vector designs). The zip is **not built** in this build.

Unchanged behavior, covered by `tests/dynamodb_deploy.rs`: validation order
(design → prefix → config → identity → table requests), every refusal message
(with the escaping change below), prod markers `prod|production|prd|live`
matched case-insensitively on `"<arn> <alias>"`, attribute-type precedence
(`entities[]` < `attribute_definitions`, unknown type → `S`, three spellings
of an INCLUDE list), vector rules (1–4096 dimensions, three distance
functions, projection string shorthand, limit of 5 per table, no provisioned
capacity), `{**cfg_tags, **built_in_tags}` tag order, `rstrip("-") + "-"`
prefix normalization, auto-generated `ddb-skill-bench-<date>-<uuid8>` prefix,
`dry_run` output lines, and the global provisioned-capacity banner.

## What `generate-teardown` writes

The script is the Python template with these differences only: the header names
the Rust command; every value is shell-quoted; and the account is read with
`aws sts get-caller-identity --query Account --output text` instead of piping
the JSON through `python3 -c` (the generated script needed Python only for
that). The `--confirm`, `--dry-run` and `--delete-logs` flags, the identity
check, the prefix guard on every delete and the wait loop are byte-identical.
Deleting commands appear once each, behind those guards.

## Intentional changes

1. **Consent for teardown generation.** `--yes-generate-teardown` is required.
   Without it nothing is written (exit 2, checked before the manifest is read).
   `--yes-deploy` is not accepted as consent. Callers (SKILL.md stage 6) must
   add the flag when the Python script is retired (#4415, #4801).
2. **Offline plan instead of deployment.** See the scope ruling; `--plan-out`
   and `--recorded` are new; the 5-second interactive abort window is gone
   (nothing can be aborted); the banner says `PLAN ONLY`.
3. **Names validated before they reach an ARN or a script.** The script only
   required the prefix to start with `ddb-skill-bench-`. The prefix is in the
   IAM resource ARNs (`table/<prefix>*`) and the teardown guard, so it is now
   limited to `A-Za-z0-9_-`, at most 48 characters (so no generated name needs
   the old `[:64]` truncation). Table names (1–255 of `A-Za-z0-9_.-`), region
   (`us-east-1` shape), the profile name (printable, ≤128), tag keys/values and
   the caller account (exactly 12 digits, because it goes into the trust
   policy's `aws:SourceAccount`) are checked. Duplicate table names are
   refused (the script created the same table twice).
4. **Teardown manifest is untrusted.** Besides the script's prefix checks, the
   account must be 12 digits, the region well formed, names in their charsets;
   `null` lambda members mean "not set" (the script would have printed
   `None` into the shell). Every value is single-quoted with `'` escaped; the
   template is filled in one pass so a value cannot expand a marker. `--out`
   equal to `--manifest` is refused.
5. **Outputs**: written to a unique temporary file in the destination
   directory (`create_new`, mode `0600`; `0700` for the script), flushed,
   renamed; a symbolic link or non-regular file at the destination is refused;
   the script was `0755`, the manifest/plan were world-readable.
6. **Wrong-typed design members are diagnosed.** Where the script crashed with a
   traceback (`AttributeError`, `KeyError`), the command refuses with a message
   (exit 2). Python `or` semantics are kept: `null`, `false`, `0`, `""`, `[]`
   and `{}` mean "not set".
7. **Added range checks the script left to AWS**: GSI/vector `ProjectionType`
   in `ALL|KEYS_ONLY|INCLUDE`, `streams.view_type` in the four stream views,
   provisioned units ≥ 1, `lambda_memory_mb` 128–10240, `lambda_timeout_seconds`
   1–900. A GSI projection given as a bare string is refused (it crashed).
8. **Diagnostics escape text.** Caller, file and AWS text goes through
   `toolkit_core::quote_untrusted` / `safe_path`, so quoted values print with
   double quotes where the script printed Python `repr` (`'name'`); lists print
   as `["a", "b"]`. `tests/diag_contract.rs` passes for both commands.
9. **Alias response shape.** A malformed `AccountAliases` is a refusal, not an
   empty alias (it would hide an alias from the production check).
10. **A failed alias lookup refuses (#4822).** The script treated a failed
    `iam list-account-aliases` (for example `AccessDenied`) as "no alias" and
    ran the production check on the ARN alone. The production preflight cannot
    be proven without the alias, so a recorded IAM service error, a missing
    IAM step, a request mismatch and an exhausted recording all refuse with
    exit 2 and no output. The mismatch case matters most: before this change
    a mismatching IAM step was consumed and swallowed, which dropped an
    `acme-production` alias and let the run through. Two tests that encoded
    the old degrade-to-empty-alias behaviour were rewritten for this rule
    (`every_failed_or_unusable_alias_lookup_refuses_the_run`, unit
    `a_failed_alias_lookup_refuses_the_run`).
11. **The caller ARN is required and checked (#4823).** The script defaulted a
    missing `Arn` to `""`. Now `Arn` must be present, non-empty and an IAM or
    STS ARN (`arn:<aws|aws-cn|aws-us-gov>:<iam|sts>::<account>:<resource>`,
    resource characters `A-Za-z0-9+=,.@_-/:`) whose account equals the
    validated 12-digit `Account`; anything else refuses before any output. A
    present alias must be 3 to 63 lowercase letters, digits and hyphens (a mixed-case or credential-shaped value is refused and never shown). An absent `AccountAliases` or `[]`
    means no alias (supported). Otherwise every entry is validated against that
    shape before one is chosen, so `[""]` is refused rather than read as no
    alias, and a list of more than one entry is refused as ambiguous (AWS allows
    one alias per account, and a later production alias must not be discarded
    behind a valid first one). Diagnostics name the field and shape, never a
    value (#4840). Offline
    recordings stay unauthenticated fixtures: this is structural validity only.
12. **Diagnostics for recorded and manifest values never echo them (#4824).**
    `check_account`, `check_caller_arn`, `check_alias`, the prefix, table,
    Lambda, policy, region, run id and profile checks name the field and the
    expected shape only; escaping (`quote_untrusted`) is not redaction. The
    teardown manifest takes the same rule, including the "table/function/role
    does not start with the prefix" refusals (a table count replaces the name
    list). Values that already passed validation (the account, the ARN and the
    alias in the production refusal and the banner) are still shown. Design
    and config values (the model's table names, config numbers) keep their
    escaped echo: they are the operator's own input, not a recording or a
    manifest. AWS error codes and messages of a recorded `error` step go
    through the shared AWS redactor (`[REDACTED]` for credential-shaped text).

## Security surface (for the security pre-review)

**Generated IAM policy text** (written to the plan, never applied here):

- Role trust policy: principal `lambda.amazonaws.com` only, action
  `sts:AssumeRole`, condition `StringEquals aws:SourceAccount = <12-digit
  account from the identity>`.
- Inline role policy, three statements, scoped by the validated prefix and
  region/account: (1) `dynamodb:DescribeTable, GetItem, Query, Scan,
  BatchGetItem, PutItem, UpdateItem, DeleteItem, BatchWriteItem,
  TransactWriteItems, TransactGetItems` on `table/<prefix>*` and
  `table/<prefix>*/index/*`; (2) `dynamodb:SearchVectors` on
  `table/<prefix>*/index/*`, deliberately without a condition block (the
  fine-grained condition keys are absent from its request context, so a
  conditioned statement would deny it) and with the index wildcard the script
  documents for a throwaway bench role; (3) `logs:CreateLogGroup,
  CreateLogStream, PutLogEvents` on `log-group:/aws/lambda/<prefix>*:*`.
  No `*` resource, no `iam:*`, no `sts:*`. The text is byte-identical to the
  script's (pinned by `the_iam_documents_are_exactly_the_scripts_and_scoped_to_the_run`).
- Because the prefix is interpolated into these ARNs, it can no longer carry
  `*`, `?`, `/` or quotes (change 3).

**Credential and network surface.** None added. The commands read no
environment variable, no credential file and no profile; `aws_profile` is only
a string copied into the plan and into the teardown script's `AWS_PROFILE`
hint. No socket is opened. The generated `teardown.sh` is the only artifact
that can reach AWS, and only when a human runs it with `--confirm`: it
re-checks the caller account against the manifest, restricts every delete to
the manifest prefix, and (new) holds only validated, quoted values. It is
written `0700` and never executed by the toolkit.

**Fail-closed identity (changed from the script)**: a failed
`iam list-account-aliases`, any replay mismatch, a missing or malformed
caller ARN and a malformed account or alias all refuse (see intentional
changes 10–12).

**Files**: plan and script are created exclusively, owner-only, renamed into
place; symlink destinations are refused; a failed write removes its temp file
and leaves an existing destination intact.

## Tests

`crates/toolkit-cli/tests/dynamodb_deploy.rs` (34 process tests, offline; the `review_*` tests replay the #4819 reviewer reproductions from `tests/data/dynamodb_deploy/`): plan
golden values for tables/GSI/vector/streams/provisioned, the IAM documents,
every design/config/identity refusal, prod markers, recorded-input rejection
(null, wrong type, missing, mismatch on success and failure steps, unused and
exhausted steps), output safety (mode, atomic replace, symlink, directory,
linked inputs), teardown consent, rendering, `bash -n`, running the script to
its `--confirm` refusal, hostile manifest text, prefix/lambda/role guards and
hostile paths. Unit tests in `dynamodb_deploy/tests.rs`. The existing
`iterate-design` test now expects the registered `deploy-model` step to refuse
the fixture config before any cloud action. Nothing in the tests deploys or
deletes.

## Follow-ups

- #4801: live identity client and apply path, the manifest, the Lambda package
  language decision (the benchmark handler is a Python payload for the managed
  `python3.12` runtime, an explicit AGENTS.md compatibility question to be
  resolved with #4390), and `iterate-design` wiring.
- SKILL.md stages 3 and 6 and the Python scripts are retired by #4415.
