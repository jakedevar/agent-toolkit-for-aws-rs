# Relational helpers: RDS, Aurora DSQL, Timestream for InfluxDB (#4394)

Slice `relational`, wave 2 of programme #4369. Base: integration head
`550b629d7aa376a967e194192b7be6e7563d2c08`.

## Coverage check at the base

`git grep -n -E 'safe_query|tenant_query|test_oracle_connection|rds_commitment|instance_types'`
over `crates/` found no Rust for these scripts. The Aurora analyzers (#4388,
`crates/toolkit-cli/src/aurora/`) port the six `amazon-aurora-{mysql,postgresql}`
scripts only; `rds-oss/scripts/rds_commitment_pricing_analyzer.py` is a separate
RDS MySQL/MariaDB/PostgreSQL script (different price table, instance-level
`DescribeReservedDBInstancesOfferings`, no Aurora engine handling), so nothing
here was already covered. Reused from the Aurora port, not duplicated:
`aurora::pyfmt::{dumps, round_to, round_num, Num, fmt_f}` (Python `round`,
`json.dumps(indent=2)`, `:.2f`), `aurora::api::json_float` (Python `float()`),
`aurora::args::{usage, int_value, parse_py_int, required, reject_unrecognized}`.

## Commands

| Rust | Python original |
| --- | --- |
| `toolkit dsql selftest` | `python safe_query.py` (the embedded `_selftest()`) |
| `toolkit dsql tenant-sql --tenant SLUG` | `tenant_query.select_by_tenant`'s statement, for `psql -c` pipelines (new entry point, see below) |
| `toolkit dsql tenant-query --tenant SLUG [--recorded FILE]` | `tenant_query.select_by_tenant(cursor, req)` |
| `toolkit rds commitment [offline] [--region R] [--format json] [--instance ID] [--instance-type T --engine E --num-instances N --multi-az] [--recorded FILE]` | `rds_commitment_pricing_analyzer.py` |
| `toolkit rds-oracle test-connection ENDPOINT PORT SERVICE USER [--verbose] [--recorded FILE]` | `test_oracle_connection.py` |
| `toolkit timestream-influxdb validate-input [KEY=VALUE...]` | `input_validator.py KEY=VALUE...` |
| `toolkit timestream-influxdb instance-types [recommend W Q D]` | `instance_types.py [recommend W Q D]` |

