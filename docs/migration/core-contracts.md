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
| `SKILL.md` check/read TOCTOU under concurrent mutation | Partially mitigated without `unsafe`, for the **final component only**: the file is `lstat`ed, opened, and the opened handle's device/inode must equal the `lstat` (Unix). The final file swapped for a link or non-file between the scan and the read is refused (`PathError::Changed`) or skipped. **Not mitigated (accepted, see "Threat model" below):** an *ancestor* directory renamed away and replaced with a symlink to an outside directory after the walk listed it; the `lstat`, open and `read_dir` all resolve through the swapped path, the identity check passes against that path, and outside content can be reported under an inside-looking relative path. Also residual: content may change between open and read, and non-Unix platforms keep only the `lstat` check. | `symlinked_skill_md_file_is_skipped_not_read`, `symlinked_files_are_never_read` |
| Recipe duplicate-merge concern | Resolved in the #4426 note (harness resolver read); no repair, no change here. | — |

Other intentional changes: a `SKILL.md` over 1 MiB or not valid UTF-8 is a
runtime error that names the file (it was an unbounded read, and a non-UTF-8
file already failed); repeated flags are accepted once.

## Threat model for filesystem handling

- **Always refused:** static symbolic links. `resolve_within` fails with
  `PathError::Symlink` for a link at any component below the root, and skill
  discovery never follows a linked directory and never reads a linked
  `SKILL.md`, whatever it points to (covered by core and actual-binary tests).
- **Not defended:** concurrent mutation of the tree during a run. The walk is
  pathname-based. The reviewed (#4455) ancestor-swap window: discovery lists a
  real child directory, an attacker renames it away and puts a symlink to an
  outside directory in its place, and the later `symlink_metadata`,
  `read_regular_file` open and queued `read_dir` all resolve through the
  swapped ancestor. The final-component identity check cannot see this,
  because both the `lstat` and the opened handle refer to the same outside
  file. Manager ruling on #4374: this is outside the tool's model (an
  adversary renaming directories inside the user's own checkout while the
  tool runs; the Python original had the same window), and closing it needs
  `openat`-style handle-anchored traversal, i.e. a non-`std` dependency or
  `unsafe`, which core forbids. Therefore no claim in the docs or module
  documentation says the walk "stays inside" ROOT under concurrent change.
- **Upgrade path:** callers only use `discover_skills`, `resolve_within` and
  `read_regular_file`. A later slice that needs adversarial containment can
  replace their implementation with a verified-directory-handle, no-follow
  per-component walker without changing these public signatures.

## Parser limits (explicit)

- JSON: no BOM, no duplicate keys, depth ≤ 128, integers outside `i64` are
  floats.
- YAML subset: see `crates/toolkit-core/src/yaml.rs`; at most 64 container
  levels in one budget shared by block mappings, block sequences (including
  same-indent sequences) and flow collections (a mixed document of 40 block
  plus 40 flow levels is rejected, exactly 64 is accepted); unsupported
  constructs are errors, never guesses, and the anchor/alias/tag and
  reserved-indicator rules apply to flow keys and flow plain scalars exactly
  as to block ones (quoted forms stay literal). The `SKILL.md` frontmatter reader
  remains the separate, lenient two-key reader documented in the CLI README.

## Not in scope

No domain command beyond `skills list`; no AWS SDK dependency; no change to
the foundation verification adapter, `.rsi/*`, `Makefile` or `Cargo.lock`
(no dependency changes, so the lockfile is unchanged).
