# Heroku migration report validation and plan emission (#4406)

Slice `heroku`, wave 2 of programme #4369. Base: integration head `fb6a9db9`,
which carries the startup validators of #4399 (migration-report validator, the
Python word-class adapter `rx`, link-safe reads and `startup emit-plan`).
Verification evidence lives in the Issue #4406 handoff, not in this file.

## Scope and result

| Python file | Rust replacement |
| --- | --- |
| `skills/heroku-to-aws/scripts/validate-heroku-migration-report.py` | `toolkit startup heroku-report` (`crates/toolkit-cli/src/startup/report/heroku/`) |
| `.../test_validate_heroku_migration_report.py` | `heroku/tests.rs` (structure, verdict, flip list, cost-optimization, accessibility, sections, decision mode, shipped decision-gate fixtures) |
| `.../test_validate_heroku_migration_report_currency.py` | `heroku/tests.rs` (currency section) |
| `.../test_validate_heroku_migration_report_cost.py` | `heroku/tests.rs` (cost-figure section) |
| `skills/heroku-to-aws/scripts/emit-plan-json.py` | `toolkit startup emit-plan`, already shipped by #4399: the file is byte-identical to `plugins/aws-startup-advisor/scripts/emit-plan-json.py` (`cmp` verified), so no second emitter exists (AC2) |

The Python files and every caller stay in place; their retirement, with the
caller updates in the skill documents and `fixtures/heroku-decision-gate/check_expected_decide.py`,
is #4415. Until then the callers keep working unchanged. This slice adds the
Rust command, its README section (`crates/toolkit-cli/README.md`, "startup
heroku-report") and this file (AC3); it does not edit shipped skill content.

## Command

```
toolkit startup heroku-report REPORT [--migration-dir DIR] [--mode full|decision]
```

Routing: one `COMMANDS` entry (`path: ["startup", "heroku-report"]`,
`value_flags: ["--migration-dir", "--mode"]`, `strict_values: true`,
`max_positionals: 1`, `passthrough: false`). `strict_values` is on because a
directory or mode starting with `-` is never intended; argparse parity for
`--flag=VALUE` and `--flag VALUE` comes from the router. A missing REPORT or an
invalid `--mode` is a usage error (exit 2, as argparse); every other failure is
exit 1. Output matches the Python messages:

* pass: `REPORT_OK | structure=complete | sections=N/N[ | optional=what-if-scenarios]` on stdout;
* fail: `REPORT_FAIL | file=PATH | errors=N`, one `  - problem` line per problem on stderr,
  then the router's `error: N report problem(s)` summary line (the same convention as
  every other `startup` validator);
* missing report: `REPORT_FAIL | file=PATH | reason=not_found` on stderr (exit 1).

## What is shared with `startup migration-report`

The Heroku validator is a thin subset of the full one and reuses its building
blocks instead of copying them (AC2):

* the `html.parser` / `html.unescape` port (`html.rs`), the `fancy-regex` Python
  word-class adapter (`rx.rs`) and `data-cost-key` anchor collector (`walk.rs`);
* the cost-figure cross-check: `figures::cost_figures` became a thin wrapper over
  `figures::check_cost_figures(html, estimate, anchors, required, exec_costs)`, which the Heroku
  command calls with its one anchor (`aws_monthly_balanced`) and the `exec-costs` fragment of
  its own section parser;
* the currency patterns (`style::cents_pattern`, `rate_suffix_pattern`,
  `whole_dollars_at_least_threshold`, `chars_after`) and `readability_scope`
  (`_body_scope`);
* the flip-list matching: `decision::rendered_list_after_heading` and the phrase
  matcher, which now take the phrase normalizer as a parameter
  (`phrase_rendered_by`, `unmatched_phrases_by`; the full validator keeps `plain_text`, the
  Heroku one uses `walk::normalize_phrase`, which is the tail of `plain_text` without the
  markup re-parse) and `Rendered`, which gained a phrase mode;
* `decision::scenario_len` (`len(index.get("scenarios") or [])`).

New in `heroku/parsers.rs` because the Python classes differ from their GCP
namesakes: the rendered `<section>` index (counts and fragments, inert and
comment content excluded), the decoded-text runs (table cells are block-level
here and there is no Calculation/Notes column), the rendered start tags
(`lang`, `class`), the populated-opportunity-row scan and the accessibility
scan (parser based here, regex based in the full validator).

## Deliberate differences from the Python

