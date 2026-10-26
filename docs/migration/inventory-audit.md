# Python inventory audit — #4371

Baseline: `f86cc8fb464b26fd4d187bfa9d5cd0eb59615f26` (matches `baseline_commit`
in `python-inventory.json`). Audit method: read-only shell inspection of the
committed blobs (`git ls-files`, `git cat-file`, `ast.parse` on in-memory text).
No Python was executed from the repository tree and no live AWS calls were made.
This audit validates mapping, classifications, canonical duplicates, payload
kinds and documentation metadata. It is not a runnable replacement test.

## Summary

`docs/migration/python-inventory.json` lists 391 paths. All 391 are tracked,
unique and match `git ls-files -- '*.py'` exactly — no missing and no extra
paths. Blob-level deduplication of the 391 paths yields 216 distinct blobs;
line totals reconcile with `docs/migration/README.md` (127,558 raw /
97,599 dedup by blob) except one test-line figure that does not reconcile
(finding F-1).

Classifications, as committed:

| classification | paths | distinct blobs |
|---|---|---|
| toolkit-helper | 253 | — |
| test-or-diagnostic | 88 | — |
| external-service-payload | 50 | — |

Slices: 35 distinct (`payments-policy` 9, `payments-bridge` 3, `security-hooks`
1, `bedrock-fetch` 2, `ai-data` 6, `ai-payloads` 56, `ai-models` 12, `cw-api`
26, `cw-domain` 26, `serverless` 4, `launch` 12, `msk` 6, `startup-fixtures`
16, `startup-validators` 11, `telemetry` 62, `startup-models` 7,
`startup-diagram` 4, `startup-scoring` 9, `heroku` 5, `bedrock-resolve` 8,
`bedrock-probes` 6, `bedrock-report` 6, `startup-contracts` 37,
`startup-tools` 3, `sync` 3, `marketplace` 11, `aurora` 6, `docgraph` 6,
`dynamo-bench` 3, `dynamo-cost` 2, `dynamo-deploy` 2, `cache-analysis` 9,
`cache-ops` 3, `relational` 6, `manifests` 2, `spec` 1).

## Findings

### F-1. README test-line figure does not reconcile (evidence-only)

`docs/migration/README.md` states 28,208 lines in named tests/diagnostics.
Measured from committed blobs, the `test-or-diagnostic` classification
dedup-by-blob line count is 29,065 (raw 29,781). Controlled exclusions
(conftest only: 28,959; tools/tests only: 28,978; both: 28,872) also do not
reproduce 28,208. Both files were introduced in the same commit, so the README
figure is a stale or differently-filtered count, not a drift caused by later
commits. No inventory correction is warranted: the inventory itself is
internally consistent. Follow-up recommended to recompute the README figure
from the inventory (F-2).

### F-2. Canonical-duplicate mapping is correct (46 + 45 + 3 + 1 = 95 non-self canonical)

Every entry's `canonical_identical_path` resolves to a tracked path whose blob
is byte-identical (0 errors across all 391 entries). The 95 non-self-canonical
entries fall into four families:

1. 46 `code_templates` notebook templates: 23 distinct blobs, each mirrored
   once between `plugins/aws-core/skills/aws-ai-ml/.../code_templates/` and
   `skills/core-skills/aws-ai-ml/.../code_templates/`; the plugins/aws-core
   side canonicalizes to the skills/core-skills path.
2. 45 telemetry copies: 9 vendored files in each of 4 skills
   (azure-to-aws, gcp-to-aws, heroku-to-aws, llm-to-bedrock) plus the 9 files
   under `plugins/aws-startup-advisor/skills/shared/telemetry/` — all
   canonicalize to `plugins/aws-startup-advisor/scripts/telemetry/...`.
3. 3 aws-data-analytics plugin scripts (`msk_sizing.py`, `compatibility.py`,
   `simulation_load_test_config.py`) canonicalize to
   `skills/specialized-skills/analytics-skills/...`.
4. 1 `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/emit-plan-json.py`
   canonicalizes to `plugins/aws-startup-advisor/scripts/emit-plan-json.py`.

No canonical path is missing from the repo and no canonical pair differs in
content, so no metadata correction is needed for this audit.

### F-3. Notebook-template payload classification is correct (14 syntax errors)

`ast.parse` fails on 14 of the 46 notebook templates — the 7
`processing-jobs/code_templates/` and 7 `training-jobs/code_templates/` files
(`dataset_definition_processing.py`, `framework_processing.py`,
`script_processing.py`, `spark_processing.py`, `custom_training.py`,
`jumpstart_training.py`, `recipe_training.py` in both mirror copies), because
they contain IPython magic (`%pip install`-style) lines. All 46 are already
classified `external-service-payload` in slice `ai-payloads`, which is correct:
they are shipped as source for external service payloads (SageMaker/Bedrock
notebook code templates), not migration targets. The 14 syntax errors are
expected for this classification and require no inventory correction.

### F-4. Archive-payload fixtures are data, not Python

