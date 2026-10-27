# Startup reference checking and model-staleness tooling (#4407)

Slice `startup-tools`, wave 2 of programme #4369. Base: integration head
`a41cfbb5f5dfbb9272fcb168d797ee4b2bcab961` (startup validators #4399, scoring #4400,
models #4401 and diagram #4402 are in). The code extends `crates/toolkit-cli/src/startup`
(`skill_refs.rs`, `model_staleness.rs`) and reuses the startup module's bounded,
link-refusing readers (`read_text`, `read_text_under`, `is_regular_file_under`), the
artifacts module's `pyjson::{loads, dumps}` and `patterns::fnmatch`,
`manifests::{py_splitlines, universal_newlines, resolve_lenient}`, `toolkit_core::neutralize_line`
and the scoring test fixtures (`TempDir`, `write`, `exec`, `real_skill`). No second router,
JSON reader, glob matcher or escaping helper was added.

Scope: `plugins/aws-startup-advisor/tools` except `sync-vendored.py` (owned by #4377 and
still needed by the Python contract tests; see `ci-gates.md`). `application-source-review.ts`
is TypeScript and stays.

## Commands

| Rust | Python original | Result |
| --- | --- | --- |
| `toolkit startup skill-refs (--check \| --list) [--plugin-root DIR] [--baseline FILE] [--no-baseline] [--json]` | `check-skill-refs.py --check\|--list [--baseline FILE] [--no-baseline] [--json]` (`--plugin` was hidden) | exit 1 on a non-baselined `MISSING_REF` or a stale baseline entry |
| `toolkit startup model-staleness [--plugin-root DIR] [--strict]` | `model-staleness.py [--strict]` (paths were relative to the script) | exit 1 on drift or a bad contract; staleness only with `--strict` |

Both are registered in the single `COMMANDS` block (`strict_values: true`,
`passthrough: false`, no positionals; the table has no `repeatable_flags` field at this
base).

## Retired Python (callers proven gone)

| File | Replacement | Callers repointed |
| --- | --- | --- |
| `tools/check-skill-refs.py` | `startup skill-refs` | `plugins/aws-startup-advisor/mise.toml` `lint:skill-refs` |
| `tools/test_check_skill_refs.py` | `startup/skill_refs/tests.rs` (below) | only caller was the plugin `test` task glob |
| `tools/model-staleness.py` | `startup model-staleness` | `mise.toml` `lint:model-staleness`; the three byte-identical copies of `ai-model-lifecycle.md` (canonical + `gcp-to-aws` and `azure-to-aws` vendored) and the plugin `README.md` named the script |

`tools/skill-refs-baseline.json` stays (JSON) and is the default `--baseline`. The
`ai-model-lifecycle.md` copies were edited together and are still byte-identical
(`startup model-staleness` and `sync vendored --check` both verify it). `python-inventory.json`
is the baseline and is not edited.

## Acceptance criteria

- **AC1 (references, staleness checks and failure diagnostics preserved).** Every
  resolution and skip rule of `check-skill-refs.py` and every finding of
  `model-staleness.py` is ported with its message text. Evidence: the ported Python tests
  (table below), the real-tree test `the_real_repository_passes_with_no_stale_entries`
  (exit 0, no missing reference, no stale entry, more than 500 references, at least the six burn-down entries baselined)  entries), and `the_real_registry_and_its_copies_are_consistent`. The token scanner is
  checked against the original Python regular expression (`fancy_regex`, verbatim pattern)
  on hard lines, 30 000 generated lines and every line of the real skill and agent Markdown.
- **AC2 (deterministic current-date injection).** `model_staleness::check_plugin` and
  `run_model_staleness` take a `Date`; only the command handler reads the clock. Every
  staleness test passes a fixed date (boundary, over-by-one, future, leap days, strict
  and warn-only). The CLI has no `--today` option on purpose: a flag that could set the
  date would let a CI run silence the gate.
- **AC3 (no Python maintenance tools).** The three Python files above are deleted and no
  recipe invokes them; no Python was added.

## Python-to-Rust test mapping

