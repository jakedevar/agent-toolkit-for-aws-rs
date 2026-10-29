# AI payload templates (#4386, slice `ai-payloads`)

Parity document of the `ai-payloads` slice. The design is
[ai-payloads-design.md](ai-payloads-design.md) (accepted at pre-review #5119,
round 3). This file records what has landed, the command contract and the
decisions taken while implementing, slice by slice. It grows with each slice;
only slice S1 is implemented so far.

The 28 canonical Python files of the slice (and their 28 `plugins/` mirrors)
**stay in place**: 23 are client-side templates that the customer's own Python
kernel runs, four are AWS Lambda reward-function payloads and one is a
reference pattern (design sections 1 to 3). Nothing here removes Python rows
(0 of 56 removed) and nothing here runs a template.

## Slices

| Slice | Content | Status |
| --- | --- | --- |
| S1 `ai-payloads-model` (#5143) | `template.rs` (grammar, lexer, classification), `toolkit ai-payloads templates` with `--json` and `--check`, registry line, this document | implemented |
| S2a `ai-payloads-publish` | `place.rs`: no-replace publication | not started |
| S2b `ai-payloads-render` | typed substitution, `assign`, notebook and script output, `render` | not started |
| S3 `ai-payloads-reward` | `reward sample` and `reward check` | not started |
| S3r, S4, S5 | reward-template fix, caller prose, inventory | not started |

## Commands

```text
toolkit ai-payloads templates [--root DIR] [--json] [--check]
toolkit ai-payloads render TEMPLATE --mode notebook|script --project-dir DIR [--name NAME] [--values FILE] [--cell N=FILE]... [--append | --new] [--consent NAME]... [--allow-unresolved] [--dry-run]
```

`render` accepts typed values and literal assignments from a JSON file, writes
new notebooks or numbered scripts through atomic no-replace publication, and
appends by publishing a versioned copy. `--cell` checks consent identifiers as
a syntactic guard; it is not a sandbox. `--dry-run` writes nothing. The reward
command is specified in design section 4.6.

### `toolkit ai-payloads templates`

Lists every `ROOT/references/*/code_templates/*.py` file, sorted by relative
path, and reads each one as text. The command never writes and never runs a
template.

| Option | Meaning |
| --- | --- |
| `--root DIR` | The skill directory that holds `references/`. Default: `skills/core-skills/aws-ai-ml` relative to the working directory (the repository root). The `plugins/aws-core/skills/aws-ai-ml` mirror gives the identical catalog. |
| `--json` | Print the catalog as one JSON document instead of text. |
| `--check` | Lint the grammar of design section 4.3 and exit 1 on any violation, or when no template is found. Alone it prints violations and a summary line; with `--json` it prints the catalog. |

`--root` names a directory, so a bare `-` is refused with a usage error (exit
2) in both spellings like every path flag of the toolkit
(`docs/migration/cli-bare-dash.md`); `./-` names a directory called `-`. The
command takes no positional argument and no `LITERAL_DASH` or `STDIO_DASH`
entry.

Input rules, all failing with exit 1 and a message on stderr, and nothing on
stdout:

- the root is not a directory, or has no `references/` directory;
- a symbolic link where the command descends below the root
  (`references/<dir>` that points to a directory, or `<dir>/code_templates`;
  the root itself may be a link);
- a template that is a symbolic link, is not a regular file, is larger than
  1 MiB, is not UTF-8, or cannot be read;
- more than 512 templates.

Files that are not `*.py`, directories without `code_templates/`, and files
directly under `references/` are ignored. An empty result is a successful
listing (exit 0) and a failed `--check`.

Exit codes: 0 success; 1 data, refusal or runtime error (including a
`--check` violation); 2 usage error (shared router).

### Text catalog

One block per template, blank line after each, then `templates: N`:

```text
dataset-transformation/code_templates/transformation.py
  title: Dataset Transformation
  cell 0 markdown: Dataset Transformation
  cell 1: Configuration
  cell 7: Plot and Display Metrics [NOTEBOOK_ONLY_SECTION]
  slot INPUT_LOCATION kind=string code=1 sentinel=0
  slot EXTRA_PARAMS kind=string code=0 sentinel=0 comment-only
  target BASE_MODEL cell=2 line=30 kind=string literal=""
  manual cell=4 line=77: accept_eula=ACCEPT_EULA, # Uncomment for Meta models
  violation <code> line=<n> column=<n>: <message>
```

`kind` is `secret` for a name with a `_`-delimited segment `TOKEN`, `SECRET`,
`PASSWORD`, `CREDENTIAL` or `CREDENTIALS`, otherwise `value` when the name has a
bare occurrence in code, otherwise `string` (also for a name that only occurs in
comments, which is then marked `comment-only`). `code` counts quoted and bare
occurrences; `sentinel` counts the comparison operands that are never
substituted. Template-controlled text (titles, labels, file names, literals) is
passed through `neutralize_line`, so a control or bidi character shows as
`\uXXXX`.

With `--check` and no `--json` the output is one `path:line:column: code:
message` line per violation, then either `ok: N templates satisfy the template
grammar` or `check failed: V violations in M of N templates`.

### JSON catalog

```json
{
  "ok": true,
  "templates": [{
    "path": "finetuning/code_templates/sft.py",
    "title": "Fine-Tuning",
    "cells": [{"number": 0, "label": "Fine-Tuning", "markdown": true,
               "notebook_only_section": false, "line": 3, "lines": 0,
               "notebook_only_lines": 0, "text": ""}],
    "placeholders": [{"name": "PROJECT_DIR", "kind": "string", "code": 1,
                      "comment_only": false, "sentinel": 0}],
    "occurrences": [{"name": "PROJECT_DIR", "context": "quoted", "line": 117,
                     "column": 32, "quote": "\""}],
    "targets": [{"name": "BASE_MODEL", "cell": 2, "line": 30,
                 "kind": "string", "literal": "\"\""}],
    "manual_edits": [{"cell": 4, "line": 77, "text": "..."}],
    "violations": [{"code": "cell_gap", "line": 9, "column": 0, "message": "..."}]
  }]
}
```

`ok` is true when at least one template was found and none has a violation.
`--json` alone exits 0 whatever `ok` says; with `--check` it exits 1 when `ok`
is false. `lines` is the section body after trimming blank lines at both ends,
`text` (markdown cells only) its comment lines without the `# ` prefix.
`occurrences[].context` is `quoted`, `bare`, `comment` or `sentinel`;
`column` is the 1-based character column of the opening quote (quoted and
sentinel) or of the `[`.

## Grammar and lint rules (design section 4.3)

| Rule | Behaviour | Violation codes |
| --- | --- | --- |
| 1 Marker | `# Cell <n>` + optional ` [markdown]` + `: ` + non-empty label, at column 1; numbers start at 0 and increase by one; Cell 0 is `[markdown]`, no other is. A trailing `# NOTEBOOK_ONLY_SECTION` (any spacing around the hash) is a flag, not label text. `# Cell 2 is ...` and a marker that is not at column 1 are text. | `no_cells`, `cell_zero_missing`, `cell_zero_not_markdown`, `cell_gap`, `cell_duplicate`, `cell_markdown_late`, `cell_label_empty` |
| 2 Section | From the line after a marker to the line before the next; blank lines at both ends trimmed. Text before the first marker belongs to no cell. | |
| 3 `NOTEBOOK_ONLY` | A line ending in `# NOTEBOOK_ONLY` (trailing whitespace aside) is counted per cell (`notebook_only_lines`); dropping it in script mode is slice S2b. | |
| 4 Placeholders | `[` + `A-Z` + `[A-Z0-9_ ]*` + `]`, classified by a Python lexer (strings of every quote kind, triple quotes, prefixes, comments, bracket depth). Quoted slot: a plain string whose whole body is the placeholder. Bare slot: the placeholder as an expression token, not an index of a value (`CONFIG[KEY]`). Comment. Sentinel: a quoted slot that is an operand of `==` or `!=`, or a direct element of the bracketed right operand of `in` (T17:39). A placeholder inside any other string (triple quoted, f-string, prefixed, longer) is a lint error. | `placeholder_in_string`, `unterminated_string` |
| 5 Secrets | Segment match on the name, see above. | |
| 6 Typed bare slots | A bare slot needs a row in the type table of section 4.4.1. Until `types.rs` (S2b) the table's name column is `TYPED_BARE_SLOTS` in `template.rs`; the nine names of the real templates are listed there. A name that is both a quoted and a bare slot in one template is also an error. | `untyped_bare_slot`, `mixed_slot_kinds` |
| 7 Source text | LF or CR LF line ends (CR LF is read as LF), tab allowed; any other character for which `is_unsafe_char` is true, a lone CR and a leading BOM included. The diagnostic gives line, column and `U+XXXX` and never the line. | `source_char` |

### Targets and manual edit points (design section 4.4.3)

`targets` is the list `assign` (S2b) will accept: top-level `N = <literal>`
statements in a non-markdown cell where `N` is assigned exactly once in the
template (`=`, `+=`, a tuple or chain target and an indented assignment all
count, a keyword argument does not), in the single-line form or the
parenthesized three-line form, and the literal is a plain string, an integer,
a float, `True` or `False`.

Decisions taken in S1 where the design is silent:

- a string with an `r`, `b`, `u` or `f` prefix, a triple-quoted string, an
  implicit concatenation and a negative number written with a space are not
  targets (no real template has one; an `assign` rewrite would have to
  reproduce the escape rules of each);
- the lexer is not a full Python tokenizer: it does not understand the nested
  same-quote replacement fields of Python 3.12 f-strings (`f"{x["a"]}"`), which
  no real template uses; such a string would be mis-split and reported
  (`placeholder_in_string` or `unterminated_string`) rather than accepted;
- `None` literals are never listed (the design admits them only with a type
  row, and none exists);
- `line` is the line of the `N =` statement, also for the parenthesized form;
- `manual_edits` lists every comment, whole-line or trailing, that contains
  `NOTE FOR AGENT` or `Uncomment` (27 lines in 7 templates), with the first 60
  characters of the comment text. A marker inside a string is not a comment.

## What the tests pin

`crates/toolkit-cli/tests/ai_payloads.rs` runs the real `toolkit` binary:
AP-TPL-01..12 (grammar), AP-SUB-01..07 (lexer and classification), AP-CAT-01..04
and 06/07 (the 23 real templates, a golden catalog in
`crates/toolkit-cli/tests/data/ai_payloads/catalog.txt` that was derived by an
independent script, mirror equality, targets and manual edit points) and
AP-CLI-01..03 (route, help, document, exit codes, filesystem refusals). The
shared contracts `diag_contract.rs`, `recorded_contract.rs` (slice
`ai_payloads` is `NotRecorded`: it reads local template files only) and the
registry tests cover the new route too.

## Intentional behaviour changes

None yet: S1 adds a read-only command. Design section 6 lists the changes that
the later slices bring (B-1 to B-16).

## Context carried into later slices

- **Consent boundary (#5142).** S1 freezes only the *template's* consent
  literals: `ap_cat_02_every_consent_literal_in_the_real_templates_is_false`
  fails if any `ACCEPT_EULA` literal of the 23 templates stops being `False`,
  or if the bare slot moves out of T15. It says nothing about notebook cells a
  user already saved or about `--cell` source. S2b must scope the emission
  invariant of design section 4.4 to newly rendered typed slot and `assign`
  sites and to the protected template literals, keep historical cells of an
  `--append` source untouched (a saved `ACCEPT_EULA = True` needs no fresh
  `--consent`), and state that the `--cell` identifier check is a syntactic
  guard, not a sandbox for arbitrary Python.
- **T28 module cell (#5124).** `transformation.py` is catalogued like any
  template (Cell 2 is its generated module cell, its comment mentions
  `transform_fn.py`). The module-cell table, `module_path_mismatch` and the
  frozen notebook-mode import of D-5 and P-8 belong to S2b
  (AP-CAT-11, AP-T28-*); S1 changes nothing about them.
- **Not in `templates`.** The four Lambda reward templates and `mlflow_reference.py`
  (T06 to T10) live outside `code_templates/` and are not listed; the reward
  checker is slice S3.
