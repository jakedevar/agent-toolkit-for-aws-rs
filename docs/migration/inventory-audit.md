# Python inventory audit (#4371) — corrected revision

Status: documentation audit only, delivered for independent exact-SHA review. No slice
implementation, no Rust verification, no build job and no landed state is claimed here.
Implementation work belongs to the domain Issues named in the slice table (§6).

Provenance:

- Programme: #4369. This Issue: #4371 (slice `inventory`, wave 0).
- Audit source revision: `71319f9aecb7f81c872aa1ee0060f9474bc7cd1b` — the predecessor
  revision whose report the manager rejected with CHANGES REQUESTED. It is not this
  document's commit.
- Repair revision: the commit that contains this file on the allocated branch
  `rsi/7042bad4-9e9a-4dd4-95bd-e131bcd8809f`; its exact SHA is named in the #4371 handoff.
  No reset, rebase or branch change was performed; this revision is a descendant of
  `71319f9a`.
- Inventory under audit: `docs/migration/python-inventory.json`, whose `baseline_commit`
  is `86edeadd0da48e03a8d3045e2775881bd6450e68`. The inventory baseline and the audit
  source revision are different objects and are never conflated in this report; all path,
  blob and line measurements below were taken against committed Git objects at the audit
  source revision, whose tree matches the inventory baseline for every `*.py` path (each
  row's `original_blob` was verified against `git rev-parse 71319f9a:<path>`, §4).
- Write scope of this repair: exactly two documentation paths,
  `docs/migration/inventory-audit.md` and `docs/migration/python-inventory.json`.
  The manifest change is limited to the `classification_status` provenance string; no
  row, blob, path, baseline or schema field was altered.

## 1. Corrections applied to the rejected report

| # | Rejected claim at source `71319f9a` | Corrected position in this revision | Evidence |
| --- | --- | --- | --- |
| C1 | 216 distinct blobs | 271 distinct blobs; 271 canonical rows | §4, command R3 |
| C2 | 95 non-self canonical rows | 120 non-self canonical rows | §4, command R4 |
| C3 | Audit baseline `f86cc8f` | Audit source is `71319f9a`; inventory baseline is `86edeadd…`; the two are reported separately | header, command R1 |
| C4 | Duplicate families "46 notebook templates + 45 telemetry copies + 3 MSK + 1 emit-plan-json" (=95) | Eight canonical-resolved families covering 100 multi-row blob groups, 220 rows, 120 mirror rows | §5, command R5 |
| C5 | Rust scope narrowed to 341 files; 50 remote payload files treated as blanket exclusions ("not port targets") | All 391 rows carry an explicit final disposition; 341 rows are Rust port targets and the 50 payload rows remain in scope as unresolved managed-service compatibility questions owned by #4386 | §7 |
| C6 | `uvx mcp-proxy-for-aws` classified out of scope | Local Python runtime dependency, explicitly in scope of #4413; recorded as a required replacement | §8 |
| C7 | Telemetry Rust replacement must retain the nine-file vendored Python surface | Byte parity is evidence for #4398/#4377; a Rust implementation may change bundle layout, callers and sidecars atomically; no Python surface is preserved by contract | §8 |
| C8 | Secret-hook fail-open presented as a routine operator choice | Safety defect and required engineering/safety port under #4412; not an operator preference | §8 |
| C9 | "no Python executed from the repository tree" implying instruction compliance | Predecessor executed Python in at least 17 observed ToolUse events, including `python3 -c` stdin analysis and `ast.parse`; this revision states that plainly | §2 |
| C10 | `ast.parse` results used as audit evidence | Withdrawn. Replaced by permitted source inspection where possible; otherwise marked unknown | §2, U-3 |
| C11 | Findings numbering and cross-references | Renumbered F-1…F-12 with an evidence pointer each; every reference resolves inside this document | §10 |
| C12 | "391/391 paths tracked and equal to `git ls-files -- '*.py'`" | Confirmed independently and retained | §4, command R2 |

## 2. Instruction-compliance record

The work rule for this Issue, from repository `AGENTS.md` and the launch brief, prohibits
executing Python at all: no `python`, no `python3`, no `python3 -c`, no stdin script, no
parser, no wrapper, no repository import, no scratch analysis.

- **Predecessor session (source `71319f9a`).** ToolUse events 15, 19, 24, 28, 30, 32, 34,
  36, 38, 40, 42, 48, 90, 92, 94, 96 and 100 — and further events not itemised by the
  manager — invoked `python3`, including `-c` stdin analysis and `ast.parse` over notebook
  templates. That is a violation of the rule. It is not excused by the interpreter having
  been pointed at files outside the repository tree, and the rejected report's phrase
  "not from repository tree" is withdrawn as a compliance claim.
- **Consequence for evidence.** No predecessor conclusion that depended on a Python
  process is accepted in this revision. Each such conclusion was either re-derived with a
  permitted tool and is restated with its command (C1, C2, C4, C11, and the family and
  partition measurements in §5), or is marked unknown (U-3). Nothing in §4, §5 or §7 rests
  on executed Python.
- **This repair revision.** No Python was invoked in any form. Analysis used `git`, `jq`,
  `awk`, `sort`, `uniq`, `comm`, `join`, `wc` and `grep` only, over committed Git objects
  and working-tree text. No Cargo build or test run was made (docs-only work; none is
  needed). No AWS call, live or paid, was made; no network request was made. The
  corrective checks are reproducible from §3 without Python.

## 3. Method and reproduction

All commands are read-only and non-Python. `INV` is
`docs/migration/python-inventory.json`; `SRC` is `71319f9aecb7f81c872aa1ee0060f9474bc7cd1b`.

```sh
# R1  inventory baseline, provenance and path count
jq -r '.baseline_commit, .classification_status, (.paths|length)' "$INV"

# R2  inventory path set == tracked *.py set (expect empty output, then 391)
comm -3 <(jq -r '.paths[].path' "$INV" | sort) <(git ls-tree -r --name-only "$SRC" -- '*.py' | sort)
git ls-tree -r --name-only "$SRC" -- '*.py' | wc -l

# R3  distinct blobs, and manifest rows whose original_blob is wrong (expect 271, then empty)
git ls-tree -r "$SRC" -- '*.py' | awk '{print $3}' | sort -u | wc -l
git ls-tree -r "$SRC" -- '*.py' | awk '{print $4"\t"$3}' | sort > /tmp/blobs.tsv
jq -r '.paths[] | [.path, .original_blob] | @tsv' "$INV" | sort | join -t$'\t' /tmp/blobs.tsv - | awk -F'\t' '$2!=$3'

# R4  canonical mapping: every canonical_identical_path resolves to a tracked path with an
#     identical blob, and non-self rows (expect empty, then 271 canonical values, then 120)
jq -r '.paths[] | [.canonical_identical_path, .original_blob] | @tsv' "$INV" | sort -u > /tmp/canon.tsv
join -t$'\t' /tmp/canon.tsv /tmp/blobs.tsv | awk -F'\t' '$2!=$3'
cut -f1 /tmp/canon.tsv | sort -u | wc -l
jq -r '.paths[] | select(.path != .canonical_identical_path) | .path' "$INV" | wc -l

# R5  blob groups: size distribution, and per-family canonical roots
git ls-tree -r "$SRC" -- '*.py' | awk '{print $3}' | sort | uniq -c | sort -rn
# (family assignment below uses the same canonical-path prefix rules stated in §5)

# R6  line counts per path and per exact blob (no execution of any file)
while IFS= read -r p; do
  printf '%s\t%s\n' "$(git cat-file -p "$SRC:$p" | wc -l)" "$p"
done < <(git ls-tree -r --name-only "$SRC" -- '*.py') > /tmp/pathlines.tsv

# R7  inventory-row join used for the per-row table and all class/slice totals
jq -r '.paths[] | [.path, .original_blob, .classification, .slice, (.slice_issue|tostring)] | @tsv' "$INV" | sort > /tmp/rows.tsv
```

Class totals, duplicate families and the per-row disposition table in §5–§7 were produced
from `/tmp/rows.tsv` and `/tmp/pathlines.tsv` with `awk` one-liners over these fields;
each figure below is stated with the command class that produced it.

## 4. Measured totals

| Measurement | Value | Command |
| --- | --- | --- |
| Tracked `*.py` paths at audit source | 391 | R2 |
| Inventory rows | 391 (0 missing, 0 extra) | R2 |
| Rows whose `original_blob` differs from the committed blob | 0 | R3 |
| Distinct blobs = canonical rows | 271 | R3, R4 |
| Distinct `canonical_identical_path` values | 271 (every canonical path is itself a row) | R4 |
| Rows whose canonical path is not tracked, or whose canonical blob differs | 0 | R4 |
| Rows whose `canonical_identical_path` is not themselves (`mirror` rows) | 120 | R4 |
| Raw lines over all 391 paths | 127,558 | R6 |
| Lines after exact-blob deduplication | 97,599 | R6 |
| `toolkit-helper` rows: canonical / mirror / total | 158 / 95 / 253 | R7 |
| `test-or-diagnostic` rows: canonical / mirror / total | 88 / 0 / 88 | R7 |
| `external-service-payload` rows: canonical / mirror / total | 25 / 25 / 50 | R7 |
| `toolkit-helper` lines raw / dedup | 90,769 / 65,030 | R6, R7 |
| `test-or-diagnostic` lines raw / dedup | 29,781 / 29,065 | R6, R7 |
| `external-service-payload` lines raw / dedup | 7,008 / 3,504 | R6, R7 |

Arithmetic reconciliation: 90,769 + 29,781 + 7,008 = 127,558 and 65,030 + 29,065 + 3,504 =
97,599; 158 + 88 + 25 = 271 canonical rows and 95 + 25 = 120 mirror rows, so 271 + 120 =
391 rows. The manifest's own metadata is therefore internally consistent at the audit
source, and `docs/migration/python-inventory.json` required no row correction.

## 5. Duplicate structure

Exact-blob partition: 271 groups over 391 rows. 100 groups hold two or more rows and cover
220 rows; 171 groups are singletons. 120 rows are byte-identical copies of a row recorded
as canonical, and in every case the copy's canonical target is itself a tracked row with
the same blob (0 exceptions, R4). The rejected report's four-family list of 95 rows is
withdrawn; the measured partition resolves into eight families:

| Family | Canonical root | Groups | Rows | Mirrors | Owner slices |
| --- | --- | --- | --- | --- | --- |
| telemetry bundle copies | `plugins/aws-startup-advisor/scripts/telemetry/` | 17 | 62 | 45 | telemetry #4398 |
| canonical AI/ML skills | `skills/core-skills/aws-ai-ml/` | 37 | 74 | 37 | ai-data #4385 (6), ai-models #4384 (12), ai-payloads #4386 (56) |
| CloudWatch references | `skills/…/aws-observability/` | 26 | 52 | 26 | cw-api #4381 (26), cw-domain #4380 (26) |
| Launch with AWS | `skills/…/launch-with-aws/` | 6 | 12 | 6 | launch #4382 |
| startup-advisor scripts | `plugins/aws-startup-advisor/scripts/` | 5 | 10 | 5 | startup-validators #4399 (9), heroku #4406 (1) |
| MSK analytics scripts | `skills/…/analytics-skills/migrate-to-msk/` | 3 | 6 | 3 | msk #4387 |
| Lambda/Fargate examples | `skills/…/aws-serverless/` | 1 | 2 | 1 | serverless #4397 |
| Bedrock agent discovery | `skills/…/amazon-bedrock/` | 1 | 2 | 1 | bedrock-fetch #4383 |
| — | **total** | **100** | **220** | **120** | — |

Notes that constrain later slices:

- The AI/ML family's 74 rows include 46 rows (23 canonical + 23 mirror) under
  `code_templates/templates/`; the remaining 28 are skill scripts and helpers. The
  rejected report's "46 notebook templates" figure described this subset but was then
  used to justify excluding the other 4 payload rows and to mis-state the family total.
- Telemetry: 17 canonical files copied into 5 bundle locations (9 files each, 45 mirror
  rows). Byte parity at the audit source is a fact; it is evidence for #4398 and #4377,
  not a requirement to keep any particular file layout (§8).
- One mirror row crosses slice boundaries: `emit-plan-json.py` in `heroku` #4406 is
  byte-identical to a canonical row owned by `startup-validators` #4399 (F-7). Both slices
  must agree on the replacement before either lands.
- Canonical *direction* is the manifest's convention (core skills authored, plugin copies
  generated). This audit verified byte identity, not which side is authored; that question
  is U-4.

