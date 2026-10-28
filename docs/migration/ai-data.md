# AI dataset detection, transformation and metric validation (#4385, slice `ai-data`)

Rust replacements for three `aws-ai-ml` skill helpers (each exists twice: the
canonical `skills/core-skills/...` copy and the identical `plugins/aws-core/...`
mirror). The Python files stay in place until the skill documents that call them
are repointed (see "Callers").

| Python | Rust |
| --- | --- |
| `dataset-evaluation/scripts/format_detector.py` | `toolkit ai-data detect-format` (`crates/toolkit-cli/src/ai_data/format.rs`) |
| `model-evaluation/scripts/validate_custom_metrics.py` | `toolkit ai-data validate-metrics` (`ai_data/metrics.rs`) |
| `dataset-transformation/scripts/transformation_tools.py` | `toolkit ai-data transform plan`, `toolkit ai-data transform submit`, `toolkit ai-data transform describe` (`ai_data/transform.rs`) |

The commands use the canonical router (`value_flags`, `strict_values: true` for
`detect-format` and `transform`), `Output::diagnostic` and
`Output::set_exit_status`; no new router, option or stderr helper was added.
Tests are in `ai_data/tests.rs` and run offline: S3 and SageMaker are reached
only through the `ObjectStore` and `ProcessingService` traits, and this build
wires `NoObjectStore` and `NoSageMaker`.

## Parity

| Behaviour | Python | Rust | Test |
| --- | --- | --- | --- |
| 11 formats + `unknown` | `_classify_schema` order: query+response, verl, messages, DPO, SFT | same order, same `completion` fall-through | `every_format_is_detected_and_validates_cleanly`, `classification_precedence_follows_the_script` |
| Nova SFT vs DPO | last message `candidates` key, else truthy `content` | same, Python truthiness | `nova_last_message_decides_between_sft_and_dpo` |
| Role, content, candidate checks and texts | `_validate_*_messages` | same messages | `messages_need_a_valid_role`, `gpt_oss_content_must_be_a_string`, `nova_messages_need_list_content_or_candidates`, `dpo_candidates_need_a_preference_label` |
| Invalid `role` / `preferenceLabel` | `str()` of the value echoed | type name and allowed values only (intentional change 11) | `messages_need_a_valid_role`, `dpo_candidates_need_a_preference_label`, `detect_format_never_echoes_invalid_role_or_preference_label_values` |
| Required field types and prefixes | `FORMAT_SCHEMAS` | same table | `flat_formats_check_field_types_with_their_prefix`, `rlvr_checks_reference_answer_and_content` |
| Verl and Verl Legacy prompt rules | `_validate_verl_*` | same | `verl_prompts_are_checked` |
| Schema consistency | `Expected X but found Y` per record | same | `records_must_match_the_first_records_format` |
| Unknown first record | all-unknown input is valid, HIGH | same | `unknown_records_are_valid_unknown_and_mismatch_known_ones` |
| Parse errors | `parse_error`, confidence `none`, valid formats still detected | same | `malformed_lines_are_parse_errors_with_no_confidence`, `only_unparseable_lines_give_unknown_with_no_confidence` |
| Line numbers | 1-based over the non-empty sampled lines | same | `line_numbers_count_nonempty_lines` |
| Sampling | first N bytes, cut after the last `\n`, `errors="replace"`, empty lines dropped | same; reads only the prefix | `samples_keep_only_complete_nonempty_lines`, `local_sampling_reads_only_the_requested_prefix` |
| S3 sampling | `Range: bytes=0-(N-1)`, URI errors | same texts, injected store | `s3_uris_are_validated_with_the_script_messages`, `s3_sampling_sends_a_range_and_drops_the_partial_line` |
| Text and JSON report | `Format:`/`Valid: ✓`/`Confidence: NAME`; `json.dumps(indent=2)` | same bytes, ASCII only incl. U+007F | `detect_format_text_report_*`, `detect_format_json_report_is_the_scripts_document`, `detect_format_json_is_ascii_only` |
| Exit codes | 0 valid, 1 invalid or `Error:` on stderr, 2 argparse | same | `detect_format_reports_unreadable_inputs_on_stderr`, `detect_format_sample_size_is_bounded_and_validated` |
| Metric array limits | JSON array, 1 to 10 items, messages | same | `the_array_shape_is_checked_first` |
| `CustomMetric` models | pydantic v2 | hand-written, same fields, coercions and rules | `metrics_must_be_objects_with_a_definition`, `field_errors_are_reported_together_in_field_order` |
| Instructions | <= 5000 chars, needs `{{prompt}}` or `{{prediction}}` | same, counted in characters | `instructions_need_a_placeholder_and_a_bounded_length`, `lengths_count_characters_not_bytes` |
| Rating scale | definition <= 100 chars, exactly one of floatValue/stringValue, no mixing | same | `definitions_are_limited_to_100_characters`, `a_rating_value_holds_exactly_one_value`, `a_scale_must_not_mix_value_types` |
| Model-level rules run only after valid fields | pydantic `after` validators | same | `field_errors_suppress_the_model_level_rules` |
| Verdict output and exit | `✅ Valid — N custom metric(s) defined.` / `❌ Validation failed:` + `  - ...`, exit 1; no argument: usage, exit 1 | same on stdout | `validate_metrics_command_prints_the_verdict`, `validate_metrics_without_an_argument_prints_usage_and_exits_one` |
| File or inline argument | open as file, `FileNotFoundError`/`IsADirectoryError` mean inline | same | `validate_metrics_reads_a_file_before_treating_the_argument_as_json` |
| Processing job arguments | `--input`/`--output` = mount + `basename(source.rstrip("/"))` | same, including an empty name | `input_and_output_arguments_use_the_source_file_name` |
| Job defaults | role, region, SKLearn `1.2-1` image looked up in this order | same order via `ProcessingService` | `submit_resolves_defaults_in_the_scripts_order`, `submit_keeps_explicit_role_region_and_image` |
| Submit message | `Processing job '<name>' submitted. Output will be at: <s3>` | same | `submit_resolves_defaults_in_the_scripts_order` |
| Describe dict | `job_name, status, failure_reason, creation_time, processing_end_time, inputs, outputs` | same keys and order | `describe_maps_the_job_like_the_script` |

