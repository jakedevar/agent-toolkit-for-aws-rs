# Bedrock identity, image, pricing and source-model resolution (#4403, slice `bedrock-resolve`)

Rust replacements for four modules of the `llm-to-bedrock` skill of the
`aws-startup-advisor` plugin (`plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts`).
The Python files stay in place until the skill documents that call them are
repointed (#4415, see "Callers").

| Python | Rust |
| --- | --- |
| `model_identity.py` | `toolkit bedrock-resolve identity` (`crates/toolkit-cli/src/bedrock_resolve/identity.rs`, the one canonical `same_model`) |
| `image_input.py` | `toolkit bedrock-resolve image` (`bedrock_resolve/image.rs`) |
| `bedrock_pricing.py` | `toolkit bedrock-resolve pricing` (`bedrock_resolve/pricing.rs`, rate table in `pricing_table.rs`) |
| `resolve_source_model.py` | `toolkit bedrock-resolve source-model` (`bedrock_resolve/source.rs`) |
| `test_model_identity.py`, `test_image_input.py`, `test_bedrock_pricing.py`, `test_resolve_source_model.py` | `bedrock_resolve/tests.rs` |

The four commands use the canonical router (`value_flags`, `strict_values: true`,
`passthrough: false`), `Output::set_exit_status` for the scripts' published
statuses and `ToolkitError` for failures; one `COMMANDS` block entry per
command and one README section. No network client exists: the live provider
catalog listing and the AWS Pricing API call are replaced by recorded inputs
(change 1), and live clients are follow-up #4675.

## `same_model` is canonical (#4607)

`bedrock-report` carried a private, tested copy of `same_model` in
`bedrock_report/identity.rs` because `model_identity.py` was not integrated.
That file moved (with `git mv`) to `bedrock_resolve/identity.rs`; `bedrock_report/validate.rs`
(`same_model`, `strip`) and `render.rs` (`is_decimal`) now import it from
`crate::bedrock_resolve::identity`. The function body is unchanged, so
`bedrock-report` behaves identically (its tests, including
`same_model_is_conservative`, still pass unmodified apart from the import).
`same_model_family` (the project shortcut: a non-empty mapping list where every row
matches) is new in Rust and reachable through `identity --mappings`.

## Parity

| Behaviour | Python | Rust | Test |
| --- | --- | --- | --- |
| Identity rule | strip, lower, no `arn:`, `latest` or `*`; geo prefix stripped; target must carry `provider.`; Bedrock `-vN[:M]` dropped from the target only; `claude-`/`gpt-` (not `gpt-oss`) | same | `only_equal_resolved_identities_qualify` |
| Only anthropic and openai | any other provider is never the same | same | `only_equal_resolved_identities_qualify` |
| Family shortcut | non-empty and every row matches | same | `the_project_shortcut_requires_every_nonempty_mapping_to_match` |
| CLI output | `{"same_model": false}` | same text | `the_identity_command_prints_the_scripts_json` |
| Converse format | extension to wire format, `.jpg` is `jpeg`, case ignored, unsupported raises | same, pathlib suffix rules | `the_jpg_extension_normalizes_to_jpeg`, `the_extension_case_is_ignored`, `unsupported_types_are_refused_rather_than_guessed` |
| Messages | Converse image before text; Responses wrapper with `detail` always on the image block; Chat `image_url.url` | same shapes | `the_responses_message_is_wrapped_as_a_user_message`, `the_converse_message_puts_the_image_before_the_text`, `the_chat_message_keeps_the_mime_normalization` |
| Missing bytes, bad detail | `ValueError`, bytes checked before the extension, detail before bytes | same order | `an_image_path_without_bytes_is_an_error`, `every_sdk_detail_level_is_accepted_and_others_are_refused` |
| Rate table | `STATIC_FALLBACK`, 44 rows, order significant | same rows, same order | `the_rate_table_matches_the_python_source` |
| Lookup order | profile ARN, Opus 5.5 rule, static table, GPT short-circuit, Pricing API | model-id validation first (item 10: a rejected id is unavailable whatever the table or recorded answers hold), then the same order with the recorded answer as the last step, for well-formed ids outside the table only | `opus_5_5_pricing_is_region_and_profile_aware`, `table_models_are_served_without_a_price_list`, `an_unpriced_gpt_says_unavailable_not_nonexistent`, `rejected_ids_are_never_priced_even_by_a_matching_recorded_answer` |
| Partial matches | bidirectional family match, date stamp strip, proprietary GPT ids exact only | exact, or the unique row sharing the date/revision-normalized form (item 10; prefixes and ambiguous bases are unavailable); GPT exact only | `opus_4_8_is_priced_in_every_id_shape`, `unknown_astra_ids_never_inherit_another_rate`, `cris_forms_never_partial_match`, `truncated_empty_or_partial_ids_are_never_priced` |
| Inference options | Geo is 1.10x, Global is standard | same | `gpt_rates_follow_the_inference_option`, `global_and_eu_anthropic_prefixes_price_their_inference_option` |
| Price parser | base input/output token dimensions only; cache, read, write, batch skipped | same | `price_dimensions_are_the_base_input_and_output_rates` |
| Display name | `us.anthropic.claude-haiku-4-5-20251001-v1:0` is `Claude Haiku 4.5` | same | `display_names_are_derived_from_model_ids` |
| Unavailable | `{"available": false, ...}`, never an exception | same, key order kept per source | `unavailable_is_a_banner_not_an_exception` |
| `pricing` output | `json.dumps(indent=2)`, ids stripped, duplicates collapsed in first position | same | `the_pricing_command_prints_the_scripts_json` |
| Source resolution | exact, shortest date/version pin, alphabetic suffix is `not_found` + `ambiguous_prefix`, otherwise five by longest common prefix | same, plus a tie of equally short pins is `not_found` + `ambiguous_prefix` (item 11) | the ported `an_exact_match_wins` ... `no_match_ranks_by_the_longest_common_prefix` |
| Provider choice | the env file (never the ambient environment) decides; `SOURCE_PROVIDER` is authoritative | same (`--source-provider` or `SOURCE_PROVIDER`) | `the_env_file_is_authoritative_for_the_provider` |
| Pagination | Anthropic `has_more`/`last_id`, Gemini `nextPageToken` | same over recorded pages | `the_anthropic_pagination_follows_has_more`, `the_gemini_pagination_follows_the_next_page_token` |
| Statuses | `no_key` exit 2, `error` exit 3, otherwise 0 | same | `a_missing_key_exits_2_before_any_catalog_is_read`, `an_unusable_catalog_is_a_redacted_error_answer_with_exit_3` |
| Redaction | secret, its `unicode_escape` form and 6+ char fragments become `***` | same | `secrets_are_redacted_in_every_textual_form` |

## Pricing behaviour parity (carried from #4399)

`startup-validators.md` items 30 and 37 deferred the `pricing_behavior` step of
`check-bedrock-pricing-parity.py` (it executed two revisions of `bedrock_pricing.py`
and compared parser, table and lookups) to this slice. Decision: **implemented as
Rust tests, command output re-deferred**.

- `the_rate_table_matches_the_python_source` reads the `STATIC_FALLBACK` rows of
  the Python file as text (nothing is executed) and requires the Rust table to be
  equal row for row, in order, bit for bit. It returns early when the Python
  file is gone (retired by #4415, which deletes the test with it).
- `the_parity_probes_of_the_python_check_resolve_as_before` replays the probes of
  `pricing_behavior` (every table key, the two fabricated dated forms, the unlisted
  GPT tier, the parser fragment, the display name) against the Rust functions.
- `toolkit startup pricing-parity` keeps printing `pricing_behavior: not checked`.
  Its check compared two Python revisions; making it report the Rust table changes
  that command's JSON contract and goldens, which belongs with the Python
  retirement. Recorded as follow-up #4677.

## Intentional changes

1. **Recorded inputs instead of live calls.** `resolve_source_model.py` listed the
   provider's catalog over HTTPS with the API key; `bedrock_pricing.py` called
   `boto3.client("pricing").get_products`. This build has no network client.
   `source-model` takes `--catalog FILE` (a JSON list of ids, or the provider's
   own response pages in listing order: OpenAI `{"data": [{"id"}]}`, Anthropic
   pages that continue while `has_more`, Gemini pages that continue while
   `nextPageToken`, `models/` stripped). `pricing` takes `--price-list FILE`
   (`[{"region", "model" (display name), "price_list": [item JSON text]}]`, the
   first item is used like `MaxResults=1`). The API key value is read only to
   choose the provider and to redact output; it is never sent anywhere. Without
   a recorded input `source-model` answers `{"status": "error"}` (exit 3) and a
   model outside the static table is `available: false` with a note naming the
   missing recording. An incomplete recorded listing (a page missing after
   `has_more` or `nextPageToken`, or a page past the last) is also `error`, never
   `not_found`. Live clients: #4675.
2. **Command shape.** `toolkit bedrock-resolve identity|image|pricing|source-model`.
   Value options reject an empty value and one starting with `-`; argparse
   abbreviations are not accepted; usage errors print the toolkit usage text,
   exit 2. `source-model` reads `PLAN_MODEL_ID` and `SOURCE_PROVIDER` from the
   environment unless `--plan-model-id` / `--source-provider` are given; an unset
   plan id is a usage error (Python raised `KeyError`, exit 1). A missing or
   unreadable env file is `error: cannot read ...` (exit 1), as the traceback was.
3. **`image` is a CLI.** `image_input.py` was a library. `image --emit
   format|mime|data-url|converse|responses|chat` prints the value as JSON
   (`--image PATH` names the file, whose bytes are read; `--prompt TEXT` or
   `--prompt-file FILE`; `--detail` for Responses). JSON has no byte string, so a
   Converse `source.bytes` is `{"base64": "..."}`. The extension is checked
   before the file is read; a file larger than 20 MiB is refused. The prompt is
   the payload and is printed exactly (JSON-escaped), never truncated.
4. **`identity --mappings FILE`** exposes `same_model_family` (a JSON list of
   `{source_model, aws_model_id}` rows) as `{"same_model_family": bool}`. A row
   that is not an object, or whose fields are missing or not text, is "not the
   same model" (Python raised on a non-dict row). Empty `--source`/`--target`
   values are refused by strict values (argparse accepted an empty string).
5. **Safe resolution (stricter than the script).** The safe-suffix test requires
   the suffix to end exactly (Python's `$` also accepted a trailing newline) and
   uses ASCII digits (Python `\d` accepted any Unicode decimal). A resolved id
   that cannot be shown as it is (control characters, over 120 characters, or
   containing a key value) is not offered: the answer is `error`, exit 3.
6. **Output safety (#4602).** Every caller- or catalog-derived string in a
   diagnostic or in `source-model`/`pricing` output goes through the shared
   `safe_text` helper (`safe_path` for paths): ESC, BEL, CR, LF, U+202E and other
   invisible or bidirectional characters are written as `\x1b`, `‮`, ...
   and text is bounded to 120 characters. On `source-model`, the three key values
   of the env file are redacted first (`***`, same variants as the script) on
   every output string, not only on the error detail. JSON on stdout is also
   ASCII (`ensure_ascii`). Two `pricing` ids that would show under the same
   escaped name are a usage error rather than one merged entry.
7. **Links.** Every read the commands own (env file, catalog, price list,
   mappings, image, prompt file) uses `toolkit-core`'s `read_regular_file` /
   `open_regular_file`: a symbolic link, a directory or a special file is refused
   (`symbolic links are not followed`), sizes are bounded (1 MiB text, 8 MiB
   recorded input, 20 MiB image).
8. **Malformed price items.** A recorded item that is not valid JSON, not a
   string, or has an unexpected structure or an invalid USD rate is `available:
   false` with a fixed note that repeats no value of the item (Python put
   `str(exception)` in the note). A `get_products` item with no input rate is
   `rates not found in price item` as before.
9. **Unicode corners.** `display_name_guess` uses Rust's case mapping for
   `capitalize()` and an approximation of `str.isdigit()` (decimal plus
   superscript/subscript/enclosed digits). Since #4698 it only ever sees an id
   that passed the lowercase-ASCII grammar of item 10, so these corners are not
   reachable from a lookup.

10. **Fail-closed price matching (#4684, #4698, intentional change).** Python's
    forward rule (`key.startswith(base)`) priced any truncated, empty or
    ambiguous id as the first table row that began with it, so `-v1`, `anthropic`
    or `anthropic.claude` came back as Haiku 4.5, and its display-name guess
    dropped everything after the first `:`, so a recorded Pricing API answer for
    `Claude Opus 4.8` also priced `anthropic.claude-opus-4-8-v1:`,
    `...-v1:0:0` and `...:arbitrary`. The Rust lookup classifies the id once,
    **before any source of price** (the static table, the Opus 5.5 and GPT
    rules, and the recorded `--price-list` answers), as one of:

    - **Rejected**: `available: false`, null rates and one fixed note that
      repeats nothing of the id, whatever the table or the recorded answers
      hold. An id is rejected when it is not
      `[geo.]provider.model[-YYYYMMDD][-v<N>[:<M>]]` in lowercase ASCII (at
      least two dot-separated segments of nonempty `[a-z0-9]` tokens joined by
      single `-`; at most one `:`, between a `-v<N>` or `-<N>` revision and
      ASCII digits): the empty id, `anthropic`, `openai.gpt-`, `-v1`, `-v1:0`,
      `...-v1:`, `...-v1:0:0`, `...:arbitrary`, non-ASCII digits, upper case,
      a trailing separator, the provisioned-throughput `...:28k` forms and an
      ARN that is not an inference-profile ARN (`claude-opus-5-5` and such an
      ARN no longer get the Opus 5.5 note). It is also rejected when its normal
      form is shared by more than one row, or when it is outside the table and
      is a respelling of a row's model identity (#4711). The respelling check
      ignores the geo scope, whether the table lists it or not. The id is read
      without a scope and without its first segment (any spelling: `us.`,
      `apac.`, `ca.`, `xx.`); a profile ARN is unwrapped first. Each reading
      loses its `-v<N>[:<M>]` revision and `-YYYYMMDD` date, which leaves a
      `provider.model`. When a reading, or the display name a recorded lookup
      would ask for, shows as the display name of any table row's model
      identity (the row without its geo scope, any geo), the id is rejected
      unless its `provider.model` is exactly that row's. So
      `anthropic.claude-opus-4.8`, `apac.anthropic.claude-opus-4.8`,
      `ca.anthropic.claude-opus-4-8:0`, `xx.anthropic.claude-opus-4.8-v1:0`,
      `amazon.claude-opus-4-8` and `foo.claude-opus-4-8` are all rejected as
      respellings of `anthropic.claude-opus-4-8`. A matching display name
      alone never validates a model id. Table rows are
      `[scope.]provider.model` with the provider in `TABLE_PROVIDERS`.
    - **Known row**: the id is a table key, or after removing a strict
      `-v<N>[:<M>]` revision and then a `-YYYYMMDD` date stamp it equals the
      same normal form of exactly one row (`anthropic.claude-haiku-4-5`,
      `us.anthropic.claude-sonnet-4-6-20250514-v1:0`, `amazon.nova-pro`); GPT
      ids still need an exact key. The table answers; a recorded answer is not
      consulted.
    - **Well-formed unknown**: any other well-formed id (`anthropic.claude`,
      `...opus-4-8-foo`, `openai.gpt-oss-120b-1:0`, and canonical spellings
      of a table model under a scope the table does not list, such as
      `apac.anthropic.claude-opus-4-8`,
      `apac.anthropic.claude-opus-4-8-20250514-v1:0` or
      `ca.anthropic.claude-haiku-4-5-20251001-v1:0`). Only these reach
      `display_name_guess` and the
      recorded answers; without a recorded answer for their own display name
      they are `available: false`.

    Tests: `truncated_empty_or_partial_ids_are_never_priced`,
    `a_base_shared_by_several_rows_is_ambiguous`,
    `every_supported_normalized_id_still_prices_exactly_one_row`,
    `rejected_ids_are_never_priced_even_by_a_matching_recorded_answer`,
    `every_negative_pricing_input_stays_unpriced_with_a_recorded_answer`,
    `the_pricing_command_rejects_malformed_revisions_with_a_recorded_answer`,
    `the_pricing_command_rejects_respellings_under_any_geo_scope`,
    `well_formed_ids_outside_the_table_still_use_a_recorded_answer`,
    `every_table_row_is_a_well_formed_known_row`.
11. **Ambiguous snapshots are not resolved (#4685, intentional change).** The
    script took the first of two equally long safe snapshots in catalog order.
    The Rust resolver removes duplicate catalog ids first (one model listed twice
    is one candidate), and if distinct safe candidates tie for the shortest
    length (`gpt-5.4-2026-03-05` and `gpt-5.4-2026-04-01` for plan `gpt-5.4`) it
    answers `status: not_found` with `ambiguous_prefix: true`, no `resolved_id`,
    the tied ids as `candidates`, and every safe hit in `all_hits`. The answer is
    the same in either catalog order. Tests:
    `tied_snapshots_are_ambiguous_in_either_catalog_order`,
    `a_duplicated_catalog_id_is_one_candidate_not_a_tie`.

## Superseded parity quirks (#4676)

#4676 tracked two Python quirks that did not fail closed (the forward prefix
price match and the first-of-equal-length snapshot). Both were landing blockers
under AC2 and are replaced by items 10 and 11 above (#4684, #4685); #4676 is
superseded by them.

Nothing here is a paid probe: the commands never call a model.

## Callers (not changed in this slice)

The Python files remain; nothing is deleted (#4415). These shipped documents still
call them and move with the skill-content pass:
`skills/llm-to-bedrock/SKILL.md`, `references/helpers/behavior-delta-detection/*`
(`model_identity.py --provider ... --source ... --target ...`, now
`toolkit bedrock-resolve identity --provider P --source S --target T`),
`references/helpers/run-source-model-baseline/run-source-model-baseline.md`
(`PLAN_MODEL_ID=<id> resolve_source_model.py <REPO>/.saws-mig`, now
`toolkit bedrock-resolve source-model <REPO>/.saws-mig --catalog FILE` while no
live client exists) and the agents that import `image_input.py` or
`bedrock_pricing.py`. `source_baseline.py`, `preflight_bedrock.py` and
`iam_policy.py` are #4404.
