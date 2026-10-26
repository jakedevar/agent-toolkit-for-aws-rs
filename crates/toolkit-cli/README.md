# toolkit CLI contract (bootstrap slice #4373)

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

- The walk stays strictly inside ROOT. Symlinked directories are never
  followed, and a `SKILL.md` that is itself a symlink is skipped (never read),
  whatever it points to.
- Hidden directories (leading `.`) and the directories `node_modules` and
  `target` are skipped entirely, as entries and as descent targets. Directory
  names that are not valid UTF-8 are skipped silently.
- A directory containing `SKILL.md` is a skill; its subdirectories are still
  descended in case a nested skill exists.
- Metadata is read from the leading `---` frontmatter of `SKILL.md`: the `name`
  key falls back to the directory name, and the `description` key falls back to
  the empty string. The supported frontmatter subset and its limits are below.
- Output is sorted by `name` (byte-wise), then by the ROOT-relative directory
  path as the tie-break. Equal names from different directories are therefore
  all reported, deterministically.

### Output

Plain mode prints one line per skill:

```
<name><TAB><description>
```

Literal descriptions (frontmatter `|`) have embedded newlines replaced with a
single space on this line. `--json` instead prints one pretty-printed JSON
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
| 1 | Runtime error: ROOT does not exist, is not a directory, or files under it could not be read. `error: <message>` is printed on stderr. |
| 2 | Usage error: unknown command, unknown flag, missing `list` subcommand, more than one ROOT argument, or an argument that is not valid UTF-8. Usage text is printed on stderr after `error: invalid arguments`. |

Help, version and empty results write to stdout only; errors write to stderr
only and never mix stdout/stderr.