## Remote payload classification (AC3)

- **Local helpers (this slice):** the three scripts above run on the user's
  machine as toolkit helpers.
- **Remote managed-service payloads (not this slice):** the transformation
  script that `transformation_tools.py` uploads and runs inside the SageMaker
  Processing container (`command=["python3"]`, `--input`/`--output` under
  `/opt/ml/processing`), the generated `code_templates/transformation.py`, and
  the training, evaluation and reward-function templates run in SageMaker,
  Bedrock or Lambda, not on the user's machine. They are classified
  `external-service-payload` (slice `ai-payloads`, #4386) in
  `python-inventory.json`. `transform plan` records the container contract
  (`command`, mounts, `--input`/`--output`) as data so it can be tested, but the
  payload language is that slice's compatibility question; no local Python
  runtime is retained or called.
- **Generated notebook code:** `dataset_transformation_code.md` shows user
  notebooks importing `execute_transformation_job` / `describe_transformation_job`
  from the Python file. That is a user-side Python caller, listed below.

## Service seams

- `format::ObjectStore::get_range(bucket, key, "bytes=0-N")` is the only S3
  access. `NoObjectStore` fails with `S3 access is not available in this build;
  use a local file path.` An SDK-backed store (optional feature) is a follow-up.
- `transform::ProcessingService` (`default_role`, `default_region`,
  `sklearn_image`, `start`, `describe`) is the only SageMaker access; its
  `describe` adapter must return `str()` of the times ("" when absent, "None" when
  null), as the script did. `NoSageMaker` fails with `SageMaker SDK access is not
  available in this build`. An SDK-backed service is a follow-up.

## Intentional changes

1. **Command shape.** `toolkit ai-data detect-format|validate-metrics|transform`
   replace `python <script>`; `--sample-size` and `--json` come after the path.
   `transform` is new as a command (the Python file only exposed functions).
2. **Strict JSON parsing.** Lines and metric files are read by the shared strict
   parser (`toolkit-core`): duplicate object keys, `NaN`/`Infinity`, a leading
   BOM and nesting deeper than 128 are `parse_error` lines (Python accepts the
   first three and crashes on deep nesting); integers beyond `i64` read as floats
   (`got float`). Parse messages read `Invalid JSON: <reason>: line L column C`
   (Python: `<reason>: line L column C (char N)`).
3. **Records that crashed Python.** A record that is not a JSON object, a message
   that is not an object, non-list `candidates` and non-object candidates raised
   `AttributeError`/`TypeError` (or tested substrings of a string). They are
   `invalid_structure` findings (`Record must be a JSON object, got list`,
   `Message N must be an object, got X`, `DPO message N 'candidates' must be list,
   got X`, `DPO message N candidate M must be an object, got X`); classification
   treats such shapes as `unknown`.
4. **Bounded samples.** `--sample-size` is an integer from 0 to 16 MiB (a
   negative value read the whole file in Python; larger values would hold the
   whole file in memory). A usage error (exit 2) otherwise. S3 sampling with 0
   bytes is refused before the request (Python sent `bytes=0--1`).
5. **Bounded error lists.** At most 10 000 errors are kept; the rest are only
   counted. The text report ends with `  ... and N more error(s) not shown` and
   the JSON gains `errors_omitted` only when N > 0. `is_valid` and `confidence`
   count every error.
6. **Symbolic links.** Input files, metric files and the transformation script
   must be regular files and not symbolic links (shared `open_regular_file`);
   Python followed links. The script's directory is made absolute without
   collapsing `..` (`std::path::absolute`), so the uploaded directory is the one
   the operating system resolves; Python's `abspath` collapsed `..` lexically.
7. **Error text.** `FileNotFoundError`/`IOError` texts become the shared path
   messages (`Error: path does not exist: <path>`), still on stderr with exit 1.
   S3 and SageMaker failures that were uncaught Python exceptions (exit 1) are
   `Error: ...` (`detect-format`) or `error: ...` (`transform`), exit 1.
8. **pydantic messages.** Failures use pydantic's `N validation error(s) for
   CustomMetric`, location line and `<message> [type=..., input_type=...]`
   layout, but omit the `input_value=` repr and the documentation URL (they
   depended on the installed pydantic version). `Value error, ...` texts, the
   error `type` names and the field locations are kept. Lax coercion is
   limited to what JSON can carry: numbers, booleans and `float()`-parsable
   strings for `floatValue`; `definition`, `name`, `instructions` and
   `stringValue` accept only strings (as pydantic does).
9. **Metric file reading.** A metric file is read as UTF-8 up to 1 MiB (an
   unreadable, non-UTF-8 or oversized file is `error: ...`, exit 1, as the
   tracebacks were). An argument that cannot be a path (too long for a file name,
   NUL byte) is inline JSON; Python raised `OSError: File name too long` for
   inline JSON over 255 bytes.
10. **Transformation inputs.** The source and output locations must be
    `s3://<bucket>[/...]` and the script must exist as a regular file (Python
    passed anything to the SDK). Empty `--region`, `--execution-role` and
    `--image-uri` mean "not given".
