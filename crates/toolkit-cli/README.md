# toolkit CLI contract (bootstrap slice #4373, shared contracts #4374)

This documents the NEW `toolkit` command-line contract introduced by the Rust
workspace bootstrap. It is not a port of any existing Python CLI contract; the
Python inventory (#4371) records legacy contracts separately.

## Binary

The workspace builds one binary, `toolkit` (crate `toolkit-cli`), from the
shared library `toolkit-core`. `toolkit skills list` is read-only; `toolkit sync ...` (#4377)
writes only in its non-`--check` modes and only inside the tree it was given; the
`toolkit dynamodb` commands write only the files named in their section below. Apart
from `toolkit asm exec` (#4451: a loopback-only agent request and a command it
runs), the CLI performs no network access and no live cloud activity.

## Commands

`toolkit help` (also `-h`, `--help`) prints the complete command list: one
synopsis line per command, one description per command family, the options
and the exit codes. The text is assembled at compile time from the
registration that every command family ("slice") keeps in its own module, so
this README holds no hand-maintained command list that every new slice would
have to edit. `toolkit --version` (also `-V`, `version`) prints the version.

Each slice documents its commands in the file named by its registration
(`Slice::doc`): the sections below for the slices documented here, otherwise a
`docs/migration/*.md` file. A unit test (`registry::tests::every_slice_documents_its_commands`)
fails when a registered command is no longer mentioned in that file. How to
add a slice: [docs/migration/slice-registration.md](../../docs/migration/slice-registration.md).
The `dynamodb` commands are described in their own sections below.

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
  command table; `calculate-costs` and (offline) `deploy-model` exist, so a
  round that needs benchmark/report stops with
  `error: pipeline step ... is not available in this build` (exit 1) before any
  cloud action. `--calculator-only` is fully functional.
- Inputs are read through the symlink-refusing path contract (UTF-8, at most
  16 MiB) and outputs are written without following symlinks.

## DynamoDB deployment and teardown plans (slice `dynamo-deploy`, #4391)

```
toolkit dynamodb deploy-model --model FILE --config FILE --manifest-out FILE --yes-deploy
    [--recorded FILE --plan-out FILE] [--created-at TS] [--run-id ID]
toolkit dynamodb generate-teardown --manifest FILE --out FILE --yes-generate-teardown
```

Ports of `deploy_model.py` and `generate_teardown.py` as **offline plan
generators**: neither creates, changes or deletes an AWS resource, neither runs a
script, and a live path is a follow-up. The behavior contract, the security
surface and every intentional change are in `docs/migration/dynamodb-deploy.md`.

- `deploy-model` validates the design and `benchmark_config.json` exactly as the
  script did, reads the caller identity from a recorded document (`--recorded`,
  one `sts:GetCallerIdentity` step and an optional `iam:ListAccountAliases`
  step; the `toolkit_aws::recorded` rules apply), refuses a production ARN or
  alias, and writes the deployment plan to `--plan-out`: the `CreateTable`,
  IAM and Lambda requests a live deploy would send, with the generated IAM
  policy text. Without `--recorded` the identity step fails (exit 2) after the
  design checks. `--manifest-out` is accepted for the caller contract and is
  never written (no resource exists). `--yes-deploy` is required.
- `generate-teardown` writes `teardown.sh` from a run manifest only with
  `--yes-generate-teardown`; without it nothing is written (exit 2). Every
  manifest value is validated and shell-quoted. The script is owner-only
  executable (`0700`) and is never run by this command.
- Outputs are written to a private temporary file and renamed (`0600`, or
  `0700` for the script); a symbolic link or non-regular file at the output is
  refused. Diagnostics quote caller, file and AWS text.

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
  other `-`-prefixed word must be one of the command's flags or it is a usage
  error; a flag repeated is accepted once. A bare `-` is refused the same way
  unless the command opts in (see "The bare `-` argument").
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

### Repeatable value flags (#4670)

A plain value flag (`CommandSpec::value_flags`) keeps only the last value when it
is repeated, so `--workload a --workload b` reads as `b`. That is not what a
Python `argparse` option declared `action="append"` does. Declare such a flag in
`CommandSpec::repeatable_flags` instead and read it with
`Invocation::values_of("--name")`:

```rust
CommandSpec {
    path: &["startup", "verify-model-path"],
    flags: &[],
    value_flags: &["--output"],
    strict_values: true,
    repeatable_flags: &["--workload"],
    max_positionals: 1,
    passthrough: false,
    handler: verify_model_path,
},
// in the handler:
let workloads: Vec<&str> = invocation.values_of("--workload").collect();
```

- Every row of `COMMANDS` carries the field; write `repeatable_flags: &[]` unless
  the command has an `append` option. Nothing changes for a command that leaves it
  empty: a repeated plain value flag still keeps its last value.
- A repeatable flag is a value flag: `--name VALUE` and `--name=VALUE`, the same
  usage error for a missing value, and with `strict_values: true` an empty value or
  one starting with `-` is refused for each occurrence, not only the first. It need
  not also appear in `value_flags` (listing it in both is allowed and still means
  repeatable).
- `values_of` returns every value in command-line order, duplicates kept (argparse
  appends `--w a --w a` as `["a", "a"]`). It is empty when the flag is absent and
  has one item for a single occurrence; for a plain value flag it has at most one
  item, the last. `Invocation::value` still returns the last value of either kind.
- The order is relative to the other occurrences of the same flag. Interleaved
  switches, plain value flags and positionals do not move it (`Invocation::values`
  holds one `(name, value)` entry per occurrence of a repeatable flag, in the order
  they appeared).
- Do not split one value on commas to emulate repetition: that changes the command
  line (`--workload a,b`) and the empty-element behaviour. Where the Python option
  also accepted a comma list, split each value yourself after `values_of`.
- Values are caller text. Escape them with the `toolkit_core` diagnostic helpers
  (or `Output::diagnostic`) before they reach a message, as for any other value flag
  (see "Untrusted text in diagnostics").

### The bare `-` argument (#4743, #4780)

argparse treats a lone `-` as an ordinary value, and a few scripts use it to mean
"standard input" (`--json -`). Almost every slice turns a path argument straight
into a filesystem path, where a `-` would read or overwrite a file literally named
`-` (`elasticache dashboard --output -` truncated `./-`). So the router fails
closed, before any handler and so before any filesystem access, for EVERY
registered value flag and positional: a command receives `-` only if it is listed.
`./-` stays a legal explicit literal path. The full policy, the audited literal
parameters and the reproductions are in `docs/migration/cli-bare-dash.md`.

- There are two allowlists in `src/lib.rs`, each a list of `(command path, flag)`
  pairs (the space-joined `CommandSpec::path` and a value flag name). There is no
  field on `CommandSpec`, so adding a pair never touches a command row. A pair is
  in at most one of them.
  - `STDIO_DASH` lists parameters whose Python original reads `-` as standard
    input; a handler for a listed pair must read it deliberately (bounded, UTF-8
    checked, with a finite wait). It ships empty: no parameter reads stdin, the
    CloudWatch `--json -` / `--json-file -` included (#4935).
    `STDIO_DASH_POSITIONAL` (`"<positional>"`) names a command's positionals.
  - `LITERAL_DASH` lists value flags whose exact value `-` is a legitimate literal
    passed through unchanged: a selector, id, name or text that the Python original
    takes as an arbitrary string and that the handler never uses as a path
    (`startup verify-model-path --workload -`, #4934). Path parameters are never
    listed, a positional is never listed, and each entry is audited against its
    Python `file:line` in `docs/migration/cli-bare-dash.md`. A value that is
    composed into a path (a file name, suffix or directory name) is not listed
    either: `bedrock-report render --date-suffix -` is refused because it would
    select `MIGRATION_REPORT_-.md` (#4955).

- For a listed pair the exact token `-` is a value of that flag, spaced
  (`--workload -`) or attached (`--workload=-`), plain or repeatable, even on a
  `strict_values` command. Every other `-`-prefixed value (`-x`, `--foo`, `--`) and
  the empty value keep their previous handling.
- For every other pair `-` is a usage error (exit 2, before the handler runs):
  `error: invalid arguments: --output does not accept - (standard input/output);
  use ./- for a file named -`, one line naming the flag, then the usage text. A
  positional `-` gets the same error without a flag name. This holds for
  `strict_values` and non-strict commands alike, so a new slice cannot forget it.
- `tests/diag_contract.rs` enumerates every registered value flag and positional
  from `COMMANDS` (`every_value_flag_and_positional_dash_is_refused_or_in_exactly_one_list`):
  each is refused, or listed in exactly one of the two lists, and a new flag
  fails the test until it is one or the other. A `LITERAL_DASH` flag is run
  against a FIFO named `-` that must never be opened
  (`a_literal_dash_flag_never_reaches_the_filesystem`).
- The router never reads stdin itself.
- The global usage text is printed for every command's `--help` and appended to
  every usage error, so a slice's description edit reaches every command. Its
  length and hash are pinned to the base text
  (`the_global_usage_text_is_byte_identical_to_the_base`); a deliberate change
  updates the pin in the same commit.

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
- **Diagnostic escaping** (`diag`, #4639): see the next section.

## Untrusted text in diagnostics (contributor note, #4639)

Argument values, file names and payload fields are attacker-controlled. Echoing
them raw into stdout/stderr forges report lines (CR/LF), drives the terminal
(ESC, BEL and other controls), reorders what a reader sees (bidirectional
overrides such as U+202E) or discloses payload contents. Slices #4602, #4625
and #4634 each failed review on exactly this and each repair invented its own
escaping, so there is now one helper and one contract test.

1. **Prefer not echoing.** Report the field, location and type (for example
   `role: expected string, got object at line 3`), as the #4602 repair does.
2. **If a value must appear, quote it** with `toolkit_core::quote_untrusted`
   (re-exported from `toolkit_core::diag`):

   ```rust
   use toolkit_core::quote_untrusted;
   let message = format!("cannot read {}", quote_untrusted(&path_from_argv));
   ```

   The result is double-quoted text: `\\`, `\"`, `\n`, `\r`, `\t`
   and `\uXXXX` (valid inside a JSON string too) for every other control character, line or paragraph
   separator, bidirectional or zero-width formatting character and BOM; it
   keeps only the first 120 characters and ends with `...[N more chars]` when it
   cut something. `quote_untrusted_bounded` takes your own limit and
   `escape_untrusted` omits the quotes.
3. **Escaping is not redaction.** The helper never recognises secrets. Payload
   contents that may hold credentials must be left out of the diagnostic or
   redacted first (`toolkit_aws::redact`); do not quote them.
4. **Do not flatten or hand-roll.** Replacing controls with spaces (as
   `skills list` and `asm exec` do for their pinned output) hides the damage
   instead of showing it and each copy drifts; new code uses the helper. Those
   two call sites keep their pinned output and are not migrated: `skills list`
   flattens into a tab-separated line and `asm exec` flattens after redaction,
   so the helper would change bytes tests already pin.
5. **Safety net at the sinks.** `Output::diagnostic` and the final `error:`
   report pass their text through `toolkit_core::neutralize_text_knowing`, which
   writes every control character (tab and carriage return included),
   bidirectional override and similar invisible character as `\r`, `\t` or
   `\uXXXX` and changes nothing else. Newlines are kept because several
   messages are deliberately multi-line (the `msk size` pricing warning, the
   `sync` drift list). The one exception: `run` registers the command-line
   values that contain a newline (`Output::set_untrusted`), and any verbatim
   occurrence of such a value in a message is written with its newlines
   escaped, so an argument cannot forge a line. This closed a real hole:
   nearly every command echoed a path or flag argument with raw ESC, BEL, CR,
   U+202E or an embedded newline in its error text. It is a safety net, not the
   fix: it only knows the argument values as typed, not a value derived from
   them or read from a payload. Escape those at the call site:
   `toolkit_core::neutralize_line(&value)` also escapes newlines and keeps
   benign output byte-identical (used for the `startup program-artifacts` and
   `startup terraform-policy` not-a-directory diagnostics); `quote_untrusted`
   adds quotes and a bound. Stdout lines are not rewritten: a handler that
   prints untrusted text on stdout must escape it itself.

`tests/diag_contract.rs` runs every entry of `COMMANDS` through the real binary
with hostile argv (ESC, BEL, CR, LF, U+202E and a marker in every value flag
and positional, as plain, path-like and `--flag=value` forms, plus unknown
flags, extra words and a missing value) and with invalid file and directory
payloads that carry the same characters and a fake secret. Each spawn has an
empty environment, no stdin and a scratch working directory, so it is offline;
commands that need the network or credentials must fail closed. The test fails
on any control character other than newline (a tab is allowed on `skills list`
stdout only, its column separator), any bidirectional/invisible character, a
fake payload secret in stdout or stderr, and a line forged by a newline inside
an argument. An argument may be echoed (an error naming the path it could not
read) but only escaped. A new command is covered the moment it is added to
`COMMANDS`; make it pass by not echoing payload contents and by quoting any
value you do echo.

## Slices without a live AWS client: recorded input and responses (contributor note, #4727)

A slice whose build has no live AWS client sends nothing and replays a recorded
document (`--recorded FILE`). Reviews of the first two such slices (#4383
bedrock-agent, #4384 ai-models) found the same defects in each hand-written
copy: a wrong-typed or `null` recorded member silently defaulted (#4712,
#4724), a `null` response list read as an empty discovery (#4713, #4725), and a
failure step whose expected request was never compared (#4726). The rules now
live once in `toolkit_aws::recorded`; use it instead of writing your own.

1. **Recorded steps.** Parse each step with `step_fields`: an optional
   `request` (an object) and exactly one of `response` (an object) and `error`
   (an object). Read every optional member of the step and of its `error` with
   `optional_text`, `optional_object`, `optional_list` or `optional_member`:
   **absent** takes the default; **present with another type, `null`
   included,** rejects the whole recording before anything runs.
2. **Replay.** Store every step as a `Step` (`Step::response` or
   `Step::failure`) with its expected request, for error steps too. Replay
   in order with a `StepQueue` (a call past the last step is
   `ReplayError::Exhausted`; `unconsumed()` counts the steps never used), or
   look steps up with `Step::matches` and replay them with `Step::outcome`.
   The request is compared **before** the outcome, success or failure.
   Compare with `KeyOrder::Ignored` (Python `dict` equality); `ai-models`
   keeps `KeyOrder::Significant` because its recordings already pin it.
3. **Responses.** Read an optional response member with `optional_list`,
   `optional_object_list`, `optional_string_list`, `optional_object` or
   `optional_text`: absent or `[]` is empty; `null` or another type is an
   `aws_invalid_response`, never an empty listing. A legitimately nullable
   scalar is an explicit opt-in: `nullable_text`, `nullable_list` for a list
   whose `null` the slice documents as "none", and `toolkit_aws::next_token_from`
   for a pagination token (absent, `null` and `""` end paging). A whole value
   that must be a list or an object (the recorded document, a list entry, a
   required member already looked up) is read with `list_value` or
   `object_members`.
4. **Diagnostics.** A `MemberError` carries the member name, the expected
   type and the type found, never a value; render it with its `Display`
   (`<key> must be <expected> when present (it is <found>)`) or from its
   accessors, prefixed by the position (`calls.Search[0].error`), never by a
   recorded value.

`tests/recorded_contract.rs` enforces this without keying on any spelling you
choose (a type name, a helper name, a way of nesting `match`). It parses every
non-test file of `crates/toolkit-cli/src` with `syn` (a test file is
`tests.rs`, anything under `tests/`, or a file that only test-only module
declarations load: the file of a `#[cfg(test)] mod NAME;` (its `#[path]` if
given, else `NAME.rs` or `NAME/mod.rs`) and its submodules, whatever `NAME`
is, #4869. An inline `#[cfg(test)] mod NAME { .. }` claims no file, and a file
that any other declaration also loads, `#[cfg(not(test))]` included, stays
scanned, #4872) and decides the scope from
an **inventory** of every top-level module of `src` (#4752). A new module fails
the contract until it is listed there, as one of:

- `Class::Recorded`: it replays or reads recorded AWS calls. It must import
  `toolkit_aws::recorded` and every rule below applies;
- `Class::NotRecorded("<what it reads instead>")`: the reason must be specific.
  The module must show no recording signal;
- `Class::Pending { issue, findings }`: it reads recorded responses by hand
  and has not moved onto the shared module yet. Its findings are counted and
  must equal `findings`, so a new hand-written read fails the contract, and
  a read that moves lowers the count in the same change.

The recording signals cross-check the inventory. A module with any of them is
in scope even without an entry:

- a recording flag: the string `"--recorded"` (also `"--recording"`,
  `"--replay"`) in the module, or a `COMMANDS` row that names the flag, inline
  or through a constant, and routes to it;
- a recorded replay type, trait or constant: a name starting with a capital
  that mentions `Recorded`, `Recording` or `Replay` (`RecordedClient`,
  `RecordedAwsClient`, `ReplayQueue`, `RecordedPriceList`,
  `MAX_RECORDED_BYTES`);
- an import of `toolkit_aws::recorded` in any form: plain, grouped
  (`toolkit_aws::{recorded, ..}`), aliased (`recorded as r`,
  `toolkit_aws as aws` then `aws::recorded`), or in a nested module.

A module with a flag or a type signal must import `toolkit_aws::recorded`. Every
module in scope must:

- not define its own `optional_*`, `nullable_text` or `same_json` helper;
- not **read a JSON shape**. That is any pattern whose path ends in `Array` or
  `Object` (`Json::Array(v)`, `Json::Array { 0: v }`, `J::Object { .. }`,
  `Self::Array(v)`, an imported variant), or in a name that any `use` in `src`
  gives one (`use toolkit_core::Json::Array as List`, then `List(v)` or
  `variants::List(v)` in any file; #4863), wherever the grammar puts it:
  `match`, `if let`, `while let`, `let ... else`, an annotated `let`, a guard,
  a struct-field pattern, behind `&`, `ref`, `@`, `Some` or a tuple. It is
  also any `as_array`, `as_object`, `as_array_mut` or `as_object_mut`. A raw
  identifier is the name it spells: `r#Array` is `Array` (#4861). `matches!`
  and the standard expression macros (`assert!`, `vec!`, `format!`, ...) are
  parsed with their grammar. Any other macro body (a pattern macro, a macro
  from another crate) fails closed: a shape variant with a payload in it
  counts, even in a construction. Building a value in code
  (`Json::Array(items)` as an expression) is not a read. A file that does not
  parse is a failure;
- not **define or invoke a local macro** (#4862): a `macro_rules!` item, or
  an invocation of a macro that `src` defines, is a failure unless the
  definition carries a `// recorded-shape:` marker. The scanner never expands
  a macro, so a local one named like a standard macro (`vec!`) does not get
  the standard grammar either;
- not **alias the JSON value type**: `type J = Json;`, `type J = (Json);`,
  `type V = serde_json::Value;`, `use toolkit_core::Json as J`,
  `use toolkit_core::Json::{self as J}`, a re-export (`pub use .. as`) or an
  alias of an alias. A marked alias is allowed, but reads through it still
  count, and an alias defined elsewhere in `src` is followed into the slice.

When the value is not a response (a snapshot file, the command's own manifest,
a request or entry it built), or the member is required and every other shape
is rejected, or the code classifies every JSON type on purpose, put
`// recorded-shape: <reason>` on the line, or directly above it. The reason
must be specific: at least three distinct words. Placeholders (`todo`, `x`,
`<reason>`) and filler (`this is fine, see above`) do not count. A blank,
placeholder or filler reason is itself a failure and allows nothing. A marker
on a line allows that line. A marker above code allows the next code line, or
the whole of the single function, method or closure-binding `let` that starts
there, **except** the items nested in its body (a function, `mod`, `impl`,
`trait`, `const`, `static` or `macro_rules!`): mark each of those itself. A
marker never covers a `mod`, an `impl` or `trait` block, any other item, or a
file header (inner attribute) scope.

Every finding counts at its own position: two reads on one line are two
findings, and a `Pending` count is a count of those occurrences (#4864). The
report prints the findings of a line once, with how many it holds.

**Threat model: a tripwire, not a proof.** The scanner guards honest code
against an *accidental* raw read of a recorded response. It checks exactly
the list above (inventory, scope signals, module adoption, helper
definitions, shape reads, JSON type aliases, local macros and marker reasons)
on the syntax tree, with no type inference and no macro expansion. Deliberate
obfuscation is out of its scope: review and the adoption of the shared module
catch it, and code written to get past the scanner is a review blocker even
when the test passes (a renamed type, an indirection that hides a shape read,
a marker whose reason does not match the code, or an inventory entry that
misstates what a module reads). A further scanner evasion blocks a review only
when it falls in one of the classes above.

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

## startup validators (#4399)

```
toolkit startup model-pricing [--plugin-root DIR]
toolkit startup program-artifacts --migration-dir DIR
toolkit startup model-id-lint [--plugin-root DIR]
```

Replacements for `validate-model-pricing.py`,
`validate-startup-program-artifacts.py` and `model-id-lint.py` of
`plugins/aws-startup-advisor/scripts`; rule parity table, recorded differences
and the list of slice files not yet ported are in
`docs/migration/startup-validators.md`. All three are read-only and offline,
add no dependency, and never follow symbolic links. `--plugin-root` defaults
to the current directory.

- Findings use the Python text on stderr (`program-artifacts`:
  `STARTUP_FAIL | startup-program-artifacts` plus `  - <finding>` lines;
  `model-id-lint`: `model-id lint: N problem(s)` plus `  - <finding>` lines;
  `model-pricing`: one `<cloud>: candidate <id> has no numeric Active price
  row.` line per finding) and a final `error: ...` summary line; exit 1.
- Success lines on stdout: `Model pricing coverage: all Anthropic candidates
  have numeric Active cache rows.`, `STARTUP_OK | startup_program_status aligned
  with artifacts`, `model-id lint: OK (N retired IDs enforced from the
  registry)`; exit 0.
- An unreadable or malformed input is `error: <message>` (exit 1). A missing
  `--migration-dir` is a usage error and a missing `--plugin-root` directory in
  `model-id-lint` exits 2, like the Python scripts.

### startup artifacts

```
toolkit startup artifacts (--self-check | --fixtures | --run-dir DIR --skill NAME)
                          [--plugin-root DIR] [--manifest FILE] [--baseline FILE]
                          [--no-baseline] [--json]
```

Replacement for `validate-artifacts.py` (and the migrated cases of
`tests/test_validate_artifacts.py`). `--self-check` parses every contract
document, `--fixtures` checks the fixture goldens with the shipped baseline
(every baseline entry must still match something), `--run-dir DIR --skill NAME`
(`gcp-to-aws`, `heroku-to-aws`, `azure-to-aws`) checks one run without a
baseline unless `--baseline FILE` is given. The report (or, with `--json`, the
report object) is stdout; a run with a non-baselined error finding exits 1 and
adds `error: N artifact contract problem(s)` on stderr. Details and the recorded
differences (items 12-20) are in `docs/migration/startup-validators.md`.

### startup emit-plan

```
toolkit startup emit-plan --migration-dir DIR [--plugin-root DIR] [--plugin-json FILE]
```

Replacement for `emit-plan-json.py` and `tests/test_emit_plan_json.py`. Copies the
validated cost figures of a finished GCP or Heroku run (`estimation-infra.json`,
`estimation-billing.json`, `estimation-ai.json`) into `DIR/plan.json` for the AWS
Startups Migrate web import. Fail-open: any missing or unreadable input prints
`PLAN_SKIP | reason=...` on stdout, writes nothing and exits 0 (a deterministic skip
also drops a stale `plan.json`; an unreadable input keeps it). Success prints
`PLAN_OK | path=... | platform=... | scope=...`. Only a missing `--migration-dir` is a
usage error (exit 2). `producerVersion` comes from `.claude-plugin/plugin.json` of
`--plugin-root` (default `.`) unless `--plugin-json` names the file.

### startup consent-audit

```
toolkit startup consent-audit [--plugin-root DIR]
```

Replacement for `consent-disclosure-audit.py` and `tests/test_consent_disclosure_audit.py`.
Checks every `skills/*/references/phases/discover/discover-*-api.md` of the plugin
(default: the current directory). When the Capture Endpoint Table of a file calls a
non-`GET` method, each blanket GET-only claim in the file must name that method in an
exception sentence; an unrecognised table shape, an unreadable file and a plugin with no
such file all fail closed. Findings and setup problems go to stderr (`error: ...` summary,
exit 1); success prints `consent-disclosure audit: OK (N discovery sub-file(s) checked)`.

### startup pricing-parity

```
toolkit startup pricing-parity [--plugin-root DIR]
```

Replacement for `check-bedrock-pricing-parity.py`. Compares the canonical Bedrock pricing
cache (as the split commit `7a638343ebb2` wrote it) and the remaining GCP/Azure caches with the pre-split commit `c34fbdfde677`
(`git show`, so the history must contain both) and prints a JSON report with sorted keys on
success. A failed assertion is `error: <message>` (exit 1). The step that executed the old
and new `bedrock_pricing.py` is not ported (the report says `pricing_behavior: not
checked`; the table comparison is a Rust test of the Bedrock pricing slice #4403, wiring
it into this report is follow-up #4677). The proof is pinned to the
pre-split and split commits, so the cache's later growth does not matter (item 36 of
`docs/migration/startup-validators.md`).
### startup terraform-policy

```
toolkit startup terraform-policy DIR [--json FILE]
```

Replacement for `validate-terraform-policy.py` and its test. Reads every `*.tf` file below
DIR and checks the ALB TLS posture, public RDS, database and admin ports opened to
`0.0.0.0/0` or `::/0` in inline security-group ingress, wildcard IAM `Allow` statements and
RDS/ElastiCache encryption. Every rule is fail-open on ambiguity. `POLICY_OK | checks=...`
on stdout (exit 0); `POLICY_FAIL | ...` lines on stderr (exit 1); a missing directory or an
unwritable `--json` file is exit 2. `--json FILE` writes the verdict document.

### startup migration-report

```
toolkit startup migration-report REPORT [--estimation-infra FILE] [--estimation-ai FILE]
    [--aws-design FILE] [--migration-dir DIR] [--no-require-toc] [--no-readability]
    [--mode full|decision|ai_only]
```

Replacement for `validate-migration-report.py` and its decision-core test. Checks a
generated `migration-report.html` (or the `decision-report.html` of the Decision gate
with `--mode decision`, or an AI-only report with `--mode ai_only`): required section ids,
table-of-contents anchors, appendix depth, customer-facing readability and currency
rules, accessibility, the visual shell, the `data-cost-key` figures against
`--estimation-infra`, and the decision-core content. `--migration-dir` adds the
fixture-bleed and what-if scenario checks. `REPORT_OK | ...` on stdout (exit 0);
`REPORT_FAIL | migration-report.html` and `  - problem` lines on stderr plus an `error:`
summary (exit 1); a missing REPORT argument or an unknown `--mode` is exit 2 (a report file that does not exist is exit 1). The patterns run
on `fancy-regex`; `html.parser` and `html.unescape` are ported in `startup/report/html.rs`.

### startup heroku-report (#4406)

```
toolkit startup heroku-report REPORT [--migration-dir DIR] [--mode full|decision]
```

Replacement for `skills/heroku-to-aws/scripts/validate-heroku-migration-report.py` and its three
test files. Checks the thin stakeholder report of the heroku-to-aws skill: the required
sections (`decision-summary`, `exec-costs`, `cost-optimization` and `next-steps` in full mode;
`decision-cta` instead of the last two in `--mode decision`), the `draft for review` footer,
whole-dollar currency formatting, the `data-cost-key` figure against
`projected_costs.aws_monthly_balanced` of `estimation-infra.json` in `--migration-dir`, the
cost-optimization content, the typography-first verdict, the "What would flip this" list,
the scenario table, `decision_basis`, accessibility and the decision-mode phase state.
`REPORT_OK | ...` on stdout (exit 0); `REPORT_FAIL | file=... | errors=N` and `  - problem` lines
on stderr plus an `error:` summary (exit 1); a missing REPORT or an unknown `--mode` is exit 2.
Values read from the report path, `--mode`, or the artifacts are escaped and bounded before
they are quoted in a diagnostic (#4602). The migration directory is read without following
symbolic links: a link is reported as a problem. The shared tokenizer, cost-figure cross-check,
currency patterns and phrase matching are the ones of `startup migration-report`
(`startup/report/`). `skills/heroku-to-aws/scripts/emit-plan-json.py` is byte-identical to the
plugin copy and is replaced by `startup emit-plan`. Parity and deliberate differences:
[docs/migration/heroku.md](../../docs/migration/heroku.md).
### startup score and score-units (#4400)

```
toolkit startup score ANSWERS [--plugin-root DIR] [--runtimes-dir DIR]
toolkit startup score-units ANSWERS [--plugin-root DIR] [--runtimes-dir DIR]
```

Replacements for the agent-advisor `scripts/scoring.py` and `scripts/score_units.py` and
their tests. `score` reads an answers document (`{"entry_point": ..., "answers": {...}}`),
scores it against the runtime profiles in `skills/agent-advisor/references/runtimes` of
`--plugin-root` (default `.`; `--runtimes-dir` names the registry directly), writes
`scoring-result.json` beside ANSWERS and prints `RESULT=ok VERDICT=<verdict>`. `score-units`
scores every `agent_session` unit of an `answers.json`, loads `current-run-verifications.json`
beside it when present (an invalid artifact, or one whose `run_id` is not the run directory's
name, is an error) and prints the `{"units": {...}, <primary unit's result>}` document as
`json.dumps(indent=2)` does. Deterministic and offline. A missing ANSWERS is exit 2; an
unreadable registry, answers or evidence file, malformed data, a linked file or no
`agent_session` unit is exit 1 with a one-line message that names the field and never echoes a
value. The parity table, every deliberate difference and the Python-to-Rust test mapping are in
`docs/migration/startup-scoring.md`.

### startup model-recommend (#4401)

```
toolkit startup model-recommend INPUT [--output FILE] [--catalog FILE] [--openai-catalog FILE]
                                [--plugin-root DIR]
```

Replacement for the agent-advisor `scripts/model_recommendation.py` orchestrator and its
`anthropic_model_recommendation.py` / `openai_model_recommendation.py` engines and tests.
It reads a `model-recommendation-input.json` document, recommends a Bedrock model and API
path for every workload (OpenAI sources through the OpenAI engine, everything else through
the Anthropic engine, which marks providers it cannot reason about `provider_module_pending`),
writes `model-recommendation.json` beside INPUT (or `--output`), removes a stale
`model-verification.json` beside it when the invocation contract changed, and prints
`RESULT=ok WORKLOADS=<n> SCHEMA_VALIDATED=yes`. The dated catalogs
`references/models/anthropic-bedrock-2026-09-24.json` and `openai-bedrock-2026-09-24.json` of
the skill under `--plugin-root` (default `.`) stay data; `--catalog` and `--openai-catalog`
name other files (the OpenAI catalog is only read for an OpenAI workload). INPUT and the result
are always checked against the shipped `scripts/schemas/model-recommendation*.json` (Python only
did so when `jsonschema` was installed). Offline and deterministic. A missing INPUT is exit 2;
an unreadable, linked or invalid input or catalog, a schema finding, a catalog with no
candidate for a workload or a result that cannot be written is exit 1. Findings are
`  - $.path: message` lines on stderr that name the field and the rule but never a value of the
input, followed by one `error:` summary line. The parity table, every deliberate difference and
the Python-to-Rust test mapping are in `docs/migration/startup-models.md`.

### startup build-diagram and verify-model-path (#4402)

```
toolkit startup build-diagram RESULT CONFIRM [DESIGN]
toolkit startup verify-model-path RECOMMENDATION [--output FILE] [--workload ID[,ID...]]
```

Replacements for the agent-advisor `scripts/build_diagram.py` and `scripts/verify_model_path.py`
and their tests. `build-diagram` composes the Mermaid flowchart and ASCII fallback of a scoring
result (one unit, the legacy result/confirm path, or the multi-unit and Temporal topologies of
DESIGN), writes `diagram.md` beside RESULT and prints `RESULT=ok RUNTIME=<runtime>`
(`multi-unit` with a design). A CONFIRM or DESIGN file that does not exist is absent. It writes
text only and runs no renderer. `verify-model-path` validates RECOMMENDATION against the
`model-recommendation` schema, probes each selected `recommended` workload through the five API
paths (Mantle Messages, OpenAI Responses and Chat Completions, Bedrock Converse and InvokeModel)
after verifying an application inference profile, validates the result against the
`model-verification` schema, writes it (default `model-verification.json` beside
RECOMMENDATION) and prints `RESULT=ok|failed WORKLOADS=<n>`; exit 2 when a workload `failed` or
`needs_resolution`. A probe error is redacted before it is stored. The probe client is a seam: this
build wires none, so a probe is stored as a `RuntimeError` failure and nothing is sent. A
missing argument is exit 2; an unreadable, linked or malformed file, or a document of the wrong
shape, is exit 1 with a one-line message that names the field and never echoes a value. The
parity table, every deliberate difference and the Python-to-Rust test mapping are in
`docs/migration/startup-diagram.md`.

## startup skill-refs and model-staleness (#4407)

```
toolkit startup skill-refs (--check | --list) [--plugin-root DIR] [--baseline FILE]
                           [--no-baseline] [--json]
toolkit startup model-staleness [--plugin-root DIR] [--strict]
```

Replacements for `plugins/aws-startup-advisor/tools/check-skill-refs.py` (and its
tests) and `tools/model-staleness.py`, which are retired. Both are read-only,
offline and never follow symbolic links (a link met while scanning, resolving or
reading is an error). `--plugin-root` defaults to the current directory. Rule
parity, every deliberate difference and the Python-to-Rust test mapping are in
`docs/migration/startup-tools.md`.

`skill-refs` checks that every file path the skills' and agents' Markdown
mentions exists (`references/…`, `knowledge/…`, `scripts/…`, `$SKILL_BASE/…`,
`./…`, and the other forms listed in the module documentation of
`startup/skill_refs.rs`). Run-artifact paths, templated tokens and URLs are not
references.

- `--check`: one `ERROR  MISSING_REF  <file>:<line>  `<token>`` line per missing
  reference, one `ERROR  STALE_BASELINE  ...` line per baseline entry that
  matched nothing, a `baselined (...)` count and a final
  `PASS|FAIL — N reference(s) checked, ...` line on stdout; exit 1 (plus an
  `error: N skill reference problem(s)` line on stderr) on any missing reference
  or stale entry. `--json` prints one JSON report instead (`references`,
  `missing`, `baselined`, `stale_baseline_entries`).
- `--list`: every reference as `<file>:<line>  <token>  ->  <target>|MISSING`;
  exit 0.
- `--baseline FILE` (default `tools/skill-refs-baseline.json` of `--plugin-root`;
  a missing file is ignored) holds `ignore` rules (permanent) and `entries` (known
  dead references, STALE when they stop matching). `--no-baseline` drops it. A
  malformed baseline is `error: ...` (exit 1).
- Neither `--check` nor `--list` is a usage error (exit 2).

`model-staleness` reads `skills/shared/ai/ai-model-lifecycle.md` under
`--plugin-root`: its `**Last updated:** YYYY-MM-DD` and `**Staleness window:** N days`
lines, the required vendored copies (`gcp-to-aws`, `azure-to-aws`) and every
`skills/*/references/vendored/ai/ai-model-lifecycle.md`, which must be
byte-identical. A drifted or missing copy, a missing, malformed, impossible or
future-dated contract, and a deleted canonical file whose copies survive fail
always (stderr `  ERROR: <finding>`, then `error: model-staleness: N problem(s) —
failing.`, exit 1). A snapshot older than its window is `  WARN: ...` and exit 0, and
fails only with `--strict`. Success prints `  ok: ...` notes and
`model-staleness: model registry fresh and in sync.` on stdout. The age is measured
against the UTC date of the clock; the library entry point takes the date as a
parameter, which is how the tests pin it.

### startup check-expected (#4408)

```
toolkit startup check-expected KIND RUN_DIR [SEED_DIR] [--plugin-root DIR]
```

Replacement for the sixteen `plugins/aws-startup-advisor/fixtures/**/check_expected_*.py`
fixture asserters. They are executable checks, not fixture data: each one is run against a
migration run directory (a committed `after-*` golden tree, or a fresh replay) and reports one line per
failed assertion. `KIND` selects the program: `heroku-decide`, `heroku-decide-retained`,
`gcp-decide`, `heroku-workshop`, `gcp-workshop`, `heroku-nonweb-scaling`, `heroku-drift`,
`heroku-estimate`, `gcp-drift`, `gcp-baseline`, `azure-discover`, `azure-clarify`,
`azure-clarify-complete`, `azure-clarify-fast-path`, `azure-design` or `azure-estimate`.
The `expected-*.json` documents and golden trees stay where they are, as data; they are read
from `<plugin-root>/fixtures/<set>/` (default plugin root `.`), and the workshop kinds take an
optional `SEED_DIR` (default `<set>/seed`) for the frozen-discovery comparison. The decide kinds run
the Rust `startup migration-report`, `startup heroku-report` and `startup emit-plan` in memory
where the Python programs ran the validator and emitter scripts. Output is `PASS ...` on stdout,
or `FAIL (n):` and one `  - message` line per failed assertion and exit 1 (`FAIL` / ` - message` for
the decide kinds, `FAIL — n problem(s)` for `azure-estimate`, as the Python programs wrote them), with
`note:` lines for the programs that noted facts. A missing or unknown `KIND`, a missing `RUN_DIR`
or a `SEED_DIR` for a kind without one is exit 2. Reads refuse symbolic links at every component;
an unreadable, invalid or wrongly shaped run document or expectation document fails the check
instead of passing it; every echoed run value is a bounded, escaped single line. Offline and
deterministic. The parity table, the deliberate differences and the Python-to-Rust test mapping are
in `docs/migration/startup-fixtures.md`.

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

## aurora (#4388)

`toolkit aurora acu`, `toolkit aurora commitment` and `toolkit aurora io` replace
the six Aurora MySQL / PostgreSQL Python analyzers with one calculation core.
`--engine mysql|postgresql` is required; value flags accept `--flag VALUE` or
`--flag=VALUE` and come after the command path.

- `aurora acu estimate --engine E --instance TYPE --cpu-p95 N --cpu-max N
  [--cpu-avg N] [--connections N] [--working-set GIB] [--storage GIB]
  [--num-instances N] [--format json|table] [--region R] [--offline]`;
  `aurora acu list-instances` and `aurora acu pricing-source` take
  `--engine`, `--region` and `--offline`.
- `aurora commitment [offline] --engine E`: `offline` needs `--instance TYPE`
  (with `--num-instances N`, `--io-optimized`) or `--serverless --avg-acu N`;
  without it `--cluster ID` or `--all` read the account (see below).
- `aurora io [offline] --engine E`: `offline` needs `--instance TYPE`,
  `--storage-gib GIB` and `--monthly-io-millions N` (`--num-instances N`);
  without it `--cluster ID` or `--all` with `--days N`.

Output is the scripts' JSON (`json.dumps(indent=2)`, including `0` versus
`0.0`) or `--format table`. Exit 0 on success, 1 for a runtime error (unknown
instance class in `acu`, non-finite input), 2 for a usage error. This build has
no AWS transport: `acu` falls back to the built-in us-east-1 prices (the
reasons are in `pricing_source`), and the live `--cluster` / `--all` modes exit 2
with "AWS SDK access is not available in this build; use the 'offline'
subcommand." Parity table, mock-transport design and intentional changes:
[docs/migration/aurora.md](../../docs/migration/aurora.md).

## ai-data (#4385)

`toolkit ai-data` replaces `format_detector.py`, `validate_custom_metrics.py` and
`transformation_tools.py` of the `aws-ai-ml` skill. Value flags are `--name VALUE`
or `--name=VALUE` (not empty, not starting with `-`).

```
toolkit ai-data detect-format PATH_OR_S3_URI [--sample-size BYTES] [--json]
toolkit ai-data validate-metrics [JSON_OR_FILE]
toolkit ai-data transform plan|submit --script FILE --source-s3 URI --output-s3 URI
    [--instance-type T] [--region R] [--execution-role ARN] [--base-job-name N] [--image-uri URI]
toolkit ai-data transform describe JOB_NAME [--region R]
```

- `detect-format` samples the first `--sample-size` bytes (default 1 MiB, at
  most 16 MiB) of a regular file, keeps the complete lines, and reports which of
  the 11 formats (Nova SFT/DPO/RLVR, GPT-OSS SFT/DPO, Open Weights SFT/SFT Conv/DPO,
  Verl, Verl Legacy, SageMaker Eval) the first parsed record is, validating every
  parsed record against it. Text report (`Format:`, `Valid: ✓|✗`, `Confidence:
  HIGH|LOW|NONE`, `Lines sampled:`, `Errors:`) or `--json` (the script's document,
  ASCII-escaped). Exit 0 when valid, 1 when invalid or unreadable (`Error: ...` on
  stderr), 2 for a usage error. Symbolic links are refused. `s3://` URIs are
  validated, but this build has no S3 client, so they exit 1 with
  `Error: S3 access is not available in this build; use a local file path.`
- `validate-metrics` validates a Bedrock LLM-as-Judge custom metric array given
  inline or as the path of a file (at most 1 MiB; a missing path or a directory
  is read as inline JSON). Prints `✅ Valid — N custom metric(s) defined.` (exit
  0) or `❌ Validation failed:` with `  - Metric [i]: ...` lines on stdout (exit 1);
  no argument prints the usage line (exit 1).
- `transform plan` prints the Processing job specification (`code`, `source_dir`,
  `arguments`, input and output mounts) as JSON without contacting anything.
  `transform submit` and `transform describe` need SageMaker; this build has no
  client and exits 1 with `error: SageMaker SDK access is not available in this
  build`. The transformation script itself is a remote payload (see the
  document below).

Parity table, the S3/SageMaker service seams, the remote-payload classification
and every intentional change: [docs/migration/ai-data.md](../../docs/migration/ai-data.md).

## ai-models (#4384)

`toolkit ai-models` replaces `get_deployable_models.py`, `filter_deployable_models.py`,
`search_deployable_models.py`, `get_model_names.py`, `get_recipes.py` and
`collect_diagnostics.py` of the `aws-ai-ml` skill. Value flags are `--name VALUE`
or `--name=VALUE` (not empty, not starting with `-`); no flag repeats.

```
toolkit ai-models list-models HUB [REGION] [--list-values] [--create-snapshot]
    [--snapshot-file FILE] [--recorded FILE]
toolkit ai-models filter MODELS_JSON [KEY:VALUE...]
toolkit ai-models search SageMakerPublicHub [KEY:VALUE...] [--region R]
    [--snapshot-file FILE] [--recorded FILE]
toolkit ai-models model-names HUB [REGION] [--recorded FILE]
toolkit ai-models recipes MODEL HUB [--recorded FILE]
toolkit ai-models endpoint-diagnostics ENDPOINT REGION [--minutes N | --start TS [--end TS]]
    [--now TS] [--recorded FILE]
```

- Output is `json.dumps` of the Python value (default separators, ASCII only):
  every control or bidirectional character of a Hub or log value leaves the
  program as a `\uXXXX` escape. Diagnostics are built from fixed wording and
  escaped, quoted text; a rejected identifier is never echoed.
- `list-models` lists the Hub's models (`ListHubContents`, 100 per page,
  following `NextToken`) in the normalized shape, the distinct values per field
  (`--list-values`) or a private (mode 0600, symlink-refusing) catalog snapshot
  (`--snapshot-file FILE`, or a new `sherpa-model-catalog.*` file with
  `--create-snapshot --list-values`). A snapshot needs a region: REGION or
  `AWS_REGION` / `AWS_DEFAULT_REGION`.
- `filter` applies `KEY:VALUE` words (13 keys; values of one key OR, keys AND;
  a model without the filtered metadata is excluded) to a JSON list of models
  and prints the summary with the matches, newest first. A `bedrock` or
  `customization` value other than `true` / `false` is an error.
- `search` resolves the words against the snapshot (`--snapshot-file`, else a
  live listing) and queries SageMaker `Search` (`SageMakerPublicHub` only). A
  failed, unsupported or malformed Search uses the complete local filter and
  says so in `retrieval_backend` and `fallback_reason`; a bad constraint never
  falls back.
- `model-names` prints the sorted names of the fine-tunable models, `recipes`
  the supported techniques (`sft`, `dpo`, `rlvr`, `rlaif`) of one model (an
  unknown model is an error), `endpoint-diagnostics` the endpoint status, the
  last `--minutes` (default 5) or `--start`/`--end` window of metrics and up to
  100 log events of the trailing 15 minutes. `--now` pins the clock.
- **No live AWS client is bound.** Without `--recorded FILE` every call fails
  with exit 1 (`invalid client: this build has no live AWS client and sent
  nothing`); `--recorded` replays recorded responses (`{"calls": {"Operation":
  [{"request": {...}, "response": {...}} | {"error": {...}}]}}`), checking each
  recorded request.

Parity table, the client seam, the AWS documentation checked for the Rust
SDK/HTTP surface and every intentional change:
[docs/migration/ai-models.md](../../docs/migration/ai-models.md).

## bedrock-report (#4405)

`toolkit bedrock-report` replaces `render_report.py` and `validate_result.py` of
the `llm-to-bedrock` skill (plugin `aws-startup-advisor`). Value flags are
`--name VALUE` or `--name=VALUE` (not empty, not starting with `-`).

```
toolkit bedrock-report render [PAYLOAD_JSON] [--phase-results DIR --repo DIR]
    [--date-suffix YYYY-MM-DD] [--results-dir DIR]
toolkit bedrock-report validate (--schema analysis|ingestion|eval|rewrite|delta-decisions FILE
    | --check-run-context SAVED_JSON --current CURRENT_JSON)
```

- `render` prints the in-chat summary of a finished Execute run (`AI Migration
  Complete!`, branch, eval pass rate, files modified, parsed test counts, applied
  behavior-delta decisions, report path) from `rewrite.json`, `eval.json` and
  `delta-decisions.json` in `--phase-results DIR` (needs `--repo`), or from one
  legacy payload file; exactly one of the two inputs is required. A missing,
  unreadable or blocked/partial result file degrades to `(none)` or
  `(unavailable)`. The repository's `MIGRATION_REPORT_<date-suffix>.md` (else the
  last `MIGRATION_REPORT_*.md` by name) is copied to `--results-dir` (default
  `~/saws-migrate-results`) before the summary is printed. Only regular files
  that are not symbolic links are read, listed or written, and a date suffix with
  a path separator never names a report. Every value from the result files is
  escaped before it is printed. An unreadable legacy payload or a failed copy
  prints `render_report: ...` on stderr and exits 1.
- `validate --schema NAME FILE` checks a phase result against the schema that
  ships with the skill (`scripts/schemas/NAME.json`, compiled into the binary):
  `RESULT=valid CONTROL=ok`, `RESULT=valid CONTROL=blocked REASON=...` or
  `RESULT=valid CONTROL=partial COMPLETED=n TOTAL=m` (exit 0);
  `RESULT=invalid` plus one `$.path: finding` line per problem (exit 1; for a
  `oneOf` schema the findings are those of the control-state branch when the
  file has a `blocked` or `partial` key, else of the payload branch); and
  `RESULT=error ...` for an unreadable file (exit 2). An analysis with
  `same_model_family: true` must name an unchanged model identity in every
  `source -> target` pair. Findings name the path, the JSON type and the
  allowed values; they never repeat a value of the checked file.
- `validate --check-run-context SAVED --current CURRENT` compares two run
  contexts field by field: `RUN_CONTEXT=match` (exit 0) or `RUN_CONTEXT=mismatch`
  plus `MISMATCH $.path saved=... current=...` lines (exit 1; `source_key_sha256`
  prints only `differs`; `report_date_suffix` is ignored; the current
  `evaluation_contract_version` is always enforced); `RUN_CONTEXT=error ...`
  for an unreadable file (exit 2).

Usage errors (both or neither input, a missing companion option, an unknown
`--schema`) exit 2 with the usage text. Parity table, the schema and identity
seams and every intentional change:
[docs/migration/bedrock-report.md](../../docs/migration/bedrock-report.md).

## bedrock-agent (#4383)

`toolkit bedrock-agent fetch` replaces `fetch_bedrock_agent.py` of the
`amazon-bedrock` skill: it gathers an agent, its action groups, knowledge bases
and data sources, collaborators, aliases, versions and execution role (with its
attached and inline policies) into one manifest JSON file.

```
toolkit bedrock-agent fetch --agent-id ID --out FILE [--agent-version VERSION]
    [--agent-alias-id ID] [--region REGION] [--profile PROFILE]
    [--inline-s3-schemas] [--recorded FILE]
```

- `--agent-version` defaults to `DRAFT`; a `DRAFT` fetch warns when non-test
  aliases exist (`Fetched DRAFT but non-DRAFT aliases exist (prod->v3). ...`).
  `--inline-s3-schemas` adds `_inlinedPayload` and `_inlinedSource` (or
  `_inlineError`) to S3 `apiSchema` references.
- This build has **no live AWS client**: `--recorded FILE` replays recorded
  responses (service, operation, request and response or error per call; paged
  requests carry their `nextToken`) and without it the first call fails with
  `error: ...` and exit 1. `--profile` is accepted for compatibility and selects
  nothing offline. The recorded format and the live-client follow-up are in
  [docs/migration/bedrock-fetch.md](../../docs/migration/bedrock-fetch.md).
- The region is `--region`, else `AWS_REGION`, else `AWS_DEFAULT_REGION`; with
  none the script's `ERROR: no region resolved ...` line is printed and the exit
  status is 2. A `get_agent` service error prints `ERROR: failed to fetch agent
  "ID": ...` and exits 1 without writing a manifest. Other service errors on a
  detail call are recorded in the manifest as `_error`/`_message` when they are
  `AccessDeniedException`, `ResourceNotFoundException` or `ValidationException`
  and end the run otherwise.
- The manifest is written like `json.dump(indent=2)` (ASCII, no trailing
  newline), mode `0600` on Unix; a symbolic link at `--out` is refused. On
  success stdout is `Wrote FILE` plus `  WARNING: ...` lines. Text from AWS or the
  command line is escaped on one line (`quote_untrusted`, `neutralize_line`)
  before it reaches stdout or stderr.
## bedrock-resolve (#4403)

`toolkit bedrock-resolve` replaces `model_identity.py`, `image_input.py`,
`bedrock_pricing.py` and `resolve_source_model.py` of the `llm-to-bedrock` skill
(plugin `aws-startup-advisor`). It is offline: the provider catalog and the AWS
Pricing API answer are recorded files (live clients: follow-up #4675). Value flags
are `--name VALUE` or `--name=VALUE` (not empty, not starting with `-`).

```
toolkit bedrock-resolve identity --provider anthropic|openai
    (--source MODEL --target BEDROCK_ID | --mappings FILE)
toolkit bedrock-resolve image --emit format|mime|data-url|converse|responses|chat
    [--image PATH] [--prompt TEXT | --prompt-file FILE] [--detail low|high|auto|original]
toolkit bedrock-resolve pricing --region REGION --models ID[,ID...] [--price-list FILE]
toolkit bedrock-resolve source-model ENV_FILE --catalog FILE
    [--plan-model-id ID] [--source-provider NAME]
```

- `identity` prints `{"same_model": true|false}`: the source and the Bedrock id are
  the same first-party model (region prefix and `-vN:M` revision of the target
  ignored; moving aliases, wildcards, profile ARNs and other providers never
  match). `--mappings` takes a JSON list of `{source_model, aws_model_id}` rows and
  prints `{"same_model_family": ...}` (non-empty, every row the same model).
- `image` prints one value as JSON: the Converse `format` (`.jpg` is `jpeg`), the
  `mime_type`, the base64 `data_url`, or a Converse, Responses or Chat Completions
  user message (Converse `source.bytes` is `{"base64": ...}`; Responses always
  carries `detail`, default `auto`). Unsupported extensions fail before the file is
  read; image files of at most 20 MiB.
- `pricing` prints `{model_id: {input_per_1k_usd, output_per_1k_usd, available,
  note}}` (indent 2). Every id is validated first: a malformed, truncated or
  ambiguous id (not lowercase-ASCII `[geo.]provider.model[-YYYYMMDD][-v<N>[:<M>]]`,
  a base shared by several rows, or a respelling of a row's model under any geo
  scope, listed or not, such as `apac.anthropic.claude-opus-4.8`) is
  `available: false` with a fixed note, even when
  `--price-list` holds an answer for its display name. Then the built-in table
  (44 rows) and the region-aware Opus 5.5 rule answer; an id is priced from the
  table only when it or its date/revision-normalized form equals exactly one row;
  proprietary GPT ids need an exact row; any other well-formed id is
  `available: false` unless `--price-list FILE` (`[{region, model: display name,
  price_list: [item JSON text]}]`) holds a recorded answer. Always exit 0.
- `source-model` resolves `PLAN_MODEL_ID` (or `--plan-model-id`) against the
  catalog of the provider chosen by the env file (`SOURCE_PROVIDER` or
  `--source-provider` is authoritative; the ambient environment never chooses).
  `--catalog` is a JSON list of ids or the provider's response pages. Prints
  `{"status": "exact"|"prefix", "resolved_id", ...}` (a date or pure version suffix
  only; `mini`, `pro`, `latest` ... are never picked; two equally short pins are not picked either) or `{"status": "not_found",
  "candidates": [...], "ambiguous_prefix"?: true, "all_hits"?: [...]}` (exit 0),
  `{"status": "no_key"}` (exit 2) or `{"status":
  "error", "detail": ...}` (exit 3: no or incomplete catalog, or a resolved id
  that cannot be shown safely).

Env-file key values are only used to choose the provider and to redact output
(`***`). Text from arguments, files and catalogs is escaped (`\x1b`, `‮`) and
bounded in diagnostics and output; links are refused for every file read. Parity
table, the pricing-behaviour decision, every intentional change and the shared
`same_model` seam:
[docs/migration/bedrock-resolve.md](../../docs/migration/bedrock-resolve.md).

## asm exec (#4451)

`toolkit asm exec [--] COMMAND [ARG...]` is the Rust port of the
`aws-secrets-manager` skill's `asm-exec` helper. It replaces every
`{{resolve:secretsmanager:ID[:SecretString[:KEY[:STAGE]]]}}` reference in
`COMMAND`, its arguments and the exported environment with the secret value,
runs `COMMAND` with the result and exits with its status (a command ended by
signal N exits 256-N). The routing entry sets `CommandSpec::passthrough`, so
every word after `exec`, dash-prefixed ones included, belongs to `COMMAND`
(only one leading `--` is dropped). The handler asks for its exit status with
`Output::set_exit_status`; it prints its own `asm-exec: ...` diagnostics on
stderr (all scrubbed by `toolkit_aws::redact_text`) and nothing on stdout.
A missing command is `Usage: asm-exec <command> [args...]` with exit 1.

Secrets reach only the child's arguments and environment. The Secrets Manager
Agent (`AWS_SECRETS_MANAGER_AGENT_ENDPOINT`, default `http://localhost:2773`,
loopback only) is the working backend; the signed MCP fallback is ported behind
the `toolkit-aws` seams but fails closed in this build (follow-up #4582). The
parity table, the intentional differences and the secret-handling rules are in
[docs/migration/asm-exec.md](../../docs/migration/asm-exec.md).

## elasticache (#4392)

`toolkit elasticache price | estimate-serverless | validate-input |
validate-references | preflight | audit | dashboard` replace the nine
ElastiCache skill scripts (`price_calculator.py`, `pricing.py`,
`serverless_estimator.py`, `command_classifier.py`, `input_validator.py`,
`validate_references.py`, `migration_preflight.py`, `security_audit.py`,
`generate_dashboards.py`) with one Rust module. Value options accept
`--flag VALUE` or `--flag=VALUE`; short options are not supported.

- `price [--engine valkey|redis|memcached] [--mode serverless|node]
  [--region R] [--data-gb N] [--ecpu-millions N | --ops-per-sec N]
  [--avg-payload-kb N] [--node-type T] [--nodes N] [--ri-term TERM]
  [--show-ri-options] [--extended-support]`: JSON estimate (or the reserved /
  Extended Support text report). The region defaults to `AWS_REGION`,
  `AWS_DEFAULT_REGION`, then `us-east-1`.
- `estimate-serverless --input CLUSTERS.csv [--commandstats STATS.csv]
  [--output FILE] [--json]`: serverless cost of provisioned clusters; writes the
  CSV (and `.json` beside it), prints the summary.
- `validate-input KEY=VALUE...`: checks `cache_name`, `replication_group_id`,
  `subnet_id`, `security_group_id`, `region`, `engine_version`, `vpc_id`,
  `kms_key_id` and `snapshot_name`; exit 1 with the problems listed, never the
  rejected values.
- `validate-references [--skill-dir DIR] [--verbose] [--quiet]`: reference-file
  and script conventions of the skill directory (default `.`); exit 1 on a FAIL.
- `preflight --host H [--port N] [--snapshot FILE] [--json]` and
  `audit (--serverless NAME | --replication-group ID) [--describe FILE]
  [--json]`: migration readiness and security posture, over a recorded snapshot
  or describe file (exit 1 on a blocker or FAIL).
- `dashboard (--serverless NAME | --replication-group ID) [--node-ids A,B]
  [--no-dashboard] [--no-alarms] [--output FILE]` and the threshold options:
  CloudFormation template with a CloudWatch dashboard and alarms.

Prices come from `--pricing-csv FILE` / `--pricing FILE` (a pricing CSV) or
`--pricing-json FILE` (the AWS bulk pricing JSON or the old compact cache): this
build has no network, AWS or Redis client, so those inputs replace the live
fetch, the `boto3` calls and the server connection, and a command that needs
one without it is a usage error (exit 2) or, for `preflight` and `audit`, the
scripts' own connection-failure result (exit 1). Text taken from inputs is
escaped before it is printed (#4602). The parity table, the snapshot and
describe formats, the intentional changes and the follow-ups are in
[docs/migration/elasticache.md](../../docs/migration/elasticache.md).

## cloudwatch (#4380)

`toolkit cloudwatch render`, `toolkit cloudwatch validate` and
`toolkit cloudwatch region` expose the pure logic of the CloudWatch
dynamic-instrumentation scripts (`skills/core-skills/aws-observability/scripts/cloudwatch/`):
location and capture parsing, input validation, status assessment, error text
and the response renderers. Nothing contacts AWS; the live Application Signals
and Logs clients, the gateway and the operation entrypoints stay in Python
until slice `cw-api` (#4381) ports them on top of these modules.

```
toolkit cloudwatch render RENDERER (--json TEXT | --json-file FILE)
toolkit cloudwatch validate CHECK (--json TEXT | --json-file FILE)
toolkit cloudwatch region [--region REGION]
```

- The arguments are one JSON object using the legacy parameter names. Pass
  exactly one of `--json` (short, trusted payloads) and `--json-file` (keeps
  values off the command line). Value flags use the shared router
  (`strict_values: true`: `--name value` or `--name=value`, no empty value and
  none starting with `-`), so stdin (`-`) is not accepted.
- `RENDERER` is one of `location`, `create-success`, `list`, `get`,
  `batch-delete`, `status-history`, `status-assessment`, `snapshot-search`,
  `snapshot-sample`, `snapshot-fields`, `failure`. The text is printed
  verbatim, like the scripts' `print(result.text)`. `status-assessment` replays
  recorded ACTIVE/READY/ERROR checks (`checks.<STATUS>.events` / `error`)
  through the assessment policy and renders the verdict; `failure` renders an
  AWS failure (`failure.kind` is `client_error`, `endpoint_connection`,
  `timeout`, `credentials` or `other`).
- `CHECK` is one of `instrumentation-type`, `location-inputs`
  (`mode` `create` or `lookup`), `capture-names`, `probe`, `location-hash`,
  `snapshot-signal`. A passing check prints `OK`, the normalized type or the
  request payload; a failing check prints the legacy `ERROR: ...` text on
  stdout and exits 1, like an operation's `OpResult(ok=False, ...)`.
- `region` prints the resolved region: `--region` > `AWS_REGION` >
  `AWS_DEFAULT_REGION` > `us-east-1` (an empty value counts as unset).
- Exit codes: `0` success, `1` a failed check (or an assessment whose window
  ends before it starts), `2` a usage error or the legacy `TypeError` case: an
  unknown, missing or mistyped argument prints `error: invalid arguments for op
  '<op>': ...` on stderr. Credentials are never read, printed or logged.

Function-by-function parity, the injected-seam design for #4381 and every
intentional change: [docs/migration/cw-domain.md](../../docs/migration/cw-domain.md).

## relational (#4394)

Helpers of the `aurora-dsql`, `rds-oss`, `rds-oracle` and
`timestream-influxdb` skills. This build has **no live AWS or database client**:
every command reads a recording (`--recorded FILE`) or works offline, opens no
socket and reads no credential. Details, recorded formats, intentional changes
and the Python callers left for #4415 are in
[docs/migration/relational.md](../../docs/migration/relational.md).

```
toolkit dsql selftest
toolkit dsql tenant-sql --tenant SLUG
toolkit dsql tenant-query --tenant SLUG [--recorded FILE]
toolkit rds commitment [offline] [--region R] [--format json] [--instance ID]
    [--instance-type TYPE] [--engine mysql|mariadb|postgres] [--num-instances N]
    [--multi-az] [--recorded FILE]
toolkit rds-oracle test-connection ENDPOINT PORT SERVICE_NAME USERNAME [--verbose]
    [--recorded FILE]
toolkit timestream-influxdb validate-input [KEY=VALUE...]
toolkit timestream-influxdb instance-types [recommend WRITES QUERIES DAYS]
```

- `dsql tenant-sql` prints `SELECT * FROM "entities" WHERE tenant_id = 'SLUG'`
  for a slug of 1-64 lowercase letters, digits and hyphens; anything else is an
  error (exit 1) and prints no SQL. `tenant-query` runs that statement, the only one
  its cursor accepts (no raw SQL reaches a cursor), and prints the rows as JSON; without `--recorded` it
  fails with exit 1. `selftest` prints `safe_query self-test passed`.
- `rds commitment` prints the Reserved Instance / Database Savings Plan
  comparison as JSON (the script's keys and rounding). `offline` needs
  `--instance-type` and `--engine`; otherwise `--instance ID` is looked up. With
  no mode and no `--instance` it prints the usage and exits 0. Without
  `--recorded` only the bundled us-east-1 on-demand prices are used. No purchase
  API exists.
- `rds-oracle test-connection` prints the four-check report. Endpoint, port,
  DSN and user are hidden unless `--verbose`. Exit 1 when the driver or the
  connection fails (with the script's ORA-code hints), 0 otherwise; fewer than
  four arguments print the usage on stdout and exit 1. No password is read.
- `timestream-influxdb validate-input` prints `All inputs valid.` or
  `Validation FAILED:` with one escaped line per bad input (exit 1); unknown
  keys pass. `instance-types` lists the table or recommends a type from write
  rate, query concurrency and retention days.
- Text from a recording, AWS or the command line is redacted (`redact_text`)
  and escaped on one line before it reaches stdout or stderr.

## cloudwatch operations (#4381)

`toolkit cloudwatch instrumentation`, `snapshots`, `evaluate-traces` and
`capture-dataset` port the operation entrypoints and host scripts of the
CloudWatch skill (`di_instrumentation.py`, `di_snapshots.py`,
`evaluate_traces.py`, `capture_dataset_from_traces.py`) on top of the #4380
modules. **This build has no live AWS client**: every AWS call goes through an
injected client that is either a recorded replay (`--recorded FILE`) or one
that sends nothing and reads no credentials, so the first call of a run without
`--recorded` fails with `this build includes no live AWS client`. The live
binding is a follow-up.

```
toolkit cloudwatch instrumentation OP (--json TEXT | --json-file FILE) [--recorded FILE]
    [--now ISO] [--i-have-confirmed] [--region REGION] [--profile PROFILE]
toolkit cloudwatch snapshots OP (--json TEXT | --json-file FILE) [--out FILE] [--recorded FILE]
toolkit cloudwatch evaluate-traces (--trace-id ID | --trace-ids IDS) [--evaluator-id ID | --evaluator-ids IDS]
    [--level TRACE|TOOL_CALL|SESSION] [--session-id ID] [--window-days N] [--include-explanations]
    [--no-writeback] [--retention-days N] [--kms-key-id KEY] [--recorded FILE] [--now ISO]
toolkit cloudwatch capture-dataset --trace-ids IDS [--mode create|add] [--dataset-name NAME]
    [--dataset-id ID] [--description TEXT] [--window-days N] [--i-have-confirmed] [--recorded FILE]
```

- `instrumentation OP` is `create`, `list`, `get`, `delete`,
  `batch-delete-by-scope`, `batch-delete-by-arns`, `get-status` or
  `check-status`; `snapshots OP` is `sample` or `search`. The arguments are one
  JSON object with the legacy parameter names (`--print-contract` prints the
  schema). `--json -` and `--json-file -` are refused: stdin is not an
  argument source (`docs/migration/cli-bare-dash.md`). An unknown or mistyped
  argument exits 2; a failed operation prints
  its report on stdout and exits 1. Caller values are escaped to one line
  before they reach a request or a diagnostic, and AWS text is escaped by the
  final output pass.
- **Mutations.** `create`, `delete` and both batch deletes change live
  resources: without `--i-have-confirmed` they print `ERROR: refusing to run`
  and exit 1 before any client call. `capture-dataset` refuses the same way
  (`confirmationRequired`). `evaluate-traces` writes each score back as a
  `gen_ai.evaluation.result` record unless `--no-writeback` is given, like the
  script.
- `snapshots --out FILE` writes the result owner-only (`0600`), refusing a
  symbolic link or a non-regular file at the path. `search`/`sample` exit 1 for
  a bare `ERROR` line and for a result whose inner `status` is `ERROR`,
  `TIMEOUT`, `FAILED` or `CANCELLED`.
- `evaluate-traces` and `capture-dataset` print one JSON receipt (pretty on
  success, one compact line with exit 1 on failure). The judge's explanation is
  left off the receipt unless `--include-explanations` is given.
- `--recorded FILE` is a JSON object `{"calls": [{"service", "operation",
  "request"?, "response" | "error"}]}`. A call matches the entries of its
  service and operation whose recorded `request` is contained in the actual
  one; entries are consumed in file order and the last repeats. A member of
  the wrong type, `null` included, rejects the file. `--now ISO` pins the clock
  (ttl expiry, session names, writeback timestamps).
- Stdin (`--json -`) is not accepted: the router refuses values that start
  with `-`.

Parity table, recorded format, intentional changes, the Python callers left for
#4415 and the live-binding follow-up:
[docs/migration/cw-api.md](../../docs/migration/cw-api.md).

## telemetry (#4398)

Plugin usage-data consent, persistence and emission: the Rust port of
`plugins/aws-startup-advisor/scripts/telemetry`. The full contract, the intentional
changes and the cutover notes are in `docs/migration/telemetry.md`.

```
toolkit telemetry consent show|status|accept|opt-out|session-start
toolkit telemetry status | reconcile
toolkit telemetry skill-invoked [--skill NAME]
toolkit telemetry migration [--reconcile] [--session-end] [--via cli]
toolkit telemetry bundles [--write | --check] [--plugin-root DIR]
```

Any of them also takes `--telemetry-dir DIR` (the directory with `version.json`: a skill
bundle or `scripts/telemetry`; without it there is no plugin version and nothing is sent).

- One registered command with `passthrough: true`: every word after `telemetry` reaches the
  handler verbatim, so a host hook can never fail on a word it did not expect. The hook
  subcommands (`skill-invoked`, `migration`, `consent session-start`) always exit 0 and are
  silent (`session-start` prints the one instruction a host injects into the agent's context);
  `consent`, `status`, `reconcile` and `bundles` exit 2 on an unknown word.
- **Consent is fail-closed.** Only `~/.aws-startup-advisor/plugin-telemetry.json` with
  `consentStatus` `ACCEPTED` and a UUID `installId` permits an event. A missing, unreadable,
  malformed or oversized record, an unknown home directory and `OPT_OUT` all mean no emission;
  there is no environment switch. `consent accept` and `opt-out` write atomically (mode 0600)
  and exit 1 when they cannot; `show` prints the approved notice and records nothing.
- **This build sends nothing.** Requests go through an injected sink; the shipped one reports
  every request unreachable (no HTTP client), so `migration` holds its events rather than losing
  them. Tests use a recording sink.
- `migration` / `reconcile` diff `.migration/<id>/.phase-status.json` (and `.gate-failures.json`)
  against `.telemetry-snapshot.json`, take a per-run lock, and send `RUN_STARTED`,
  `PHASE_COMPLETED`, `GATE_FAILED`, `RUN_COMPLETED` (once per `runMode`). `reconcile` is
  `migration --reconcile --via cli`, the route for hosts without plugin hooks; it does nothing
  where the plugin's hooks are reachable.
- `status` prints `{"source", "reportingMode", "consentStatus", "pluginVersion"}` and sends and
  writes nothing.
- `bundles` generates (`--write`) or checks (`--check`, the default) the vendored copies of the
  telemetry runtime and the DSL interpreter in the four migration skills; unexpected bundle files
  are reported, never deleted.
- Diagnostics name paths and reason classes, never the contents of the record, a run file or a
  manifest.