## 6. Slice ownership

36 slices hold rows; every one of the 391 rows carries a slice key and a slice Issue, and
no row is unassigned. Class columns are T = `toolkit-helper`, D = `test-or-diagnostic`,
P = `external-service-payload`. Slice scope text and dependencies live in
`docs/migration/issues.json` and are not repeated here.

| Slice | Issue | Rows | T | D | P |
| --- | --- | --- | --- | --- | --- |
| ai-data | #4385 | 6 | 6 | 0 | 0 |
| ai-models | #4384 | 12 | 12 | 0 | 0 |
| ai-payloads | #4386 | 56 | 6 | 0 | 50 |
| aurora | #4388 | 6 | 6 | 0 | 0 |
| bedrock-fetch | #4383 | 2 | 2 | 0 | 0 |
| bedrock-probes | #4404 | 6 | 3 | 3 | 0 |
| bedrock-report | #4405 | 6 | 2 | 4 | 0 |
| bedrock-resolve | #4403 | 8 | 4 | 4 | 0 |
| cache-analysis | #4392 | 9 | 9 | 0 | 0 |
| cache-ops | #4393 | 3 | 2 | 1 | 0 |
| cw-api | #4381 | 26 | 24 | 2 | 0 |
| cw-domain | #4380 | 26 | 26 | 0 | 0 |
| docgraph | #4395 | 6 | 4 | 2 | 0 |
| dynamo-bench | #4390 | 3 | 3 | 0 | 0 |
| dynamo-cost | #4389 | 2 | 2 | 0 | 0 |
| dynamo-deploy | #4391 | 2 | 2 | 0 | 0 |
| heroku | #4406 | 5 | 2 | 3 | 0 |
| launch | #4382 | 12 | 12 | 0 | 0 |
| manifests | #4375 | 2 | 1 | 1 | 0 |
| marketplace | #4396 | 11 | 10 | 1 | 0 |
| msk | #4387 | 6 | 4 | 2 | 0 |
| payments-bridge | #4411 | 3 | 1 | 2 | 0 |
| payments-policy | #4410 | 9 | 6 | 3 | 0 |
| relational | #4394 | 6 | 5 | 1 | 0 |
| security-hooks | #4412 | 1 | 1 | 0 | 0 |
| serverless | #4397 | 4 | 4 | 0 | 0 |
| spec | #4376 | 1 | 1 | 0 | 0 |
| startup-contracts | #4409 | 37 | 0 | 37 | 0 |
| startup-diagram | #4402 | 4 | 2 | 2 | 0 |
| startup-fixtures | #4408 | 16 | 16 | 0 | 0 |
| startup-models | #4401 | 7 | 3 | 4 | 0 |
| startup-scoring | #4400 | 9 | 2 | 7 | 0 |
| startup-tools | #4407 | 3 | 2 | 1 | 0 |
| startup-validators | #4399 | 11 | 9 | 2 | 0 |
| sync | #4377 | 3 | 2 | 1 | 0 |
| telemetry | #4398 | 62 | 57 | 5 | 0 |

