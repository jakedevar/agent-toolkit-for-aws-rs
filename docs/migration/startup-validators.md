# Startup advisor validators (#4399)

Slice `startup-validators`, wave 2 of programme #4369. Base: the pinned stack
`81cc6189` (#4376 spec validator on #4375 on #4374) plus the accepted #4374
head `98957f87` (YAML depth/flow fixes and docs; merged cleanly, no conflict
resolution needed). Nothing here has landed; landing waits on #4425.

The first delivery ported `validate-model-pricing.py`, `validate-startup-program-artifacts.py`
and `model-id-lint.py`; later deliveries ported `validate-artifacts.py`,
`emit-plan-json.py`, `consent-disclosure-audit.py`, `check-bedrock-pricing-parity.py`,
`validate-terraform-policy.py` and finally `validate-migration-report.py`, which
completes **all eleven** files of the slice (see the end). Verification evidence lives
in the Issue #4399 handoff, not in this file.

## Commands

| Command | Replaces | Behaviour |
| --- | --- | --- |
| `toolkit startup model-pricing [--plugin-root DIR]` | `plugins/aws-startup-advisor/scripts/validate-model-pricing.py` | every default Anthropic candidate has a numeric Active price row in both vendored Bedrock pricing caches |
| `toolkit startup program-artifacts --migration-dir DIR` | `.../scripts/validate-startup-program-artifacts.py` | artifacts do not imply a confirmed Activate tier when `startup_program_status` is `unknown`, and link to the Activate page when they mention it |
| `toolkit startup model-id-lint [--plugin-root DIR]` | `.../scripts/model-id-lint.py` and its tests (`tests/test_model_id_lint.py`) | no retired or fabricated Bedrock model ID outside the catalog files |
| `toolkit startup artifacts (--self-check \| --fixtures \| --run-dir DIR --skill NAME) [--plugin-root DIR] [--manifest FILE] [--baseline FILE] [--no-baseline] [--json]` | `.../scripts/validate-artifacts.py` (+ `scripts/artifact-contracts.json`, `artifact-contracts-baseline.json`, `scripts/contracts/*.schema.json`) and the migrated cases of `tests/test_validate_artifacts.py` | published artifact contracts (JSON examples in the schema documents plus two JSON Schemas) checked against run directories and the fixture goldens |
| `toolkit startup emit-plan --migration-dir DIR [--plugin-root DIR] [--plugin-json FILE]` | `.../scripts/emit-plan-json.py` (and the byte-identical copy in `skills/heroku-to-aws/scripts`) and `tests/test_emit_plan_json.py` | copies validated cost figures of a finished GCP or Heroku run into `plan.json` for the web import; fail-open (`PLAN_OK` / `PLAN_SKIP`, exit 0) |
| `toolkit startup consent-audit [--plugin-root DIR]` | `.../scripts/consent-disclosure-audit.py` and `tests/test_consent_disclosure_audit.py` | usage-API discovery sub-files (`skills/*/references/phases/discover/discover-*-api.md`) must not carry a blanket GET-only claim that omits a non-GET method their Capture Endpoint Table calls |
| `toolkit startup pricing-parity [--plugin-root DIR]` | `.../scripts/check-bedrock-pricing-parity.py` | the Bedrock pricing split kept the data of the pre-split commit `c34fbdfde677` and the split commit `7a638343ebb2` (read with `git show`); JSON report |
| `toolkit startup terraform-policy DIR [--json FILE]` | `skills/tf-best-practices/scripts/validate-terraform-policy.py` and `test_validate_terraform_policy.py` | static policy checks on generated Terraform (ALB TLS, public RDS, public DB/admin ports, wildcard IAM, RDS/ElastiCache encryption), `POLICY_OK` / `POLICY_FAIL` |
| `toolkit startup migration-report REPORT [--estimation-infra FILE] [--estimation-ai FILE] [--aws-design FILE] [--migration-dir DIR] [--no-require-toc] [--no-readability] [--mode full\|decision\|ai_only]` | `.../scripts/validate-migration-report.py` and `scripts/test_validate_migration_report_decision_core.py` | generated `migration-report.html` / `decision-report.html` completeness: section ids, TOC, appendix depth, readability, accessibility, visual shell, `data-cost-key` figures against the estimate, decision-core content; `REPORT_OK` / `REPORT_FAIL` |

`--plugin-root` defaults to the current directory (the Python scripts defaulted
to the directory they lived in; `mise` tasks already run in the plugin
directory, so the caller update in #4378 is `cd` plus the new command).
Registered in `toolkit_cli::COMMANDS`; routing, `Output::diagnostic` and
`Invocation::value` are the #4374/#4375 contracts, unchanged. The other validators
replace the Python regular expressions with exact hand-written matchers
(`startup/text.rs`, `startup/model_ids.rs`) pinned by tests; `migration-report` is the
exception and runs its ~80 patterns on `fancy-regex` (item 39).

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

### artifacts

Module `startup/artifacts/` (`pyjson`, `jsonc`, `patterns`, `shape`, `schema`,
`contracts`, `mod`). Findings, codes, ordering, the text report, the `--json`
report and the exit status (0 without a non-baselined error finding, else 1)
follow the Python tool. The report (findings, `PASS`/`FAIL` summary or the
`--json` object) is stdout data exactly as in Python; a failing run adds the
shared `error: N artifact contract problem(s)` line on stderr.

| Python behaviour | Rust | Tests |
| --- | --- | --- |
| `json.loads`: duplicate key keeps first position and last value; `NaN`/`Infinity`/`-Infinity`; error `msg` and `lineno` (`Expecting ',' delimiter`, `Unterminated string starting at`, BOM message, ...) | `pyjson::loads` (not the strict core parser, so contract examples and artifacts that Python accepts still load) | `loads_keeps_first_position_and_last_value_of_a_duplicate_key`, `loads_accepts_python_constants_and_rejects_the_rest`, `loads_reports_python_line_and_column` |
| `json.dumps` (`ensure_ascii`, `", "`/`": "`, `indent=2`, `sort_keys`) for `--json` and the `uniqueItems` identity | `pyjson::dumps` | `dumps_matches_python_formatting` |
| `strip_jsonc`: `//` outside strings, key attribution by nesting depth (inline value closed on the line / still open), whitespace before `:`, bare `...`, `true\|false`, trailing commas | `jsonc::strip_jsonc` and the three rewrites as exact matchers | `strip_jsonc_*`, `trailing_commas_are_removed_before_a_closing_bracket_only`, `a_lone_ellipsis_line_becomes_a_member` |
| `parse_block` candidate order (lone `"..."`, as is, braces, braces + lone), own-line comment binds to the key that opened the object, `SCHEMA_PARSE` message with the block line | `contracts::parse_block` | `parse_block_reads_objects_arrays_and_fragments`, `a_comment_on_its_own_line_binds_to_the_key_that_opened_the_object` |
| enum recognition: `a\|b` strings, comment enums (bare and quoted members, regex backtracking of `_COMMENT_ENUM_RE`), `"+"-joined set`, `// REQUIRED`/`ALWAYS present` and the negative qualifiers, `or null`/`nullable`, placeholder strings and keys | `patterns`, `shape::build_node` | `enums_come_from_pipe_strings_and_comments`, `comment_enums_cover_bare_and_quoted_members`, `joined_set_qualifier_is_recognised_in_both_spellings`, `required_comments_and_placeholders`, `nullable_comments_add_null`, `a_placeholder_key_makes_the_object_open_with_an_any_key_template` |
| `Node.merge` (unspecified-`any` replacement, enum kept across a concrete example, copies not shared), `_walk_path`, `_unwrap_to_path`, `_merge_example_at`, `allowed_anywhere`, `open_paths` | `shape` | `an_empty_array_item_is_replaced_when_a_concrete_shape_arrives`, `a_declared_enum_survives_a_merge_with_a_concrete_example`, `merged_children_are_copies_so_a_named_refinement_does_not_leak`, `a_wrapped_array_example_merges_at_the_array_level`, `allowed_anywhere_vocabulary_is_accepted_at_every_level` |
| `validate_shape`: `UNKNOWN_KEY`, `MISSING_REQUIRED`, `TYPE_MISMATCH` (null tolerated), `ENUM_VIOLATION` (joined sets), `_` keys ignored, wildcard/any-key objects | `shape::validate_shape` | `unknown_keys_are_reported_but_underscore_keys_are_not`, `missing_required_type_mismatch_and_enum_violation`, `messages_name_the_kinds_and_the_allowed_values`, `null_is_tolerated_and_open_objects_accept_anything`, `an_any_key_template_applies_to_named_children`, `a_joined_set_allows_plus_joined_members_but_not_unknown_ones` |
| document scan (fence regex, headings), heading match/token, `(REQUIRED)` headings with table bodies, manifest `sections`/`@L<line>` anchors/`section_rules`/`ignore_headings`/`extra_shapes`/`required`/`open_paths` | `contracts::scan_doc`, `build_shape_contract`, `patterns::heading_*` | `documents_are_scanned_into_blocks_and_headings`, `heading_tokens_and_matches`, `a_required_heading_with_a_table_body_is_enforced`, `manifest_sections_rules_ignores_and_extras_refine_the_shape`, `a_manifest_anchor_that_misses_a_block_is_reported`, `shape_contract_failures_are_schema_parse_findings` |
| draft-07 subset (`type` incl. `integer`, `enum`, `const`, `pattern`, length/size/number bounds, `required`, `additionalProperties`, `items`, `uniqueItems`, `allOf`/`anyOf`/`oneOf`/`not`, local `$ref`), Python message wording, any unlisted keyword → `SCHEMA_PARSE` at load | `schema::SubsetValidator`, `unsupported_keywords`; `pattern` runs on the spec validator's bounded `jsonschema`/`fancy-regex` adapter (`spec::engine`) | `the_json_schema_subset_reports_each_violation_with_python_wording`, `numeric_string_object_and_array_limits`, `refs_combinators_and_type_lists`, `schema_patterns_use_the_bounded_engine_and_refuse_invalid_ones`, `unsupported_keywords_are_listed_with_their_pointer`, `every_keyword_of_the_two_real_schemas_is_enforced_or_an_annotation` |
| schema-declared properties admitted into the shape (`_node_from_schema`, `_admit_schema_properties`); fixture-only shared contracts; route shapes by `when_path`/`when_value` | `contracts` | `route_shapes_select_the_shape_by_the_artifact_marker`, `target_only_ai_contracts_do_not_validate_current_skill_outputs`, `a_neutral_run_skips_provider_artifacts_and_expected_snapshots` |
| `fnmatch` semantics for manifest globs and baseline `artifact`/`path` globs (bracket classes such as `resources[[]*[]].id`), baseline matching, `STALE_BASELINE` for unmatched entries | `patterns::fnmatch`, `apply_baseline` | `fnmatch_follows_python_including_bracket_classes`, `the_baseline_marks_matches_and_reports_stale_entries`, `a_stale_baseline_entry_fails_the_gate` |
| artifact discovery: `rglob("*.json")` sorted by path components, skipped directories (`live-capture`, `workspace-terraform`, `.terraform`, `node_modules`), `expected-*`/`clarify-answers*` skipped, `scenario-NNN` normalisation, `NO_CONTRACT` only with a skill | `find_artifacts`, `artifact_name` | `find_artifacts_sorts_by_path_components`, `artifact_names_normalise_scenario_numbers` |
| modes: `--self-check`, `--fixtures` (shipped baseline applied by default), `--run-dir DIR --skill NAME` (no baseline unless `--baseline`), `--no-baseline`, `--json` | `artifacts_command` | `self_check_parses_every_contract_document_of_the_real_plugin`, `the_fixture_goldens_pass_with_the_shipped_baseline_and_no_stale_entry`, `the_unmodified_golden_run_passes_without_a_baseline`, `an_off_contract_key_in_a_copied_golden_fails_with_its_path`, `a_real_schema_violation_fails`, `empty_phases_fail_min_properties_and_a_valid_status_passes`, `text_output_lists_errors_infos_and_the_summary`, `usage_errors_exit_two`, binary test |

Python unit tests migrated: the unit cases of `tests/test_validate_artifacts.py`
(JSONC stripping, template inference, merging, shape validation, schema subset,
baseline) and the CLI cases that need only the real plugin tree (self-check,
fixtures, injected off-contract key, stale baseline, schema violation, empty
`phases`, neutral/target-only artifacts, documented default run-dir command).
**Not ported one by one:** the ~25 per-document cases that rebuild a mutated
golden for one Heroku/GCP/Azure document (`test_heroku_*`, `test_gcp_*`,
`test_azure_*`, `test_ai_workload_capability_*`, `test_workshop_snapshots_*`).
Their engine behaviours (enum merge, route shapes, required inline arrays,
joined sets, closed objects, scalar after an empty seed, allowed-anywhere keys)
have unit tests above, and the real-tree `--self-check` and `--fixtures` runs
check every contract document and every golden; the document-specific mutation
cases stay in `tests/test_validate_artifacts.py` (not deleted) and are listed in
the remainder below.

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


### Intentional changes of `startup artifacts`

12. **Report streams.** The report (including the `--json` object) is stdout as
    in Python; the failing exit adds one `error: N artifact contract
    problem(s)` line on stderr. `--run-dir` without `--skill`, an invalid
    `--skill`, a missing mode or two modes are usage errors (exit 2) with the
    shared usage text instead of argparse's.
13. **Where the plugin root comes from.** Python used the directory of the
    script; `--plugin-root DIR` (default `.`) replaces it. Finding paths are
    relative to it as before (`_rel`); `--manifest` still overrides the manifest.
14. **Symbolic links** under a run directory, the fixture tree or any read
    file are a runtime error (`symbolic links are not followed`); Python's
    `rglob`/`read_text` followed them. Contract documents, schemas, the
    manifest and artifacts are read with the 64 MiB bound and universal
    newlines.
15. **JSON edge cases of the Python-compatible reader.** An integer outside
    the `i64` range is a float; a lone surrogate escape (`\ud800`) becomes
    U+FFFD (Python kept a lone surrogate); nesting deeper than 500 is the
    error `maximum recursion depth exceeded` (Python raised `RecursionError`);
    OS error texts are Rust texts (`cannot read <path>: file does not exist`),
    but `JSONDecodeError` messages, lines, columns and positions are
    Python's.
16. **Deterministic order.** `MISSING_REQUIRED` findings for one object (sorted
    by key name) and unmatched manifest anchors (sorted by anchor) have a fixed
    order; Python iterated hash sets, so their order varied between runs.
17. **`SCHEMA_PARSE` for an unsupported keyword** names the Rust constants
    (`ENFORCED`/`ANNOTATIONS` in `startup/artifacts/schema.rs`) instead of the
    Python ones; everything else in the message is unchanged. A JSON `null`
    contract block is reported as `the block is JSON null` (Python crashed with
    `AttributeError`).
18. **`pattern` keyword.** Evaluated by the spec validator's bounded
    `fancy-regex` adapter (search semantics), not Python `re`: `$` does not
    match before a trailing newline, a pattern the engine cannot compile or
    finish is a runtime error (never a pass), and `\d` is the engine's Unicode
    class. All eight patterns of the shipped schemas behave identically.
19. **Unsupported schema values.** A manifest or schema value of an unexpected
    type (a non-string `json_schema`, a non-list `route_shapes`, ...) is a
    runtime error naming the field; Python raised `TypeError`/`KeyError`. An
    unresolvable or non-local `$ref` is ignored as before; a `$ref` cycle is
    refused (`schema nesting or $ref chain is too deep`) instead of
    overflowing the stack.
20. **`scenario-\d+\.`** in `artifact_name` and `\w` in headings follow the
    same ASCII-digit and Unicode-word conventions as items 6 and 7.

### Intentional changes of `startup emit-plan`

21. **Plugin manifest location.** Python read `producerVersion` from the plugin
    next to the script; `--plugin-root DIR` (default `.`) or `--plugin-json
    FILE` replaces it. The heroku skill copy of the script is the same command,
    so the byte-parity guard test has nothing left to guard.
22. **Messages of unreadable input.** `PLAN_SKIP | reason=unreadable input: ...`
    keeps Python's `JSONDecodeError` text (same pure-Python reader as item 15);
    OS and decode errors carry Rust texts, and a symbolic link is refused as
    unreadable input (Python followed it). Write failures read `could not write
    plan.json: <Rust io error>`.
23. **Number handling.** Integers beyond `i64` are floats; a sum that overflows
    `i64` continues as a float (Python integers are unbounded; the USD cap
    makes both a skip). `\d` in the accuracy band is ASCII-only (Python's
    matched any Unicode digit and `float()` converted it); the band is matched
    by an exact hand-written matcher (no regex engine), with the `\s`, `\b`
    and `\w` conventions of items 6 and 7. A digit string too long for a float
    fails open (`cannot convert float infinity to integer`) like Python.
24. **Temp file.** The unique write temp is created exclusively with mode 0600
    on Unix (`create_new`), renamed into place atomically; orphan temps
    (`.plan-*.json.tmp` older than an hour) are reclaimed on every run, without
    following links.
25. **Displayed path** is normalised the way `pathlib` does (no `.`
    components, no repeated or trailing separators).

### Intentional changes of `startup consent-audit`

26. **Matchers instead of `re`.** The Python regular expressions are exact hand-written
    matchers (the shared phrase matcher of `startup/text.rs`, now with a
    `finditer`-style `find_starts` and literal alternatives, plus small functions for
    the table, heading, list-item and sentence rules). `\d` in a list item marker is
    ASCII-only; `IGNORECASE` is the simple case folding of the other startup
    validators.
27. **Streams and exit codes.** Findings go to stderr as before; a failing run adds one
    `error: ...` summary line (exit 1, like `model-id-lint`). A missing plugin
    directory is exit 2 with `Plugin directory not found: <resolved path>`.
28. **Unreadable files.** Still a failure per file, never a skip, but the reason is the
    Rust text (`not valid UTF-8`, `not a regular file`, `symbolic links are not
    followed`) instead of the Python exception text.
29. **`--plugin-root`** defaults to the current directory (the Python default was the
    plugin the script lived in).

### Intentional changes of `startup pricing-parity`

30. **The Python-behaviour step is not ported.** The script `exec`d the pre-split and the
    current `bedrock_pricing.py` and compared their parser, fallback table and lookup
    functions. That needs a Python runtime, so the report carries
    `"pricing_behavior": "not checked: needs the Python bedrock_pricing.py modules"` and
    `static_fallback_keys` is dropped. The `llm-to-bedrock` slice that converts the module
    carries that comparison as a Rust test; until then this one step has no replacement
    (recorded in the handoff, nothing is deleted).
31. **Failure reporting.** A failed assertion is `error: <message>` (exit 1) with the Python
    message; assertions that had none say `assertion failed: <what>`. Pre-split files are
    read with `git show` (needs `git` and the pre-split commit in the history); a missing
    commit is an error naming it. `--plugin-root` defaults to the current directory.
32. **Matchers instead of `re`.** Heading, metadata (`\s*(.+)` backtracking included), data
    row, accuracy band and embeddings-strip rules are hand-written; `\d` is ASCII-only.

36. **Re-pinned comparison commits (manager ruling, adjusted).** The Python script pinned
    `883270642209`; against the current tree it fails at the first data check
    (`Bedrock section changed during extraction`) because the canonical Bedrock cache has
    grown (GPT-6 rows before the split, Opus 5.5 rows and notes and a Sonnet 5.5 note
    after it). The ruling was to re-pin `BASE_SHA` to the migration baseline `86edead`
    (`python-inventory.json` `baseline_commit`); that commit is *after* the split, so its
    GCP cache has no `## Bedrock Models (On-Demand)` section and the proof would fail with
    `missing section`. The pins are therefore two commits that make the one-off proof
    hold on today's tree:
    - `BASE_SHA` = `c34fbdfde677` (the parent of the split commit): the pre-split GCP
      cache, Azure cache and GCP `SKILL.md`; this includes the GPT-6 rows that the pinned
      `883270642209` predates.
    - `SPLIT_SHA` = `7a638343ebb2` (the split commit, upstream "split Bedrock pricing
      cache"): the canonical cache as the split wrote it, used for the Bedrock-section
      equality and the lookup-key/rate comparison, so later legitimate growth does not
      matter. Everything else (vendored copies byte-identical to the live canonical
      cache, estimate documents, neutral consumers, Region/Currency/Accuracy, the
      remaining GCP/Azure caches) is still read from the live tree.
    - The `Last updated` equality is dropped for the canonical cache: the split commit kept
      `2026-10-05` while the pre-split cache already said `2026-10-10` (upstream defect
      in the data, not a regression). The Azure cache's `Last updated` is still compared.
    The drift-failure test now uses a synthetic grown split section; a second test runs
    the checks against the real tree and the pinned commits.
37. **`pricing_behavior` is carried by the Bedrock pricing slice.** The step that executes
    the old and new `bedrock_pricing.py` (item 30) is carried by #4403 (`bedrock-resolve`,
    llm-to-bedrock `bedrock_pricing.py` and its tests) as a Rust test; the report keeps
    `pricing_behavior: not checked`.

### Intentional changes of `startup terraform-policy`

33. **Command shape.** `toolkit startup terraform-policy DIR [--json FILE]`. A missing
    directory or an unwritable `--json` path is exit 2 with the `POLICY_FAIL` line; a failing
    verdict keeps the Python stderr lines (`POLICY_FAIL \| checks=...`, one line per
    violation) and adds `error: N policy violation(s)`.
34. **File discovery.** `.tf` files are found by a directory walk that does not follow
    links; a linked `.tf` file is a runtime error and a non-UTF-8 file is `cannot read`
    (Python raised a traceback). Order and relative paths are Python's (sorted by path
    components).
35. **Matchers instead of `re`.** The HCL lexer is a direct port; every pattern
    (`resource` headers, attribute reads, list reads, the IAM wildcard forms and the
    approved-statement removal) is an exact hand-written matcher. `\d` in a port is
    ASCII-only; an integer too large for `i128` compares as the maximum. The whole-internet
    test follows `ipaddress.ip_network(cidr, strict=False).prefixlen == 0` for IPv4/IPv6
    literals with `/0` (and `/0.0.0.0`), through the Rust address parsers.

### Intentional changes of `startup migration-report`

38. **Command shape.** `toolkit startup migration-report REPORT [flags]` with the Python
    options (`--estimation-infra`, `--estimation-ai`, `--aws-design`, `--migration-dir`,
    `--no-require-toc`, `--no-readability`, `--mode`). `REPORT_FAIL | migration-report.html`
    and the `  - <problem>` lines go to stderr and a final `error: N report problem(s)` line
    is added (exit 1); a missing report prints `REPORT_FAIL | file=... | reason=not_found`
    plus the `error:` line; `REPORT_OK | ...` is unchanged. A missing REPORT or a bad
    `--mode` is a usage error (exit 2). An optional artifact that is not a file is skipped
    as in Python; one that is a symbolic link, unreadable, not JSON or not a JSON object is
    a runtime error (Python raised a traceback or an `AttributeError`); a JSON `null` counts
    as absent. `scenarios/index.json` that is a link or unreadable counts as absent.
39. **Dependency.** `fancy-regex` 0.19 is a direct dependency of `toolkit-cli` (a
    `Cargo.toml` line and one `Cargo.lock` dependency entry). It is the engine
    `jsonschema` already builds, with the same features, so no crate is added to the
    build. The ~80 Python `re` patterns of the validator (look-behind, look-ahead and a
    back-reference) run on it unchanged. Differences from `re`: `\s` is Unicode
    `White_Space`, which lacks U+001C–U+001F that Python adds; offsets are bytes, not
    characters (the 25-character windows of the currency check count characters).
40. **HTML tokenizer.** Python's `html.parser.HTMLParser` (`convert_charrefs=True`) and
    `html.unescape` are ported by hand in `startup/report/html.rs` and the table of
    2231 HTML5 named references is generated from the standard library module
    (`entities.rs`). The reference is the `html.parser` of Python 3.14 read from the
    system standard library (the repository pins 3.12): the two differ only on malformed
    markup (comment closes, unterminated tags, raw-text end tags) and in the decoding of
    named references inside attribute values (`&amp=`).
41. **Python values.** `float()`, `int()`, `str()` and `repr()` of JSON values follow
    Python for numbers, booleans and strings; a numeric string with underscores is not
    accepted by `float()`. Python crashes (`AttributeError`, `OverflowError`) on a
    non-object `recommendation`, a non-finite figure or a non-numeric
    `security_baseline` component; the port reports the figure as not numeric or skips
    the value instead. `round()` is halves-to-even as in Python.
42. **Validation split.** `validate_report` lives in `startup/report/mod.rs` with the
    same check order as Python; the five parser subclasses are handlers in `walk.rs`.
    The decision-core test cases are migrated one for one (`report/tests.rs`) next to
    unit tests for the tokenizer, every extractor and every validator, the shipped
    reference, stub and decision fixtures, and the command.
43. **Artifact tests.** The document-specific cases of `tests/test_validate_artifacts.py`
    (heroku/gcp/azure contract regressions against the real documents and fixtures) are
    migrated in `startup/artifacts/tests/documents.rs`. Not migrated:
    `test_invalid_neutral_shared_artifacts_fail_their_schemas`, which calls the validator
    without a `--skill` (the command requires one).

## File membership (#4371 U-10)

Of the inventory rows with `slice == "startup-validators"` (manager rulings on the
earlier open questions: `check-bedrock-pricing-parity.py` stays in this slice and is
ported; `validate-terraform-policy.py` and its test are assigned to this slice):

- Ported: `validate-model-pricing.py`, `validate-startup-program-artifacts.py`,
  `model-id-lint.py`, `validate-artifacts.py` (`artifact-contracts.json`,
  `artifact-contracts-baseline.json` and `scripts/contracts/*.schema.json` are its
  data and stay in place), `emit-plan-json.py` (with `tests/test_emit_plan_json.py`;
  the byte-identical copy under `skills/heroku-to-aws/scripts` is the same command),
  `consent-disclosure-audit.py` (with its test),
  `check-bedrock-pricing-parity.py` (all data assertions; the Python-behaviour step is
  item 30) and `skills/tf-best-practices/scripts/validate-terraform-policy.py` (with
  `test_validate_terraform_policy.py`), `validate-migration-report.py` (with
  `scripts/test_validate_migration_report_decision_core.py`; items 38-42).
- All eleven files of the slice are ported. Remainder, still read-only evidence, not
  deleted, callers untouched (#4378): `tests/test_validate_artifacts.py` keeps the
  unit and real-tree cases in `startup/artifacts/tests.rs` and the document-specific
  cases in `tests/documents.rs`; the one case without a Rust counterpart is listed in
  item 43. The `pricing_behavior` comparison of the old and new `bedrock_pricing.py` is
  carried by the Bedrock pricing slice #4403 (items 30 and 37).

## Not in scope

No Python deleted, no CI/`mise.toml` change (#4378), no change to
`Cargo.toml`/`Cargo.lock` beyond the `fancy-regex` line (item 39), no change to the foundation
adapter or `.rsi/*`. `manifests.rs` gained visibility changes only (`is_py_space`, `py_splitlines`,
`py_eq` are `pub(crate)`); `spec.rs` made its private `engine` module
`pub(crate)` so the artifact validator reuses the bounded pattern engine.
`startup/text.rs` gained `Tok::Either` and `Pattern::find_starts` for the consent audit.
