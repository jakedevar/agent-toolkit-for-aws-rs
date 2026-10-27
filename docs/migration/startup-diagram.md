# Startup diagram composer and model-path verifier (#4402)

Slice `startup-diagram`, wave 2 of programme #4369. Base: the integration head
`7dd92dc7b51978a7d839be42f7e620fa59603c7e`, which holds the startup validators
(#4399), scoring (#4400) and `bedrock-report` (#4405). The code extends
`crates/toolkit-cli/src/startup` (`startup/diagram.rs`, `startup/model_path/`) and
reuses `safe_text`, `spec::engine` (schema evaluation), `toolkit_aws::redact_text`,
`manifests::read_failure` and the scoring test fixtures; no second router, schema
engine, redactor or escaping helper was added.

Python source (read-only migration evidence, **not** deleted or re-wired here):
`plugins/aws-startup-advisor/skills/agent-advisor/scripts/build_diagram.py`,
`verify_model_path.py`, `test_build_diagram.py`, `test_verify_model_path.py` and the
`build_diagram` assertions of `test_unit_grouping.py` / `test_collapse_invariant.py`
(#4613, taken by this slice). Their callers (`references/diagram/build-diagram.md`,
`phases/model-recommend/model-recommend.md`, `phases/poc/poc.md`, `generate.md`) still
run the Python; retiring the files and flipping those prose callers is #4415.

## Commands

| Rust | Python original | Result |
| --- | --- | --- |
| `toolkit startup build-diagram RESULT CONFIRM [DESIGN]` | `build_diagram.py RESULT CONFIRM [DESIGN]` | writes `diagram.md` beside RESULT, prints `RESULT=ok RUNTIME=<runtime>` (`multi-unit` with a non-empty design) |
| `toolkit startup verify-model-path RECOMMENDATION [--output FILE] [--workload ID[,ID...]]` | `verify_model_path.py RECOMMENDATION [--output FILE] [--workload ID]...` | writes `model-verification.json` (default beside RECOMMENDATION), prints `RESULT=ok\|failed WORKLOADS=<n>`, exit 2 when a workload is `failed` or `needs_resolution` |

Both entries use the canonical router (`value_flags`, `strict_values: false`,
`passthrough: false`); exit statuses go through `ToolkitError` (usage 2, runtime 1) and
`Output::set_exit_status` (the verifier's published 2).

## Acceptance criteria

- **AC1 (output files and path checks offline).** `diagram.md` and
  `model-verification.json` are written by `write_regular_file`; every read is
  `read_text` (bounded, UTF-8, no symlink). Tests use temp directories and an injected
  `ProbeClient`; nothing touches a network or credentials. Tests:
  `the_command_writes_diagram_md_beside_the_result`, `linked_inputs_and_outputs_are_refused`
  (both modules), `the_command_stores_a_no_live_client_failure_and_exits_two`.
- **AC2 (optional renderers explicit).** The composer writes Mermaid and ASCII *text* and
  runs no renderer. The optional probe clients the Python built lazily (`boto3`,
  `anthropic`, `openai`, the token generator) are one explicit seam, `ProbeClient`,
  called only for a `recommended` workload with a resolved id; this build wires
  `NoLiveClient`, which sends nothing. Tests: `an_unresolved_runtime_id_creates_no_client_call`,
  `a_decision_required_workload_is_not_probed`, `the_default_client_sends_nothing_and_says_so`.
- **AC3 (token/endpoint errors redacted).** A probe error's type and message pass
  `toolkit_aws::redact_text` (URLs with keys, `Bearer` tokens, `token=` labels), then
  control/bidirectional characters are escaped and the message is bounded to 512 characters
  before it is stored. Schema findings are redacted the same way and bounded to 400
  characters per line. Test: `probe_errors_are_redacted_escaped_and_bounded_before_they_are_stored`.
- **AC4 (no Python diagram runtime).** No Python was added; the Python files stay as
  evidence until #4415.

## Parity

| Behaviour | Python | Rust | Test |
| --- | --- | --- | --- |
| Runtime resolution | `resolve_runtime` | same (`co_recommend` choice, first entry, `no_viable_runtime` → `none`) | `resolve_runtime_*` |
| Services | confirm first, unknown ids dropped, de-duplicated | same | `resolve_services_*` |
| Single-unit diagram from the design unit, with an authoritative empty `agentcore_services` | `build_diagram` | same; duplicates in a unit's own list are kept like the script | `a_single_unit_design_renders_from_the_unit`, `an_empty_unit_service_list_is_authoritative`, `a_single_unit_keeps_duplicate_services_like_the_script` |
| Collapse invariant (one-unit design = legacy diagram) | `test_diagram_identical_for_single_unit_design` | same | `a_single_unit_design_collapses_to_the_legacy_diagram` |
| Handoff set | `HANDOFF_RUNTIMES == {ecs, eks, fargate, batch}` | same | `handoff_runtimes_are_exactly_ecs_eks_fargate_and_batch`, `units_needing_handoff_cover_exactly_the_four_runtimes` |
| Model-less units render no Bedrock node | source check `if model and model != "unknown"` | behaviour test (mermaid and ASCII, `null`, `""`, `unknown`) | `model_less_units_render_no_bedrock_wiring` |
| Multi-unit and Temporal topologies, queue/gateway mix, per-fleet queue edges | `render_multi_unit_*` | same text byte for byte | `multi_unit_*`, `queue_*`, `gateway_*`, `*temporal*`, `each_fleet_connects_only_*` |
| Exact Mermaid/ASCII of a full AgentCore diagram | – | pinned | `mermaid_exact_text_of_a_full_agentcore_diagram` |
| Probe construction (five API paths) | `_probe_*` | same requests; Python keyword arguments become one JSON request | `the_mantle_probe_*`, `the_converse_probe_*`, `the_invoke_probe_*`, `the_openai_*` |
| No substitution, structured failure | `verify_workload` | same | `a_probe_failure_is_structured`, `openai_failures_never_substitute_a_model` |
| `needs_resolution` without a client call | same | same | `an_unresolved_runtime_id_creates_no_client_call` |
| Application inference profile checks (active, selected model, in-region allowance, allowed references, lookups before inference) | `_verify_application_profile` | same, including lookup order | `application_profile_*`, `a_wrong_model_*`, `the_london_*`, `an_unreadable_reference_*` |
| Output schema | `jsonschema` with `FormatChecker` | `model-verification` schema through `spec::engine` | `the_verification_output_matches_its_schema` |
| Exit codes | 0 / 2 | same | `the_command_exits_zero_when_nothing_failed_*` |

Python tests that call `model_recommendation.recommend()` use hand-built, schema-checked
recommendation fixtures here. The recommendation engine itself (`system_profile_arn_mismatches_*`,
`recommendation_cli_*`, London recommendation selection) belongs to the model-recommendation slice, not to
the verifier.

## Intentional changes

1. **Command names.** `startup build-diagram` and `startup verify-model-path` replace the scripts.
2. **No tracebacks.** A document of the wrong shape (`units` not a list, a unit without a text `id`, a
   non-object result...) is a one-line `error:` (exit 1) naming the field and JSON type, never the value.
   A recommendation that breaks its schema prints up to 20 findings (path with escaped keys, rule)
   and a count; Python raised `ValidationError`.
3. **Lenient `null`s.** `platform`, `temporal` and a result's `model_recommendation` set to `null` count as
   absent (Python crashed); non-text entries of a service or queue list name no service/queue.
4. **Escaped diagram text.** Unit ids, models, runtimes, deployment models and task queues are escaped
   (`\x1b`, `‮`, ...) before they reach `diagram.md`, so hostile names cannot add lines. Everything
   else that reaches Mermaid goes through the one encoder of item 13 (#4679).
5. **Stdout.** `RUNTIME=` is redacted and escaped (bounded to 120 characters).
6. **Symlinks.** RESULT, CONFIRM, DESIGN, RECOMMENDATION are read without following links, and a link
   at `diagram.md` / the verification output is never written through. An existing CONFIRM or DESIGN
   that is a link, directory or unreadable is an error; only a missing one is "absent".
7. **`--workload`.** The router keeps the last value of a repeated flag, so `--workload` takes
   `ID[,ID...]` (a value that names a workload is used whole, so ids with commas stay selectable).
   An unknown id is reported by count with the recommendation's ids listed escaped (Python listed the
   unknown ids). Follow-up: #4670.
8. **Probe errors.** Redacted, escaped, bounded (AC3); the exception type is escaped too.
9. **Invoke body.** The `InvokeModel` request carries `body` as compact JSON text (Python passed bytes of
   `json.dumps`, which only differs in whitespace).
10. **No live client.** `NoLiveClient` fails every operation with `RuntimeError: ... requires a live
    Bedrock client, which this build does not include`, so the default command stores `failed` probes. The
    SDK-backed client is #4669.
11. **`response_model_id`.** A non-text `model` / `modelId` in a response is stored as `null`; the
    `date-time` format is not re-checked because the tool writes the timestamps itself.
12. **Duplicate JSON keys** are rejected by the shared parser (Python kept the last).

13. **Mermaid-safe encoding (#4679).** Python wrote unit ids, models, runtimes, deployment models and task
    queues into the flowchart with at most hyphen replacement, so a value such as
    `q| injected[Injected] -->|q` added a node and an edge. Every caller-derived string now passes
    one encoder in `startup/diagram.rs`; fixed text (runtime and service display names, `User / Client`,
    `Queue`, ...) is written as before.
    - **Labels** (node text, subgraph titles, edge labels, queue names, models, runtimes, deployment
      models): control, bidirectional and invisible characters become visible `\x..` / `\u....` text
      first; then every ASCII character other than letters, digits, space and `_ . , : / - + \` is a
      Mermaid entity (`"` is `#quot;`, everything else `#<decimal>;`, so `|` is `#124;`, `[` is `#91;`,
      `;` is `#59;`, `<` is `#60;`, `#` is `#35;`, `%` is `#37;`). Non-ASCII text that is neither control
      nor invisible stays. A label therefore cannot close its quotes, open a bracket, start a pipe or
      comment, or carry HTML, whether it is in `["..."]` or in an unquoted `-->|edge label|`. A Mermaid
      viewer shows the entity as the character, so the label reads the same.
    - **Node ids** (one per unit, plus the inner `<id>_node` node): an id of the form
      `[A-Za-z][A-Za-z0-9_-]*` keeps its readable form with hyphens as underscores (`chat-agent` is
      `chat_agent`, as before) unless it is a Mermaid word (`end`, `subgraph`, `class`, ...), one of the
      ids the composer writes itself (`user`, `rt`, `model`, `svcs`, `handoff`, `queue`, `gateway`,
      `temporal_cloud`, `n`), or would equal another unit's id or `<id>_node` id. Every other id
      (spaces, brackets, quotes, pipes, semicolons, a leading digit, non-ASCII, empty) becomes
      `u_<8 hex digits of its FNV-1a digest>` (`_2`, `_3`, ... on a collision). The real id stays visible
      in the subgraph title. The mapping depends only on the ordered unit ids, so output stays
      byte-identical for the same input. This is the only change to ordinary diagrams: a unit named
      like a reserved word, or a model/queue containing one of the encoded characters (a model with
      parentheses, `=`, `@`), renders differently from the Python.
    - **ASCII** output is not Mermaid and keeps only the control-character escaping.
    - Tests: `a_queue_cannot_add_a_node_or_an_edge`, `a_unit_id_cannot_become_graph_syntax`,
      `node_ids_are_readable_when_safe_and_deterministic_when_not`, `mermaid_text_keeps_names_and_encodes_syntax`
      and `every_caller_field_that_reaches_the_diagram_is_encoded`, which renders fourteen hostile values
      through every caller field (legacy, single-unit, multi-unit queue/gateway, Temporal) and checks that
      each line is one of the statement shapes the composer writes.

## Verification

`cargo fmt`, `clippy` and `workspace-test` recipe jobs and one `scoped_test` on the committed source; job
ids are in the handoff on #4402. Hostile-input tests (ESC, BEL, CR, LF, U+202E, a secret-looking marker)
cover every diagnostic and report path, stdout included: `hostile_*` and
`schema_findings_escape_hostile_keys_and_bound_every_line` and `malformed_input_errors_never_echo_values`.
