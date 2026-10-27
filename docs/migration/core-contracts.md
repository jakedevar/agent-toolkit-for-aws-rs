# Shared CLI, JSON/YAML, schema, path and error contracts (#4374)

Slice `core`, wave 1 of programme #4369. Base: frozen foundation
`e56707540064d6a8bfbf721c7984f5e1a7e2dd1f` (#4373, verified, not yet landed).
Verification evidence lives in the Issue #4374 handoff, not in this file, so
this page never changes after the verified commit.

## What this slice adds

`toolkit-core` (no new dependencies, no AWS or service SDK):

| Module | Contract |
| --- | --- |
| `error` | `ToolkitError` = class (usage → exit 2, runtime → exit 1) + stable code + message, `to_json()` structured form, `From` conversions for `SkillsError`, `PathError`, `ParseError` and schema `Violation`s |
| `json` | typed `Json`, strict parser, deterministic compact/pretty writer |
| `yaml` | strict documented YAML subset into `Json`, line-numbered errors |
| `schema` | small structural schema, `$`-rooted violation paths in deterministic order |
| `paths` | `normalize_relative`, `resolve_within`, `read_regular_file` (symlink refusal, bounded UTF-8 read, post-open identity check) |
| `skills` / `frontmatter` | discovery now uses `paths`; decisions below |

`toolkit-cli` becomes a library plus a thin binary: a routing table
(`COMMANDS`), an `Invocation`, an `Output` writer and `run(args, out, err)`
that tests drive in-process with injected writers as well as through the real
binary.

AC1 mapping: typed JSON/YAML (`json`, `yaml`), error contracts (`error`,
`report` in the CLI), filesystem/symlink handling (`paths`, `skills`),
deterministic outputs (insertion-ordered objects, sorted discovery,
byte-identical repeat runs), command routing (`route`, `COMMANDS`) — each with
positive and negative tests. AC2: the only dependency of `toolkit-core` is the
standard library; service SDKs stay out of core and must arrive as optional
domain crates or features.

## Decisions on the #4426 carry-forward observations

| Observation | Decision | Pinned by |
| --- | --- | --- |
| Empty-valued `name` versus absent `name` | Treated the same: an empty or whitespace-only `name` falls back to the directory name. The frontmatter parser still reports `Some("")` so the distinction stays available; an empty `description` stays empty. | `empty_or_blank_frontmatter_name_falls_back_to_the_directory_name`, `empty_values_are_reported_as_empty_not_absent`, binary `empty_name_falls_back_and_bom_is_ignored` |
| Literal multiline names in plain output | Plain output flattens every control character and U+2028/U+2029 in `name` and `description` to one space, so a skill is one line with one tab. JSON keeps the exact text, escaped. Intentional behaviour change: the bootstrap only replaced `\n` in the description. | `plain_line_flattens_controls_and_json_keeps_them`, binary `multiline_names_stay_on_one_plain_line_and_exact_in_json` |
| UTF-8 BOM frontmatter | A leading BOM is ignored by the frontmatter reader (and by the YAML subset). The JSON parser rejects a BOM (strict RFC 8259 reader). | `utf8_bom_before_the_opening_marker_is_ignored`, `bom_prefixed_skill_md_is_read`, JSON malformed-document cases |
| Non-`BrokenPipe` stdout error messaging | `error: cannot write to stdout: <cause>` on stderr, exit 1, output stops. Previously exit 1 without a message. A closed pipe stays exit 0. | in-process `other_stdout_write_errors_are_reported_with_exit_one`, binary `stdout_write_failure_is_reported_with_exit_one` (`/dev/full`, Linux) |
| Is ROOT itself eligible for discovery? | No. Only directories under ROOT are candidates; `ROOT/SKILL.md` is ignored. Point ROOT at the parent to list one skill. This preserves the published bootstrap behaviour. | `root_itself_is_not_eligible_but_nested_skills_are`, binary `root_itself_is_not_listed_but_children_are` |
| Actual-binary symlink and closed-pipe JSON coverage | Added, because this slice changed the `SKILL.md` read path and the output writer. | binary `symlinks_never_leak_content_or_listings_through_the_binary`, `closed_stdout_pipe_with_json_is_success_not_panic` |
| Temp fixture cleanup on failure | All fixtures use a drop-guarded temporary directory (core `testutil::TempDir`, CLI test-local guard), so a failing assertion no longer leaks directories. | every fixture-based test |
| `SKILL.md` check/read TOCTOU under concurrent mutation | Mitigated without `unsafe`: the file is `lstat`ed, opened, and the opened handle's device/inode must equal the `lstat` (Unix). A swap between check and open is refused (`PathError::Changed`) or skipped; it is never followed. Residual: content may change between open and read (normal file semantics), and non-Unix platforms keep only the `lstat` check. | `symlinked_skill_md_file_is_skipped_not_read`, `symlinked_files_are_never_read` |
| Recipe duplicate-merge concern | Resolved in the #4426 note (harness resolver read); no repair, no change here. | — |

Other intentional changes: a `SKILL.md` over 1 MiB or not valid UTF-8 is a
runtime error that names the file (it was an unbounded read, and a non-UTF-8
file already failed); repeated flags are accepted once.

## Parser limits (explicit)

- JSON: no BOM, no duplicate keys, depth ≤ 128, integers outside `i64` are
  floats.
- YAML subset: see `crates/toolkit-core/src/yaml.rs`; depth ≤ 64; unsupported
  constructs are errors, never guesses. The `SKILL.md` frontmatter reader
  remains the separate, lenient two-key reader documented in the CLI README.

## Not in scope

No domain command beyond `skills list`; no AWS SDK dependency; no change to
the foundation verification adapter, `.rsi/*`, `Makefile` or `Cargo.lock`
(no dependency changes, so the lockfile is unchanged).
