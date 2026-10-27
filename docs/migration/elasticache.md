# ElastiCache pricing, validation and command analysis (#4392)

Slice `cache-analysis`, wave 2 of programme #4369. Built on the #4374 core/CLI
contracts and the #4379 integration head. Verification evidence lives in the
Issue #4392 handoff, not in this file.

Nine scripts of the `amazon-elasticache` skill are replaced by one Rust module,
`crates/toolkit-cli/src/elasticache/`. The Python files are untouched (read-only
evidence). Their callers are skill documents (`SKILL.md` and `references/*`) that
tell the model to run `python3 scripts/*.py`; rewriting that guidance and
deleting the scripts is the retirement slice's job (#4415) once this command is
landed and verified. The tunnel and connection scripts (`start_tunnel.py`,
`find_tunnel_host.py`, `test_connection.py`) and `collect_metrics.sh` belong to
#4393 and are not touched.

| Command | Replaces |
| --- | --- |
| `toolkit elasticache price` | `price_calculator.py` (and the shared `pricing.py`) |
| `toolkit elasticache estimate-serverless` | `serverless_estimator.py` (and `command_classifier.py`) |
| `toolkit elasticache validate-input` | `input_validator.py` |
| `toolkit elasticache validate-references` | `validate_references.py` |
| `toolkit elasticache preflight` | `migration_preflight.py` |
| `toolkit elasticache audit` | `security_audit.py` |
| `toolkit elasticache dashboard` | `generate_dashboards.py` |

## Layout

- `pricing.rs`: the one price table loader (AC3). `price` and `estimate-serverless`
  both read it; nothing else parses prices.
- `calculator.rs`, `serverless.rs`, `classifier.rs`: the three estimators.
- `validator.rs`, `references.rs`: identifier and reference-file validation.
- `preflight.rs`, `audit.rs`: migration readiness and security posture.
- `dashboards.rs`: CloudFormation dashboard and alarm template.
- `text.rs`: Python-compatible rounding, `sum`, `splitext`, `json.dumps`
  separators and the escaping every echoed value goes through.
- `tests.rs`: offline tests (canned pricing files, recorded snapshots).

Routing uses the shared router: seven `CommandSpec` entries in one block of
`COMMANDS`, value options with `value_flags` (`--flag VALUE` or `--flag=VALUE`),
`strict_values: false` for numeric options (so `--throttle-threshold -1` reaches
the range check, as `argparse` does) and `true` for the one path option of
`validate-references`. `validate-input` takes every `KEY=VALUE` as a positional
(`max_positionals: usize::MAX`, `passthrough: false`). Non-zero statuses that
come with regular output (`validate-*`, `preflight`, `audit` verdicts) use
`Output::set_exit_status`; refusals use `ToolkitError` and the shared
`error: ...` report. Python number and `json.dumps(indent=2)` rendering reuse the
Aurora slice's `pyfmt` module (made `pub(crate)`), and the CSV reader and
argparse-style decoders reuse its `api` and `args` modules; no router,
value-option or stderr helper was added.

## Price sources (no network in this build)

The Python fetched the public AWS Bulk Pricing index over HTTPS and cached it for
seven days. This build has no HTTP client, so the prices come from a local file:

| Option | File |
| --- | --- |
| `price --pricing-csv FILE` / `estimate-serverless --pricing FILE` | the optional pricing CSV of the Python scripts (`Product Family`, `Region Code`, `Cache Engine`, `TermType`, `PricePerUnit`, `Instance Type`, `Unit`, `Operation`) |
| `--pricing-json FILE` | the AWS bulk pricing JSON (`products`, `terms.OnDemand`, `terms.Reserved`) as downloaded from `https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonElastiCache/current/index.json`, **or** the compact table the old script cached in `.pricing_cache/elasticache_pricing.json` |

With neither option the command is a usage error (exit 2) that names both. Giving
both is a usage error. The CSV loader still prints the Python's note on stderr
that a CSV carries no Reserved Instance or Extended Support data (it now points
at `--pricing-json`). Files are read through the shared bounded, symlink-refusing
reader (256 MiB cap for the bulk index, 16 MiB for other inputs).

## Parity

