# Startup model recommendation engines (#4401)

Slice `startup-models`, wave 2 of programme #4369. Base: the integration head
`7dd92dc7b51978a7d839be42f7e620fa59603c7e`, which already holds the startup validators (#4399),
the startup scoring slice (#4400) and the bedrock-report slice (#4405). The code extends
`crates/toolkit-cli/src/startup` (`startup/models/`); the JSON reader, the `json.dumps(indent=2)`
writer and the runtime-registry conventions of `startup/scoring`, the schema engine of
`spec/engine.rs` (also used by `bedrock-report`) and `safe_text.rs` are reused, nothing parallel
was created.

Python source (read-only migration evidence, **not** deleted or re-wired here):
`plugins/aws-startup-advisor/skills/agent-advisor/scripts/model_recommendation.py`,
`anthropic_model_recommendation.py`, `openai_model_recommendation.py`, `test_model_recommendation.py`
and `test_openai_model_recommendation.py`. Retirement of those files and the flip of their callers
(`model-recommend.md`, `model-recommend-assemble.md`, `model-selection.md`, `verify_model_path.py`,
`build_diagram.py`) is the follow-up slice #4415, because the phase documents pin the command line
and a prose change is a product decision, not a side effect of the port.

## Command

| Rust | Python original | Result |
| --- | --- | --- |
| `toolkit startup model-recommend INPUT` | `python model_recommendation.py INPUT` | validates INPUT, recommends a model and API path per workload, validates the result, writes `model-recommendation.json` beside INPUT (`json.dumps(indent=2)` plus a newline), removes a stale `model-verification.json`, prints `RESULT=ok WORKLOADS=<n> SCHEMA_VALIDATED=yes` |

Flags: `--output FILE` (default `model-recommendation.json` beside INPUT), `--catalog FILE`
(Anthropic catalog), `--plugin-root DIR` (default `.`, the plugin directory, like every `startup`
command). `--openai-catalog FILE` is new. The router entry follows the canonical contract:
`value_flags`, `strict_values: false`, `max_positionals: 1`, `passthrough: false`.

## What is ported

- **Orchestrator** (`recommend.rs`, `command.rs`): the two-way provider decision
  (`openai` or everything else), per-workload catalog provenance, the backward-compatible top-level
  `catalog` of the primary unit, `duplicate workload_id` and the primary-unit check, the lazy
  OpenAI catalog (loaded only for an OpenAI workload), the stale `model-verification.json` rule
  (`_invocation_contract`).
- **Anthropic engine** (`anthropic.rs`): the model order per priority and for `agentic`, hard
  filters (path availability and regions, CRIS profile coverage, capabilities, context and
  output ceilings), API-path constraints and conflicts (`decision_required` with one option per
  conflicting path), CRIS/profile/ARN resolution without guessing a geography, the Claude
  version-hop analysis (`_source_version`, `_version_tuple`, the 4.7 version-scan rule), every
  `[BLOCKS]`/`[TUNE]` finding, migration deltas, compatibility, architecture impacts, the
  adaptive-thinking and batch rules of Opus 5.5, evaluation, rollout and verification blocks.
- **OpenAI engine** (`openai.rs`): source-family detection, the exact-model catalog key with word
  boundaries, the Converse tier map, Mantle path choice (Responses or GPT-6 Chat), regional
  availability, evidence-driven candidate selection with fail-closed `unverified_capability` /
  `unverified_capacity`, GPT-6 Astra's own rules (explicit API preference, caching on the selected
  API, JSON Schema versus streaming, profile ARNs), sampling findings derived from the selected
  target, the hosted-tool impacts and modality targets, and the decision options.
- **Catalogs stay data** (AC2): `references/models/*.json` are read at run time and typed by
  `catalog.rs`; the only model knowledge compiled in is what the Python modules also hard-code
  (priority order, tier map, source identities, the Astra display name).
- **Contracts**: `scripts/schemas/model-recommendation-input.json` and
  `model-recommendation.json` are embedded (`include_str!`) and evaluated by the shared schema
  engine, and the key order of every object follows the Python dict literals, so the result file is
  comparable byte for byte.

## Deliberate differences

1. **Catalog location.** Python located the catalogs from the script's own path; a separate binary
   needs a root. They are read from `PLUGIN_ROOT/skills/agent-advisor/references/models`, or named
   with `--catalog` / `--openai-catalog` (the second flag is new: Python only had a function
   argument). A missing catalog is an error naming the escaped file.
2. **Schema validation is not optional.** Python validated only when `jsonschema` was importable and
   printed `SCHEMA_VALIDATED=no` otherwise. The port always validates the input and the result and
   always prints `yes`. The schemas are draft-07 documents evaluated by the draft 2020-12 engine
   of `spec/engine.rs`; every keyword they use (`$ref`/`$defs`, `if`/`then`/`else`, `oneOf`,
   `propertyNames`, `const`, `enum`, `uniqueItems`, `pattern`, the size bounds) means the same in both.
   A result that fails its own schema is an internal error (exit 1) and nothing is written.
3. **No untrusted value in a diagnostic (#4602).** Python printed a `jsonschema` traceback that
   repeated the offending value, and `primary_unit not found in workloads: <value>` repeated an
   arbitrary string. Schema findings are `  - $.path: message` lines that name the field, the JSON
   type and the allowed values; property names go through `safe_text` and findings stop at 20
   (`... and N more`). The `primary_unit` message names the field only. File names, catalog model
   keys and the workload id of an engine error are printed through `safe_path`/`safe_text`
   (escaped, single line). The JSON documents are data and keep every value, ASCII-escaped by the
   JSON writer like Python's `json.dumps`.
4. **Typed catalogs, one-line errors.** Python checked six top-level keys and seven fields per model
   and crashed with `KeyError`/`TypeError` anywhere else. The port reports, with the escaped file
   name, a missing required key, a missing or mistyped model field, a limit that is not a positive
   integer or `"unknown"` (both catalogs; Python checked the OpenAI one only), a version that is not
   a string, an available path without `model_id`/`requires_cris`,
   and a selection table that names a model the catalog lacks. An `"unknown"` limit in the
   Anthropic catalog cannot satisfy a positive requirement (Python compared it with an integer and
   raised `TypeError`).
5. **Links are refused.** The input, both catalogs and the previous result are read with the
   shared symlink-refusing reader; the output is written with `write_regular_file`, which refuses a
   link. A linked previous result is not trusted: the invocation contract counts as changed, so the
   stale `model-verification.json` is removed, as for any unreadable previous result.
6. **Strict JSON.** `parse_json` refuses duplicate object keys, `NaN`/`Infinity`, lone surrogates
   and integers outside `i64` (see `startup-scoring.md`).
7. **ASCII digits in version patterns.** `_source_version` and `_version_tuple` read ASCII digits
   (`[0-9]`); Python's `\d` also matched other Unicode digits. Version components saturate at
   `u64::MAX`. The OpenAI patterns keep `\d` (Unicode) because their matches are compared as text.
8. **`integer` accepts an integral float** (`5.0`), as `jsonschema` does; booleans are not integers.
9. **Previous-result shape.** `_invocation_contract` of a previous file raised `AttributeError` for
   some wrong shapes and sorted a string into characters for others; every non-result shape is a
   changed contract here.
10. **Usage.** A missing INPUT is a usage error (exit 2) with
    `usage: toolkit startup model-recommend INPUT.json [--output FILE] ...`; Python used `argparse`.

Everything else is intended parity: the model orders, the findings and their texts, the quirks of
`_source_version` (`claude-opus-4-20250514` reads as `4.20250514`, pinned by a test), the ordering
of every key, the lazy OpenAI catalog, deletion of `model-verification.json` only when the
invocation contract changed.

`startup/models` has its own source-identity matcher (`same_model_catalog_key`, a catalog-key
lookup with word boundaries). It is not `model_identity.same_model` (slice #4403, a conservative
equality rule that `bedrock-report` currently holds privately, #4607); when #4403 lands the two
stay separate rules.

## Pool-versus-lifecycle drift (#4612)

`test_scoring.py`'s `_pool_models`, `test_pool_keys_cover_every_selectable_model` and
`test_no_pool_model_is_legacy_or_excluded` import the recommendation modules and were left out of
#4400. They live next to the lifecycle matcher in `startup/scoring/locks.rs`
(`pool_keys_cover_every_selectable_model`, `no_pool_model_is_legacy_or_excluded`, with the
`_POOL_LIFECYCLE_KEYS` table) and take the pool from the Rust priority tables and the two real
catalogs, so a model added to either fails them until it has a lifecycle key.

## Test mapping (AC1-AC3)

| Python test file | Rust |
| --- | --- |
| `test_model_recommendation.py` | `startup/models/tests/anthropic.rs` (path/model selection, conflicts, version hop, findings, CRIS, providers, provenance, catalog records) and `tests/cli.rs` (schemas through the command, catalog readers, goldens) |
| `test_openai_model_recommendation.py` | `startup/models/tests/openai.rs` (every section of the handoff matrix, the GPT-6 and Astra matrices, the gap-review regressions, the tier map) |
| `test_scoring.py` model-pool drift tests | `startup/scoring/locks.rs` (#4612) |
| `jsonschema.validate(...)` calls | the command itself: every CLI success validates input and result |
| `test_current_catalog_readers_match_engine_defaults` | `tests/cli.rs::phase_documents_name_the_default_catalogs` |

New (no Python original): hostile-input tests for every diagnostic path
(`tests/hostile.rs`), link refusal, the stale-verification matrix, catalog shape errors, the
router/README/migration-note single-entry checks.

AC1 (capability filtering, model identity and output contracts preserved): the engine tests and the
schema-checked CLI runs. AC2 (catalog data separate from logic): `catalog.rs` reads the dated
files; the tests use the real catalogs and tiny fixture catalogs. AC3 (no live inference): nothing
here opens a network connection or an AWS client.

## Follow-ups

- #4415: retire the three Python modules and their tests, point `model-recommend.md`,
  `model-recommend-assemble.md`, `model-selection.md`, `verify_model_path.py` and
  `build_diagram.py` at the Rust command, and add the fail-closed launcher for a missing binary.
- #4403: when `model_identity.py` is ported, decide whether `bedrock-report`'s private
  `same_model` moves there (unrelated to the engine's catalog-key matcher).
