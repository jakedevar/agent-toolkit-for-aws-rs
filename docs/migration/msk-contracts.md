# MSK sizing, compatibility and simulation tools (slice `msk`, #4387)

Rust replacement for the three unique legacy scripts (six inventory rows: each
canonical script and its identical `plugins/aws-data-analytics` copy):

| Legacy script | Rust command | Library module |
| --- | --- | --- |
| `managing-amazon-msk/scripts/msk_sizing.py` | `toolkit msk size` | `toolkit-msk`: `sizing`, `pricing`, `size_report` |
| `migrate-to-msk/scripts/compatibility.py` | `toolkit msk compat` | `toolkit-msk::compat` |
| `migrate-to-msk/scripts/simulation_load_test_config.py` | `toolkit msk sim compute \| render \| validate` | `toolkit-msk::simulation` |

`crates/toolkit-msk` is pure (no I/O, no AWS dependency, no JSON/YAML engine of
its own). It builds on `toolkit-core` ([`Json`], the strict JSON parser, the
pretty writer). `crates/toolkit-cli/src/msk.rs` does argument parsing, file
access (through `toolkit-core` path and bounded-read contracts) and
exit-status mapping. The Python files are untouched (retirement is #4378).

## CLI contract extensions (shared, additive)

The foundation router had boolean flags and positionals only. Three additive
extensions serve every later slice and keep `skills list` unchanged:

- `CommandSpec.value_flags` and `Invocation::option(name)`: options that take a
  value, as `--name value` or `--name=value`. The value is always the next
  word (so `--avg-data-in-mbs -5` reports an invalid value instead of an
  unknown flag); a repeated option keeps its last value, like `argparse`; a
  missing value is a usage error (exit 2).
- `Output::diagnostic(text)`: queues a line for stderr. `run` writes queued
  lines after the handler returns, whatever the outcome.
- `Output::set_exit_status(n)`: the status `run` returns when the handler
  itself succeeds. Needed because two legacy contracts print regular output and
  exit non-zero (`validate` REJECT exits 1; `msk size` prints `ERROR:` and
  exits 1; `msk sim` errors exit 2 with a JSON error object).

## Command contracts

### `toolkit msk size`

Options (names as in the legacy script; defaults in brackets): `--region`
[us-east-1], `--pricing-source auto|api|static` [auto], `--emit-static-pricing`,
`--avg-data-in-mbs`, `--peak-data-in-mbs`, `--avg-data-out-mbs`,
`--peak-data-out-mbs`, `--num-partitions`, `--retention-hours`,
`--primary-retention-hours` (the seven workload options are required unless
`--emit-static-pricing`), `--replication-factor` [3], `--utilization-standard`
[0.50], `--utilization-express` [0.75], `--pst-per-broker-mbs`,
`--use-max-partitions`, `--no-rack-affined-consumers`, `--discount-pct` [0],
`--broker-classes standard|express|standard,express` [standard,express],
`--broker-quota` [60].

- stdout: the report (pricing banner, "Sizing inputs", Standard and Express
  sections with a hoisted "Shared monthly costs" block when a cost dimension is
  identical for every instance, and "Recommended pick per class").
- stderr: `WARNING: ...` lines; failures as `ERROR: <message>`.
- Exit: 0; 1 for a pricing failure or an invalid workload; 2 for usage errors
  (missing required options, invalid int/float/choice values).
- Units: throughput MiB/s, storage GiB (binary factor 1024), money USD; field
  names ending `_mbs`/`_gb` keep their legacy spelling.

### `toolkit msk compat CLUSTER_CONFIG [--out-dir DIR]`

Reads a discovery `cluster-config.json`, writes
`compatibility.<cluster_name>.json` into `--out-dir` (default: the current
directory) with `json.dumps(indent=2)` formatting (two-space indent, ASCII
only, trailing newline) and reports the written path and `overall: <verdict>`
on stderr. Exit 0 on success, 1 on any runtime error, 2 for a missing argument.

### `toolkit msk sim compute|render|validate`

- `compute [--instance-type T] [--broker-count N] [--kafka-version V]` prints
  the derived capacity/fleet/test-bounds document.
- `render --instance-type T --broker-count N --kafka-version V --output PATH`
  bakes the sizing and fleet into the six `Default:` values of the
  `simulation-stack.yaml` template (embedded in the binary) and prints the
  result document (`output_path`, `deploy_command`, `baked_parameters`, fleet
  fields).
- `validate --test e2e_latency|broker_restart --config JSON [--instance-type T]
  [--broker-count N]` prints `{decision, errors, warnings}`.
- Output is `json.dumps(indent=2)` (ASCII only). Exit 0 for PASS, 1 for REJECT
  (document still on stdout), 2 for invalid sizing/config/output, reported as
  `{"error": "<message>"}` on stderr.

## Parity table (calculation and rule evidence)

Hand-computed from the legacy formulas; each row is pinned by a Rust test.

| Rule / figure | Legacy source | Expected | Rust test |
| --- | --- | --- | --- |
| Brokers rounded to multiples of 3 AZs | `_brokers_for` | 10/5→3, 16/5→6, 21/5→9, 0→0 | `sizing::tests::brokers_are_rounded_up_to_a_multiple_of_three_azs` |
| Standard m5.large, 10/20/30/40 MiB/s, 3000 partitions, 168h/24h retention, util 0.5, list price | `calculate_standard_sizing` | 6 brokers, bottleneck `ingress`, broker 919.80, EBS 506.25, TS 303.75, cross-AZ 342.1875 | `standard_sizing_matches_the_hand_computed_worked_example`, CLI `size_static_report_matches_the_worked_example` |
| No Tiered Storage factors (6.5 / 5.7) | storage/network factor constants | m5.large capacity `96/8.7*0.5` | `without_tiered_storage_the_io_factors_are_lower` |
| Partition limit rec vs max | `partition_key` | 12000 partitions → 12 brokers (rec) / 9 (max) | `partition_limits_switch_between_recommended_and_maximum` |
| PST sizing, clamp, price | PST branch | 500 MiB/s → 3 brokers, 120.00; 1000 clamps to 593.75 → 142.50 | `provisioned_storage_throughput_is_sized_clamped_and_priced` |
| Discount on every dimension | `discount_multiplier` | ×0.8 at 20%; 100% rejected | `discount_applies_to_every_cost_dimension` |
| Cross-AZ consumer fetch | `rack_affined_consumers` | 1368.75 vs 342.1875 | `consumer_rack_awareness_only_changes_the_cross_az_cost` |
| Express m7g.large | `calculate_express_sizing` | 3 brokers, 893.52 broker, data-in 256.640625 | `express_sizing_uses_ingress_egress_and_partition_limits` |
| Recommendation per class under quota, ties → first | `recommend_per_class` | cheapest within 60 | `recommendations_pick_the_cheapest_instance_within_the_quota` |
| Static price table and refresh literals | `STATIC_*`, `format_static_pricing_literals` | 22 prices, `:g` formatting | `pricing::tests::*`, CLI `size_emit_static_pricing_prints_the_refresh_literals` |
| API lookup by `computeFamily`, cross-AZ fallback, unavailable ≠ missing | `fetch_pricing`, `resolve_pricing` | see tests (injected client) | `pricing::tests::api_*`, `resolve_*`, `credential_failures_*` |
| Number rendering (`:,.2f`, `:g`, `repr`) | Python formatting | e.g. `0.125`→`0.12`, `1e-05` | `pyfmt::tests::*` |
| Compatibility pillars, thresholds, forced values, ranges | `compatibility.py` | per rule positive + negative | `compat::tests::*` (one or more tests per rule) |
| Timestamp format | `_utcnow_iso` | `%Y-%m-%dT%H:%M:%SZ` | `utc_timestamps_match_strftime` |
| express.m7g.4xlarge × 12 | `compute` | 1498.8 / 2250.0 MB/s, 9 producers, 3600 MB/s, target 1199 | `default_sizing_matches_the_hand_computed_values` |
| Fleet floor of 2; large clusters | `compute` | m7g.large×3 → 2; 16xlarge×60 → 169 | `small_clusters_get_the_two_node_fleet_floor`, `large_clusters_scale_the_fleet` |
| Template defaults baked | `render` | 5 changed lines for 8xlarge×9, 3.8.x | `rendering_bakes_sizing_and_fleet_into_six_defaults` |
| Throughput warning tiers (>60% sustained, >sustained, >max quota) | `validate` | messages pinned | `e2e_throughput_warning_tiers`, `broker_restart_throughput_tiers_match_e2e` |
| Reboot window `[2, duration-2]` | `validate` | 13 ok, 14 rejected for 15 min | `broker_restart_ranges_and_reboot_window` |

## Intentional changes

1. **Pricing API client.** `--pricing-source api` (and the `auto` first
   attempt) needs an AWS SDK client; the workspace keeps service SDKs
   optional (AC2), so the shipped binary links none. The lookup logic is ported
   and tested against an injected `PricingApi`, but the CLI's client reports the
   pathway as unavailable: `auto` falls back to the static table for
   `us-east-1` with the legacy two-line warning, `api` fails with
   `ERROR: this toolkit build has no AWS Pricing API client ...`, and `auto`
   for any other region fails with the legacy "falling back ... is not
   possible" error. A live client is a follow-up (an optional crate/feature
   implementing `PricingApi`).