| Python test | Rust test |
| --- | --- |
| `test_skill_relative_and_variable_prefixes_resolve` | `skill_relative_and_variable_prefixes_resolve` |
| `test_missing_reference_is_reported` | `a_missing_reference_is_reported` |
| `test_run_artifacts_templates_urls_and_relative_outputs_are_not_references` | `run_artifacts_templates_urls_and_relative_outputs_are_not_references` |
| `test_library_origin_files_resolve_against_shared` | `library_origin_files_resolve_against_shared` |
| `test_gcp_dependents_may_name_gcp_tree_and_agents_search_skills` | `gcp_dependents_may_name_the_gcp_tree_and_agents_search_one_skill` |
| `test_agents_resolve_only_against_llm_to_bedrock` | `agents_resolve_only_against_llm_to_bedrock` |
| `test_run_artifact_skip_is_limited_to_the_current_token` | `the_run_artifact_skip_is_limited_to_the_current_token` |
| `test_explicit_base_prefixes_do_not_fall_back_to_other_skills` | `explicit_base_prefixes_do_not_fall_back_to_other_skills` |
| `test_relative_prefixes_do_not_fall_back_to_the_unprefixed_skill_path` | `relative_prefixes_do_not_fall_back_to_the_unprefixed_skill_path` |
| `test_baseline_ignore_vs_entries_and_stale` | `baseline_ignore_rules_entries_and_stale_entries` |
| `test_cli_exit_codes` | `the_command_exits_one_on_a_missing_reference_and_zero_when_clean` |
| `test_real_repository_passes_with_no_stale_entries` | `the_real_repository_passes_with_no_stale_entries` |

`model-staleness.py` had no test; `startup/model_staleness/tests.rs` covers fresh, boundary,
stale (warn and strict), future date, impossible date, missing and incomplete contract,
contract spacing, oversized window, drift (including line endings), missing required copy,
missing canonical file with and without surviving copies, no registry, unreadable canonical,
links, hostile names, routing and usage errors, and the real tree at two injected dates.

## Deliberate differences

| Behaviour | Python | Rust | Why |
| --- | --- | --- | --- |
| Symbolic links | followed (`rglob`, `Path.resolve`, `read_text`) | a link met while scanning, resolving a candidate, reading the baseline, or reading a registry copy is an error / hard finding | owned-file reads refuse links (programme rule) |
| Plugin selection | hidden `--plugin`; `model-staleness.py` used the script's own location | `--plugin-root DIR` (default `.`) on both, like the sibling commands | the Rust binary is not installed beside the plugin |
| Repository root | two levels above the script | two levels above the canonical `--plugin-root` | same value in the repository; keeps `_rel` and the `REPO_ROOT / path` candidate meaningful for any root |
| Absolute-path quirks | the skill name, the `skills`/`fixtures`/`node_modules` skip and the library-origin test were computed on the ABSOLUTE path, so a checkout under a directory named `skills` or `fixtures` changed behaviour | computed on the path inside the plugin | a checkout location must not change the verdict |
| Output streams (`model-staleness`) | notes on stdout, findings and the failing summary on stderr | the same, but the summary is `error: model-staleness: N problem(s) — failing.` (the shared `error:` line); the leading blank line of the Python summary is dropped | shared exit contract; `startup` convention |
| Output streams (`skill-refs`) | everything on stdout, exit 1 | the same, plus `error: N skill reference problem(s)` on stderr | shared exit contract |
| Malformed baseline | `AttributeError`/`TypeError` traceback | `error: invalid baseline: ...` (exit 1); invalid JSON names the file and the parse position only | no traceback; no value is echoed |
| Unreadable or non-UTF-8 Markdown | skipped silently | skipped silently (kept) | published behaviour |
| `rglob` order | filesystem order | sorted | deterministic `--list` |
| Date source | local date (`date.today()`) | UTC date | no timezone database in the workspace; at most one day of difference at midnight |
| Digits in the contract | `\d` matches any Unicode decimal digit | ASCII digits only; a window of more than 38 digits is reported as "too large to be a day count" (Python accepted any size) | exact hand-written matcher; unrepresentable sizes are malformed, not silently capped |
| `isalnum` glue check | Python `str.isalnum` | `char::is_alphanumeric` | differs only for a few combining marks |
| Caller text in output | printed raw | source paths and registry names pass `toolkit_core::neutralize_line`; JSON goes through `ensure_ascii` and DEL is written `\u007f` | diagnostic safety (#4639); `tests/diag_contract.rs` covers both commands |
| Token scan cost | quadratic in the length of a run of path characters | linear | a hostile 400 KB line cannot stall the gate |

## Changes to existing tests and recipes

- `crates/toolkit-cli/tests/ci_gates.rs`: the plugin recipe now runs three toolkit commands
  (`sync vendored`, `startup skill-refs --check`, `startup model-staleness --strict`, each with
  `--plugin-root .`); the "every recipe routes and passes on the real tree" test executes seven
  commands. It runs `model-staleness` without `--strict` because that test has no date to
  inject and the strict check would start failing on its own once the registry passes its
  45-day window (the strict path is tested with injected dates). The retired-Python test lists
  the three deleted files.
- `plugins/aws-startup-advisor/mise.toml`: `lint:skill-refs` and `lint:model-staleness` call the
  Rust commands through the same `cargo run --locked ... toolkit` form as `lint:vendored-parity`.
  The Python tooling that remains in that file belongs to other slices.
