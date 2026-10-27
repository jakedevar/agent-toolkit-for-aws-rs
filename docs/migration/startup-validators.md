# Startup advisor validators (#4399, first delivery)

Slice `startup-validators`, wave 2 of programme #4369. Base: the pinned stack
`81cc6189` (#4376 spec validator on #4375 on #4374) plus the accepted #4374
head `98957f87` (YAML depth/flow fixes and docs; merged cleanly, no conflict
resolution needed). Nothing here has landed; landing waits on #4425.

This delivery ports **three** of the eleven slice files and records the rest as
an explicit remainder (see the end). Verification evidence lives in the
Issue #4399 handoff, not in this file.

## Commands

| Command | Replaces | Behaviour |
| --- | --- | --- |
| `toolkit startup model-pricing [--plugin-root DIR]` | `plugins/aws-startup-advisor/scripts/validate-model-pricing.py` | every default Anthropic candidate has a numeric Active price row in both vendored Bedrock pricing caches |
| `toolkit startup program-artifacts --migration-dir DIR` | `.../scripts/validate-startup-program-artifacts.py` | artifacts do not imply a confirmed Activate tier when `startup_program_status` is `unknown`, and link to the Activate page when they mention it |
| `toolkit startup model-id-lint [--plugin-root DIR]` | `.../scripts/model-id-lint.py` and its tests (`tests/test_model_id_lint.py`) | no retired or fabricated Bedrock model ID outside the catalog files |

`--plugin-root` defaults to the current directory (the Python scripts defaulted
to the directory they lived in; `mise` tasks already run in the plugin
directory, so the caller update in #4378 is `cd` plus the new command).
Registered in `toolkit_cli::COMMANDS`; routing, `Output::diagnostic` and
`Invocation::value` are the #4374/#4375 contracts, unchanged. No new
dependency: the Python regular expressions are replaced by exact hand-written
matchers (`startup/text.rs`, `startup/model_ids.rs`) pinned by tests, not by a
second regex engine (the `fancy-regex` engine of #4376 stays private to the
spec validator's schema adapter).

## Parity table

Messages are the Python text, byte for byte, on the same stream. A failing run
additionally ends with one `error: ...` summary line on stderr (the shared
`report` contract); the Python scripts printed no such line. Exit status is
unchanged unless listed under "Intentional changes".

### model-pricing

| Python behaviour | Rust | Tests |
| --- | --- | --- |
| `DEFAULT_CATALOG = MODELS_DIR / "<name>"` located with a multiline `^` regex; none found → `Cannot locate the default Anthropic catalog.` (exit 1) | `default_catalog_name`: line-start prefix, non-empty name up to the next `"` | `pricing_selector_must_name_the_default_catalog_at_the_line_start` |
| both caches `gcp-to-aws`, `azure-to-aws` (order kept); table rows only; `cells = split("\|")[1:-1]` stripped; need ≥ 8 cells | `priced_models` | `pricing_ignores_rows_that_are_not_complete_table_lines` |
| status cell matches `active\b` case-insensitively (`Active`, `active (…)`; not `activated`, `active_x`, `inactive`) | `starts_with_active` | `pricing_status_must_start_with_the_word_active` |
| input (cell 3) and output (cell 4) must parse as `float`, be finite and `> 0`; unparsable → row skipped; underscores between digits allowed | `positive_rate` | `pricing_rates_must_be_finite_and_positive_numbers`, `pricing_both_rate_columns_are_required_and_other_columns_ignored` |
| one leading `us.`/`eu.`/`au.`/`jp.`/`global.` stripped from the model id | `strip_region` | `pricing_region_prefixes_are_stripped_once_and_only_at_the_start` |
| each catalog model's `paths.runtime_converse.model_id` must be priced; finding text `<cloud>: candidate <id> has no numeric Active price row.`; findings gcp first | `validate_model_pricing` | `pricing_reports_missing_candidates_per_cloud_in_order`, `pricing_requires_the_row_in_each_cloud_separately` |
| `except (OSError, ValueError, KeyError)` → the message alone, **earlier findings discarded**, exit 1 | `Err(message)` | `pricing_missing_inputs_abort_with_one_message`, `pricing_malformed_catalogs_are_reported_not_ignored` |
| `KeyError` text is `'key'` | same | `pricing_malformed_catalogs_are_reported_not_ignored` |
| success line `Model pricing coverage: all Anthropic candidates have numeric Active cache rows.` (stdout, exit 0) | same | `pricing_passes_when_every_candidate_has_an_active_numeric_row`, `real_plugin_pricing_coverage_passes`, binary test |
| universal newlines (`\r\n`, `\r`) on every read | `universal_newlines` + `py_splitlines` | `pricing_reads_crlf_caches_and_catalogs` |

### program-artifacts

| Python behaviour | Rust | Tests |
| --- | --- | --- |
| `--migration-dir` required (argparse exit 2); not a directory → `STARTUP_FAIL \| reason=not_a_directory \| path=…` (exit 1) | usage error / diagnostic + runtime error | `program_requires_the_migration_dir_option`, `program_missing_directory_is_a_runtime_failure_with_the_python_line` |
| no `preferences.json` → nothing enforced | same; a directory named `preferences.json` also counts as absent (`Path.is_file`) | `program_without_preferences_enforces_nothing`, `program_unreadable_preferences_are_runtime_errors` |
| status searched in `startup_constraints`, `ai_constraints`, `design_constraints` (every block, never short-circuiting on a truthy block without the key); the first block whose `startup_program_status` is an object wins; `value` list → first element or none | `load_status` | `program_searches_every_status_block_and_takes_the_first_object`, `program_status_block_without_a_value_or_with_other_shapes_is_not_enforced` |
| only the string `"unknown"` enforces | same | `program_only_the_string_unknown_enforces` |
| files checked: `STARTUP_PROGRAMS.md`, `migration-report.html`, `ai-migration/STARTUP_PROGRAMS.md` (that label), in that order | same | `program_checks_all_three_artifacts_with_their_labels` |
| six case-insensitive forbidden phrases, first matching pattern in list order only, message prints the pattern source | `FORBIDDEN_WHEN_UNKNOWN` | `program_every_forbidden_phrase_is_reported_with_its_pattern`, `program_forbidden_phrases_respect_word_boundaries`, `program_reports_one_phrase_finding_per_file_in_pattern_order` |
| `aws activate` mention without an `href` link to `…/startups/credits` and without the literal `https://aws.amazon.com/startups/credits/` → second finding | `ACTIVATE_MENTION`, `ACTIVATE_LINK`, URL check | `program_activate_mentions_need_a_clickable_link`, `program_one_file_can_have_a_phrase_and_a_link_finding` |
| output: `STARTUP_FAIL \| startup-program-artifacts` then `  - <finding>` lines on stderr (exit 1); `STARTUP_OK \| startup_program_status aligned with artifacts` on stdout | same | `program_honest_artifacts_pass`, binary test |

### model-id-lint

| Python behaviour | Rust | Tests |
| --- | --- | --- |
| banned IDs read from `skills/shared/ai/ai-model-lifecycle.md`: Model ID column (second cell) of table rows whose line contains `excluded` (case-insensitive); IDs of the `**Removed` list up to any `replacement:` | `load_banned_ids` | `lint_registry_rows_read_only_the_model_id_column`, `lint_replacements_and_notes_are_not_banned`, `lint_legacy_model_outside_the_exclusion_zone_is_allowed` |
| backtick ID grammar (`_ID_IN_BACKTICKS`, incl. the bare `vN:M` continuation and `*` wildcards) | `is_model_id`, `ids_in_backticks` (exact span semantics, closing backtick reused) | `lint_ids_inside_backticks_follow_the_python_regular_expression`, `lint_findall_skips_non_ids_and_reuses_the_closing_backtick` |
| dual IDs rebuilt (`amazon.nova-reel-v1:0` / `v1:1` → `…-v1:1`) | `rebuild_dual_ids` | `lint_dual_ids_rebuild_against_the_preceding_full_id`, `lint_dual_id_continuations_are_rebuilt_not_banned_as_bare_text` |
| `*` is a token-bounded wildcard (`[^\s`]*`), not `.*` | `IdMatcher` | `lint_glob_ids_stay_inside_one_token`, `lint_every_retired_id_fails_outside_the_catalog` |
| zero parsed IDs per section → setup problem, no-op gate refused (exit 1) | same | `lint_zero_parse_guardrails_refuse_a_no_op_gate` |
| catalog allowlist (3 canonical paths), vendored copies collapse onto `skills/shared/`; fabricated-ID allowlist (the repair helper) | `canonicalize`, `CATALOG_ALLOWLIST`, `FABRICATED_ALLOWLIST` | `lint_catalog_files_may_name_retired_ids_and_vendored_copies_inherit`, `lint_vendored_paths_collapse_onto_the_shared_tree`, `lint_fabricated_dated_ids_fail_except_in_the_repair_helper` |
| fabricated `claude-(sonnet-4-6\|opus-4-8)-\d{8}` | `has_fabricated_id` | `lint_fabricated_heuristic_boundaries` |
| scanned: `.md .py .json .ts .tf .sh .template .html` (Python `Path.suffix` rules), skip any path containing `node_modules`, files processed in `Path` order (component-wise, not string order), one finding per retired ID per line, line numbers after universal newlines and `str.splitlines` | `collect_files`, `suffix`, `lint` | `lint_checks_every_scanned_extension_and_nothing_else`, `lint_orders_files_like_python_paths_not_like_strings`, `lint_reports_line_numbers_with_universal_newlines`, `lint_one_line_can_report_several_ids_in_registry_order` |
| non-UTF-8 / unreadable files skipped silently | same for non-UTF-8 and read errors | `lint_skips_non_utf8_files_like_the_python_lint` |
| missing `--plugin-root` directory → `Plugin directory not found: …` exit **2** | usage error (exit 2; the shared usage text follows the message) | `lint_missing_registry_and_plugin_root_are_distinct_failures` |
| real tree passes (`test_real_plugin_passes`) | same | `real_plugin_model_ids_pass_the_lint`, binary test |
| the Python script excluded itself (`SELF`) | `scripts/model-id-lint.py` is skipped by path | `lint_skips_the_legacy_python_script_only_at_its_own_path` |

Python unit tests migrated: ten of the eleven cases of `tests/test_model_id_lint.py`
(clean copy, every retired ID, glob expansion, legacy allowed, caches
allowlisted, HTML fixture, fabricated + helper, zero-parse guardrail, missing
root exit 2, real plugin) plus the registry, grammar and ordering cases above.
The remaining one, `test_script_resolves_paths_inside_the_plugin`, asserts
the source text of the Python script and has no Rust counterpart; the Rust
lint resolves everything from `--plugin-root` and never from the repository
root (`lint_registry_rows…`, `lint_never_follows_symbolic_links`).

## Intentional changes (recorded)

1. **Streams.** A failing run ends with an `error: <summary>` line on stderr in
   addition to the Python lines. Exceptions the Python scripts let propagate or
   printed as `str(exc)` (`pricing`: `OSError`, `ValueError`, `KeyError`) are
   `error: <message>` with exit 1; their texts are Rust texts
   (`cannot read <path>: file does not exist`, parser positions) rather than
   `[Errno 2] ...` or `JSONDecodeError` text. A Python traceback (uncaught
   `AttributeError`/`TypeError` on a malformed catalog or `preferences.json`)
   becomes the same exit status 1 with a one-line message.
2. **Symbolic links are refused**, never followed (core threat model): a
   linked cache, selector, catalog, `preferences.json` or artifact is a runtime
   error (`symbolic links are not followed`) instead of being read or — for
   `Path.is_file()` — silently passing. In the lint, linked files and
   directories are not scanned (Python read linked files). A path component
   that makes the catalog name escape (`..`, backslash, absolute) is refused;
   Python joined it.
3. **JSON strictness.** The strict core parser rejects duplicate keys, `NaN`
   and `Infinity` literals, a BOM and depth > 128, which Python `json.loads`
   accepted (duplicate keys: last wins).
4. **Bounded reads.** Files larger than 64 MiB are an error (the lint treats
   it as a runtime error, never a skip).
5. **Unreadable directories** while walking the plugin tree are a runtime
   error; Python's `rglob` ignored them (a silent hole in a gate).
6. **`\d{8}`** in the fabricated-ID heuristic is ASCII digits only (Python
   `str` patterns also matched other Unicode decimal digits).
7. **Case folding / `\w` / `\s`.** Case-insensitive matching uses simple
   Unicode lower-casing plus Python's extra equivalences (dotted/dotless i,
   long s, Kelvin sign); `\s` is Unicode whitespace plus `\x1c`-`\x1f`;
   `\w` is alphanumerics and `_`. Python's tables follow its own Unicode
   version; characters whose class differs between versions may differ.
8. **Unicode digits in prices.** `float()` accepts non-ASCII digits; the Rust
   rate parser accepts ASCII digits only (a non-ASCII price cell counts as
   unparsable, i.e. unpriced).
9. **Default plugin root** is the current directory (see above), and the three
   commands are subcommands of `toolkit startup`.
10. **`argparse` messages** are replaced by the shared usage error
    (`error: the following arguments are required: --migration-dir` plus the
    usage text, exit 2).

11. **Kept quirk.** Dual-ID rebuilding treats any later token that starts with `v` as a continuation of the previous revisioned ID (`startswith("v")`), so a full ID beginning with `v` listed after a revisioned one is mangled exactly as in Python; real provider prefixes (`amazon.`, `anthropic.`, `meta.` ...) are unaffected. Pinned by `lint_dual_ids_rebuild_against_the_preceding_full_id`.

## File membership (#4371 U-10)

Of the eleven inventory rows with `slice == "startup-validators"`:

- Ported here: `validate-model-pricing.py`, `validate-startup-program-artifacts.py`,
  `model-id-lint.py` (belong: all three are plugin-health scripts in
  `plugins/aws-startup-advisor/scripts`).
- Remainder, still read-only evidence, not deleted, callers untouched (#4378):
  - `validate-migration-report.py` (2434 lines) and
    `test_validate_migration_report_decision_core.py` (449) — the multi-thousand-line
    report validator (AC2). Needs its own session: the decision-core contract,
    the HTML report grammar and the fixtures under `plugins/aws-startup-advisor/fixtures`.
  - `validate-artifacts.py` (1255) with `tests/test_validate_artifacts.py`
    (1363) and `artifact-contracts*.json` — schema/identity contracts.
  - `emit-plan-json.py` (658) with `tests/test_emit_plan_json.py` (158).
  - `consent-disclosure-audit.py` (460) with `tests/test_consent_disclosure_audit.py`
    (853): regex-offset heavy (sentence splitting, soft-wrap line mapping); a
    faithful port needs the same matcher approach as above plus careful
    offset tests, so it was left whole rather than ported partially.
  - `check-bedrock-pricing-parity.py` (305): **does not belong as a port.** It is
    a one-off proof for the pre-split commit `883270642209` — it runs `git show`
    against that SHA and *executes* `bedrock_pricing.py` (old and new) through
    `exec`, i.e. it needs a Python runtime and Python module behaviour. Proposed
    disposition for the manager: retire it with `bedrock_pricing.py`'s own
    conversion (the `llm-to-bedrock` slice) and keep its data-only assertions
    (cache sections, vendored byte parity) as a Rust test there.
  - `skills/tf-best-practices/scripts/validate-terraform-policy.py` (1029) and
    `test_validate_terraform_policy.py` (844): **questionable membership.** The
    Issue scope is `plugins/aws-startup-advisor/scripts/*.py`; these two live
    under the `tf-best-practices` skill. Left for the manager's U-10 ruling
    (they may belong with a Terraform/skills slice).

## Not in scope

No Python deleted, no CI/`mise.toml` change (#4378), no change to
`Cargo.toml`/`Cargo.lock` (no dependency added), no change to the foundation
adapter or `.rsi/*`. `manifests.rs` gained two visibility changes
(`is_py_space`, `py_splitlines` are `pub(crate)`) and no behaviour change.