Slice-size facts relevant to AC4 (see §9, §12): `telemetry` #4398 holds 62 rows,
`ai-payloads` #4386 56, `startup-contracts` #4409 37, `cw-domain` #4380 and `cw-api` #4381
26 each, `startup-fixtures` #4408 16. Those six slices hold 223 of 391 rows (57%).

## 7. Final per-row dispositions

Every one of the 391 rows carries exactly one disposition. Nothing is excluded from the
programme, and no row is left without an owner. Codes:

- `PORT-RUST` — canonical row; the owning slice must deliver Rust behaviour for it.
- `PORT-RUST-VIA-CANONICAL` — byte-identical mirror; the owning slice ports the canonical
  row and re-synchronises this copy, or removes it if the replacement makes the copy
  unnecessary.
- `PAYLOAD-COMPAT-Q` — canonical managed-service payload; disposition is an explicit
  compatibility question, not an exclusion. #4386 AC1/AC3 must record the language
  contract and the disposition per file before the row can be retired.
- `PAYLOAD-COMPAT-Q-MIRROR` — mirror of a payload row; follows its canonical decision.

Counts: 246 `PORT-RUST`, 95 `PORT-RUST-VIA-CANONICAL`, 25 `PAYLOAD-COMPAT-Q`, 25
`PAYLOAD-COMPAT-Q-MIRROR` (391 total). Class column: T = `toolkit-helper`,
D = `test-or-diagnostic`, P = `external-service-payload`; role column: `canonical` or
`mirror`.