2. **Error rendering.** Python tracebacks (uncaught `ValueError`, `KeyError`,
   `ZeroDivisionError`, JSON/IO errors) become clean messages with exit 1
   (`msk size` as `ERROR: ...`, `msk compat` as `error: ...`). `argparse` usage
   errors become `error: <message>` plus the usage text, exit 2. Prefix
   abbreviations of option names (`--avg` for `--avg-data-in-mbs`) are not
   accepted.
3. **Hardening where the script crashed or produced garbage.** Non-finite
   throughput values and a non-positive utilization are rejected with a
   message (the script raised `ZeroDivisionError` or computed with `nan`).
4. **Stricter JSON input.** Inputs use the shared strict parser: duplicate
   object keys, trailing commas, `NaN`/`Infinity` and a byte-order mark are
   rejected (Python's `json.loads` accepted them). Integers beyond `i64` are
   read as floats.
5. **Paths and symlinks (shared contract).** `msk compat` reads the config
   through the bounded regular-file reader (64 MiB cap instead of unbounded)
   and refuses a symbolic link as the config path or as the output file;
   `msk sim render` refuses a symbolic link as the output file. The legacy
   scripts followed links. A `cluster_name` containing a path separator, NUL,
   control character or only dots is refused (the script interpolated it into
   the output file name, so `../x` wrote outside `--out-dir`).
6. **Skill-package rail.** The script refused output inside the directory that
   held the script. The binary has no skill directory (the template is
   embedded), so the rail refuses output inside any directory that contains
   both `SKILL.md` and `assets/simulation-stack.yaml`.
7. **Embedded template.** `render` embeds
   `migrate-to-msk/assets/simulation-stack.yaml` at build time instead of
   reading it relative to the script. The template text itself, which still
   names `simulation_load_test_config.py` in two parameter descriptions, is
   unchanged shipped content.
8. **Dates.** `--emit-static-pricing` stamps the UTC date (Python used the
   local date); `assessed_at` is UTC as before.
9. **`--emit-static-pricing` format.** The printed table-refresh literals keep
   the Python spelling; the Rust tables are `STATIC_BROKER_PRICE_PER_HR` and
   `STATIC_SHARED_PRICING` in `crates/toolkit-msk/src/pricing.rs`.
10. **Per-version Kafka default tables.** The script keeps identical tables for
    3.6/3.8/3.9 plus a generic one; Rust keeps one table (behaviour is
    identical).
11. **Number rendering limits.** Floats with a decimal exponent below -4 or at
    least 16 print as `1e-05`/`1e+16` in messages (Python spelling) but via the
    shared JSON writer in documents (`1e-5`/`1e16`, equal values). `repr()` of
    strings escapes control characters, DEL and C1 controls but does not
    reproduce Python's full Unicode printability table; `\d` in the version
    pattern matches ASCII digits only.
12. **Metric types.** `compat` requires `metrics.*` values to be numbers or
    null (the script would raise `TypeError` when compared); booleans are not
    numbers.

## Not covered / remainder

- A live Pricing API client (item 1) and the optional feature wiring.
- Updating the skills' `SKILL.md` and reference files (and the plugin copies)
  from `uv run .../msk_sizing.py` to `toolkit msk ...`: shipped content, changed
  with the sync/distribution slices once the binary is distributed (#4377,
  #4378). The Python scripts stay until then.
- Retiring the six Python inventory rows (#4378).