All seven are registered once in `COMMANDS` (`strict_values: true`,
`passthrough: false`, `repeatable_flags: &[]`: no Python flag here is
`action="append"`). Code is `crates/toolkit-cli/src/relational/`:
`sql.rs` (safe_query), `tenant.rs`, `rds/{mod,client}.rs`, `oracle.rs`,
`timestream.rs`, `members.rs` (the `toolkit_aws::recorded` accessors in this slice's wording, #4799),
`tests/`. CLI-level tests: `relational/tests/commands.rs` (in-process router)
and `tests/diag_contract.rs::relational_reports_escape_hostile_recorded_and_argument_text`
(real binary, valid hostile recordings). The generic `diag_contract` loop covers
all seven commands automatically.

## No live client in this slice

Nothing here opens a socket or reads a credential.

- RDS / Savings Plans: `rds::client::AwsRead` (one `(service, region,
  operation, request)` per call). `RecordedClient` replays `--recorded FILE`;
  `NoLiveClient` sends nothing and fails every call (the `boto3`-missing path of
  the script). Pagination uses `toolkit_aws::{collect_pages, next_token_from,
  PageLimits}` (a repeated token or a runaway walk is an error, the script would
  loop forever); errors are `toolkit_aws::AwsError`/`ServiceError`, redacted by
  `redact_text`.
- Oracle: `oracle::OracleDriver` (`driver`, `connect`, `query`). `RecordedDriver`
  replays `--recorded FILE`; `NoLiveDriver` fails at the driver check (exit 1).
  The password is **not read at all** in this build (no `ORACLE_PASSWORD`, no
  prompt). `ConnectParams` carries no password; `ConnectParams::dsn()` is the
  only constructor of the connection string and is printed only with
  `--verbose`, redacted and escaped.
- DSQL: `tenant::SqlCursor` (`execute`, `fetchall`). `RecordedCursor` replays
  `--recorded FILE`; `NoLiveCursor` fails every call. `execute` takes a
  `TenantRead`, not SQL text: the only statement a cursor can be asked to run
  is the tenant read (see intentional change 11).

Recorded formats (rustdoc of `rds/client.rs`, `oracle.rs`, `tenant.rs`): RDS
`{"calls": [{"service": "rds"|"savingsplans", "region"?, "operation", "request"?,
"response"|"error"}]}` (a paged request carries its `Marker`/`nextToken`);
Oracle `{"driver"?, "connect"?, "probes"?: {"basic"|"protocol"|"banners"|"auth":
{"rows"|"error"}}}`; DSQL `{"statements": [{"sql", "rows"?|"error"?}]}`. Every
optional member is absent (default) or of its type; a present member of another
type, `null` included, rejects the recording (exit 1, reason names the member
and expected type, never the value). An unmatched call/statement fails without
echoing the request.

## Acceptance criteria

- **AC1 parameterized queries, tenant isolation, read-only safety.**
  `relational/tests/sql.rs` (validators, `build` mismatch/format-feature
  rejection, injection corpus, 63/64/65-character boundaries),
  `tests/tenant.rs` (the tenant filter is in every statement; an invalid or
  non-string tenant never reaches the cursor; two tenants' statements name only
  their own; only template statements for valid tenants reach a spy cursor, and
  writes, stacked statements and write-capable `SELECT` forms such as
  `SELECT * INTO`, `SELECT nextval(...)`, `FOR UPDATE` and data-modifying CTEs
  cannot be expressed through the public surface). The scripts build raw SQL
  where the driver cannot bind; the Rust `Safe` type cannot be made from a raw
  string.
- **AC2 connection failures and price/instance boundaries.** Oracle: each
  hint code (12170, 12541, 1017, 12514, 12505, 28000), exit 1 on driver or
  connection failure, later check failures keep the script's exit 0
  (`tests/oracle.rs`). RDS: swallow-all fetch semantics, 94,000,000-second term
  cut-off (strict), Multi-AZ doubling for on-demand, RI and DSP, region
  warning, unknown class `$0` baseline, integer-zero percentages
  (`tests/rds.rs`). Timestream: every validator boundary, every sizing
  threshold (`< 1000/10000/50000/100000` writes, `< 5/20/50` queries), the 90
  and 365-day retention bumps (`tests/timestream.rs`).
- **AC3 no external database.** No test needs a database, AWS or the network;
  recordings are literals in the test source or temporary files.

## Intentional behaviour changes

1. **No live client.** `--recorded FILE` or the offline analysis only. RDS
   without a client behaves like the script without `boto3` (bundled prices,
   no RI/DSP rates; a live `--instance` lookup reports the failure in the JSON
   `error` member, exit 0, as the script did for an API error). Oracle without
   a recording prints `FAIL: this build includes no live Oracle driver`, exit 1.
2. **Oracle password.** Not read or prompted in this build (see above). The
   live binding must read `ORACLE_PASSWORD` or a no-echo prompt, hold it in
   `toolkit_aws::Secret`, and never accept it as an argument.
3. **Oracle arguments validated.** `PORT` must be 1-65535 digits; endpoint,
   service name and user name are restricted to host/identifier characters (no
   `/ @ ? " ' ` whitespace or controls) because they form the connect string.
   More than four positionals is a usage error (Python ignored extras). Fewer
   than four prints the usage on stdout and exits 1, like the script.
4. **Script quirk kept.** `=== All tests passed ===` and exit 0 follow failed
   query/encryption/auth checks; only a driver or connection failure exits 1.
5. **ASCII-only patterns.** Python's `re.IGNORECASE`/`\d` also matched non-ASCII
   letters/digits (`ſ`, the Kelvin sign, Arabic-Indic digits) in identifiers and
   dates; they are rejected here. Timestream `re.match(...$)` accepted a value
   with one trailing newline; it is rejected (the values feed AWS CLI calls).
6. **safe_query messages** quote the value with `toolkit_core::quote_untrusted`
   (escaped, bounded) rather than Python `repr`; any malformed template
   (unmatched brace included) is an `UnsafeSqlError` rather than a bare
   `ValueError`. Python's non-`str`/`bool` checks are type errors in Rust;
   `select_by_tenant` still reports a non-string tenant with the Python type name.
7. **Added entry points.** `dsql tenant-sql` prints the validated tenant
   `SELECT` for `psql -c` pipelines (the Python docs told shell users to compose
   it in a `python3` subshell). The cursor surface is narrowed (change 11).
8. **Usage errors** replace Python tracebacks: `KEY` without `=`, non-integer
   workload numbers, an unknown `instance-types` word, `--region` that is not
   an AWS region shape, a flag of the other `rds commitment` mode (argparse's
   `unrecognized arguments`). `--num-instances` is an `i64`.
9. **DescribeDBInstances / offering members of the wrong type** end the RI
   fetch with no offerings (the script's `except Exception`) or report
   `invalid DescribeDBInstances response: ...` in the document (the script
   would have continued with a mistyped value).
10. **Warning** the script raised with `warnings.warn` (non-us-east-1 price) is
    `warning: ...` on stderr, escaped on one line.
11. **Narrowed read-only surface (#4738).** The first port wrapped cursors in
    `ReadOnly`, a guard over SQL text that accepted any statement starting with
    `SELECT` without `;`/NUL, so `SELECT * INTO copied FROM entities`,
    `SELECT nextval(...)`, `SELECT ... FOR UPDATE` and side-effecting functions
    passed. It and `sql::require_read_only` are removed, and no keyword filter
    replaces them. `SqlCursor::execute` now takes a `tenant::TenantRead`, whose
    field is private and whose only constructor, `TenantRead::for_tenant`, fills
    the fixed template `SELECT * FROM {t} WHERE tenant_id = {tid}` with the
    `entities` table and a `TENANT_SLUG`-validated tenant. It implements no
    `From`, `FromStr`, `Default` or deserialization, so no other SQL can reach a
    cursor (`tests/tenant.rs` checks this with compile-time trait probes and
    with the counterexamples offered as tenants). Recorded `sql` entries are
    only compared with the tenant read, never run. No ported caller needs a
    general raw-SQL read path (`tenant_query.py` runs only this statement). A
    future one is out of scope here: it needs a database-enforced read-only
    design (read-only transaction or role), not a statement filter.

## Python callers left for #4415

The Python scripts stay in place. Callers to re-point when they are retired:

- `skills/specialized-skills/database-skills/aurora-dsql/`: `SKILL.md`,
  `references/{database-tools,input-validation,mcp-tools,scripts-guide,workflow-patterns}.md`
  (all `from safe_query import ...` examples and `python3 scripts/safe_query.py`),
  `scripts/psql-connect.sh`.
- `rds-oracle/`: `references/compute-runtime.md`, `references/troubleshooting.md`
  (`python3 scripts/test_oracle_connection.py`), `SKILL.md` bundled-script list.
- `rds-oss/`: `SKILL.md`, `references/commitment-pricing-workflow.md`
  (three `python3 scripts/rds_commitment_pricing_analyzer.py` invocations),
  `references/verify-dependencies.md` (Python 3.10+ requirement).
- `timestream-influxdb/`: no document references `input_validator.py` or
  `instance_types.py`.

## Follow-ups

- Live bindings behind the `toolkit-aws` transport/signer: RDS +
  Savings Plans read client, Oracle driver (with the password handling above),
  Aurora DSQL cursor (IAM-token authenticated TLS connection). Filed as #4733,
  no credential, IAM or network exposure exists in this slice. The live DSQL
  cursor must implement `SqlCursor` over `TenantRead` (change 11) and add no
  raw-SQL entry point.