11. **No payload echo in diagnostics (#4602).** Python printed an invalid
    value: `Invalid role '<str(role)>' in message N` and `Invalid
    preferenceLabel '<str(label)>' in message N candidate M`. That copied
    untrusted record content (secrets, nested objects, newlines, CR and terminal
    escape sequences) into the text report, where it could forge report lines or
    drive the terminal, and into the JSON report. The messages now name the
    field, location, Python type and allowed values only: `Invalid role in
    message N (got str; allowed: 'user', 'assistant', 'system')` and
    `Invalid preferenceLabel in message N candidate M (got dict; allowed:
    'preferred', 'non-preferred')`. Error type `invalid_structure`, line
    numbers and exit 1 are unchanged. No other `detect-format` message carries
    record values (parse errors are fixed reason texts with positions).

## Callers (not changed in this slice)

The Python files remain; nothing is deleted. These shipped documents still
point at them and move with the skill-content pass once the `toolkit` binary is
distributed: `dataset-evaluation/overview.md` (runs `python scripts/format_detector.py`),
`model-evaluation/references/llmaaj-custom-evaluation.md` (runs
`python scripts/validate_custom_metrics.py`) and
`dataset-transformation/{overview.md,references/dataset_transformation_code.md}`
(import `execute_transformation_job` in generated notebooks). Each exists in the
canonical skill and the plugin mirror, which `toolkit sync plugin-skills` keeps
identical.

## Follow-ups

- SDK-backed `ObjectStore` and `ProcessingService` (optional feature).
- Repoint the callers above and retire the six Python files (canonical and
  mirror) after the binary ships.
