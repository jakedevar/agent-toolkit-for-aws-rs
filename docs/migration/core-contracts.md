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

## Additions for the `dynamo-cost` slice (#4389)

Extensions of the shared interfaces, each with unit tests, no new dependency:

- Value options and stderr warnings use the canonical `value_flags` router and
  `Output::diagnostic` from #4376/#4377; this slice's parallel versions were
  dropped in the merge of that head.
- `ToolkitError::with_exit_code`: a command keeps a published non-default exit
  status (`0` is ignored).
- `toolkit_core::write_regular_file`: refuses a symlink or non-regular target;
  a missing file is created with `create_new`, so a link planted after the
  check is refused by the OS. Same concurrent-mutation limit as
  `read_regular_file`.
- `toolkit_core::sha256_hex`: FIPS 180-4 SHA-256 for stable content
  fingerprints (not for secrets).

## Caller text in diagnostics: redact, bound, escape (#4941)

Two slice reviews in a row (#4907/#4908 docgraph, #4939/#4940 bedrock-probes)
blocked on caller paths printed through `toolkit-cli` `safe_text::safe_path`
(escape only) and on values cut before redaction. The shared helpers now apply
one order for every slice:

1. **redact** the whole value (`toolkit_aws::redact_text`);
2. **bound** the redacted text on a character boundary, then `...`: 200
   characters for `safe_path` and the sinks, 120 for `safe_text` (unchanged);
   `safe_prefix(text, n)` is the unmarked `text[:n]` form;
3. **escape** control, bidirectional and invisible characters.

Redaction happens **at the value, before it is formatted** (review #4971):
`safe_text::shown_value` (redact, then bound) is what a renderer that escapes on
its own is given (a Python-style `repr`, a quoted choice, an SQL safety error),
so an escaped quote cannot change the credential's spelling before it is hidden
(#4979); `safe_text::redacted_value` (redact, never cut) is what a handler puts
into stdout data (a JSON report member such as `elasticache preflight`'s
`info.host`, a generated template). Stdout is **never rewritten after it is
formatted**: `Output::line` writes the line as given, so JSON stays valid and
numeric, boolean and enum fields keep their schema (#4981). Both helpers also
hide what another argument of the same run taught, so a flag repeated with the
bare credential (`--host token=S --host S`) is not an ordinary value; a typed or
fixed field never passes through them and is never rewritten. `msk sim`'s
`{"error": ...}` on stderr masks its message before it is rendered, for the same
reason. A path named by a
shared `toolkit-core` file-contract error (`PathError`, `SkillsError`, plugin
sync and vendored errors) is shown through the rendering `toolkit-cli`
registers once with `toolkit_core::set_path_display` (`shown_value`): the core
crate cannot call the redactor, which lives in `toolkit-aws` and depends on it.

The stderr sinks keep a safety net (`CallerText`, replacing
`neutralize_text_knowing` in `toolkit-cli`), never the only protection: the
`error:` report and `Output::diagnostic` show a credential-shaped or overlong
argument value that they echo verbatim redacted, bounded and on one line, and
hide what remains of an argument's credential. Every match (each known value,
each credential piece and each part of it, whole or cut after 8 characters) is
found against the original text before any is replaced, and overlapping matches
are hidden as one, so one redaction cannot consume the prefix another needs
(#4978). Backslashes are ignored while matching, so a renderer's escaping
(`\'`, `\"`, a doubled `\\`) cannot change a credential's spelling (#4979). A
piece is hidden whatever its length, labelled or not (#4980): one to three
characters match as a plain substring, so a run that passes such a credential
may see ordinary letters of its diagnostics hidden. Never leaking wins over a
readable message there, and a document with a schema is kept valid by masking
its caller strings where they are built rather than by weakening the mask. A
cut echo shorter than 8 characters of a longer piece, and an echo that starts in
the middle of a piece, are not recognized. Past a work or span bound the whole
diagnostic is hidden line by line (#5012).
Only displayed text changes; files are still read and written at the exact
path given. A passthrough command (`asm exec`) keeps only the newline rule at
the sinks: its words are another program's arguments and may be secret
references such as `{{resolve:secretsmanager:prod/db}}` (which the redactor
would take for a labelled credential), and it redacts resolved values itself.

Deliberate behaviour change, only for credential-shaped text (anything the
shared redactor changes, so for example a `secretsmanager:` ARN segment too) or
text over the bound: such a path is shown redacted and a path over 200
characters ends in `...` in a diagnostic (it used to be shown in full). Stdout
data is redacted but never cut. A credential-shaped value given to a strict
parser (a number, a timestamp, a broker class, an instance type) is parsed in
its redacted form, so the parser's own error cannot quote it; such a value is
never valid, so the outcome is the same. Ordinary values of at most 200
characters are byte-identical; the unchanged `diag_contract` and help tests pin
that. `tests/diag_contract.rs`
(`every_registered_path_value_is_shown_without_its_credential`) runs every
registered command with each value flag and positional set to
`https://user:FAKE_SECRET@example.invalid/x.json`, a 300-character path ending
in `password=FAKE_SECRET`, a 300-character userinfo URL whose `@host` lies past
the bound, `password=ab'c"FAKE_SECRET` (mixed quotes) and `password=Zq9` (a
short labelled credential), and fails on any 10-character prefix of the secret
or on `Zq9` in stdout or stderr. It also passes with the stderr net switched
off, which is how the value-level sites were audited. `tests/caller_value_redaction.rs`
pins the four review reproductions. The docgraph slice-local helpers were
removed for the shared ones. The bedrock-probes slice-local `diag::path`
(#4404, commit 6f4e4af8, not on this base) is migrated by the round-16
integration: scrub its known env-file values first (#4939), then pass the whole
string to `safe_path` (diagnostics) or `redacted_value` (stdout data), with no
earlier cut.

## Not in scope

No domain command beyond `skills list` and the `dynamodb` commands (#4389); no AWS SDK dependency; no change to
the foundation verification adapter, `.rsi/*`, `Makefile` or `Cargo.lock`
(no dependency changes, so the lockfile is unchanged).
