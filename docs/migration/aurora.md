# Aurora ACU, commitment and I/O analyzers (#4388)

Slice `aurora`, wave 2 of programme #4369. Built on the #4374 core/CLI contracts
and the #4378 integration head. Verification evidence lives in the Issue #4388
handoff, not in this file.

The six Python scripts (`acu_calculator.py`, `commitment_pricing_analyzer.py`,
`io_optimized_analyzer.py` for Aurora MySQL and for Aurora PostgreSQL) are
replaced by one Rust calculation core under `crates/toolkit-cli/src/aurora/`.
The MySQL and PostgreSQL copies differ only in the Pricing API engine name
(`Aurora MySQL` / `Aurora PostgreSQL`), the RDS engine (`aurora-mysql` /
`aurora-postgresql`) and three messages (empty-cluster reasons and the
fleet-table skip cell), so the engine is the required `--engine` option:

| Command | Replaces |
| --- | --- |
| `toolkit aurora acu estimate\|list-instances\|pricing-source --engine E` | `acu_calculator.py` `estimate`, `list-instances`, `pricing-source` |
| `toolkit aurora commitment [offline] --engine E` | `commitment_pricing_analyzer.py` |
| `toolkit aurora io [offline] --engine E` | `io_optimized_analyzer.py` |

The Python files are untouched (read-only evidence). Their callers are shipped
skill documents that tell the model to run `python .../scripts/*.py`
(`amazon-aurora-mysql/SKILL.md` and `references/*`, the same for
`amazon-aurora-postgresql`); rewriting that guidance and deleting the scripts is
the retirement slice's job once this command is landed and verified. There is no
Python unit test for these scripts; the Rust tests below are new.

## Layout

- `aurora/pyfmt.rs`: Python-compatible numbers, rounding, `:.Nf`, `:,.Nf` and
  `json.dumps(indent=2)`.
- `aurora/args.rs`: `argparse`-style decoding and messages.
- `aurora/api.rs`: the `AwsApi` transport trait, `NoAws`, and the parsers for
  Pricing API items, bulk CSV text, reserved offerings and Savings Plans rates.
- `aurora/acu.rs`, `commitment.rs`, `io.rs`: the three analyzers.
- `aurora/tests.rs`: offline tests (mock transport, canned payloads).

Routing uses the shared router: three `acu` subcommands plus `aurora acu`
(help), and `aurora commitment` / `aurora io` with an optional `offline`
positional. No parallel router, value-option or stderr helper was added.

## Mock pricing APIs (AC3)

Every remote read is a method of `AwsApi` that returns raw, already-paginated
payloads (`PriceList` JSON strings, CSV text, RDS / Savings Plans / EC2 /
CloudWatch records). All filtering, parsing and arithmetic on those payloads is
Rust in this crate, so the tests feed canned payloads through a mock transport
and assert on the filters sent (engine name, location, deployment option) and on
the results. No test touches AWS or the network.

This build ships `NoAws` only: `available()` is false and every call fails with
`AWS SDK access is not available in this build`. That reproduces what the Python
scripts did when `boto3` was missing, which they already handled:

- `acu`: refresh falls back to the built-in us-east-1 data and lists the errors
  in `pricing_source`; `--offline` skips the attempt.
- `commitment`: on-demand price from the static table, no RI offerings, no
  Savings Plan rates.
- `io`: static prices with a `note` in `pricing_source`.
- live modes (`--cluster`, `--all`): usage error, exit 2, nothing on stdout.
  `offline` always works.

An SDK-backed `AwsApi` (optional Cargo feature, so default builds stay light) is a
follow-up; the analyzers need no change for it.

## Parity

Numeric parity cases are derived from the Python formulas and asserted in
`aurora/tests.rs`.