| Behaviour | Python | Rust | Test |
| --- | --- | --- | --- |
| Bulk parsing: AWS Region only, skip Outposts and snapshot/backup rows, normalize `Valkey`/`Redis`/`Memcached`, Extended Support years from `usagetype` | `_parse_bulk_pricing` | same table builders | `bulk_pricing_is_parsed_into_every_table`, `outpost_and_snapshot_rows_are_ignored` |
| Reserved terms: `No/Partial/All Upfront` only, `Hrs` hourly + `Quantity` upfront, effective monthly `round(upfront/term + hourly*730, 2)`, sorted by that cost with ties in file order | `list_reserved_options` | ordered table, stable sort | `reserved_options_are_sorted_by_effective_monthly_cost` |
| CSV loader skips non-positive prices, `GB-months`, `Snapshot`, non-`OnDemand` instance rows | `_load_csv` | same | `csv_pricing_reads_on_demand_node_and_serverless_rows_only` |
| Lookup failure messages (`No node pricing found...`, `Available: a, b and N more`) | `ValueError` text | same text | `missing_prices_are_reported_with_the_python_messages`, `available_regions_are_sorted_and_capped_at_five` |
| Serverless estimate: min billing 0.1 GB (valkey) / 1 GB, payload scaling above 1 KB, operand order, `round(x, 2)` half to even | `estimate_serverless` | same | `serverless_estimate_matches_the_python_formulas`, `serverless_minimum_storage_and_payload_scaling` |
| Node estimate and reserved comparison (`discount_pct` is an integer, `round()`) | `estimate_node_based` | same | `node_estimate_with_a_reserved_comparison` |
| Missing on-demand price is `{"error": ...}` with exit 0; zero nodes divides by zero | `except ValueError` | same | `node_estimate_edge_cases` |
| Reserved options and Extended Support reports (widths, `,` grouping, totals) | `format_*` | same | `reserved_options_report_layout`, `extended_support_estimate_and_report` |
| `--ops-per-sec` conversion, explicit ECPUs win, `0` ops/s means unset | `estimate_ecpu_from_ops` | same | `ops_per_second_convert_to_millions_of_ecpus`, `price_command_defaults_and_ops_per_second` |
| Command classes: 51 fixed, 27 internal, everything else nonfixed `max(calls, usec/3)` | `command_classifier.py` | same lists | `commands_are_classified_case_insensitively`, `nonfixed_commands_take_the_higher_of_calls_and_cpu_time` |
| Cluster CSV defaults (`primary_nodes` heuristic `max(1, n // 2)`), commandstats normalization by `uptime_days` with truncating `int()` | `load_clusters`, `parse_commandstats_file` | same | `cluster_csv_defaults_and_validation`, `commandstats_csv_is_normalized_by_uptime` |
| Simple vs detailed ECPU mode, higher of CPU and network, memcached falls back with a warning | `estimate_cluster` | same | `simple_mode_estimate_uses_list_price_for_the_current_cost`, `detailed_mode_bills_the_higher_of_cpu_and_network`, `memcached_ignores_commandstats_with_a_warning` |
| Burst multiplier: 24-point sine wave, `peak_to_avg_ratio`, flat/zero guards | `compute_burst_multiplier` | same, `sin` of the C library | `burst_multiplier_models_a_daily_sine_wave`, `bursty_clusters_get_a_note_and_a_higher_cost` |
| Result rounding and types (`0` vs `0.0` for an unknown cost, integer ECPUs, `None` as `null`/empty cell) | dict + `csv.DictWriter`, `json.dump(indent=2)` | same, `\r\n` CSV rows | `provided_cost_minimum_storage_and_unknown_cost`, `cluster_report_csv_and_json_use_python_number_formats` |
| Summary table (widths, `{:+.0f}%`, sort by saving) | `print_summary` | same | `summary_lists_totals_savings_and_sorted_clusters` |
| Identifier rules and messages for the nine keys | `input_validator.py` | same text | `cache_names_follow_the_aws_rules` and the following validator tests |
| Reference checks: kebab-case, title, 50 non-blank body characters, `SKILL.md` references, script docstring and entrypoint, summary line, exit 1 on any FAIL | `validate_references.py` | same | `reference_files_are_checked_one_by_one`, `validate_references_command_output_and_exit_status` |
| Preflight: version bands, modules, restricted commands, Lua, large keys, persistence, eviction policy, verdict, sorted report | `migration_preflight.py` | same findings over a snapshot | the `a_healthy_source_*` ... `preflight_command_end_to_end` tests |
| Audit: serverless and replication-group checks, security-group CIDR rules, tags, recommended actions, exit 1 on FAIL | `security_audit.py` | same findings over a describe file | the `serverless_audit_*` ... `audit_command_end_to_end` tests |
| Dashboard/alarm template: widgets, alarm defaults, `80` vs `80.0` thresholds, logical ids, `DashboardBody` as `json.dumps` text, `SNSTopicARN` parameter | `generate_dashboards.py` | same | `serverless_template_defaults` ... `dashboard_output_file_and_deploy_hint` |

