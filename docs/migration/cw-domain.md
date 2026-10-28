# Slice `cw-domain` (Issue #4380): CloudWatch parsing, validation, status and rendering

Rust port of the pure modules of the dynamic-instrumentation scripts under
`skills/core-skills/aws-observability/scripts/cloudwatch/` (and their plugin
mirrors under `plugins/aws-core/`). The Python files are read-only migration
evidence: none is edited or deleted here, so the mirrors stay byte-identical to
the canonical copies and `toolkit sync plugin-skills --check` is unaffected.
Retirement of the Python artifacts waits for slice `cw-api` (#4381), whose live
clients and operation entrypoints still call them, and then for the cutover and
retire slices (#4414, #4415).

| Python source | Rust module (`crates/toolkit-cli/src/cloudwatch/`) |
| --- | --- |
| `di_constants.py` | `consts.rs` |
| `di_formatting.py` | `timefmt.rs` (`format_timestamp`, `UtcTime`) |
| `di_result.py` | `result.rs` (`OpResult`) |
| `di_region.py` | `region.rs` |
| `di_validation.py` | `validation.rs` |
| `di_location.py` | `location.rs` |
| `di_capture.py` | `capture.rs` |
| `di_error_translation.py` | `errors.rs` (plus the pure dispatch of `di_gateway.render_error`) |
| `di_snapshot_parsing.py` | `snapshot_parsing.rs` |
| `di_snapshot_rendering.py` | `snapshot_rendering.rs` |
| `di_status_assessment.py` | `status_assessment.rs` |
| `di_status_rendering.py` | `status_rendering.rs` |
| `di_crud_rendering.py` | `crud_rendering.rs` |
| (Python text semantics) | `pytext.rs` (`json.dumps(indent=2)`, `str.strip`, `dict.get` idioms) |
| (offline command surface) | `cli.rs`: `toolkit cloudwatch render | validate | region` |

These are exactly the 13 files the inventory assigns to slice `cw-domain`.
`di_app_signals_client.py`, `di_logs_client.py`, `di_session.py`,
`di_gateway.py`, `di_snapshot_queries.py`, `di_crud_tools.py`,
`di_status_tools.py`, `di_snapshot_tools.py`, `di_instrumentation.py` and
`di_snapshots.py` belong to `cw-api` (#4381): they hold the live SDK clients,
the polling loop, the operation entrypoints and the host scripts' flags
(`--json`, `--json-file`, `--region`, `--profile`, `--out`, `--print-contract`).

## Design for #4381

Everything here is a pure function over values. The seams #4381 fills in:

- **Clients**: an Application Signals client and a Logs client behind injected
  traits in `toolkit-aws`. They hand responses to these modules as `Json`
  objects with the API's member names (`LocationHash`, `LatestConfigurations`,
  `Events`, ...). Timestamps cross that boundary as ISO strings produced with
  `UtcTime::format_seconds_z` (or epoch seconds); `format_timestamp` passes
  strings through exactly like the Python helper passes non-`datetime` values.
- **Failures**: SDK errors map onto `AwsFailure` (client error with code and
  message, endpoint connection, timeout, credentials, other); `render_failure`
  is `gateway.render_error` and `translate_aws_error` /
  `render_client_error` are the two templates.
- **Query result**: `QueryResult` is the dict `_execute_cloudwatch_query`
  returns (`status` in `Complete`, `Failed`, `Cancelled`, `Error`,
  `Polling Timeout`).
- **Status policy**: `assess` takes the three-argument `check_status` callback
  the Python `assess` takes, so the tool layer injects the AWS query.
- **Ops**: the validators and parsers return the Python error strings
  verbatim, and `OpResult` carries the exit-status rule (`0` if `ok` else `1`).

## Behavior contract

All user-visible text is copied from the Python: the section headers and
bullets of the CRUD and status reports, the consolidated-status tip and
troubleshooting lines (including the `ACTIVE not confirmed yet` line that the
READY and ERROR renderers cut), the `field_documentation` block, the
`ERROR: ...` validation messages, the location-troubleshooting text and the
failure templates. The tests assert whole strings, derived by hand from the
Python source (there are no Python tests in this slice's inventory rows).

Parity details worth knowing:

| Area | Behavior kept |
| --- | --- |
| `json.dumps(indent=2)` | Insertion-ordered keys, `ensure_ascii` (`é`, surrogate pairs), `1.0` floats, empty `[]`/`{}`, `NaN`-free. |
| `str(value)` / `repr` | `None`, `True`, `{'a': 1}` for values that reach an f-string (shared `dynamodb::pyfmt`). |
| `dict.get(key, default)` | The default only applies to an absent key; a JSON `null` renders as `None`; `if value:` uses Python truthiness. |
| `str.strip()` / `upper()` / `lower()` | Python white space (including `\x1c`-`\x1f`), Unicode case mapping. |
| `re.escape` | The Python 3.7+ special-character set (`()[]{}?*+-\|^$\.&~#`, space, tab, newline, CR, VT, FF), then `/` -> `\/`. |
| Logs Insights string escape | Backslashes first, then double quotes. |
| Location hash | Exactly 16 lowercase hex characters (no quote can pass). |
| Status windows | ACTIVE clamped to `max(created_at, requested_start)`; an empty clamped window skips the query with the `Skipped: ...` note; READY and ERROR use the full window. |
| Snapshot size rule | Raw message over 10 KiB (UTF-8 bytes) and not `include_raw` is replaced by the parsed summary plus the `Raw snapshot was N,NNN bytes` note. |
| Line-key order | Numeric line keys by value, then the rest lexically (a `-1` key can never crash the sort). |

## Intentional changes (recorded, none changes a documented contract)

1. **No Python datetime / botocore types.** `datetime` is `UtcTime` (UTC,
   microseconds, years 1-9999). Booleans/ints that Python accepted as API
   values in rendered fields are JSON values and print with `str()`.
2. **Hash locations have no payload or detail methods.** `HashLocation.to_api_payload()`
   and `format_details()` only raised `NotImplementedError`; the Rust type has
   no such methods (`ResolvedLocation::to_api_payload` returns `None` for a
   hash). `parse_create_inputs` / `parse_lookup_inputs` drop the unused
   `normalized_type` argument.
3. **`datetime.fromisoformat` is a documented subset.** Supported: `YYYY-MM-DD`
   and `YYYYMMDD`, one separator character, `HH[:MM[:SS[.f]]]` and the basic
   forms, `Z`, `+HH[:MM[:SS]]` and `-HH...` offsets; naive values read as UTC
   and fractions keep six digits. ISO week dates (`2025-W05-3`) are rejected
   with `Invalid isoformat string: '...'`. Range errors use CPython's texts
   (`month must be in 1..12`, `day is out of range for month`, ...); an offset of
   24 hours or more reports `offset must be a timedelta strictly between
   -timedelta(hours=24) and timedelta(hours=24)` without CPython's trailing
   `, not datetime.timedelta(...)`. Arithmetic that leaves years 1-9999
   (Python `OverflowError`) returns `date value out of range`.
4. **Strict JSON for snapshot messages.** `@message` is read with the shared
   strict parser. Python's `json.loads` also accepts duplicate keys (last
   wins), `NaN`/`Infinity` and arbitrarily large integers; such a message is
   treated as an unparseable snapshot (`{}`), and an integer outside the `i64`
   range reads as a float. OTLP records written by the instrumentation agents
   contain none of these. A lenient mode is tracked as a follow-up.
5. **Mis-shaped input does not crash.** Where Python raised `AttributeError`
   or `TypeError` on a wrong JSON shape (a `@message` that is a JSON array, an
   `attributes` value that is a list in the search summaries, a capture-name
   list that is not an array), the Rust renderers read the part as empty/unset.
   The CLI reports mis-shaped *arguments* with the legacy `TypeError` exit
   status 2.
6. **Line keys use ASCII digits.** Python's `str.isdigit()` also accepts other
   Unicode digits and then raises on `int()` for some of them; the Rust sort
   treats only `0-9` strings as numeric. Equal numeric keys (`01` and `1`) are
   tie-broken lexically, where Python's order depended on set iteration.
7. **The clock is an argument.** `render_create_success_message` takes `now`
   (the fallback `CreatedAt`) instead of reading the clock, so output is
   deterministic. The CLI reads the system clock only when `now` is omitted.
8. **Errors are values.** `AwsFailure` replaces the botocore exception classes;
   `GatewayError` and the SDK mapping are #4381's.

### Diagnostic text safety (#4634)

The Python code interpolated caller-derived strings into its messages
verbatim. A caller-supplied value (an `instrumentation_type`, a JSON key, an
unknown renderer/check/mode choice, a language, a file path, an AWS message, an
attempted-parameter name or value, a location response field, the region from
the environment) containing a newline, carriage return, ESC or another control
could therefore clear a terminal or forge an apparent report line. The Rust
port escapes these, centrally, in `cloudwatch::pytext`:

- `safe_text` replaces every control character (C0, DEL, C1, including `\n`,
  `\r`, `\t`, ESC), the Unicode line/paragraph separators, the bidirectional
  controls and the BOM with a visible Python-style escape (`\n`, `\x1b`,
  `\uXXXX`). Ordinary text, including non-ASCII letters, joiners, and the
  backslash, is unchanged. It is applied to every caller-derived scalar that
  lands inside a diagnostic line: the `received:` values and `LOCATION INPUTS
  RECEIVED` block, the Java class-name suggestion, the capture field name, the
  `invalid arguments for op '...'` detail (unknown keys, mode/kind/op choices,
  argument and JSON-parse errors), the failure action/code/message/display,
  attempted-parameter keys and values, the location detail lines (values, keys,
  hashes) and the region.
- `safe_block` does the same but keeps newlines and tabs. It is reserved for
  multi-line text the toolkit itself authors: the library's `Prose` (attempted
  label, possible-cause and troubleshooting bullets, and the trailer, such as
  the location troubleshooting footer) and the complete text printed by
  `cloudwatch render`. No renderer can therefore emit a raw control other
  than the layout characters `\n` and `\t`. Caller-supplied text never goes
  through `safe_block`; it is escaped once at parse time (see "Caller text is single-line" below).

Intentional differences: a real AWS message that contains a newline now shows
`\n` on one line instead of breaking the `Error:` line, and a path or name that
contains a tab shows `\t` inside a diagnostic line. Streams and exit statuses are
unchanged: a failed check still prints on stdout and exits 1; unknown or
mistyped arguments still print on stderr and exit 2. The regression tests are
`tests_safety.rs` (unit) and the `#4634` section of
`crates/toolkit-cli/tests/cloudwatch.rs` (binary, fake-secret sentinels in the
environment, both channels inspected byte by byte).

### Caller text is single-line (#4652, #4653, #4662)

The rule: any text whose origin is the caller is rendered on one line with
`safe_text`, so its newlines appear as `\n` and it cannot start a forged
report line. Caller text here means argv values, JSON argument keys and
values (whatever their type or nesting), caller-supplied context or prose,
and AWS error messages.

**One escaping step, at parse time (#4662).** Rounds #4652 and #4653 escaped
named fields one by one, and #4662 then found eight more (the `create-success`
capture limits, copied as raw JSON into `CaptureLimits`). The rule is now
enforced in one place. `cloudwatch::cli::RENDERER_ARGUMENTS` lists every
renderer argument with its `Origin`, and `CHECK_ARGUMENTS` does the same for
`cloudwatch validate`. `Args::new` refuses any key the table does not list,
escapes the arguments by origin, and keeps only the escaped copy, so no
accessor can hand a renderer a raw caller string. Reading a key the table does
not list panics in debug builds.

| `Origin` | Arguments | Parse-time step |
|---|---|---|
| `Caller` | every report context scalar, `limits`, `attribute_filters`, `location`, `capture_*`, `now` and the other times, the whole `failure` object, `action`, `attempted`, the four prose fields, every `validate` argument | every string and object key, at any depth, through `safe_text` |
| `EachMember(spec)` | `status-assessment` `checks` | each member parsed by `CHECK_RESULT_ARGUMENTS`: `error` and `has_events` are `Caller`, `events` is `AwsResponse` |
| `JsonEncoded` | the `snapshot-search` / `snapshot-sample` context (service, environment, hash, filters, times, `query_string`, ...) | none at parse; the renderer prints them only inside `json.dumps` (`ensure_ascii`), which escapes every newline, control and bidi character |
| `AwsResponse` | `data`, `config`, `response`, `query_result`, `result`, `checks.*.events` | none at parse (manager ruling below) |

`JsonEncoded` stays verbatim because the toolkit builds a multi-line Logs
Insights query (`fields ...\n| filter ...`). Escaping it at parse would change
its JSON value; JSON encoding is its single escaping step.

Consequences, all intentional:

- A caller capture limit (or any other caller value) holding a newline now
  shows `\n` on its line. Clean values are byte-identical. Argument
  evaluation order is unchanged, so missing-argument and mistyped-argument
  errors are unchanged.
- A caller string nested in a list or dict that the legacy text prints with
  `str()` (for example an `attempted` list value) shows the escaped text
  inside the Python repr, which doubles its backslashes
  (`['a\\nb']`).
- `validate location-inputs` prints its payload JSON from the escaped
  values, so a file path containing a control character shows the escape
  text in that JSON. No real path contains one.
- In the library, `Prose` is trusted, tool-authored text and may span lines
  (the location troubleshooting trailer of `create_instrumentation` does).
  Through the CLI every `Prose` field comes from caller JSON, already
  single-line. A caller who passes a multi-line trailer sees it on one line
  with `\n` escapes, and no continuation-line marker is needed.
- AWS error messages embedded in reports stay single-line in the renderers
  themselves: the `batch-delete` `Errors[]` `ResourceArn` / `Code` /
  `Message` and the status `ErrorCause`.

The residual limit (manager ruling on #4380): AWS-structured response data
other than error messages (for example a `Description`, a location field
returned by the service, or a `NextToken`) is not reshaped line by line. Only
its terminal controls are escaped, by the final `safe_block` pass. Those
fields come from the service response, not from the caller.

Regression tests:

- `src/cloudwatch/tests_caller_text.rs` (unit, exhaustive). It walks
  `RENDERER_ARGUMENTS` over fixtures for every renderer. It fails if a
  renderer has no fixture or if no fixture sets one of the listed arguments
  (or nested check keys). Each string, number and boolean leaf, and each
  object key, under a `Caller` or `JsonEncoded` argument is replaced on its
  own: first by a token, then by ESC `[2J`, CR, LF, U+202E and a
  `FORGED_REPORT` marker. Then either the hostile report equals the benign
  report with the token replaced by its escaped form (`safe_text`, its repr,
  or the JSON escape for `JsonEncoded`), or the input is refused with a
  single-line diagnostic. No raw control and no forged line may appear. The
  test also asserts that all eight `create-success` limits reach the report.
- `crates/toolkit-cli/tests/cloudwatch.rs` (binary): the reviewer's `list`
  reproduction, all four prose reproductions, and the #4662 reproduction for
  each of the eight limits, checked byte for byte. It also runs the
  substitution test per text renderer (now with all eight limits) and the
  JSON-renderer check.

### Calendar construction (#4635)

`UtcTime::from_civil` now rejects a microsecond outside `0..=999999` with
Python's `datetime` text `microsecond must be in 0..999999` (checked after the
other fields, in `datetime`'s order). Previously the unchecked sum could carry
into the next second, or past year 9999, or overflow. No current command reaches
it with such a value; the boundary tests cover -1, 0, 999999, 1000000 and the
`i64` extremes at both calendar endpoints.

## New command surface

`toolkit cloudwatch render|validate|region` (documented in
`crates/toolkit-cli/README.md`) is new: the Python host scripts only run the
live operations, which stay with #4381 (including `--profile`, `--out` with its
0600 file mode, and `--print-contract`). The commands read their JSON arguments
through `--json` / `--json-file` and do not support stdin (`-`): they have no Python original, so the shared router refuses a bare `-`
(`cli-bare-dash.md`). An unknown,
missing or mistyped argument keeps the legacy exit status 2 through
`ToolkitError::with_exit_code`; a failed check keeps the legacy "text on
stdout, exit 1" contract through `Output::set_exit_status`.

## Safety

No AWS call, credential, profile or token is read or printed by any code in
this slice; the region command reads only `AWS_REGION` / `AWS_DEFAULT_REGION`.
The Logs Insights escapers and the 16-hex hash check are ported with their
injection-guard tests (`"` and `\` cannot break out of a string literal; a hash
containing a quote is rejected). Caller text in diagnostics is escaped (see
"Diagnostic text safety" above). Real captured snapshots are never fixtures.

## Verification

Offline unit tests in `crates/toolkit-cli/src/cloudwatch/tests_core.rs` and
`tests_render.rs` (whole-string expectations, boundary and negative inputs) and
the binary tests in `crates/toolkit-cli/tests/cloudwatch.rs`. Job ids and the
verified SHA are in the PIPELINE HANDOFF on Issue #4380.

## Follow-ups (filed as Issues, out of scope here)

- Live Application Signals / Logs clients, the gateway, the polling loop, the
  operation entrypoints and host-script flags: #4381.
- A lenient JSON mode (duplicate keys, `NaN`, big integers) for Python parity
  of snapshot messages, and ISO week-date parsing.