| Behaviour | Python | Rust | Test |
| --- | --- | --- | --- |
| ACU rounding `ceil(x*2)/2` | `math.ceil` | `round_up_to_half`; `-0.3` gives `0.0`, not `-0.0` | `acu_rounding_boundaries` |
| Typical ACU, `db.r6g.xlarge`, P95 35, max 72 | 6.0 / peak 12.0 / avg 3.5 / max 16.0 | same | `estimate_matches_the_python_formulas_for_the_documented_example` |
| Serverless vs provisioned, same case | $575.60 vs $428.87, `-34.2`, `not_recommended` | same | same |
| Connection floor 1000 conn, working set 12 GiB | 5.0 / 6.0 and the advisory text | same | `estimate_connection_and_memory_floors_raise_the_minimum` |
| Cap at 256 ACU | `min(x, 256)` returns the **int** 256 | `Num::Int` kept, JSON `256` not `256.0` | `estimate_above_the_serverless_ceiling_keeps_integer_caps` |
| Exactly 256 raw ACU | not over capacity, float | same | `estimate_exactly_at_the_ceiling_is_not_over_capacity` |
| `savings_pct` with no provisioned cost | int `0` (`else 0`) | `Num::Int(0)` | `costs_use_an_integer_zero_when_there_is_no_provisioned_cost` |
| Recommendation thresholds | `>10`, `>-5`, `>-30` | same, with boundary cases | `recommendation_thresholds_have_exact_boundaries` |
| Defaults echoed as ints | `cpu_avg: 0`, `storage_gib: 10` | same; explicit `0` is `0.0` | `estimate_json_echoes_python_int_and_float_inputs` |
| `round(x, n)` | exact value, ties to even | same via exact formatting | `round_matches_python_half_even_on_the_exact_binary_value` |
| `f"{x:.0f}"`, `:,.0f` | ties to even; `nan`/`inf` | same | `fixed_point_formatting_matches_python` |
| `json.dumps(indent=2)` | `ensure_ascii`, `1e+16`, `NaN`, `Infinity` | `pyfmt::dumps` | `json_dumps_matches_python_for_floats_ints_and_text` |
| RI term split | duration `> 94_000_000` s is 3 years | same, exclusive boundary | `the_duration_boundary_between_one_and_three_years_is_exclusive` |
| RI effective rate | `fixed/term_hours + recurring`, 6 places | same | `offerings_are_parsed_deduplicated_and_the_cheapest_term_wins` |
| I/O-Optimized RI draw-down | RI rate x 1.30 | same | `io_optimized_clusters_pay_the_premium_on_demand_and_on_ri_units` |
| DSP key from usage type | `InstanceUsage:db.(\w+).(\w+)`, size map | hand-written equivalent | `savings_plan_rates_are_keyed_by_normalized_class` |
| Unknown class on demand | price `0.0`, "on_demand" | same | `unknown_and_ineligible_classes_have_no_price_and_a_family_note` |
| Serverless DSP | `0.12 x ACU x 730`, DSP `rate x ACU x 730` | same operand order | `serverless_comparison_prices_the_average_acu` |
| I/O vs Standard, 1000 compute / 800 GiB / 1200M | standard $1320 vs $1480, "18%", `standard` | same | `comparison_recommends_standard_when_the_premium_outweighs_io` |
| Break-even | decided by dollars, not the 25% rule | same | `comparison_follows_dollar_savings_not_the_twenty_five_percent_rule` |
| Ties in `:.0f` | `$837.5` prints `$838` | same | same |
| `io_cost_pct` with nothing to price | int `0` | `Num::Int(0)` | `comparison_with_nothing_to_price_has_an_integer_zero_percentage` |
| CloudWatch window | now floored to the hour, `days` back, 730/observed hours | same, injectable clock | `live_io_analysis_extrapolates_cloudwatch_totals` |
| Fleet sums | `sum()` of empty is int `0` | `Num` fold | `fleet_summary_excludes_errors_and_skips_from_totals` |
| Bulk CSV | `DictReader` after the `SKU` header | hand-written equivalent | `bulk_csv_parsing_matches_dictreader` |
| Per-tool region maps | three different maps | one table with flags | `region_maps_differ_per_tool_exactly_as_the_scripts_do` |
| Table layouts | f-string widths | same widths and wording | `estimate_table_matches_the_python_layout`, `commitment_offline_commands`, `io_fleet_table_labels_skipped_clusters_per_engine` |