| Path | Slice | Class | Role | Disposition |
| --- | --- | --- | --- | --- |
| `plugins/aws-agents/skills/agents-build/scripts/process_payment_tool.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-build/scripts/setup_payment_user.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/packages/openclaw/runtime/agentcore_bridge.py` | payments-bridge | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/packages/openclaw/test/test_agentcore_bridge.py` | payments-bridge | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/packages/openclaw/test/test_setup_openclaw.py` | payments-bridge | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/agents_pay_admin.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/test_portability.py` | payments-policy | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/test_resource_resolution.py` | payments-policy | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/test_x402_policy.py` | payments-policy | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/x402_fetch.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/x402_fetch_cli.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/x402_policy.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-core/com.anthropic.claude-code/hooks/secret-safety.py` | security-hooks | T | canonical | PORT-RUST |
| `plugins/aws-core/skills/amazon-bedrock/scripts/fetch_bedrock_agent.py` | bedrock-fetch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/dataset-evaluation/scripts/format_detector.py` | ai-data | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/dataset-transformation/scripts/transformation_tools.py` | ai-data | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/endpoint-diagnostics/scripts/collect_diagnostics.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning-technique/scripts/get_recipes.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/dpo.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/rlaif_builtin.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/rlaif_custom_prompt.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/rlvr.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/sft.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/scripts/mlflow_reference.py` | ai-payloads | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/templates/nova_rlvr_reward_function_source_template.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/templates/rlvr_reward_function_source_template.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-jumpstart-sagemaker.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-bedrock.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-sagemaker.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-bedrock.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-sagemaker.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/optimize-benchmark.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/optimize-huggingface-to-s3.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/optimize-recommendation.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/code_templates/custom_scorer_evaluator.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/code_templates/llmaaj_evaluator.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/scripts/nova_reward_function_source_template.py` | ai-payloads | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/scripts/reward_function_source_template.py` | ai-payloads | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/scripts/validate_custom_metrics.py` | ai-data | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/filter_deployable_models.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/get_deployable_models.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/get_model_names.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/search_deployable_models.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/dataset_definition_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/framework_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/script_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/spark_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/training-jobs/code_templates/custom_training.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/training-jobs/code_templates/jumpstart_training.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/training-jobs/code_templates/recipe_training.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch-omni/capture_dataset_from_traces.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch-omni/evaluate_traces.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch-omni/test_evaluate_traces.py` | cw-api | D | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_app_signals_client.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_capture.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_constants.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_crud_rendering.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_crud_tools.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_error_translation.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_formatting.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_gateway.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_instrumentation.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_location.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_logs_client.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_region.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_result.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_session.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_parsing.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_queries.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_rendering.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_tools.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshots.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_status_assessment.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_status_rendering.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_status_tools.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_validation.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-serverless/assets/powertools-handler.py` | serverless | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/archive.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/auth.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/auth_callback_server.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/launch_api_client.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/launch_config.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/launch_with_aws.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-data-analytics/skills/managing-amazon-msk/scripts/msk_sizing.py` | msk | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-data-analytics/skills/migrate-to-msk/scripts/compatibility.py` | msk | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-data-analytics/skills/migrate-to-msk/scripts/simulation_load_test_config.py` | msk | D | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_clarify.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_clarify_complete.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_clarify_fast_path.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_design.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_estimate.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_iac_terraform.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-decision-gate/check_expected_decide.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-live-capture/check_expected_baseline.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-live-capture/check_expected_drift.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-workshop/check_expected_workshop.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-decision-gate/check_expected_decide.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-decision-gate/check_expected_decide_with_retained_pack.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-live-capture/check_expected_drift.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-live-capture/check_expected_estimate.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-nonweb-scaling/check_expected_nonweb_scaling.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-workshop/check_expected_workshop.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/check-bedrock-pricing-parity.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/consent-disclosure-audit.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/emit-plan-json.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/model-id-lint.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/cli.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/accept.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/cli.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/notice.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/opt_out.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/record.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/session_start.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/client.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/migration.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/migration_attributes.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/skill_invoked.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/sync_bundles.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/conftest.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_cli.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_consent.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_metric_emission.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_migration.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/test_validate_migration_report_decision_core.py` | startup-validators | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-artifacts.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-migration-report.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-model-pricing.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-startup-program-artifacts.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/anthropic_model_recommendation.py` | startup-models | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/build_diagram.py` | startup-diagram | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/model_recommendation.py` | startup-models | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/openai_model_recommendation.py` | startup-models | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/score_units.py` | startup-scoring | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/scoring.py` | startup-scoring | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_build_diagram.py` | startup-diagram | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_collapse_invariant.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_cost_levers.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_model_recommendation.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_openai_model_recommendation.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_opus55_generation_contracts.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_opus55_integration.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_poc_shapes.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_scoring.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_temporal_decision_refs.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_unit_grouping.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_verify_model_path.py` | startup-diagram | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_workload_classes.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/verify_model_path.py` | startup-diagram | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/emit-plan-json.py` | heroku | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/test_validate_heroku_migration_report.py` | heroku | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/test_validate_heroku_migration_report_cost.py` | heroku | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/test_validate_heroku_migration_report_currency.py` | heroku | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/validate-heroku-migration-report.py` | heroku | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/bedrock_pricing.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/iam_policy.py` | bedrock-probes | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/image_input.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/model_identity.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/preflight_bedrock.py` | bedrock-probes | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/render_report.py` | bedrock-report | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/resolve_source_model.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/source_baseline.py` | bedrock-probes | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_astra_execution_contract.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_bedrock_pricing.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_delta_reference_schema.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_iam_policy.py` | bedrock-probes | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_image_input.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_model_identity.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_preflight_bedrock.py` | bedrock-probes | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_render_report.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_resolve_source_model.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_source_baseline.py` | bedrock-probes | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_validate_result.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/validate_result.py` | bedrock-report | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/shared/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/tf-best-practices/scripts/test_validate_terraform_policy.py` | startup-validators | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/tf-best-practices/scripts/validate-terraform-policy.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_capability_catalog_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_migration_matrix_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_only_option_c_framework_route.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_workload_profile_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_aws_design_ai_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_clarify_fast_path_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_discover_ordering.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_discovery_source_guidance.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_rdfa_discover_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_consent_disclosure_audit.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_cost_completeness.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_credential_lifecycle.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_decision_gate_wiring.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_emit_plan_json.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimate_assemble_stale_artifacts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimate_clarify_volume_resolution.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimate_option_a_single_close.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimation_ai_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gcp_design_guide_selection.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gemini_guide_body_parity.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_generate_mantle_dispatch.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_generation_ai_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_review_round2.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_review_round5.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_round4_findings.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_source_breakpoint.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_heroku_discover_addon_identity.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_heroku_estimate_deferred_cost_comparison.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_heroku_postgres_advanced_pricing.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_llm_to_bedrock_run_state.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_model_id_lint.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_openrouter_byok_detection_fixes.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_report_generator_capture_warnings_gate.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_report_generator_cost_pricing.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_resume_and_sentinel_consistency.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_usage_api_request_params.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_validate_artifacts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/check-skill-refs.py` | startup-tools | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/model-staleness.py` | startup-tools | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/sync-vendored.py` | sync | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/test_check_skill_refs.py` | startup-tools | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/test_sync_vendored.py` | sync | D | canonical | PORT-RUST |
| `skills/core-skills/amazon-bedrock/scripts/fetch_bedrock_agent.py` | bedrock-fetch | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/dataset-evaluation/scripts/format_detector.py` | ai-data | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/dataset-transformation/scripts/transformation_tools.py` | ai-data | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/endpoint-diagnostics/scripts/collect_diagnostics.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/finetuning-technique/scripts/get_recipes.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/dpo.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/rlaif_builtin.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/rlaif_custom_prompt.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/rlvr.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/sft.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/scripts/mlflow_reference.py` | ai-payloads | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/finetuning/templates/nova_rlvr_reward_function_source_template.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/templates/rlvr_reward_function_source_template.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-jumpstart-sagemaker.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-bedrock.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-sagemaker.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-bedrock.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-sagemaker.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/optimize-benchmark.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/optimize-huggingface-to-s3.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/optimize-recommendation.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/code_templates/custom_scorer_evaluator.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/code_templates/llmaaj_evaluator.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/nova_reward_function_source_template.py` | ai-payloads | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/reward_function_source_template.py` | ai-payloads | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/validate_custom_metrics.py` | ai-data | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/filter_deployable_models.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/get_deployable_models.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/get_model_names.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/search_deployable_models.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/dataset_definition_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/framework_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/script_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/spark_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/training-jobs/code_templates/custom_training.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/training-jobs/code_templates/jumpstart_training.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/training-jobs/code_templates/recipe_training.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-observability/scripts/cloudwatch-omni/capture_dataset_from_traces.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch-omni/evaluate_traces.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch-omni/test_evaluate_traces.py` | cw-api | D | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_app_signals_client.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_capture.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_constants.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_crud_rendering.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_crud_tools.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_error_translation.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_formatting.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_gateway.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_instrumentation.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_location.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_logs_client.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_region.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_result.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_session.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_parsing.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_queries.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_rendering.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_tools.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshots.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_status_assessment.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_status_rendering.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_status_tools.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_validation.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-serverless/assets/powertools-handler.py` | serverless | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/archive.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/auth.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/auth_callback_server.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/launch_api_client.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/launch_config.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/launch_with_aws.py` | launch | T | canonical | PORT-RUST |
| `skills/specialized-skills/analytics-skills/managing-amazon-msk/scripts/msk_sizing.py` | msk | T | canonical | PORT-RUST |
| `skills/specialized-skills/analytics-skills/migrate-to-msk/scripts/compatibility.py` | msk | T | canonical | PORT-RUST |
| `skills/specialized-skills/analytics-skills/migrate-to-msk/scripts/simulation_load_test_config.py` | msk | D | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/aggregator.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/cleanup.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/deprovision_cleanup.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/discoverer.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/expiry.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/metering_core.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/query_metering.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/register.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/submitter.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/subscription.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/test_event_publisher.py` | marketplace | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-mysql/scripts/acu_calculator.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-mysql/scripts/commitment_pricing_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-mysql/scripts/io_optimized_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-postgresql/scripts/acu_calculator.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-postgresql/scripts/commitment_pricing_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-postgresql/scripts/io_optimized_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-documentdb/scripts/wa_review.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/benchmark_lambda.py` | dynamo-bench | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/benchmark_model.py` | dynamo-bench | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/calculate_costs.py` | dynamo-cost | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/deploy_model.py` | dynamo-deploy | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/generate_perf_report.py` | dynamo-bench | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/generate_teardown.py` | dynamo-deploy | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/iterate_design.py` | dynamo-cost | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/command_classifier.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/find_tunnel_host.py` | cache-ops | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/generate_dashboards.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/input_validator.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/migration_preflight.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/price_calculator.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/pricing.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/security_audit.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/serverless_estimator.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/start_tunnel.py` | cache-ops | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/test_connection.py` | cache-ops | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/validate_references.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/agentic_memory.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/cdk_test_env.py` | docgraph | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/graphrag_pipeline.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/input_validator.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/seed_test_data.py` | docgraph | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/aurora-dsql/scripts/safe_query.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/aurora-dsql/scripts/tenant_query.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/rds-oracle/scripts/test_oracle_connection.py` | relational | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/rds-oss/scripts/rds_commitment_pricing_analyzer.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/timestream-influxdb/scripts/input_validator.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/timestream-influxdb/scripts/instance_types.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions/scripts/fargate_processor.py` | serverless | T | canonical | PORT-RUST |
| `skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions/scripts/lambda_function.py` | serverless | T | canonical | PORT-RUST |
| `tools/sync-plugin-skills.py` | sync | T | canonical | PORT-RUST |
| `tools/tests/test_validate.py` | manifests | D | canonical | PORT-RUST |
| `tools/validate.py` | manifests | T | canonical | PORT-RUST |
| `tools/validate_spec.py` | spec | T | canonical | PORT-RUST |