Generated templates are checked structurally in `assert_schema_valid` (AC2): the
`toolkit_core` schema validator over the template, every alarm, every dashboard
resource and the decoded `DashboardBody`, plus CloudFormation's logical-id rule
and the comparison/statistic vocabularies.

Boundary and negative coverage: 40/41-character names, 8/17/18 hex digits,
255/256-character serverless snapshot names, exactly 50 body characters, 1 MiB
large keys, the first-20 sampling window, zero nodes, non-finite numbers,
`uptime_days` of zero and an underflowing one, symbolic links as input and
output, missing/duplicate/unknown options and malformed files.

## Intentional changes

1. **Prices from a file.** See "Price sources". Live fetching, the 7-day cache and
   the `RuntimeError("Failed to fetch pricing ...")` path are gone; a missing
   source is a usage error (exit 2). `--pricing-json` also reads the compact
   cache of the old script.
2. **Preflight and audit read recorded data.** The scripts opened a Redis/Valkey
   connection (`valkey` client) and called `boto3`. This build has neither, so:
   `preflight --snapshot FILE` and `audit --describe FILE` run the same checks
   over a JSON file holding what the scripts read (formats in the module docs of
   `preflight.rs` and `audit.rs`). Without the file, `preflight` reports the
   Python's connection-failure result (`error` + a `Connectivity FAIL` finding,
   exit 1) and `audit` fails with `error: Failed to create AWS session: AWS SDK
   access is not available in this build ...` (exit 1). The `--password`,
   `--username`, `--tls`, `--region` and `--profile` options belong to the live
   clients and are not accepted until those exist (a secret-bearing option that
   does nothing would only mislead); `--endpoint` of `estimate-serverless` is a
   usage error that says so. Live clients are a follow-up.
3. **No interactive mode.** `price --interactive` read prompts from stdin; it is a
   usage error that points at the options. Short options (`-i`, `-e`, `-c`, `-p`,
   `-o`) are not supported by the shared router; use the long names.
