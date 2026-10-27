# toolkit CLI contract (bootstrap slice #4373, shared contracts #4374)

This documents the NEW `toolkit` command-line contract introduced by the Rust
workspace bootstrap. It is not a port of any existing Python CLI contract; the
Python inventory (#4371) records legacy contracts separately.

## Binary

The workspace builds one binary, `toolkit` (crate `toolkit-cli`), from the
shared library `toolkit-core`. `toolkit skills list` is read-only; `toolkit sync ...` (#4377)
writes only in its non-`--check` modes and only inside the tree it was given; the
`toolkit dynamodb` commands write only the files named in their section below. The
CLI performs no network access and no live cloud activity.

## Commands

```
toolkit skills list [ROOT] [--json]
toolkit sync plugin-skills [--root DIR] [--plugin NAME] [--check]
toolkit sync vendored [--plugin-root DIR] [--check]
toolkit msk size [options]
toolkit msk compat CLUSTER_CONFIG [--out-dir DIR]
toolkit msk sim compute|render|validate [options]
toolkit dynamodb calculate-costs --model FILE [--requirements FILE] [--output FILE]
toolkit dynamodb iterate-design --model FILE --config FILE --loop-state FILE [options]
toolkit --version
toolkit help
```

`-h` / `--help` and `-V` / `--version` work in place of the last two lines.
The `dynamodb` commands are described in their own section below.

### skills list

Enumerates *skills* under ROOT: every directory that directly contains a
`SKILL.md` file. `ROOT` is one positional path argument (absolute or relative to
the current directory) and defaults to `skills` in the current directory.

Discovery rules:

- Static symlinks are always refused: a symlinked directory is never followed
  and a `SKILL.md` that is itself a symlink is skipped (never read), whatever
  it points to. The walk does not defend against concurrent mutation of the
  tree during a run (see the last bullet of this list).
- Hidden directories (leading `.`) and the directories `node_modules` and
  `target` are skipped entirely, as entries and as descent targets. Directory
  names that are not valid UTF-8 are skipped silently.
- A directory containing `SKILL.md` is a skill; its subdirectories are still
  descended in case a nested skill exists.
- Metadata is read from the leading `---` frontmatter of `SKILL.md` (a leading UTF-8 byte-order mark and CRLF line endings are accepted): the `name`
  key falls back to the directory name (also when it is empty or only whitespace), and the `description` key falls back to
  the empty string. The supported frontmatter subset and its limits are below.
- Output is sorted by `name` (byte-wise), then by the ROOT-relative directory
  path as the tie-break. Equal names from different directories are therefore
  all reported, deterministically.
- ROOT itself is not eligible: only directories *under* ROOT are candidates, so
  a `SKILL.md` directly in ROOT is ignored (point ROOT at the parent to list a
  single skill). Decision recorded for #4374; pinned by tests.
- The `SKILL.md` read re-checks the opened file against the pre-open `lstat`
  (device and inode on Unix), so the *final* file swapped for a symlink or
  non-file between the directory scan and the read is skipped or refused.
  Other platforms keep the `lstat` check only.
- Concurrent mutation is out of the tool's model. Walking is pathname-based,
  so if an *ancestor* directory is renamed away and replaced with a symlink to
  an outside directory after it was listed but before it is opened, the
  `lstat`, open and listing all resolve through the swapped path and outside
  content could be reported. Do not run the tool against a tree that is being
  rewritten by an untrusted party. The public API (`discover_skills`,
  `resolve_within`, `read_regular_file`) is shaped so a handle-anchored walker
  can replace the implementation later if a slice needs adversarial
  containment.
- `SKILL.md` larger than 1 MiB, or not valid UTF-8, is a runtime error (exit 1)
  naming the file.

### Output

Plain mode prints one line per skill:

```
<name><TAB><description>
```

In this line every control character (newline, carriage return, tab, DEL, C1
controls) and U+2028/U+2029 in `name` and `description` is replaced with one
space each, so one skill is always exactly one line with exactly one tab (for
example a literal `name: |` block). Intentional change (#4374): the bootstrap
only flattened `\n` in the description. `--json` instead prints one pretty-printed JSON
array; each element is an object with exactly these string fields:

```json
[
  {
    "name": "skill-name",
    "description": "frontmatter description",
    "path": "category/skill-name"
  }
]
```

`path` is the skill directory relative to ROOT with `/` separators. Strings are
RFC 8259 escaped (quotes, backslashes, control characters as `\u00XX`). An
empty result prints `[]` in JSON mode and nothing in plain mode.

If the consumer closes stdout early (`toolkit skills list | head`), the write
error is treated as normal completion and the exit code stays 0 (it never
panics).

## Frontmatter subset

The reader is deliberately not a YAML engine (`toolkit-core/src/frontmatter.rs`).
Supported, per key (`name`, `description` only):

- Only keys at the top level of the `---` block, in `key: value` form, are read.
  Indented lines are treated as nested or continued content and never supply
  `name`/`description` — including the body of a block scalar belonging to
  another key.
- Inline values, with optional surrounding single or double quotes (stripped;
  no escape sequences inside quotes are interpreted).
- Block scalars `>` (folded; lines joined with a single space) and `|`
  (literal; lines joined with newlines), each optionally followed by chomping
  indicators (`-`, `+`), which are dropped.
- The block ends at a second `---` (or `...`); the scan is bounded (256 lines)
  so a missing closing marker cannot turn the body into metadata.

Observed unsupported forms — behavior, not bugs:

- Nested mappings (`metadata:` with indented children) are ignored; their keys
  do not become `name`/`description`.
- Escape sequences inside quotes are kept literally (`"a \"b\""` keeps the
  backslashes); doubled single quotes are not collapsed.
- Inline `value # comment` keeps the comment in the value.
- Multi-line plain (unquoted, non-block) scalars: continuation lines are
  dropped.
- Only the first `:` in a line splits key from value; quoted keys, anchors,
  tags, flow collections and multi-document documents are not supported and are
  ignored or misread only to the extent of falling back to the directory name
  and empty description.

These limits are intentional for this slice; anything not confidently parsed
falls back to the documented defaults instead of failing discovery.

## DynamoDB commands (slice `dynamo-cost`, #4389)

```
toolkit dynamodb calculate-costs --model FILE [--requirements FILE] [--output FILE]
toolkit dynamodb iterate-design --model FILE --config FILE --loop-state FILE
    [--manifest FILE] [--apply-change FILE] [--mode MODE] [--timestamp TS]
    [--yes-deploy] [--allow-spend] [--skip-deploy] [--calculator-only] [--dry-run]
```

Ports of `calculate_costs.py` and `iterate_design.py`; the parity table and
every intentional change are in `docs/migration/dynamo-cost.md`. Neither
command contacts AWS.

- `calculate-costs` prices a `dynamodb_data_model.json` (RRU/WRU, GSI write
  amplification, storage, vector indexes), writes the Markdown report to
  `--output` (default `cost_report.md` beside the model) and prints
  `Cost report written to <path>` and `Analyzed N access patterns across M
  tables.`. A missing/zero-pattern model, an unreadable model or an impossible
  vector design is a runtime error (exit 1); an unreadable `--requirements` file
  is a warning on stderr and the run continues with defaults.
- `iterate-design` runs ONE round of the design loop and stops. Exit codes keep
  the legacy script's: `2` for a missing input file or a refused
  `--apply-change`/`--skip-deploy`, `4` for a round that needs a real deploy
  without `--yes-deploy`, and the failed step's own status when a pipeline step
  fails. Pipeline steps run as `toolkit dynamodb <step>` commands through the
  command table; only `calculate-costs` exists in this slice, so a round that
  needs deploy/benchmark/report stops with
  `error: pipeline step ... is not available in this build` (exit 1) before any
  cloud action. `--calculator-only` is fully functional.
- Inputs are read through the symlink-refusing path contract (UTF-8, at most
  16 MiB) and outputs are written without following symlinks.

## Value options and diagnostics (#4389)

The `dynamodb` commands use the shared value-flag router (`value_flags`,
`strict_values: true`; `--name value` or `--name=value`, the value may not be
empty or start with `-`, the last repeat wins; read with `Invocation::value`)
and `Output::diagnostic` for the stderr warnings, both from #4376/#4377. The
only additions from this slice are `ToolkitError::with_exit_code`, which lets a
command keep a published non-default exit status (never `0`), plus the
symlink-refusing `write_regular_file` and `sha256_hex` in `toolkit-core`.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success, including help, version and an empty skill list. |
| 1 | Runtime error: ROOT does not exist, is not a directory, files under it could not be read (a `SKILL.md` that is larger than 1 MiB or not valid UTF-8 is such a file), or stdout failed with an error other than a closed pipe (for example a full disk: `error: cannot write to stdout: <os error>`). `error: <message>` is printed on stderr. |
| 2 | Usage error: unknown command, unknown flag, missing `list` subcommand, more than one ROOT argument, or an argument that is not valid UTF-8. Usage text is printed on stderr after `error: invalid arguments`. |

Help, version and empty results write to stdout only; errors write to stderr
only and never mix stdout/stderr.

## Routing and output contract (#4374)

Commands are registered in one table (`COMMANDS` in `src/lib.rs`): each entry
names the command words, the long flags it accepts, its maximum number of
positional arguments and a handler. Domain slices add rows; they do not
re-parse `argv`.

- `help`, `-h`, `--help` and `version`, `-V`, `--version` as the first word win
  over everything after them. Inside a command, `-h`/`--help` selects help; any
  other `-`-prefixed word (including a bare `-`) must be one of the command's
  flags or it is a usage error; a flag repeated is accepted once.
- Handlers return `Result<(), ToolkitError>`. `ToolkitError` has a class (usage
  = exit 2, runtime = exit 1), a stable machine code (for example
  `root_not_found`, `invalid_arguments`, `stdout_write`, `parse_error`,
  `schema_violation`, `path_symlink`) and a message. The CLI prints
  `error: <message>` on stderr, followed by a blank line and the usage text for
  usage errors only. `ToolkitError::to_json()` gives the structured form
  `{"error": {"kind", "code", "message"}}` for commands that later add JSON
  error output; no current command emits it.
- Stdout goes through one writer: a closed pipe stops output and exits 0, any
  other write or flush failure is `error: cannot write to stdout: <cause>` with
  exit 1 (previously such a failure exited 1 without a message).

## Shared library contracts (`toolkit-core`)

All dependency-free; AWS or other service SDKs must never be added to this
crate (use optional domain crates or features).

- **JSON** (`json`): typed `Json` value (objects keep insertion order), strict
  RFC 8259 parser (rejects trailing data, duplicate keys, trailing commas,
  leading zeros, lone surrogates, control characters, a byte-order mark and
  nesting deeper than 128) and deterministic compact/pretty writers. Integers
  outside `i64` read as floats; non-finite floats are written as `null`.
- **YAML subset** (`yaml`): block mappings/sequences, plain/quoted scalars,
  `|`/`>`/`|-`/`>-` block scalars and one-line flow collections, parsed into the
  same `Json` type. Everything else (tabs, anchors/aliases/tags, complex keys,
  duplicate keys, multiple documents, multi-line plain or quoted scalars,
  `|+`/indentation indicators, nesting deeper than 64) is a line-numbered
  error. The 64-level nesting budget is one budget for the whole document:
  block mappings, block sequences (including same-indent sequences) and flow
  collections all count. The anchor/alias/tag and reserved-indicator rules
  apply to flow keys and flow plain scalars too (`{&a k: v}`, `[]` are
  errors); quote the text to use it literally (`{"&a k": v}`). YAML 1.1 booleans (`yes`/`no`/`on`/`off`), leading-zero numbers, `+5`,
  hex/octal and `.inf`/`.nan` stay strings. See the module documentation.
- **Schema** (`schema`): a small structural check (scalar types, arrays,
  objects with required/optional members, unknown members rejected unless
  allowed) with `$.a[0].b` paths in deterministic order. The `--json` output
  of `skills list` has a published schema (`skills_output_schema`) that the
  tests validate real output against.
- **Paths** (`paths`): `normalize_relative` (rejects empty, absolute,
  drive-prefixed, `..`, backslash and NUL), `resolve_within` (refuses a static symlink
  at any component below the root; not a defence against concurrent mutation,
  see the discovery rules) and `read_regular_file` (no symlinks, only
  regular files, size-bounded, UTF-8, identity re-check after open).

## Frontmatter additions (#4374)

A leading UTF-8 byte-order mark is ignored. `name:` with no value or an empty
quoted value is reported by the parser as an empty string; skill discovery
treats an empty or whitespace-only `name` as absent and uses the directory name.

## manifests validate (#4375)

```
toolkit manifests validate [ROOT] [--plugin NAME]
```

Replacement for `python3 tools/validate.py [--plugin X]`; full rule parity table
and the recorded differences are in `docs/migration/manifests-validator.md`.
`ROOT` defaults to the current directory. The command is read-only and offline.

- Without `--plugin` it validates the Claude Code, Codex and Cursor marketplace
  manifests, every directory under `ROOT/plugins` (Claude, Codex, optional
  Cursor and Devin manifests, optional `.mcp.json`, the `SKILL.md` frontmatter
  of each direct child of `skills/`) and every `SKILL.md` below `ROOT/skills`.
  With `--plugin NAME` (or `--plugin=NAME`) only `ROOT/plugins/NAME` is checked.
- `Validating ...` progress lines go to stdout, in order. Each finding is a
  `  ERROR: <message>` line on stderr and the run continues.
- Success prints a blank line and `All validations passed.` (exit 0). Any finding
  ends with `error: Validation failed with N error(s).` on stderr (exit 1). An
  unknown or unsafe plugin name (`error: Plugin not found: NAME`) and an unusable
  root are runtime errors (exit 1); bad arguments are usage errors (exit 2).

The routing table gained value flags (`--name value` / `--name=value`,
`Invocation::value`, last value wins, missing value is a usage error) and
`Output::with_diagnostics` / `Output::diagnostic` for stderr lines (#4375).

## spec validate (#4376)

```
toolkit spec validate [ROOT]
```

Replacement for `python3 tools/validate_spec.py`; full rule parity table and
the recorded differences are in `docs/migration/spec-validator.md`. `ROOT`
defaults to the current directory. The command is read-only and offline; it has
no Python, PyYAML or Python `jsonschema` dependency. Schemas are evaluated by
the Rust `jsonschema` crate (local references only, no resolvers) with
`fancy-regex` under a backtracking limit; names are compared under exact NFKC
(`unicode-normalization`). A schema that cannot be compiled or evaluated is a
runtime error (exit 1), never a pass. Dependencies are listed and justified in
`docs/migration/spec-validator.md`.

- Checks every directory under `ROOT/plugins`: no symbolic links, `plugin.json`
  and `mcp.json` against the vendored schemas in `ROOT/tools/schemas/1.0.0`,
  the rules the schemas cannot express (path containment, placeholders, MCP URL
  and header rules) and the frontmatter, length and naming rules of every
  `plugins/*/skills/*/SKILL.md`.
- `Validating plugin: plugins/NAME` and `Validating skills` progress lines go to
  stdout. Each finding is a `  ERROR: <label>: <message> (at <path>)` or
  `  ERROR: <message>` line on stderr and the run continues.
- Success prints a blank line and `All Agent Plugins v1.0.0 checks passed.`
  (exit 0). Any finding ends with `error: Spec validation failed with N
  error(s).` on stderr (exit 1). An unusable root, `plugins/` directory or
  schema is a runtime error (exit 1); bad arguments are usage errors (exit 2).

## sync (#4377)

`toolkit sync plugin-skills` makes every `plugins/<plugin>/skills/<name>` copy
byte-identical to the canonical `skills/**/<name>` directory; `toolkit sync
vendored` does the same for each skill's `references/vendored` copies of
`skills/shared` and checks their README tables. `--check` reports drift and
writes nothing (exit 1 when out of sync); value flags accept `--flag VALUE` or
`--flag=VALUE`. Symbolic links are never followed; writes are atomic. The full
parity table against the Python tools, the deletion and symlink boundaries and
the telemetry seam are in [docs/migration/sync-contracts.md](../../docs/migration/sync-contracts.md).

Merge note (#4378): both slices extended the value-flag routing. The combined
parser keeps #4375's `Invocation::set_value` (last value wins) and #4377's
rule that sync value flags reject an empty value or one starting with `-`
(`--plugin-root --check` is a usage error) through the per-command
`CommandSpec::strict_values` switch: off for `manifests validate`, whose
`--plugin` takes the next word whatever it looks like, on for `sync`.

## msk commands (slice #4387)

`toolkit msk size`, `toolkit msk compat` and `toolkit msk sim compute|render|validate`
replace the three legacy MSK Python scripts. Their options, output formats,
exit statuses, parity evidence and intentional changes are documented in
`docs/migration/msk-contracts.md`.

Router: they use the canonical value-option router (`CommandSpec::value_flags`,
`Invocation::value`, #4376/#4378) with `strict_values: false` (the next word is
the value, as argparse does, so `--avg-data-in-mbs -5` reaches the range check),
and `Output::diagnostic` for stderr lines. The only router addition of this
slice is `Output::set_exit_status`/`exit_status`: a command whose published
contract prints regular output together with a non-zero status (`sim validate`
REJECT, `sim` JSON errors exit 2) cannot express that through `Err`, which
always reports on stderr as `error: ...`.