## 8. Assigned contracts and unresolved compatibility questions

These are the local Python dependencies and compatibility contracts the audit found. Each
is owned by a slice Issue; this audit records evidence and does not decide the port.

1. **Managed-service payloads — #4386 (56 rows).** 50 `external-service-payload` rows
   (25 canonical + 25 mirror) are SageMaker/Bedrock/customer-side training, evaluation,
   deployment and processing templates. They are product data whose language contract is a
   managed-service requirement, not a repository implementation detail. #4386 AC3 requires
   an explicit documented disposition per payload and forbids silently counting payload
   text as full Python removal. This audit therefore records them as unresolved
   compatibility questions (`PAYLOAD-COMPAT-Q`), keeps them in scope, and asserts no
   language contract for them (U-1).
2. **`uvx` MCP proxy — #4413.** `plugins/aws-core/.mcp.json` launches the MCP server with
   `"command": "uvx"` (mcp-proxy-for-aws). That is a local Python runtime dependency on the
   plugin's launch path and is explicitly in #4413 scope (AC5: no `uvx` runtime
   dependency). It is not out of scope and cannot be classified away by this audit.
3. **Other `uvx` runtime references — no owning slice (F-9).** The aws-iam skill instructs
   agents to run `uvx iam-policy-autopilot@latest generate-policies` as the mandatory policy
   path (`plugins/aws-core/skills/aws-iam/references/aws-iam-policy-generation.md`, several
   instructions including "You MUST verify `uvx` is available"). #4413's stated scope is
   `plugins/*/.mcp.json`, `plugins/*/mcp.json` and `mcp-proxy-for-aws-cli` references, which
   does not name this tool. It is a real `uvx` dependency on the same posture; the manager
   decides whether it joins #4413 or gets its own slice (§12, proposed follow-up).
