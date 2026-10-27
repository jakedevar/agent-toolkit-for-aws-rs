# Startup fixture assertion programs (#4408)

Slice `startup-fixtures`, wave 2 of programme #4369. Base: the integration head
`dcd4714791aac1d77286b3a4f3a7d3e95c298f5c`, which already holds the startup validators
(#4399: `startup migration-report`, `startup emit-plan`, link-safe reads), the Heroku report
validator (#4406: `startup heroku-report`) and `bedrock-report` (#4405). Nothing covered the
`check_expected_*.py` programs (`git log -S check_expected -- crates docs` finds only the
#4406 note that names `heroku-decision-gate/check_expected_decide.py` as a caller left to
this slice), so all sixteen are ported here; the earlier commits are reused, not duplicated:
the decide programs call `startup migration-report`, `startup heroku-report` and
`startup emit-plan` in memory, and the tests use `startup/scoring/fixtures.rs`.

## Why one command and not tests

The Python programs are not test-only: their READMEs tell a person or a replay harness to run
`python3 check_expected_*.py <run-dir>` against a **fresh** run (`/tmp/azure-probe/.migration/...`,
`/path/to/.migration/MMDD-HHMM`) as well as against the committed golden trees. They are therefore
a command, `toolkit startup check-expected KIND RUN_DIR [SEED_DIR] [--plugin-root DIR]`, with one
`COMMANDS` entry (`value_flags: ["--plugin-root"]`, `strict_values: true`, `passthrough: false`,
`repeatable_flags: &[]`; no Python `append` flag exists, so `diag_contract` needs no change) and one
README section. The golden trees and `expected-*.json` documents are fixture **data** and stay where
they are; the executable checks are Rust in `crates/toolkit-cli/src/startup/fixtures/`.

## Result

| Python file | KIND | Rust |
| --- | --- | --- |
| `heroku-decision-gate/check_expected_decide.py` | `heroku-decide` | `decide.rs` |
| `heroku-decision-gate/check_expected_decide_with_retained_pack.py` | `heroku-decide-retained` | `decide.rs` (the wrapper only re-ran the same program) |
| `gcp-decision-gate/check_expected_decide.py` | `gcp-decide` | `decide.rs` |
| `heroku-workshop/check_expected_workshop.py` | `heroku-workshop` | `workshop.rs` |
| `gcp-workshop/check_expected_workshop.py` | `gcp-workshop` | `workshop.rs` |
| `heroku-nonweb-scaling/check_expected_nonweb_scaling.py` | `heroku-nonweb-scaling` | `nonweb.rs` |
| `heroku-live-capture/check_expected_drift.py` | `heroku-drift` | `capture.rs` |
| `heroku-live-capture/check_expected_estimate.py` | `heroku-estimate` | `capture.rs` |
| `gcp-live-capture/check_expected_drift.py` | `gcp-drift` | `capture.rs` |
| `gcp-live-capture/check_expected_baseline.py` | `gcp-baseline` | `capture.rs` |
| `azure-iac-terraform/check_expected_iac_terraform.py` | `azure-discover` | `azure/discover.rs` |
| `azure-iac-terraform/check_expected_clarify.py` | `azure-clarify` | `azure/clarify.rs` |
| `azure-iac-terraform/check_expected_clarify_complete.py` | `azure-clarify-complete` | `azure/clarify.rs` |
| `azure-iac-terraform/check_expected_clarify_fast_path.py` | `azure-clarify-fast-path` | `azure/clarify.rs` |
| `azure-iac-terraform/check_expected_design.py` | `azure-design` | `azure/design.rs` |
| `azure-iac-terraform/check_expected_estimate.py` | `azure-estimate` | `azure/estimate.rs` |

Shared machinery: `ledger.rs` (the assertion ledger, Python `dict.get` views and a typed window on the
expectation documents) and `mod.rs` (dispatch, reads, report). Python `==`, truthiness, `repr` and
`json.dumps` reuse `manifests::{py_eq, is_falsy, py_repr}` and
`startup::artifacts::pyjson` (`dumps_unicode` was added there for the estimate program's
`ensure_ascii=False`).

## Acceptance criteria

- **AC1.** One program per Python file (table above). `tests.rs` asserts that the sixteen shipped
  files are exactly the registered programs, and every committed golden tree
  (`after-decide-complete`, `retained-execution-pack`, `after-arm64-reprice`, `after-graviton-reprice`,
  `after-design` of nonweb scaling, and the six `azure-iac-terraform/after-*` trees) passes its
  program. The drift, baseline and estimate scenarios of the two `*-live-capture` sets ship their
  captured *inputs* but no replay *output* (it is model-produced), so the tests build a passing run
  from the committed `expected-*.json` offline.
- **AC2.** Fixture data (`expected-*.json`, golden trees, captures) is untouched; the executable
  checks are Rust. The Python files remain as migration evidence.
- **AC3.** For every program there are tests that damage a copy of the fixture (a wrong phase, a
  dropped service, a leaked secret, a non-reconciling total, a deleted file...) and assert exit 1
  with the specific message, plus fail-closed tests for an empty or missing run directory (all
  sixteen programs), invalid JSON, a symbolic link, and a malformed, invalid or missing expectation
  document.

## Compatibility and deliberate differences

1. **One command, a `KIND` word.** The sixteen scripts located their fixture directory from
   `__file__`. The command reads `<plugin-root>/fixtures/<set>/` (`--plugin-root`, default `.`, as
   the other `startup` commands do). The azure Clarify base program took an optional second
   argument naming an expectation file; the three kinds select the three committed sets instead, so
   a caller-supplied file name never reaches a path join.
2. **Output.** `PASS`/`FAIL` text, `note:` lines and exit 1 on failure are kept on stdout; the
   command additionally ends a failing run with the standard `error: N assertion(s) failed for KIND`
   line on stderr. Usage mistakes (Python `print(__doc__)` and status 2) are the router's usage error.
   `gcp-baseline`'s missing-input line is `FAIL (1):` / `  - neither ...` instead of the one-line
   `FAIL: neither ...`.
3. **Fail closed instead of a traceback.** Python raised `KeyError`, `TypeError` or
   `JSONDecodeError` on a malformed expectation or run document (exit 1 with a traceback), or on a
   missing required key. Here a missing, mistyped or unparsable document is a recorded failure and a
   key the Python program would have indexed with `[...]` is checked, so a fixture that cannot be
   evaluated never passes. Optional files that Python probed with `.exists()` stay optional.
4. **Symlink-refusing reads.** Every run, seed, expectation and referenced skill file is read with
   `startup::{is_regular_file_under, read_text_under}`: a link at any component is a failure, and a
   directory is not a file. The scratch copy for `emit-plan` refuses links instead of following them
   (`shutil.copytree` followed them) and is removed on drop.
5. **Bounded, escaped echo.** Assertions read every run value in full (`str(v)` / `repr(v)` are
   never shortened, #4746); only a recorded message is bounded, to 1200 characters with a
   `...[N more chars]` suffix. Every report line goes through `neutralize_line` (control,
   bidirectional and invisible characters become escapes; the line cannot be forged). Parse
   failures name the file and position, never content. The messages that name a secret marker name
   the marker, not the value.
6. **No subprocess.** The decide programs ran `validate-migration-report.py`,
   `validate-heroku-migration-report.py` and `emit-plan-json.py` through `sys.executable`; they now
   call the Rust handlers that replace those scripts in memory. A validator failure shows the first
   twelve lines of its output.
7. **Regular expressions.** The azure ID patterns come from the expectation document and are matched
   with `fancy-regex` anchored at the start (`re.match`); the markdown-table scans of the design
   program (`` `azurerm_..` ``, `` `Microsoft...` ``, `` `name.md` ``, the warning-code table) are
   hand-written scanners with the same token rules.
8. **Repairs after review #4744 (fail closed beats Python parity).** Where the Python program was
   laxer or silent, the Rust check fails and says why:
   - *Empty documents* (#4747). An empty or whitespace-only run or expectation document, or a
     falsey JSON root (`{}`, `[]`, `null`, `false`, `0`, `""`), is a named failure. The Azure estimate
     program printed a bare `FAIL`; here it names the file. A program that stops before it recorded
     any finding reports `KIND stopped before evaluating its assertions` and exits 1, never `PASS`.
   - *Warnings keep their type* (#4748). `warnings`, `pricing_source`, `capture_warnings` and
     `excluded` are serialised as supplied (`est.get("warnings", [])`: only an absent key reads as
     the default), so a forbidden warning is found whatever its JSON shape. `aws-design.json`
     `services` that is neither absent nor a list is a failure.
   - *Booleans are never numbers* (#4753). A boolean `monthly_difference` or `annual_difference`
     fails the reconciliation, as the Python `close()` guard did, instead of reading as 1/0.
   - *Explanations are text* (#4754). A cost-line `basis` and a `$0.00` cluster `note` must be
     non-blank strings; a truthy value of another type is reported (`... must be explanatory text,
     not bool`) instead of counting as an explanation.
   - *Derived paths are link-checked* (#4759). Every read below `--plugin-root` that the command
     derives itself (the `fixtures` directory, the fixture set, golden trees, table directories,
     shipped skill files, `.claude-plugin/plugin.json`, the estimate handed to the report
     validator) is checked component by component from the plugin root. The explicit `--plugin-root`,
     RUN_DIR and SEED_DIR keep their own caller-chosen-root semantics (the root itself may be a
     link).
   - *Reads are bounded and a bound hit is an error*. Every file read uses the shared 64 MiB
     limit of `startup::read_text_under` / `read_regular_file`; a larger file is an unreadable-file
     failure, never a silently shortened input.

## Callers (left in place; retired by #4415)

No CI workflow, `mise` task or skill document runs these programs. Callers that remain:
`fixtures/README.md`, the READMEs of `azure-iac-terraform`, `heroku-workshop`,
`heroku-nonweb-scaling`, `heroku-live-capture`, `gcp-live-capture` and `gcp-workshop` (each tells the
reader to run `python3 check_expected_*.py`), `tests/test_azure_clarify_fast_path_contracts.py`, which
imports `check_expected_clarify.py` as a module, and a comment in
`skills/heroku-to-aws/scripts/test_validate_heroku_migration_report.py`. The equivalent invocation is
`toolkit startup check-expected KIND RUN_DIR`. The Python files are not deleted here.

## Tests

`startup/fixtures/tests.rs` (registry coverage, README, usage, fail-closed, hostile input, symlinks,
`tests/repair.rs` for the #4744 repairs,
malformed expectations), `tests/decide_workshop.rs`, `tests/capture.rs`, `tests/azure.rs`, plus unit
tests in `ledger.rs`, `azure/clarify.rs`, `azure/design.rs` and `azure/estimate.rs`. They run offline
against the committed trees and temporary copies; the `diag_contract` integration test covers the
command automatically through `COMMANDS`.
