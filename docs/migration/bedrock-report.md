# Bedrock migration report and result validation (#4405, slice `bedrock-report`)

Rust replacements for two scripts of the `llm-to-bedrock` skill of the
`aws-startup-advisor` plugin (`plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts`).
The Python files stay in place until the skill documents that call them are
repointed (see "Callers").

| Python | Rust |
| --- | --- |
| `render_report.py` | `toolkit bedrock-report render` (`crates/toolkit-cli/src/bedrock_report/render.rs`) |
| `validate_result.py` | `toolkit bedrock-report validate` (`bedrock_report/validate.rs`) |
| `schemas/{analysis,ingestion,eval,rewrite,delta-decisions}.json` | the same files, compiled into the binary with `include_str!` (single source of the contract) |
| `test_render_report.py`, `test_validate_result.py` | `bedrock_report/tests.rs` |
| `test_delta_reference_schema.py`, documentation half of `test_astra_execution_contract.py` | `bedrock_report/contracts.rs` |

Both commands use the canonical router (`value_flags`, `strict_values: true`,
`passthrough: false`), `Output::diagnostic` and `Output::set_exit_status`; no new
router, option or stderr helper was added. Schemas are evaluated by the engine
of slice `spec` (`spec/engine.rs`, a local-only, bounded draft 2020-12 validator);
files are read with the link-safe readers of `toolkit-core` (`read_regular_file`,
`write_regular_file`). the same-model rule is the canonical `bedrock_resolve/identity.rs` of slice #4403
(`model_identity.py`, see `docs/migration/bedrock-resolve.md`); pricing and image
normalization are #4403 too; preflight and IAM are #4404.

## Parity

| Behaviour | Python | Rust | Test |
| --- | --- | --- | --- |
| Summary text | `AI Migration Complete!` + branch, pass rate, files, optional tests and delta lines, report | same lines, same order | `the_summary_is_the_scripts_text` |
| Zero golden cases | `no_golden_cases: true` in the eval notes renders `N/A`, never `100%` | same | `zero_golden_cases_are_not_rendered_as_a_hundred_percent` |
| Degenerate payloads | missing or non-object sections degrade to `(none)`, `(unavailable)`, `0 files` | same | `a_degenerate_payload_does_not_crash` |
| Pass rate | `round(pass_rate * 100)` (half to even), booleans count as integers, partial coverage suffix | same | `the_pass_rate_rounds_half_to_even_like_python`, `partial_coverage_is_appended` |
| Test counts | `(\d+)\s*/\s*(\d+)\s+passing` over the rewriter notes (Unicode digits and spaces) | hand-written scan with the same semantics | `test_counts_are_parsed_from_the_rewriter_notes` |
| Report lookup | `MIGRATION_REPORT_<suffix>.md`, else the last `MIGRATION_REPORT_*.md` by name | same (see changes 7 and 8) | `the_report_with_the_date_suffix_is_preferred`, `without_a_suffix_match_the_last_report_in_name_order_wins` |
| Report copy | `copy2` into `--results-dir` (default `~/saws-migrate-results`), then print | content copy, then print | `the_report_is_copied_to_the_results_directory`, `the_results_directory_keeps_its_meaning_in_both_modes` |
| Phase results | `rewrite.json`, `eval.json`, `delta-decisions.json`; blocked/partial files are not payloads | same | `the_phase_results_mode_prints_the_summary`, `missing_or_control_state_files_degrade` |
| Input modes | exactly one of PAYLOAD_JSON / `--phase-results`; `--phase-results` needs `--repo` | usage error, exit 2 | `render_arguments_follow_the_script_usage` |
| Unreadable payload | `render_report: cannot read payload JSON: ...` on stderr, exit 1 | same text frame | `an_unusable_legacy_payload_is_reported_on_stderr` |
| Golden payloads | five schemas accept the documented shapes | same schemas | `golden_payloads_validate` |
| Required fields | every documented-mandatory field is enforced | same | `each_required_field_is_enforced_and_named`, `special_patterns_require_all_four_booleans` |
| Control states | `blocked` reasons per schema, `partial` only in `eval`, `ingestion` has none | same | `blocked_reasons_belong_to_their_own_schema`, `control_states_that_do_not_fit_are_invalid`, `partial_exists_only_in_the_eval_schema` |
| Output lines and exit status | `RESULT=valid CONTROL=...` (0), `RESULT=invalid` + findings (1), `RESULT=error ...` (2) | same on stdout | `control_lines_follow_the_script`, `unreadable_files_exit_two_with_a_result_error_line` |
| Finding paths | `$.a[0].b: message`, sorted by path | same paths and order | `findings_use_the_documented_paths_and_wording` |
| `oneOf` reporting | control-state branch when `blocked`/`partial` is present, else the payload branch, else the whole schema | same | `partial_exists_only_in_the_eval_schema` |
| Same-model rule | `same_model_family: true` needs every `source -> target` pair to be the same first-party model | same rule | `a_provider_only_identity_is_rejected_before_reuse`, `an_unchanged_identity_stays_eligible_even_with_a_pending_api_delta`, `same_model_is_conservative` |
| Run context | field-wise diff of the flattened JSON; `report_date_suffix` excluded; `source_key_sha256` prints `differs`; the current contract version is enforced | same | `an_identical_run_context_matches`, `each_changed_field_is_named`, `the_current_evaluation_contract_is_enforced_even_when_the_caller_omits_it`, `a_key_fingerprint_difference_never_prints_either_hash` |
| Value equality and spelling | Python `==` on leaves (`1 == 1.0 == True`), `json.dumps` spelling, `<absent>` / empty-container sentinels | same | `leaves_compare_like_python_values`, `values_are_printed_with_python_json_spelling`, `nested_and_structural_differences_are_reported` |
| Delta reference guard | every `resolution_kind` the references mention is accepted by `analysis.json`; evaluator reasons fit `eval.json`; evaluator prompt guards | same scans over the files | `every_reference_resolution_kind_is_accepted_by_the_analysis_schema`, `every_evaluator_control_state_reason_is_representable`, `the_evaluator_*` |
| Astra documentation contract | text assertions over SKILL.md, agents and the gcp/azure references | same assertions | the `contracts.rs` tests named after the Python ones |

