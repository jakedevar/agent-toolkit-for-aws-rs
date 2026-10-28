# The bare `-` argument (Issue #4780)

argparse passes a lone `-` as an ordinary value, and a few Python originals use
it to mean standard input. Almost every slice turns a path argument straight into
a filesystem path, where an accepted `-` reads or overwrites a file literally
named `-`. The `toolkit` router therefore applies one policy, before any handler
and so before any filesystem access, to every registered value flag and
positional argument:

- The exact token `-` is a usage error (exit 2, `error: invalid arguments: <flag>
  does not accept - (...)`, one escaped line naming the flag, then the usage text)
  in both spellings (`--flag -` and `--flag=-`), for plain and repeatable flags,
  on `strict_values` and non-strict commands alike. A positional `-` is refused
  the same way. Nothing is read, written or created.
- `./-` stays a legal explicit literal path: `--output ./-`, `skills list ./-`.
- `-x`, `--` and other dash-prefixed words keep their previous behavior
  (unknown flags, or on `strict_values` commands refused values; a non-strict
  command still takes `-x` as a value).
- The policy lives in the router (`STDIO_DASH`, `value_allowed` in
  `crates/toolkit-cli/src/lib.rs`), not in the slices, so a new slice cannot
  bypass it and core regular-file helpers are unchanged. The contract test
  `an_unlisted_bare_dash_is_a_usage_error_and_touches_no_file`
  (`crates/toolkit-cli/tests/diag_contract.rs`) enumerates every registered
  command's value flags and positionals from `COMMANDS` and fails if any of them
  routes a `-` that `STDIO_DASH` does not list.

Assumption (engineering ruling): the policy covers every value flag, not only the
ones that name paths, because no registered non-path flag (numbers, enums, ids,
regions, ARNs, free text) has a Python original that takes a lone `-`, and a
registry cannot tell a path flag from a text flag without a per-flag declaration
a new slice could forget.

## Parameters that keep `-`

Only parameters whose Python original explicitly reads `-` as stdin are listed in
`STDIO_DASH`. Each reads at most 1 MiB (1048576 bytes) of UTF-8; more bytes, invalid
UTF-8 or a read error is a usage error (exit 2, the `ap.error` status of the
original), and an empty stdin is not a JSON document. `./-` is never opened.

| Command | Parameter | Python original |
| --- | --- | --- |
| `cloudwatch instrumentation OP` | `--json -` (arguments JSON text from stdin) | `di_instrumentation.py` `_read_payload` |
| `cloudwatch instrumentation OP` | `--json-file -` (arguments JSON from stdin) | `di_instrumentation.py` `_read_payload` |
| `cloudwatch snapshots OP` | `--json -` | `di_snapshots.py` `_read_payload` |
| `cloudwatch snapshots OP` | `--json-file -` | `di_snapshots.py` `_read_payload` |

No registered parameter writes to stdout for `-`. `cloudwatch render`, `cloudwatch
validate` and the rest of the `cloudwatch` commands are new surface or have no
stdin in the original, so they refuse `-`.

## The four reproductions

| Reproduction | Before | Now |
| --- | --- | --- |
| `elasticache dashboard --serverless demo --output=-` | truncated `./-` | exit 2, `./-` untouched or not created |
| `startup terraform-policy terraform --json -` | overwrote `./-` | exit 2 |
| `cloudwatch validate instrumentation-type --json-file -` | read `./-` | exit 2 (refused earlier by `strict_values` since #4743) |
| `skills list -` | enumerated `./-/...` | exit 2 (positional `-` refused since #4743) |

`tests/diag_contract.rs` (`the_4780_reproductions_refuse_dash_and_leave_a_file_named_dash_alone`,
`an_explicit_dot_slash_dash_is_still_a_literal_path`) runs the real binary with
complete valid companion arguments, with `./-` present (preserved byte for byte)
and absent (not created), in both spellings, and checks that `./-` still works as
an explicit literal.