1. **Untrusted values are escaped and bounded in diagnostics (#4602).** Values read from the
   report path, `--mode`, `estimation-infra.json` (the non-numeric figure and the unmatched flip
   conditions) and the JSON error text are written with control characters, line and paragraph
   separators and bidirectional or zero-width format characters as `\n`, `\r`, `\t` or
   `\u{XXXX}` escapes and cut at 120 characters with `...`; the report path and the file-system
   messages (which end in the reason, for example "symbolic links are not followed") get the same
   escapes without the length cut. Ordinary text is unchanged, so every
   Python message is still produced for ordinary input. The shared cost-figure message of the
   full validator gets the same treatment (`values::untrusted`).
2. **Symbolic links are refused for validator-owned reads.** The report file itself and each
   artifact below `--migration-dir` (`estimation-infra.json`, `scenarios/index.json`,
   `.phase-status.json`, and a link at any directory component) are never followed. Python's
   `is_file()` / `read_text()` follow links. A link is reported as a problem
   (`cannot read PATH: symbolic links are not followed`, exit 1); the linked content is never
   read. This is the one place the validator fails closed where Python read through the link.
3. **Unreadable artifacts are problems, not silent skips or crashes.** Python fails open only on
   `OSError` / `JSONDecodeError`; a non-UTF-8 file raised `UnicodeDecodeError` (a traceback,
   exit 1). The Rust command reports an artifact it cannot read (link, I/O error, invalid
   UTF-8, over 64 MiB) as one problem line. A *corrupt* estimate (invalid JSON, or a JSON value
   that is not an object) still fails open exactly as in Python.
4. **A non-object estimate or `.phase-status.json` no longer crashes.** Python raised
   `AttributeError` for a truthy non-object (`[1]`, `"x"`) in the verdict and phase checks; Rust
   treats it as carrying no `recommendation` / `phases.generate`. `phases.generate` is compared as
   a string, as Python did for the two blocking values.
5. **`.phase-status.json` messages.** The corrupt-state message keeps its text but quotes the JSON
   error position (`Expecting value: line 1 column 1 (char 0)`), as `json.JSONDecodeError`
   does, instead of the Python exception object; `generate_status!r` is printed as `'completed'`
   (Python repr of a string) by construction.
6. **Section fragments re-escape instead of replaying raw text.** Python reads the document with
   `convert_charrefs=False` and re-emits references verbatim; the shared tokenizer decodes text, so
   fragments re-escape `&`, `<` and `>`. Every later parser decodes again, so the rendered text is
   identical. The one observable difference is `_what_if_column_errors`, which matches words in
   the *raw* `<thead>` text: an entity name such as `&alpha;` could satisfy `"ha" in text` in
   Python and no longer does.
7. **`emit-plan-json.py` default `producerVersion`.** The skill-local copy derived its default
   plugin manifest path from its own location (`skills/heroku-to-aws/`, which has no
   `.claude-plugin/plugin.json`), so a call without `--plugin-json` omitted `producerVersion`.
   The skill documents always pass `--plugin-json`, and `startup emit-plan` takes `--plugin-root`
   or `--plugin-json` explicitly; the `PLUGIN_ROOT`-relative default is not reproduced.
8. **Report path.** `REPORT` must be a regular file; a link is an error (exit 1) rather than
   read through. A non-UTF-8 report is exit 1, as the Python traceback was.

## Callers (left in place; retired by #4415)

`SKILL.md`, `references/phases/{generate/generate.md, generate/generate-report.md,
generate/generate-assemble.md, estimate/estimate-assemble.md, workshop/workshop.md}`,
`references/shared/report-decision-core.md` and
`fixtures/heroku-decision-gate/check_expected_decide.py` invoke
`python3 .../validate-heroku-migration-report.py` and `.../emit-plan-json.py`. The equivalent
invocations are `toolkit startup heroku-report REPORT --mode decision --migration-dir DIR` and
`toolkit startup emit-plan --migration-dir DIR --plugin-json FILE`.

## Tests

`heroku/tests.rs` runs the command end to end through the router (`crate::run`) on inline
fixtures and the checked-in `heroku-decision-gate` fixtures: every case of the three Python test
files (the packaging assertion becomes "the Python validator is still in the skill directory"),
plus usage errors, the not-found and summary lines, link refusal for the report, the estimate, the
scenario directory and `.phase-status.json`, escaped hostile values (ESC, CR/LF, over-long
values), decision-mode phase states (pending, absent, completed, in progress, corrupt, empty,
non-object) and parser unit cases (nested and unclosed sections, decoded boundaries, `lang`,
class tokens, accessibility indexes). All tests are offline and deterministic.