## Intentional changes

1. **Command shape.** `render_report.py` and `validate_result.py` become
   `toolkit bedrock-report render|validate`. Value options reject an empty value
   and one starting with `-` (strict values); argparse abbreviations of long
   options are not accepted; usage errors print the toolkit usage text and exit
   2 (argparse printed its own usage and also exited 2). An invalid `--schema`
   value is not echoed.
2. **Findings never echo values of the checked file (#4602).** Python printed
   `jsonschema` messages such as `'sk-...' is not one of [...]`. The Rust
   findings name the path and the JSON type of the value (`a string value is not
   one of ['good', 'poor', 'unknown']`, `an integer value is less than the
   minimum of 0`) and keep the schema-supplied limits; unexpected property names
   are printed escaped and bounded (`spec/engine.rs` `validate_redacted`).
   Blocked reasons are printed only after the schema has restricted them to its
   enum; the `detail` text is never printed. Error type, order and exit status
   are unchanged.
3. **Escaped output.** Keys, file names and report paths taken from the files
   are printed through `safe_text`/`safe_path` (control, format, separator and
   tag characters become `\x1b`/`‮` escapes; summary values are cut after
   120 characters). A hostile key or value therefore cannot forge a `RESULT=`,
   `MISMATCH` or summary line. Ordinary text is unchanged. Run-context values
   keep Python's `json.dumps` spelling, which is already ASCII and escaped.
4. **Stricter JSON.** The toolkit parser rejects duplicate object keys,
   `NaN`/`Infinity`, a leading BOM and nesting deeper than 128, and reads
   integers beyond 64 bits as floats. Python accepted the first three (the last
   duplicate won). Such a file is `not valid JSON` (exit 2). The position text is
   `message: line L column C`.
5. **Link-safe, bounded reads.** Phase-result, context and payload files are read
   with `read_regular_file`: a symbolic link, a directory, a file over 1 MiB or
   invalid UTF-8 is refused (exit 2 for `validate`, `cannot read payload JSON`
   for a legacy payload, and "missing" for a phase file of `render`). Operating
   system error text (`[Errno 2] ...: 'path'`) is replaced by short reasons that
   never repeat the path.
6. **Wrongly typed values.** Python crashed with a traceback on
   `files_changed: 3`, `notes: 7`, a non-string `repo` or a non-object payload or
   `--current` context. Rust treats `files_changed` that is not an array as 0
   files, non-string notes as empty, a non-string `repo`/`reportDateSuffix` as
   absent, and reports a non-object payload (`render`, exit 1) or current context
   (`validate`, `RUN_CONTEXT=error`, exit 2). Python counted the characters of a
   string or the members of an object `files_changed`.
7. **Report lookup is link- and traversal-safe.** Only regular files that are not
   symbolic links qualify, so a directory named like a report is not picked
   (Python then crashed in `copy2`) and a link cannot copy another file into the
   results directory. A `reportDateSuffix`/`--date-suffix` containing `/`, `\`
   or NUL never names a report (`MIGRATION_REPORT_a/../../x.md` reached outside
   the repository).
   A bare `-` suffix is refused by the router for the same reason, in both
   spellings (`docs/migration/cli-bare-dash.md`, #4955); the Python original took it.
8. **Safe copy.** The report is copied as UTF-8 text of at most 16 MiB, content
   only (no mode or timestamps), and the destination is never written through a
   symbolic link. A copy failure prints `render_report: cannot copy the report:
   ...` on stderr, exits 1 and prints no summary (Python raised a traceback). A
   non-UTF-8 report is refused. `~/saws-migrate-results` is resolved from `HOME`
   only when a report is found and `--results-dir` is absent.
9. **`same_model` is shared.** The private copy in `bedrock_report/identity.rs`
   moved to the canonical `bedrock_resolve/identity.rs` (#4403, follow-up #4607);
   `validate` and `render` import it from there. Behaviour is identical.
10. **Large integers.** `COMPLETED`/`TOTAL` of a `partial` result print with
    Python's `str()` of the parsed value; an integer beyond 64 bits therefore
    prints as a float spelling instead of the exact digits.

## Out of scope

- `model_identity.py`, `bedrock_pricing.py`, `image_input.py` and `resolve_source_model.py`
  are ported by #4403 (`bedrock-resolve.md`); `source_baseline.py`, `preflight_bedrock.py`, `iam_policy.py` (#4404).
  The tests of `test_astra_execution_contract.py` that import those modules are
  listed in follow-up #4608.
- The tests of that file that execute Python embedded in skill documents
  (evaluator snippets, the ROI formula, the GCP harness template, the
  agent-advisor comparator) cannot run without a Python runtime. Their document
  text is still guarded by `contracts.rs`; the decision on a Rust-side contract
  for the embedded snippets is follow-up #4609.
- The telemetry copies under `references/vendored` are covered by the `sync`
  slice.

## Callers (not changed in this slice)

The Python files remain; nothing is deleted. These shipped documents still run
`uv run --project $SCRIPTS python $SCRIPTS/validate_result.py` or `render_report.py`
and move with the skill-content pass once the `toolkit` binary is distributed:
`skills/llm-to-bedrock/SKILL.md` (phase gate, resume check, final report) and the
agents `llm2bedrock-code-analyzer.md`, `llm2bedrock-log-ingestor.md`,
`llm2bedrock-prompt-evaluator.md` and `llm2bedrock-code-rewriter.md`. The
equivalent invocations are `toolkit bedrock-report validate --schema NAME FILE`,
`toolkit bedrock-report validate --check-run-context SAVED --current CURRENT` and
`toolkit bedrock-report render --phase-results DIR --repo REPO --date-suffix S`.
