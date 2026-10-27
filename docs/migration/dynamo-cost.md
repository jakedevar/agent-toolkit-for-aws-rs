# Slice `dynamo-cost` (Issue #4389): DynamoDB cost calculation and design iteration

Rust replacement for two legacy scripts under
`skills/specialized-skills/database-skills/amazon-dynamodb/scripts/`
(read-only migration evidence; neither is deleted or edited here, and callers
are retired by a later slice after this one is verified):

| Python source | Rust command | Code |
| --- | --- | --- |
| `calculate_costs.py` | `toolkit dynamodb calculate-costs` | `crates/toolkit-cli/src/dynamodb/cost.rs`, `mod.rs` |
| `iterate_design.py` | `toolkit dynamodb iterate-design` | `crates/toolkit-cli/src/dynamodb/iterate.rs` |
| `repr` / `round` / `format(",")` / `json.dumps` renderings | (library) | `crates/toolkit-cli/src/dynamodb/pyfmt.rs` |

Shared contracts: the value options (`value_flags` + `strict_values: true`,
`Invocation::value`) and stderr warnings (`Output::diagnostic`) are the
canonical router support from #4376/#4377; this slice's earlier parallel
`ValueFlag`/`Invocation::option`/queued-diagnostic helpers were removed in the
merge of that head. The only additions kept, all strictly additive and unused by
the other commands, are `ToolkitError::with_exit_code` (published exit statuses
4 and the pipeline step's own status have no canonical equivalent),
`toolkit_core::write_regular_file` (symlink-refusing writer; the canonical
tree-sync writer copies whole trees and cannot write one report) and
`toolkit_core::sha256_hex` (schema fingerprints). No dependency was added.

There are no Python tests for this slice (2 files in the inventory). Numeric
parity cases come from the formulas and the worked examples in the Python
docstrings; every expectation in `cost_tests.rs` and `iterate_tests.rs` is
derived by hand and commented with its arithmetic.

## Safety

- No AWS call, pricing API or credential is used anywhere in this slice. The
  calculator is a pure function over the model JSON.
- `iterate-design` evaluates the same gates as the Python script before any
  step: `--yes-deploy` (exit 4 without it), `--skip-deploy` (exit 2 when a
  redeploy is needed) and forwards `--allow-spend` to the benchmark step.
- The deploy, benchmark and performance-report steps belong to the
  `dynamo-deploy` (#4391) and `dynamo-bench` (#4390) slices. `iterate-design`
  runs every pipeline step as `toolkit dynamodb <step>` through the shared
  command table: `calculate-costs` (this slice), and `deploy-model`,
  `benchmark-model`, `generate-perf-report` (expected names, with the Python
  flags forwarded unchanged: `--model --config --manifest-out --yes-deploy`;
  `--model --config --manifest --raw-out --summary-out [--allow-spend]`;
  `--model --summary --output --findings-out`). Until those slices register
  them, a round that needs one stops with the runtime error
  `dynamodb_step_unavailable` before any cloud action. The calculator-only
  path is complete today.

## Parity table (Python behavior -> Rust)

| Area | Python behavior kept | Rust location / evidence |
| --- | --- | --- |
| WRU | `ceil(size/1024)`, minimum 1; size <= 0 costs 1 | `wru_for_item`; boundaries 0/1/1024/1025 |
| RRU | `ceil(size/4096)` min 1, eventual = half, missing item still 0.5/1 | `rru_for_item`; 0/1024/4096/4097 |
| Query / Scan | total bytes rounded once to 4 KB, then halved | `rru_for_query`; 5x1 KB = 1.0 vs per-item 2.5 |
| Batch read / write | rounded per item, then summed; explicit `item_sizes` beat `items_per_request` | `rru_for_batch_read`, `wru_for_batch_write` |
| Transactions | reads: strong x2; writes: base table x2, GSI writes stay 1x | `calc_pattern_capacity`; (2+2) x 2 = 8 WRU |
| Put / Update / Delete | sized on the larger of before/after (`previous_item_size_bytes`) | put 500 B / prev 3000 B = 3 WRU |
| Conditional failures | `1 + conditional_fail_rate` multiplier on writes only, plus note text | `pattern_monthly_cost`; 16.2 -> 24.3 |
| Page cap | 900 KB page, projected attribute sizes (S=100, N=8, ... +10 B name), first declaring entity wins, unknown = 100 B | `cap_items_per_request`; 128 B -> 7200, 110 B -> 8378 |
| GSI amplification | membership by GSI PK (+SK), table keys count, per-projection write ratio (ALL 1.0, INCLUDE 0.3, KEYS_ONLY 0.1, `int()` truncation), UpdateItem key move = 2 writes, projection gate, indeterminate membership charged and flagged, multi-entity upper bound | `calc_gsi_amplification_wru` + 8 scenario tests |
| Prices | WRU $0.625/M, RRU $0.125/M, 2,592,000 s/month, op order `(rcu*P + wcu*P) * rps * s` | constants + 16.20 / 32.40 / 48.60 fixtures |
| avg_rps | defaults to peak; second "Expected" headline only when some pattern has the key | `average_volume_*` tests |
| Storage | `(bytes + 100)` per item (+48 global tables), decimal GB, $0.25/GB-month, no free tier; write-driven bytes beat entity counts; GSI share = projection ratio x member-entity byte fraction | `storage_cost` tests (1.12 GB/$0.28 etc.) |
| Write action / retention | pattern > requirements > operation default; retention pattern (`int`) > entity > metadata > 30 | `resolve_*` tests |
| Vector items | KEYS_ONLY `dims*4+35`; INCLUDE + declared attrs (100 B default); ALL + mean entity size + other vectors at 5.5 B/number | `vector_item_bytes` (4131 / 4331 / 5131 / 5835) |
| Vector writes | 1 KB floor once per request, `attributes_written` gate, IA = x1.25 (`INFREQUENT`/`IA`), $0.52/GB | `vector_write_bytes_per_call` |
| Vector search | never priced (cell `not priced²`, footnote, driver table), spend-gate bound x1.5 | gate reproduces the Python docstring table (2,608,645 / 57,564 / 45,145 / 31,929 / 4,108 / 60,145) |
| Vector validation | all messages, `sorted(...)` key lists, `repr` of bad values, unnamed index never matches a missing `index` | `validate_vector_model` tests |
| Report | exact headline, disclaimer, padded Markdown tables (character widths), 30% concentration notes (`{:.0f}` rounding), GSI rows/footnote, vector section; no trailing newline | golden-line tests in `cost_tests.rs` and `tests/dynamodb.rs` |
| CLI output | `Cost report written to <path>` and `Analyzed N access patterns across M tables.` on stdout; default `cost_report.md` beside the model | `tests/dynamodb.rs` |
| Fingerprint | sha256 of `json.dumps(sig, sort_keys=True)` (Python separators, ensure_ascii), first 12 hex; tables/GSIs sorted by name, attributes sorted, type upper-cased; volume/size/consistency excluded | `schema_fingerprint` tests (blob hand-derived; SHA-256 against FIPS vectors); U+007F in a key name stays `\u007f` (ensure_ascii escapes everything outside U+0020..U+007E), regression digest `2212db5492d3` from `sha256sum` of the literal legacy bytes (#4543) |
| Reuse-vs-redeploy | calculator-only > schema changed > manifest+same > manifest+no prior > deploy | round tests |
| Change files | `merge` deep-merge (null removes), `ops` add/replace/remove, list `-`, negative indices, insert clamping | `apply_change` tests |
| Mode presets | quick / representative values and "explicit config wins" | `resolve_config` tests |
| Loop state | same keys and order (`loop_id`, `model_path`, `created_at`, `current_schema_fingerprint`, `active_manifest`, `rounds[...]`), deltas, banner and summary lines | `calculator_only_round_*`, `a_reuse_round_*` |
| Exit codes | calculator 0/1; usage 2; iterate `_die` 2; deploy without `--yes-deploy` 4; failed step -> the step's own status | `tests/dynamodb.rs`, `iterate_tests.rs` |

## Intentional changes (recorded per the worker contract)

1. **Command shape.** `python3 calculate_costs.py ...` / `python3
   iterate_design.py ...` become `toolkit dynamodb calculate-costs ...` /
   `toolkit dynamodb iterate-design ...` with the same option names (`--model`,
   `--requirements`, `--output`; `--model --config --loop-state
   --manifest --apply-change --mode --timestamp --yes-deploy --allow-spend
   --skip-deploy --calculator-only --dry-run`). Options also accept
   `--name=value`. argparse prefix abbreviations (`--req`) and the `-r`/`-o` short forms are not
   accepted (the canonical router has no short value flags), and an option value
   may not be empty or start with `-`. The step lines printed by
   `iterate-design` show the `toolkit ...` command instead of
   `python3 <script path> ...`; the calculator runs in-process and prints the
   same two lines the child script printed.
2. **Error text.** Messages follow the shared `error: <message>` contract on
   stderr instead of `Error loading model: ...` / `Error: ...` / `ERROR: ...`
   (the text after the prefix is kept, e.g. `error: No access patterns found in
   the data model.`, `error: the vector portion of this data model cannot be
   priced as written.` followed by the same bullet list and fix hint). The
   requirements warning keeps its text and stays on stderr
   (`Warning: Could not load requirements (...), using defaults.`) and is now
   also emitted for any unreadable requirements file, not only JSON/missing
   errors.
3. **Python tracebacks become model errors.** Wrong-typed or missing required
   fields (a string `peak_rps`, a pattern without `operation`/`pattern_id`, a
   table without `table_name`, a non-list `tables`) report
   `error: <what is wrong>` with exit 1, where Python raised `TypeError` /
   `KeyError` (exit 1 with a traceback). The lenient `int()` coercions that the
   Python source relied on are kept (numeric strings and floats are truncated
   for `dimensions`, `top_k`, `retention_days`, counts; booleans are refused
   where `_as_int` refused them).
4. **File safety (shared #4374 path contract).** Inputs are read with
   `read_regular_file` (symlinks and non-regular files refused, UTF-8 only, at
   most 16 MiB; no BOM); outputs are written with `write_regular_file` (refuses
   a symlink or non-regular target, so `cost_report.md` and the loop-state /
   model rewrites cannot be redirected). Duplicate object keys in the input
   JSON are an error (Python kept the last).
5. **JSON files written by `iterate-design`** (`loop_state.json`, the model
   after `--apply-change`, the config overlays) use the shared two-space
   writer: non-ASCII text is written as UTF-8 rather than `\uXXXX` and float
   exponents are Rust-style; no trailing newline, like `write_text(json.dumps)`.
   The fingerprint blob and the printed `delta vs round N` JSON use the
   Python-exact serializer (`pyfmt::dumps`) so the fingerprint is stable
   across the language change.
6. **Deterministic delta ordering.** `throttle_delta` and `p99_delta_ms` list
   patterns in sorted order; Python iterated a hash-randomized `set`.
7. **Generated values.** `loop_id` is 8 random hex digits from the standard
   library's randomized hasher (Python used `uuid4().hex[:8]`); timestamps come
   from the system clock in the same `YYYY-MM-DDTHH:MM:SSZ` form.
8. **Mode presets** (`quick`, `representative`) are copied from
   `benchmark_model.py` (`_PRESETS`) because that script is owned by slice
   #4390. Whoever ports the benchmark should make this table the single shared
   source; until then the two must be kept equal.
9. **Pipeline steps** other than the calculator are dispatched through the
   command table and are unavailable until their slices land (see Safety).

## Verification

Recipe jobs (`fmt`, `clippy`, `workspace-test`) are diagnostic only. Trusted
candidate evidence is the committed-source `scoped_test` owned by the
manager/harness (#4425).