4. **Secret-safety hook — #4412 (safety, not preference).** Both hook registrations in
   `plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json` launch
   `secret-safety.py` through `sh -c 'python3 "$0" 2>/dev/null || python "$0" 2>/dev/null
   || py -3 "$0"'`. If no interpreter exists the whole command succeeds without running the
   hook, i.e. the control fails open (F-10). #4412 AC4 requires replacing the Python
   launcher with a Rust binary *without removing protection*; AC1 requires preserving the
   deny/allow protocol. A missing interpreter must not silently disable the control, so the
   fail-open behaviour is a defect to resolve in #4412, not a routine operator choice.
5. **Telemetry bundles — #4398 / #4377 (parity evidence, not layout lock).** 17 canonical
   telemetry files exist in 5 byte-identical bundle copies (62 rows). The same hook files
   launch telemetry through `python3 … || python … || true`
   (`plugins/aws-startup-advisor/.cursor-plugin/hooks.json`,
   `…/com.anthropic.claude-code/hooks/hooks.json`), so telemetry failures are currently
   non-fatal by construction — evidence #4398 AC1 must weigh against its fail-closed
   consent requirement. Byte parity is evidence for the sync contract (#4377); it does not
   oblige the Rust implementation to keep a nine-file Python surface. A Rust port may change
   bundle layout, callers and `version.json` sidecars atomically in one change, provided
   #4398 AC5 (bundle sync verified) still holds.
6. **CI and developer entry points require a Python toolchain (AC5).** `mise.toml` pins
   `python = "3.12"` and runs `python3 tools/validate.py`, `python3
   tools/sync-plugin-skills.py --check`, `uv run … tools/validate_spec.py` and `python3 -m
   unittest discover -s tools/tests`; `plugins/aws-startup-advisor/mise.toml` runs eight
   Python tasks including `uv run --no-project --with pytest python -m pytest tests -q`.
   Those tasks are executed by `.github/workflows/build.yml` and
   `.github/workflows/startup-advisor.yml`, and a CodeQL workflow analyses `python`. The
   owning slices (#4378 ci, #4375, #4376, #4377, #4398) must retire them; this audit
   neither edits workflows nor claims they are already Python-free.
7. **Extensionless Python executable outside the `*.py` inventory (F-8).**
   `plugins/aws-core/skills/aws-secrets-manager/references/asm-exec` is a tracked,
   executable Python script (`#!/usr/bin/env python3`) that resolves
   `{{resolve:secretsmanager:…}}` references. It is the only such file in the repository
   (R8) and no slice currently owns it, because the inventory is keyed on the `*.py`
   suffix. Proposed owner in §12; the row is not added to the manifest by this repair
   because the manifest's path key and schema are the programme's contract, not this
   document's.

## 9. Acceptance criteria — status and limitations

#4371's own ACs, as stated on the Issue, are met to the extent a documentation audit can
meet them. Each is stated with what was *not* done.

- **AC1 (slice and file mapping is complete and correct).** Met for path coverage: 391
  inventory paths equal the 391 tracked `*.py` paths, every row has a slice and a slice
  Issue, 0 unassigned, 0 blob mismatches (§4, §6). Limitations: (a) the manifest has no
  disposition field beyond `status: "pending"`, so the per-row disposition vocabulary
  introduced here lives in this document, not in machine-readable form (U-6); (b) slice
  boundaries are the programme's, and one byte-identical row pair crosses two slices (F-7);
  (c) no managed-service language contract was determined — that is #4386 AC1 (U-1);
  (d) classification is a static judgement over file content, never an execution result
  (F-6, U-3).
- **AC2 (duplicate and mirror structure measured).** Met: exact-blob partition, 271
  groups, 220 rows in 100 multi-row groups, 120 mirror rows, 0 canonical mismatch (§5).
  Limitation: canonical *direction* is taken from the manifest and was not verified
  against the sync tool's behaviour, because running it would execute Python (U-4).
- **AC3 (per-row dispositions).** Met in this document for all 391 rows (§7). Limitation:
  these are audit-level dispositions (port / port-via-canonical / payload compatibility
  question and its mirror). They are not implementation acceptance, and the 50 payload rows
  remain open questions owned by #4386.
- **AC4 (oversized slices).** **Not met.** This Issue may publish only to #4371, so it
  cannot file child Issues, and dispatch belongs to the manager. Measured sizes and proposed
  splits are in §6 and §12 as proposals, not filed work.
- **AC5 (install and runtime dependencies identified).** Partially met: local Python
  runtime dependencies are identified with evidence — the `uvx` MCP proxy (#4413), the
  second `uvx` tool (F-9), the two hook launchers (#4412, #4398), the mise/CI tasks and
  CodeQL python analysis (§8.6), and the extensionless `asm-exec` (F-8). No dependency was
  removed or replaced, and the repository is not Python-free: the install posture still
  assumes a Python toolchain for development and CI.

Not claimed anywhere in this document: Rust verification, contract tests, job ids, review
outcome, landed SHA, or live/paid AWS behaviour. No build job was run for this docs-only
revision.

## 10. Findings

- **F-1 Inventory completeness.** 391 rows = 391 tracked `*.py` paths; 0 missing, 0 extra,
  0 `original_blob` mismatches at `71319f9a` (R2, R3).
- **F-2 Deduplication.** 271 distinct blobs; 127,558 raw / 97,599 deduplicated lines
  (R3, R6).
- **F-3 Canonical mapping integrity.** 271 distinct canonical paths, all of them inventory
  rows, all with byte-identical blobs; 0 errors (R4).
- **F-4 Duplicate partition.** 100 multi-row groups covering 220 rows, 171 singletons,
  120 mirror rows, eight families (§5, R5).
- **F-5 `docs/migration/README.md` line-count claim does not reconcile.** README states
  28,208 lines "in named tests/diagnostics". Measured at the audit source: the manifest's
  `test-or-diagnostic` class is 29,781 raw / 29,065 dedup, and a basename rule
  (`test_*`, `*_test.py`, `conftest.py`) gives 84 paths / 28,630 raw / 28,314 dedup.
  Neither equals 28,208. The README is outside this repair's write scope; recorded as U-5
  and proposed as a follow-up (§12).
- **F-6 Classification divergence is small and deliberate.** 4 `test-or-diagnostic` rows
  are not name-based tests: `simulation_load_test_config.py` (two copies),
  `cdk_test_env.py`, `seed_test_data.py`. 84 rows match the basename rule; the class holds
  88 rows. Recorded so reviewers can audit the judgement rather than discover it.
- **F-7 Cross-slice duplicate.** `emit-plan-json.py` (heroku #4406) is byte-identical to a
  canonical row in startup-validators #4399; the two slices must coordinate.
- **F-8 One Python executable is invisible to the `*.py` inventory.**
  `plugins/aws-core/skills/aws-secrets-manager/references/asm-exec` (shebang
  `#!/usr/bin/env python3`) is the only tracked non-`.py` file whose first line selects
  Python, out of 3,160 tracked non-`.py` files (R8). It is unowned by any slice.
- **F-9 A second `uvx` runtime dependency is unowned.** `uvx iam-policy-autopilot@latest`
  is a mandatory instruction path in the aws-iam skill reference and is not named in
  #4413's scope (§8.3).
- **F-10 Secret-safety hook fails open.** Both registrations swallow a missing interpreter
  (`python3 … || python … || py -3 …` with no fallback control), so the guard silently
  stops guarding on a Python-free host (§8.4, #4412).
- **F-11 Telemetry hooks also fail open by construction** (`… || true`), which must be
  reconciled with #4398 AC1 fail-closed consent (§8.5).
- **F-12 Slice concentration.** Six slices hold 223 of 391 rows (57%): telemetry 62,
  ai-payloads 56, startup-contracts 37, cw-domain 26, cw-api 26, startup-fixtures 16 (§6).

## 11. Unknowns

- **U-1 Managed-service language contracts.** Which payload rows are genuine
  managed-service text requirements is undetermined; it needs primary documentation review
  under #4386 AC1.
- **U-2 Payload execution reality.** Static inspection shows template-shaped files, but
  whether any payload row is executed by repository tooling rather than by a customer was
  not established. No execution was permitted or attempted.
- **U-3 Notebook/template parse validity.** The predecessor's `ast.parse` claims are
  withdrawn as prohibited evidence. Permitted inspection shows 7 of the 23 canonical
  `code_templates/templates/` files begin with IPython magic lines (`%`/`!`), which are not
  valid standalone Python; parse validity for the remaining 16 is **unknown** and is not
  needed for their disposition (they are payload compatibility questions either way).
- **U-4 Canonical direction.** Whether the core-skills side or the plugin side is authored
  is the manifest's assertion; verifying it requires running the sync tool, which is Python
  (#4377 AC1 will settle it).
- **U-5 Basis of the README line-count claim** (F-5); the README is outside this repair's
  write scope.
- **U-6 No machine-readable disposition field.** The manifest records `status: "pending"`
  per row; a per-row disposition vocabulary would be a schema change owned by the
  programme, so this audit keeps the vocabulary in this document.

## 12. Proposed follow-ups (not filed by this Issue)

These are proposals for the manager. This Issue publishes only to #4371 and therefore files
no programme Issues.

1. Reconcile `docs/migration/README.md` with measured line counts, or record its counting
   rule (F-5, U-5).
2. Give `asm-exec` an owner and decide whether the inventory key widens beyond `*.py`
   (F-8).
3. Decide whether `uvx iam-policy-autopilot` joins #4413 or needs its own slice (F-9).
4. Consider child Issues for the six concentrated slices (F-12): telemetry 62 rows,
   ai-payloads 56 (of which 50 are payload questions), startup-contracts 37, cw-domain and
   cw-api 26 each, startup-fixtures 16.
5. Require the #4412 and #4398 ports to state explicitly what happens when the new Rust
   binary is missing, replacing today's fail-open launchers (F-10, F-11).

## 13. Reproduction summary

Every number in §4–§7 comes from commands R1–R8 in §3 run against committed Git objects at
`71319f9aecb7f81c872aa1ee0060f9474bc7cd1b`, joined with `jq` and reduced with `awk`. No
Python process, parser, wrapper or repository import was used at any point in this repair
revision, and no file under audit was executed.

Friction: #4371