4. **Untrusted text is never echoed raw (#4602).** Anything taken from a file, a
   snapshot or the command line that appears in a report, a diagnostic or a
   lookup message (region, node type, cluster and module names, versions,
   policies, file names, choice values) goes through `text::show`: control
   characters and bidirectional overrides are written as `\xNN` / `\uNNNN` and a
   value is cut at 120 characters. `validate-input` goes further and never prints
   the rejected value at all (the messages carry only a length), and an argument
   without `=` is reported by position (`argument 2 is not in key=value format`)
   instead of being echoed. Large-key names are never printed. JSON output keeps
   the values as data (JSON escapes them).
5. **Stricter validators.** The Python regexes ended in `$`, which also matches
   before a trailing newline, so `my-cache\n` passed `validate_cache_name`; `\d`
   matched any Unicode digit. Both are rejected here (ASCII only, whole value).
6. **Deterministic ordering.** The Python iterated sets and `os.walk` directory
   lists in hash/OS order. Restricted commands are reported in name order,
   replication-group security groups are sorted, and `validate-references` walks
   `references/` in name order.
7. **Links are not followed.** `validate-references` never descends into a
   symbolic-linked directory and reports a linked file as `Could not read file:
   refusing to follow symbolic link`; every input and output file goes through
   the shared symlink-refusing reader/writer. `--output` of `dashboard` and
   `estimate-serverless` refuses an existing link.
8. **Errors.** Tracebacks (`ValueError`, `ZeroDivisionError`, bad numbers in the
   CSV, missing columns) are `error: <message>` with exit 1; argparse errors are
   `error: ...` plus the usage text with exit 2 (the scripts printed
   `ERROR:`/`Error:` lines, some on stdout). `validate-input`'s failure list still
   goes to stdout with exit 1.
9. **Hints.** Messages that told the reader to run `python scripts/price_calculator.py`
   name `toolkit elasticache price`; the `dashboard --output` deploy hint
   shell-quotes the template path and stack name.
10. **`validate-references --skill-dir DIR`** replaces the location of the script
    (default: the current directory). It still checks the legacy `.py` scripts
    (docstring, entrypoint) until they are retired.
11. **Dropped helpers.** `sanitize_for_cli` (shell-metacharacter stripping, no
    caller anywhere in the toolkit), `get_node_monthly_rate` and
    `get_all_node_memory` (no caller) are not ported; validation, not stripping,
    is the safety control. The `valkey` endpoint collector
    (`collect_from_endpoint`) is the live-client follow-up.
12. **Python `sum()`.** Totals use compensated summation like CPython 3.12+.
13. **Integers.** Integer options are `i64` (as in the Aurora slice); the CSV
    reader is the Aurora one, so a short row leaves a column absent instead of
    `None` (a missing value is the default instead of a `TypeError`). A
    snapshot field of the wrong JSON type is treated as absent.
14. **Dashboard numbers must be finite (#4636).** The script accepted `nan`,
    `inf` and `1e999` for the threshold options and wrote `NaN`/`Infinity` into
    the template, which strict JSON parsers and CloudFormation reject. Every
    numeric option of `dashboard` (`--max-storage-gb`, `--storage-alarm-pct` and
    all thresholds) now refuses a non-finite value (including an overflow to
    infinity) with a usage error, exit 2, before anything is written to stdout
    or `--output`. Finite values, defaults and `80` vs `80.0` rendering are
    unchanged.
15. **Security groups are never "passing" without a record (#4637).** The script
    asked AWS for the groups and either got every answer or an error. A
    recorded `--describe` file can omit them, so `audit` reports `PASS` only
    when every requested group has a record with a valid `IpPermissions` array
    (an explicitly empty array is a pass). A missing or wrong-type
    `security_groups` list gives an `ERROR` finding `Security groups` ("Could not
    inspect: no security group records were supplied for ..."); a requested group
    with no record, or with a missing/wrong-type `IpPermissions` (or `IpRanges` /
    `Ipv6Ranges`, or a non-object rule), gives an `ERROR` finding
    `Security group <id>` ("Not inspected: ..."). `ERROR` is not `FAIL`: like the
    `security_groups_error` path it leaves the exit status alone but is shown as
    `[ERR]`. `WARN` findings of inspected groups are kept, and records of groups
    that were not requested are ignored.
16. **Malformed range entries are not inspected either (#4644).** A group is
    also "not inspected" (the same `Security group <id>` `ERROR`, never a
    `PASS`) when any `IpRanges` entry is not an object or lacks a string
    `CidrIp`, when any `Ipv6Ranges` entry is not an object or lacks a string
    `CidrIpv6`, or when such a string is not a well-formed CIDR of its family:
    `address/prefix` with a parseable IPv4 or IPv6 address and a canonical
    decimal prefix (no sign, no leading zero, at most 32 or 128). The earlier
    scan read these members with an empty default, so null, scalar,
    wrong-type or unparseable entries vanished and left a false `PASS`. Port
    members (`FromPort`/`ToPort`) are only displayed, never decide a verdict,
    so they stay optional (all-traffic rules carry none). In the same pass any
    IPv6 CIDR with prefix `/0` (for example `0::/0`), not just the exact text
    `::/0`, now gets the broad-rule `WARN`.

## Follow-ups

- Live clients: an AWS pricing/ElastiCache/EC2 transport and a Redis/Valkey
  client behind optional features, so `price` can refresh prices, `audit` can
  call `describe_*`, and `preflight` / `estimate-serverless --endpoint` can read
  a server. The analyzers take plain data and need no change.
- Skill-document rewrite and Python retirement (#4415).
- Tunnel and connection operations: #4393.