Boundary and negative coverage: zero or missing prices (static `0.0`, unparsed
or non-positive API prices ignored), zero instances, zero/negative lookback,
unknown regions, invalid numbers, missing required options, mutually exclusive
modes, non-finite inputs, 128-bit connection arithmetic and a failing API.

## Safety of user-supplied magnitudes

- Connection counts are `i64`; `connections * 10` is computed in `i128`, so no
  count overflows (`--connections 9223372036854775807` works; larger is an
  invalid `int` value).
- `--days` is checked against Python's `timedelta` limit (`999999999`) and the
  epoch range of `datetime`, with checked multiplication; the observed I/O total
  must fit `i64`.
- A float that Python's `math.ceil` rejects (`nan`, `inf`, or an overflow while
  doubling) is a runtime error (exit 1) instead of a panic or a silent value.
- Nothing here writes a file, so no output path helper is involved.

## Intentional changes

1. **Engine option.** `--engine mysql|postgresql` replaces the choice of script.
   It is required (no default), because the two scripts live in different skill
   directories and a silent default would price the wrong engine.
2. **Command shape.** Python's shared `--region` / `--offline` could appear
   before or after the subcommand and `--region` before `offline` was reset by
   the subparser default in `commitment` (a script bug that `io` already fixed).
   Here every flag comes after the command path (`toolkit aurora acu estimate
   --region X`), there is one value per flag (the last wins), and the
   `commitment` clobbering bug is gone. Main-level `--cluster` / `--all` given
   with `offline` are ignored, as in Python.
3. **Live AWS access.** See "Mock pricing APIs": the Python reached AWS through
   `boto3` and `urllib`; this build has no transport (follow-up Issue). The
   bulk-CSV fallback needed no credentials but used the network, so it is also
   unavailable. Messages that named `boto3` ("boto3 required for live AWS
   analysis", "boto3 not installed") say "AWS SDK access is not available in this
   build" / "AWS SDK not available". Exit status 2 is unchanged.
4. **Error reporting.** `ERROR: ...` lines on stderr with exit 2 (argparse and the
   explicit `--serverless requires --avg-acu > 0` checks) are `error: ...` with
   the usage text and exit 2, through the shared usage-error path. Python
   tracebacks (unknown instance type in `acu`, `nan`/`inf` CPU) are
   `error: <message>` with exit 1. No print-then-exit helper was needed.
5. **`io offline` with an unknown class** still prints `{"error": ..., "pricing_source":
   ...}` and exits 0 in JSON. In table format the script raised `KeyError`
   after printing a partial header; Rust prints nothing and reports the error
   (exit 1).
6. **No subcommand.** `toolkit aurora acu` prints the toolkit help (exit 0)
   without refreshing pricing first.
7. **Integers.** Integer options are limited to `i64` (see above); Unicode digits
   are not accepted. Float and integer syntax otherwise follows `float()` /
   `int()` (whitespace, `_` between digits, `inf`, `nan`).
8. **Help text.** `toolkit help` lists the commands instead of each script's
   `--help`.
9. **Pricing source tracking.** The `ACU_PRICE_IO_OPTIMIZED` and
   `STORAGE_IO_OPT_PER_GIB` values are tracked (and refreshed) exactly as before
   but, as in the Python, no `acu` calculation reads them.

## Follow-ups

- An SDK-backed `AwsApi` implementation behind an optional feature, plus the
  skill-document rewrite and Python retirement (#4415 owns retirement).