Three zip fixtures exist at `plugins/aws-startup-advisor/fixtures/`
(`azure-rdfa-report-multi-zip/AllSubscriptions_ResourcesReport_2025-06-01_12-00-00.zip`,
`azure-rdfa-report-multi-zip/CompanyName_ResourcesReport_20240101000000083a.zip`,
`azure-rdfa-report-corrupt-zip/AllSubscriptions_ResourcesReport_2024-03-01.zip`).
They are consumed by the startup-advisor validators as report archives and are
not tracked as `.py`; they are not in the inventory and correctly so. The
`startup-fixtures` slice (16 paths) covers the Python fixtures, not these
archives.

### F-5. External proxy / runtime dependencies

- `plugins/aws-core/.mcp.json` runs the AWS MCP server through
  `uvx mcp-proxy-for-aws-cli` — a Python-based external proxy. It is a
  documented product dependency of the aws-core plugin's MCP server, not a
  migration target, and no Python file in the inventory drives it.
- `plugins/aws-startup-advisor/mise.toml` tasks (`test`, `lint:artifacts`,
  `lint:vendored-parity`, `lint:skill-refs`, `lint:consent-disclosure`,
  `lint:model-staleness`, `lint:model-id`, `lint:startup-model-pricing`)
  require `python3` and `uv`; the referenced plugin-local tools
  (`plugins/aws-startup-advisor/tools/sync-vendored.py`,
  `check-skill-refs.py`, `model-staleness.py`) are all present in the
  inventory under slices `sync` and `startup-tools`.
- `plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json` invokes
  `secret-safety.py` via `python3 || python || py -3`. If no interpreter is
  present the hook silently fails open (non-blocking PreToolUse exit), so the
  security control is skipped rather than enforced — a soft runtime dependency
  documented in `setup-instructions/setup.md` ("no python3 runtime beyond
  shell and uv"). See follow-up F-6.

### F-6. Remaining development-instruction conflict: customer-facing setup vs. hook/telemetry runtime

`setup-instructions/setup.md` (line 45) requires installation to work without
`python3`, and the telemetry README documents that unavailable Python skips
telemetry. These are consistent for the customer surface. The open product
conflict is the aws-core `secret-safety.py` PreToolUse hook: it silently
fails open when no Python interpreter exists, so the secret-fetch deny
behaviour is not guaranteed on a Python-free install even though the docs
state the runtime requirement is satisfied. Not fixable in this audit's scope
(hook code and hooks.json are outside the permitted doc paths). Filed as
follow-up F-6 (see Report section).

`CONTRIBUTING.md` line 13 already carries the fork policy ("Legacy Python
remains migration evidence until each replacement passes") — no conflict.

### F-7. CLI/JSON/error contracts observed (evidence, not change requests)

- Telemetry CLI contract: `cli.py` exposes `show | status` (consent) and
  `status | reconcile` (top-level); JSON output shapes are locked by the
  consent-disclosure-audit and the `artifact-contracts.json` /
  `artifact-contracts-baseline.json` baselines.
- Migration state contract: `.migration/<id>/.phase-status.json` and
  `.gate-failures.json` are the durable gate artifacts; the telemetry emitter
  diffs them against `.telemetry-snapshot.json` and posts one
  `migrationActivity` event per transition (RUN_STARTED, PHASE_COMPLETED,
  GATE_FAILED, RUN_COMPLETED), idempotent and run-locked.
- HTTP emission contract: 403 (launch gate closed), 429 and 5xx are retryable
  (leave the event unrecorded so only it is re-sent); 400 and post-send
  timeout are not retried. Consent gate: `record.is_accepted()` re-checked
  inside `client.post_event` so a new call site cannot emit by forgetting to
  ask.
- Vendored bundle contract: each of the 4 migration skills vendors the 9
  telemetry files plus a `version.json` sidecar under
  `references/vendored/telemetry/`; `sync_bundles.py --check` enforces byte
  parity and rejects unexpected files.

### F-8. Rust SDK / service-only-language blockers

- The 50 `external-service-payload` files are code templates for customer
  environments (SageMaker processing/training, Bedrock deployment/evaluation).
  They are product data, not code to port; the Rust migration scope is the
  253 `toolkit-helper` + 88 `test-or-diagnostic` files.
- The aws-ai-ml code templates import boto3, sagemaker, botocore and
  jsonschema at the customer side; no Rust SDK is expected to replace them.
- The telemetry bundle copies and the shared/telemetry subset are what the
  4 migration skills ship, so any Rust replacement for the telemetry module
  must keep the vendored bundle's exact 9-file surface (or update the 4
  `version.json` sidecars in the same change) — the `sync` slice's
  `sync_bundles.py` is the parity guard.

## Report: bounded follow-up issues to propose (not silently implemented)

1. F-6 — Recompute/fix `docs/migration/README.md` test-line figure from the
   inventory (docs-only change, no code).
2. F-5 — Decide whether the aws-core `secret-safety.py` hook should fail
   closed or degrade to an explicit warning when no Python interpreter is
   present on a Python-free install (product decision; touches
   `plugins/aws-core/com.anthropic.claude-code/hooks/` which is outside this
   audit's scope).
3. F-7 — When porting the telemetry module to Rust, keep the 9-file vendored
   bundle surface and `version.json` sidecar contract as a compatibility
   constraint, or explicitly plan to update all four sidecars in the same
   slice.

None of these expand the audit's implementation scope; all are recorded here
for the manager to dispatch as bounded follow-up slices.
