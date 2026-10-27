# toolkit CLI contract (bootstrap slice #4373, shared contracts #4374)

This documents the NEW `toolkit` command-line contract introduced by the Rust
workspace bootstrap. It is not a port of any existing Python CLI contract; the
Python inventory (#4371) records legacy contracts separately.

## Binary

The workspace builds one binary, `toolkit` (crate `toolkit-cli`), from the
read-only shared library `toolkit-core`. The CLI performs no writes, no network
access and no live cloud activity.

## Commands

```
toolkit skills list [ROOT] [--json]
toolkit --version
toolkit help
```

`-h` / `--help` and `-V` / `--version` work in place of the third line.

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
