# Bedrock baseline, preflight and IAM policy tools (#4404, slice `bedrock-probes`)

Status: implemented in `crates/toolkit-cli/src/bedrock_probes/`; the Python
files below are kept (retirement is #4415) and nothing that calls them is
changed in this slice (see [Cutover](#cutover-for-the-callers)).

| Python (`plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/`) | Rust |
| --- | --- |
| `iam_policy.py` + `test_iam_policy.py` | `toolkit bedrock-probes iam-policy` (`iam.rs`) |
| `preflight_bedrock.py` + `test_preflight_bedrock.py` | `toolkit bedrock-probes preflight`, `api-path`, `classify` (`preflight.rs`, `client.rs`, `recorded.rs`) |
| `source_baseline.py` + `test_source_baseline.py` | `toolkit bedrock-probes source-baseline` (`baseline.rs`, `http.rs`) |
| `iam_policy` / `preflight_bedrock` cases of `test_astra_execution_contract.py` (#4608) | the tests named under [#4608 coverage](#4608-coverage) |

Nothing in this slice calls a network, reads a credential, signs a request or
generates a Bedrock bearer token. The AWS and provider answers are **recorded
files** (`--recorded FILE`); a run without one fails closed (see
[Recorded inputs](#recorded-inputs-and-the-no-live-build)). Live clients are
follow-up work (see [Follow-ups](#follow-ups)).

## Commands

```text
toolkit bedrock-probes iam-policy --models ID[,ID...] --region REGION --account-id ACCOUNT_ID [--output FILE]
toolkit bedrock-probes preflight --region REGION --models ID[,ID...] [--dataset-size N]
    [--runtime-api converse|responses|chat_completions] [--recorded FILE]
toolkit bedrock-probes api-path [--models ID[,ID...]] [--plan-path PATH] [--runtime-api API]
toolkit bedrock-probes classify --models ID[,ID...] [--region REGION]
toolkit bedrock-probes source-baseline ENV_FILE [--source-model-id ID] [--dataset FILE]
    [--output FILE] [--source-provider NAME] [--recorded FILE]
```

* `bedrock-probes iam-policy` is `python iam_policy.py --models --region --account-id [--output]`.
  stdout is `json.dumps(policy, indent=2) + "\n"` printed with `print`, so it ends
  in one blank line, exactly like the script; with `--output` the same text is
  written to the file and `Policy written to <path>` goes to stderr.
* `bedrock-probes preflight` is `python preflight_bedrock.py ...`: one JSON
  verdict on stdout (`json.dumps(indent=2)`, ASCII), exit 0 only when every model
  passes. Member order is the script's.
* `bedrock-probes api-path` is `preflight_bedrock.normalize_api_path(plan_path,
  model_ids, runtime_api)` for the callers that imported it: `{"api_path": "..."}`;
  the script's `ValueError` is `error: <message>` on stderr, exit 1, with the
  script's message text.
* `bedrock-probes classify` replaces the callers' `from preflight_bedrock import
  is_mantle_model` (and the IAM-side `iam_policy.is_mantle_model`, which also
  looks at the region): one row per id with `mantle`, `iam_mantle`, `embedding`,
  `inference_profile` and `gpt6_cris`.
* `bedrock-probes source-baseline` is `python source_baseline.py ENV_FILE` with
  the script's inputs `SOURCE_MODEL_ID`, `GOLDEN_DATASET_PATH`, `OUTPUT_PATH` and
  `SOURCE_PROVIDER`, each also available as a flag (`--source-model-id`,
  `--dataset`, `--output`, `--source-provider`). Each input is resolved the
  way the script saw it after `load_env_file` (`source_baseline.py:42-55`) put
  every file pair into `os.environ`: the flag first, then the env file's pair
  (an empty one included, so it hides the ambient value), then the ambient
  environment. Output rows, the `RESUME:` line and `live source baselines: N/M`
  are the script's.

Every value flag refuses an empty value and one that starts with `-` (the router's
`strict_values`). `STDIO_DASH` lists none of these commands, so a bare `-` is a
usage error everywhere (`--output -` does **not** write a file named `-`, and no
path argument reads stdin).

## Recorded inputs and the no-live build

`--recorded FILE` of `preflight` replays AWS and provider answers; of
`source-baseline` it replays provider replies. Both read the recording only
through `toolkit_aws::recorded` (`recorded_contract` passes, no allowlist
entry). The formats are documented in the module headers of `recorded.rs` (the
`bedrock-runtime`, `service-quotas`, `bedrock-mantle` and `bedrock-runtime-openai`
calls of a preflight) and `http.rs` (the provider HTTP exchanges of a baseline).
A recorded request is compared (key order ignored) with the request the tool
builds, so a recording pins the wire contract: the Converse and `InvokeModel`
probe bodies, the mantle Responses body, the runtime OpenAI URL
(`https://bedrock-runtime.<region>.amazonaws.com/openai/v1/{responses,chat/completions}`)
and the provider request bodies. A failure entry's request is compared before
the failure is replayed. Every optional member is absent or of its type; a
present member of another type, `null` included, rejects the recording naming
the member, never a value.

Without `--recorded` nothing is sent and every probe **fails closed**:

* `preflight` reports `ok: false` with reason `live_client_unavailable` for every
  model (the script's missing-SDK path, `mantle_deps_missing`, also failed closed;
  that reason has no Rust equivalent because there are no Python packages).
  A request the recording has no entry for is reason `unrecorded_call`.
* `source-baseline` records an `error: ConnectionError: this build includes no
  live HTTP client...` row per prompt (rows that are already `live` are kept),
  which a later live run retries, like any failed row.

## Intentional divergences

Every item changes behavior of the Python scripts; none changes what a
successful recorded run prints.

1. **No live calls, no token signing.** Both the boto3 and the `OpenAI` SDK calls
   of `preflight`, the `urllib` request of `source-baseline` and
   `aws_bedrock_token_generator.provide_token` (the Bedrock bearer token) are
   replaced by the recorded/no-live clients above. `create_openai_client` (an SDK
   constructor imported by the evaluator document) has no Rust equivalent; the
   endpoint it selected is pinned by `runtime_openai_url` and the recorded
   request. Live clients and the token signer are a follow-up.
2. **`live_client_unavailable` and `unrecorded_call`** are new reasons (see
   above). A transport failure reason keeps the script's text with the exception
   class name of the SDK it replaces (`NoCredentialsError`,
   `EndpointConnectionError`, `APIConnectionError`).
3. **IAM inputs are validated.** The script pasted `--models`, `--region` and
   `--account-id` into the resource ARNs, so `*`, `?`, `${...}` or a control
   character in any of them widened (or broke) the policy. `iam-policy` refuses a
   model id with one of those, a region that is not an AWS region name
   (`toolkit_aws::is_valid_region`) and an account id that is not 12 digits
   (usage error, exit 2). `generate_policy` itself stays as pure as the script's.
4. **`--output` refuses links and non-regular files** (`write_regular_file`); the
   script followed a link. `--output -` is a usage error (STDIO_DASH policy).
5. **`--dataset-size` takes `+5`, ` 7 `, `1_0` like `int()` but refuses a negative
   value** (the router refuses a leading `-`) and a non-ASCII digit; a value past
   the integer range saturates. `--models` takes at most 256 ids.
6. **Caller text in a verdict is redacted, truncated (300 characters) and
   escaped on one line** (`toolkit_aws::redact_text`, `toolkit_core::escape_untrusted`):
   the service message in every `detail`, the region in the malformed-region
   verdict, the recorded error code. Quotes and backslashes in such a message are
   escaped too, so a quoted message reads with `\"`. A message that held a
   secret reads `[REDACTED]` for that part. A caller path in a diagnostic (`cannot
   read ...`, `cannot write ...`, `not valid JSON: ...`, `Policy written to ...`)
   goes through the same order, bounded at 200 characters, with the env file's key
   values replaced by `***` first in `source-baseline`; the script printed it raw.
   Only the rendering changes: the file is read or written at the path given.
7. **A malformed region is `InvalidRegionError`** as botocore reports it, without
   the trailing-newline allowance of Python's `$`; nothing is probed.
8. **A malformed recorded quota listing is an error.** The script's `fetch_bedrock_quotas`
   returned `[]` on any `BotoCoreError`/`ClientError` and so does this tool for a
   failed or stuck (repeated `NextToken`, more than 100 pages) listing, but a
   listing whose `Quotas` member is `null` or not a list of objects, a `Value`
   that is not a number or a non-string `NextToken` is an invalid response and
   exits 1 (`error: invalid ListServiceQuotas response ...`) instead of reading
   as "no quotas".
9. **`source-baseline` reads its three paths and the env file through
   `read_regular_file` and writes through `write_regular_file`**: links and special
   files are refused, texts are UTF-8 and bounded (`MAX_TEXT_FILE_BYTES`; an image
   is bounded at 20 MiB). An image that is a link is an `error: OSError:` row.
10. **Inputs are checked before the first request**: a dataset line that is not
    JSON or has no scalar `id`, or a previous output with such a line, ends the run
    with exit 1 naming the line number only (the script crashed with a traceback,
    possibly after spending budget). An empty `SOURCE_MODEL_ID`/`GOLDEN_DATASET_PATH`/
    `OUTPUT_PATH` is treated as unset (`FAIL: required env var ... not set`,
    exit 2); an empty value in the env file hides the ambient one, as the
    script's `os.environ[k] = ""` did.
11. **The output directory is created when missing and skipped when the path has no
    directory part**; the script's `os.makedirs(os.path.dirname(path))` raised
    for a bare file name after all requests were made.
12. **A model id in a Gemini URL must be `[A-Za-z0-9._-]+`** (`FAIL:` exit 2
    otherwise), so a model id cannot add a path, query, fragment or host to the
    official `generativelanguage.googleapis.com` URL.
13. **A non-text provider reply is an error row.** A `null` OpenAI message content
    (a refusal, a tool call) was recorded as a `live` row with a `null` response;
    it is now `error: ValueError: the response has no ... text`. A malformed reply
    (no choices, no candidates, a text block without `text`) is an error row with
    that message instead of `KeyError`/`IndexError`/`TypeError` text; an Anthropic
    reply keeps the script's `No complete Anthropic text response (stop_reason=...)`.
    A non-string `user_prompt`, `system_prompt` or `image_path` is an error row
    (`TypeError`), not a request with that value in it.
14. **A live response that contains an API key value is redacted** (the script
    only redacted failure text), and failure text in a row is redacted, truncated
    (600 characters) and escaped on one line, with the script's `http_<code>:
    <reason>` prefix kept. Auth (401/403) bodies still carry no detail. The
    non-JSON error body is redacted (the key values, then `toolkit_aws::redact_text`)
    as a whole BEFORE the script's 200-character cut: the script cut first, so a
    key crossing the cut kept its first characters. A body over 256 KiB reads
    `[REDACTED]`.
15. **The env file is parsed once and never exported**: the script copied its pairs
    into `os.environ` so the request builders could read them; the builders now take
    the key as an argument. The ambient environment never selects the provider (as
    before: the file is authoritative; `SOURCE_PROVIDER` decides when it is set).
    The file's values still configure the run: `SOURCE_MODEL_ID`,
    `GOLDEN_DATASET_PATH`, `OUTPUT_PATH` and `SOURCE_PROVIDER` come from one lookup
    (flag, then file pair, then ambient variable), so a file-only configuration
    runs and a file value beats a conflicting ambient one, as in the script.
16. **`api-path` and `classify` are new commands** with no script entry point
    (the script's functions were imported). `classify` adds no behavior beyond the
    two `is_mantle_model` predicates and three other predicates of `iam_policy.py`.

## Policy statements and the wildcards the Python emitted

The policy is the script's, statement for statement. Every wildcard in a generated
policy is one the script already produced, listed here for review; the test
`the_only_wildcards_are_the_documented_ones` fails if another appears.

| Statement | Where | Why it is kept |
| --- | --- | --- |
| `BedrockInvokeModelScoped` | `Resource` `arn:<partition>:bedrock:*::foundation-model/<id>` (the region segment is `*`) | foundation models are served in whichever region a cross-region profile routes to; the id stays exact |
| `BedrockMantleInference` | `Action` `bedrock-mantle:Get*` and `bedrock-mantle:List*` | the script's read actions on mantle projects |
| `BedrockMantleInference` | `Resource` `arn:<partition>:bedrock-mantle:<region>:<account>:project/*` | mantle authorizes per project, not per model; narrow the model set with a service control policy |
| `BedrockMantleCallWithBearerToken` | `Resource` `*` | AWS does not support narrowing `CallWithBearerToken`; needed for Bedrock API-key auth, omit only for SigV4-only apps |

Not wildcards: `bedrock:InvokeModel`, `bedrock:InvokeModelWithResponseStream`,
`bedrock-mantle:CreateInference`, the inference-profile ARNs (exact id, region and
account) and the default project ARN (`project/default`, GPT-6 CRIS ids only).
An all-mantle run emits no `BedrockInvokeModelScoped` statement; an empty model
list keeps the legacy shape (one statement with an empty `Resource`).

## Parity table

| Behavior | Python | Rust | Test |
| --- | --- | --- | --- |
| Dual ARN for an inference profile | profile ARN + base foundation-model ARN | same | `inference_profile_generates_dual_arns` |
| Profile ARN as a model id | kept as given, base id from its last `/` segment | same | `system_profile_arn_keeps_its_arn_and_base_model_and_default_project` |
| GPT-6 CRIS id | + `project/default` ARN | same | `gpt6_cris_detection`, `astra_cris_ids_...` |
| Bare GPT-5/6 and bare Opus 5.5 | mantle statements only (Opus 5.5 is runtime in `eu-west-2`) | same | `bare_opus_5_5_is_mantle_except_in_eu_west_2` |
| Partition | `aws-us-gov`, `aws-cn` by region prefix | same | `partition_follows_the_region` |
| Probe request | Converse `maxTokens: 1`; `InvokeModel` for embeddings with the family body | same bodies | `a_chat_model_is_probed_...`, `embedding_models_go_to_...` |
| AccessDenied | `model_access` vs `authz` by message | same | `access_denied_maps_to_authz_or_the_console_fix` |
| Throttling | `ok: true`, `throttled_ok` | same | `other_service_codes_map_as_in_the_script` |
| Mantle statuses | 401/403, 404 (family split remedy), 429 | same texts | `mantle_errors_...`, `mantle_404_remedy_is_family_split` |
| Runtime OpenAI statuses | 401/403/404/429 mapped to botocore codes | same | `runtime_openai_statuses_use_the_botocore_codes` |
| Opus 5.5 outside `eu-west-2` | `inference_profile_required`, nothing sent | same | `bare_opus_5_5_outside_eu_west_2_...` |
| Quota RPM | per-minute request quotas sharing a name token; lowest wins | same | `quota_rpm_...` |
| Astra notes | mantle and runtime quota notes | same | `astra_dispatch_preserves_the_id_and_the_endpoint` |
| Failure summary | first failing `reason`/`detail`, all `failing_models` | same | `the_report_has_the_scripts_member_order_and_failure_summary` |
| Empty `--models` | `no_models`, exit 1 | same | `an_empty_model_list_is_a_failure_not_a_vacuous_pass` |
| API path default | mantle for all-mantle ids, `converse` for runtime, explicit plan kept, mixed/empty is an error | same | `dispatch_and_resume_share_the_api_default` |
| Baseline request shapes | OpenAI `max_completion_tokens`, Anthropic top-level `system`, Gemini header key | same | `the_openai_shape_...`, `the_gemini_key_is_a_header_...` |
| Key hygiene | only to its official host, never in a URL or the output | same | `every_provider_url_is_an_official_host_...` |
| Partial resume | live rows kept, failed rows retried | same | `live_rows_are_kept_and_failed_rows_are_retried` |
| Error detail | message/param, 401/403 empty, raw body capped at 200 characters | same, the body redacted before the cut | `error_detail_extracts_...`, `a_key_crossing_the_200_character_cut_leaves_no_prefix` |

## #4608 coverage

The contract tests of `test_astra_execution_contract.py` that import `iam_policy` and
`preflight_bedrock` and belong to this slice (`image_input` and `bedrock_pricing`
halves are #4403 and stay there):

| Python test | Rust test |
| --- | --- |
| `test_dispatch_and_resume_share_api_default` | `dispatch_and_resume_share_the_api_default` |
| `test_mixed_or_empty_legacy_targets_require_resolution` | `mixed_or_empty_legacy_targets_require_resolution` |
| `test_explicit_runtime_api_survives_normalization` | `explicit_runtime_api_survives_normalization` |
| `test_api_handoff_paths_and_real_evaluator_classifier_are_consistent` (the `is_mantle_model` asserts) | `api_handoff_classifier_is_consistent` |
| `test_astra_system_profile_arn_keeps_invocation_price_and_permissions` (`iam_policy.generate_policy` half) | `system_profile_arn_keeps_its_arn_and_base_model_and_default_project` |
| same test, `preflight_bedrock.main` half | `a_system_profile_arn_is_probed_by_its_arn_with_the_astra_quota_note` |
| `test_runtime_preflight_uses_selected_api` | `the_runtime_api_selects_the_openai_probe_for_runtime_gpt_ids_only` |
| `test_runtime_openai_client_uses_real_sdk_wire_contract` (URL, auth-free body, model id) | `the_runtime_openai_probe_keeps_the_wire_contract` |
| `test_astra_preflight_dispatch_preserves_id_and_endpoint` (`test_preflight_bedrock.py`) | `astra_dispatch_preserves_the_id_and_the_endpoint` |

The documentation-text half of that file is #4405 (`bedrock_report/contracts.rs`).
The one assertion that cannot be ported literally is
`"from preflight_bedrock import is_mantle_model" in EVALUATOR.read_text()`: it tests
a Python import in a document. When the evaluator document stops importing the
script (see Cutover), that assertion becomes a check that the document names
`toolkit bedrock-probes classify`.

## Cutover for the callers

Nothing below is done in this slice; each step is a content change for the
skill-content pass, after which #4415 retires the Python.

1. `skills/llm-to-bedrock/SKILL.md` lines that run `uv run ... python
   $SCRIPTS/preflight_bedrock.py --region R --models ...` become `toolkit
   bedrock-probes preflight --region R --models ... [--runtime-api API]
   --recorded FILE` while no live client exists (follow-up below), and the
   `from preflight_bedrock import normalize_api_path` block becomes
   `toolkit bedrock-probes api-path --models ... [--plan-path P] [--runtime-api A]`
   (read `api_path` from the JSON).
2. `agents/llm2bedrock-report-generator.md` (`iam_policy.py --models --region
   --account-id --output`) becomes `toolkit bedrock-probes iam-policy` with the same
   flags; the account id must be 12 digits and no id may hold a wildcard.
3. `agents/llm2bedrock-prompt-evaluator.md` (`preflight_bedrock.is_mantle_model`,
   `create_openai_client`): the classifier becomes `toolkit bedrock-probes classify`;
   the evaluator's `create_openai_client` snippets need the live OpenAI client and
   token signer follow-up before they can leave Python.
4. `references/helpers/run-source-model-baseline/run-source-model-baseline.md`
   (`source_baseline.py`) becomes `toolkit bedrock-probes source-baseline ENV_FILE`
   with the same environment variables (or the flags).
5. After the callers move, update `test_astra_execution_contract` and the retired
   scripts' tests per #4415 and delete the Python files named above.

## Follow-ups

* Live clients for `preflight` (`bedrock-runtime` Converse and `InvokeModel`,
  `service-quotas`, the mantle and runtime OpenAI endpoints) and for
  `source-baseline` (the three provider endpoints, 60 s timeout), behind the
  `toolkit-aws` transport and signer seams.
* The Bedrock API-key (bearer) token signer (`aws_bedrock_token_generator.provide_token`):
  a pure SigV4 presign with an injected clock and credentials and a fixed-vector
  test, then wired into the mantle and runtime OpenAI probes.
