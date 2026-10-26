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
  followed.
- Hidden directories (leading `.`) and the directories `node_modules` and
  `target` are skipped entirely, as entries and as descent targets.
- A directory containing `SKILL.md` is a skill; its subdirectories are still
  descended in case a nested skill exists.
- Metadata is read from the leading `---` frontmatter of `SKILL.md`: the `name`
  key falls back to the directory name, and the `description` key (inline,
  quoted, folded `>` or literal `|` scalar) falls back to the empty string.
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

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success, including help, version and an empty skill list. |
| 1 | Runtime error: ROOT does not exist, is not a directory, or files under it could not be read. `error: <message>` is printed on stderr. |
| 2 | Usage error: unknown command, unknown flag, missing `list` subcommand, or more than one ROOT argument. Usage text is printed on stderr after `error: invalid arguments`. |

Help, version and empty results write to stdout only; errors write to stderr
only and never mix stdout/stderr.
