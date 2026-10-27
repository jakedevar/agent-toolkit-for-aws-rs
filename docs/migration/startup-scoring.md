# Startup architecture scoring (#4400)

Slice `startup-scoring`, wave 2 of programme #4369. Base: the integration head
`87a6b38b5fb819dfe7d9efcdac0426641ce4955f`, which already holds the startup
validators slice (#4399). The code extends `crates/toolkit-cli/src/startup`
(`startup/scoring/`); nothing parallel was created.

Python source (read-only migration evidence, **not** deleted or re-wired here):
`plugins/aws-startup-advisor/skills/agent-advisor/scripts/scoring.py` and
`score_units.py`, plus the tests listed in the mapping below. Retirement of
those files and the flip of their callers (`clarify.md` Step 5 runs
`uv run "$SCRIPTS/score_units.py" "$RUN_DIR/answers.json"`; `confirm.md`
re-scores through the same wrapper) is the follow-up filed from this slice,
because the content locks ported here pin the caller sentences (`score_units.py"
"$RUN_DIR/answers.json"`, "scoring.py is NOT modified") and a prose change is a
product decision for that slice, not a side effect of the port.

## Commands

| Rust | Python original | Result |
| --- | --- | --- |
| `toolkit startup score ANSWERS` | `python scoring.py ANSWERS` | reads `{"entry_point", "answers"}`, writes `scoring-result.json` beside ANSWERS (`json.dumps(indent=2)`, no trailing newline), prints `RESULT=ok VERDICT=<verdict>` |
| `toolkit startup score-units ANSWERS` | `uv run score_units.py ANSWERS` | scores every `agent_session` unit of `answers.json`, prints `{"units": {...}, <primary unit's result>}` (`json.dumps(indent=2)` plus the print newline) |

Both take `--plugin-root DIR` (the plugin directory, default `.`, like every
`startup` command) and `--runtimes-dir DIR`. The registry is
`DIR/skills/agent-advisor/references/runtimes` unless `--runtimes-dir` names it.
Python located it from the script's own path; a separate binary needs an
explicit root. `score-units` also reads `current-run-verifications.json` beside
ANSWERS exactly as `score_units.py` does: absent means no evidence; invalid or
for another run (`run_id` is not the name of the run directory) is an error.

The router entries follow the canonical contract: `value_flags`,
`strict_values: false`, `max_positionals: 1`, `passthrough: false`.

## What is ported

- `load_profiles`: every `*.json` of the registry, sorted by file name, checked
  for the four required keys, filtered by status (`ga`), sorted by `id`.
- `is_run_materialized_evidence` and the source-backed checks (`_has_current_run_evidence`,
  `_is_current_run_verified`): exact key set, pinned type and version, non-blank
  `run_id`, records limited to `status`/`source`/`value`, verified records need
  an `https://aws.amazon.com/` or `https://docs.aws.amazon.com/` source and a
  non-blank value; a constraint is verified only when source and value match the
  constraint's accepted sources and expected value exactly.
- Hard constraints (first match per profile eliminates it, or is deferred when it
  needs verification the run lacks), affinity scores with the neutral default,
  the verdict (single winner, co-recommendation within 2 points, no viable
  runtime), deployment model, AgentCore compute type and services, assumptions,
  selection-verification requirements and warnings, and the result's exact key
  order.
- `score_units`: per-unit merge of system and unit answers (minus the evidence
  and provenance metadata), `agent_session` filter, primary mirror.
- Python semantics for the JSON data the profiles carry: `True == 1 == 1.0`,
  `in` on lists/strings/objects, truthiness, insertion-ordered maps with
  overwrite-in-place.

## Deliberate differences

1. **Registry location.** See above; a missing or unreadable registry
   directory is an error (exit 1). Python's `glob` on a missing directory
   silently yielded no profiles, so `score` returned `no_viable_runtime` for a
   mistyped path.
2. **Strict JSON.** `parse_json` is stricter than `json.loads`: duplicate object
   keys, `NaN`/`Infinity`, lone surrogates, a byte-order mark and integers
   outside `i64` (read as floats) are refused or lose precision. Registry,
   answers and evidence files are produced by tools that emit none of these.
3. **One-line errors instead of tracebacks.** Where Python crashed on data of the
   wrong shape (a profile that is not an object or whose `id` is not a string,
   a constraint without `field`/`value`/`reason`, a non-list `hard_constraints`,
   an affinity score that is not a number, a list used as a lookup key, an
   `answers.json` without `system`/`units`, a unit that is not an object, no
   `agent_session` unit to mirror) the port exits 1 with a message that names
   the field and the file. An unhashable evidence `status` makes the artifact
   invalid where Python raised `TypeError`.
4. **Links are refused.** A registry profile, `current-run-verifications.json`
   or the `scoring-result.json` output that is a symbolic link (or not a regular
   file) is an error. `Path.exists()` followed links and treated a dangling one
   as absent.
5. **No untrusted value in a diagnostic (#4602).** Error messages name fields,
   files and types, never a value from the data. Registry file names, paths and
   the verdict printed on the `RESULT=ok VERDICT=` line are written with control
   characters, escapes and other non-printing characters as `\u{..}` so a hostile
   file name or runtime id cannot forge or hide output lines. The JSON documents
   are data and keep every value, ASCII-escaped by the JSON writer like Python's.
   This holds for the link refusal too: a linked registry profile (a dangling
   link included) is refused with a diagnostic that names only the escaped file
   name (#4625), never the raw path the shared link walk would format.
6. **Usage.** A missing ANSWERS is a usage error (exit 2) with
   `usage: toolkit startup score ANSWERS.json [--plugin-root DIR]`; Python
   printed the docstring's last line for `score_units.py` and used `argparse`
   for `scoring.py`. Extra positionals are a router usage error (exit 2).
7. **Run directory.** `answers.json` given without a directory component has
   an empty parent name, so evidence beside it never matches its `run_id`;
   this is Python's behaviour and is kept.

Everything else is intended parity: the ordering of every key, the exact warning
and reason texts, the tie threshold, the neutral score, the treatment of an
explicit `null` answer (dropped from the scoring answers but not counted as an
assumption), duplicate runtime ids (the later profile overwrites the score in
the first one's position), and Python's number semantics for scores.

Parity decision (#4624): `score` assigns the trusted `_entry_point` exactly as
`scoring.py` does, after the raw answers are merged. It is the input's
`entry_point` (default `build_scratch`; a JSON `null` stays null, as Python's
`None`), so a hard or selection constraint on `_entry_point` behaves as in
Python and a raw `answers._entry_point` is overwritten and can never steer it.
`score-units` passes the document's `entry_point` to every unit the same way.
The bundled registry has no such constraint; custom registries (`--runtimes-dir`)
may.

## Test mapping (AC1-AC3)

| Python test file | Rust |
| --- | --- |
| `test_scoring.py` (engine, evidence, golden, profile shape, `score_units` command tests) | `startup/scoring/tests.rs` (registry, constraints, evidence, scores, verdict, golden results on the real registry, schemas, CLI) |
| `test_scoring.py` lifecycle drift self-proofs | `startup/scoring/locks.rs` |
| `test_scoring.py` model-pool drift tests (`_pool_models`, `test_pool_keys_cover_every_selectable_model`, `test_no_pool_model_is_legacy_or_excluded`) | **not ported**: they import the model-recommendation modules (`startup-models` slice); the matcher and its self-proofs are held in `locks.rs` |
| `test_collapse_invariant.py` (scoring half) | `single_unit_collapse_is_identical_to_the_direct_score`; the diagram half belongs to `startup-diagram` |
| `test_cost_levers.py`, `test_poc_shapes.py`, `test_temporal_decision_refs.py`, `test_workload_classes.py` | `startup/scoring/locks.rs` |
| `test_unit_grouping.py` | `startup/scoring/locks/grouping.rs` and the CLI tests; the three `build_diagram` assertions (`HANDOFF_RUNTIMES`, single-unit render, authoritative empty services, the `if model and model != "unknown":` source check) belong to `startup-diagram` |

The content locks read the real skill tree through `CARGO_MANIFEST_DIR` and
use the Python patterns (`fancy-regex`), so a prose edit that weakens a pinned
sentence fails the Rust suite exactly as it failed the Python one.
`test_contributing_does_not_mislabel_llm_to_bedrock_as_dsl` returns early when
`CONTRIBUTING.md` is absent, as it does in this tree (vacuous in Python too).

The goldens run the real registry through the engine, so a registry edit that
changes a published verdict is caught here as well.

## Follow-ups

- Retire `scoring.py`, `score_units.py` and their tests, update `clarify.md`,
  `confirm.md`, `SKILL.md` and the freshness/decision references to the Rust
  commands, and add the fail-closed launcher for a missing binary.
- `startup-models`: port the model-pool lifecycle drift tests.
- `startup-diagram`: port the `build_diagram` assertions listed above.
