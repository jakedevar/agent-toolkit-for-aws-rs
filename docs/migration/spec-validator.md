# Agent Plugins specification validator (#4376)

Slice `spec`, wave 1 of programme #4369. Built on the #4374 core contracts
(commit `c208205a`, frozen foundation `e5670754`) and the #4375 manifests
validator (commit `66f9f1df`). Verification evidence lives in the Issue #4376
handoff, not in this file.

`toolkit spec validate [ROOT]` is the Rust replacement for
`uv run --with jsonschema --with pyyaml python tools/validate_spec.py`. ROOT
defaults to the current directory (the Python tool derived the root from its
own location). The legacy Python file is untouched: its caller (`mise.toml`
task `lint:spec`, reached from `mise run validate` in `.github/workflows/build.yml`)
is repointed and `tools/validate_spec.py` is retired by the CI slice #4378 once
this command is verified and landed. The vendored schemas
`tools/schemas/1.0.0/{plugin,mcp}.schema.json` are data, stay where they are and
are read at run time (spec section 7.2.1 forbids fetching a schema while loading
a plugin; the command is offline).

## Slice audit (#4371 U-10)

`docs/migration/python-inventory.json` assigns exactly one file to `spec`:
`tools/validate_spec.py` (toolkit-helper). It belongs: it validates `plugins/`
against Agent Plugins v1.0.0 and shares nothing with its neighbours.
`tools/validate.py` and `tools/tests/test_validate.py` are `manifests` (#4375),
`tools/sync-plugin-skills.py` is `sync` (#4377). There is no Python unit test for
`validate_spec.py`; its tests are the new Rust fixtures below. No misassignment
found. Callers still pointing at Python (for #4378): `mise.toml` task `lint:spec`
(and `validate`, which depends on it).

## No Python / `jsonschema` runtime dependency (AC3)

The toolkit has no JSON Schema or regex dependency, so the command carries two
small pieces of its own, both covered by unit tests:

- `crates/toolkit-cli/src/spec/jsonschema.rs` evaluates draft 2020-12 for the
  keywords the vendored schemas use: `type`, `const`, `enum`, `minLength`,
  `maxLength`, `pattern`, `properties`, `required`, `additionalProperties`
  (boolean or schema), `items`, `propertyNames`, `not`, `oneOf` and local
  `$ref`. Annotations are ignored. Any other keyword in a schema, and any
  pattern the regex engine cannot compile, is a runtime error
  (`schema_unsupported`, exit 1) so a schema that needs more can never be
  validated too weakly. Error wording and ordering follow `jsonschema` (messages
  such as `'x' is not of type 'string'`, `'name' is a required property`,
  `Additional properties are not allowed ('a', 'b' were unexpected)`, `'x' was
  expected`, `'' should be non-empty`, `... is too long`, `... does not match
  '<pattern>'`, `... is not valid under any of the given schemas`), sorted by
  instance path with schema order kept for equal paths, printed as
  `<label>: <message> (at <path>)` with `<root>` for the document itself.
- `crates/toolkit-cli/src/spec/regex.rs` is a backtracking matcher with Python
  `re.search` semantics (`$` also matches before a final newline, `.` skips
  `\n`) for literals, classes, groups, alternation, greedy quantifiers and
  look-aheads. The two vendored patterns are tested directly.

## Output and exit behaviour

| Behaviour | Python | Rust |
| --- | --- | --- |
| Progress | `Validating plugin: plugins/P`, then `Validating skills`, on stdout | same lines, same order |
| Finding | `  ERROR: <msg>` on stderr, run continues | same text on stderr, run continues |
| All passed | blank line, `All Agent Plugins v1.0.0 checks passed.`, exit 0 | same |
| Failed | blank line, `Spec validation failed with N error(s).` on stderr, exit 1 | `error: Spec validation failed with N error(s).` on stderr (shared error report, no leading blank line), exit 1 |
| Bad arguments | none accepted | shared usage error, exit 2 |
| Unusable root, schema or `plugins/` | traceback, exit 1 | `error: <reason>`, exit 1 |

## Rule parity

Messages are identical to Python unless a row says otherwise. `rel` is the
repository-relative `/` path. Tests are in
`crates/toolkit-cli/src/spec/tests.rs` (`T:`, plus `jsonschema.rs`/`regex.rs`
unit tests) and `crates/toolkit-cli/tests/cli.rs` (`B:`, real binary).

| # | Python check | Rust check | Tests |
| --- | --- | --- | --- |
| 1 | `reject_symlinks`: any symlink under `plugins/` -> `rel: symlinks are not allowed under plugins/`, sorted by path components, symlinked directories not descended | `reject_symlinks`, `scan_links` | T: `symlinks_under_plugins_are_rejected_in_component_order`, `symlink_directories_are_reported_but_not_descended`, `symlinked_skill_files_are_rejected_and_never_read`; B: `spec_validate_rejects_symlinks_under_plugins` |
| 2 | one entry per directory of `plugins/` (sorted, files skipped), `Validating plugin: plugins/P` | `validate_tree` | T: `plugins_are_visited_in_sorted_order_and_files_are_skipped`, `one_broken_plugin_does_not_stop_the_others` |
| 3 | missing root `plugin.json` -> `rel: missing required root plugin.json (spec §5.1)` | `validate_plugin_dir` | T: `missing_plugin_json_is_reported` |
| 4 | invalid JSON -> `rel/plugin.json: invalid JSON: <err>` | `read_json` (detail is the core parser message) | T: `invalid_plugin_json_is_reported`, `invalid_mcp_json_is_reported` |
| 5 | `plugin.json` against `plugin.schema.json` -> `label: message (at path)` | `schema_errors` + `jsonschema::validate` | T: `plugin_json_must_be_an_object`, `empty_object_reports_required_members_and_explicit_checks`, `schema_violations_are_reported_sorted_by_path`, `name_pattern_and_length_come_from_the_vendored_schema` |
| 6 | `$schema` must equal the plugin schema id | `validate_plugin_dir` | T: `empty_object_reports_required_members_and_explicit_checks`, `schema_violations_are_reported_sorted_by_path`; positive: `valid_repository_passes_with_progress_lines` |
| 7 | `name` must equal the directory (`name 'x' != directory 'y'`, `None` when absent) | `validate_plugin_dir` | T: `name_must_equal_the_directory` |
| 8 | `extensions` must be an object (spec §8.1); JSON null counts as absent | `check_extensions` | T: `extensions_must_be_an_object_of_reverse_domain_namespaces` |
| 9 | extension namespaces need a `.` (spec §8) | `check_extensions` | same test |
| 10 | every string leaf of an extension, at any depth (`extensions.NS.a.b[1].c` labels), that looks like a path is checked: `\` forbidden, `./` required, `..` forbidden, resolved target must stay in the plugin root (symlinks resolved), `./` targets must exist | `check_extensions`, `collect_strings`, `looks_like_path`, `check_containment` | T: `extension_values_that_are_not_paths_or_that_exist_pass`, `extension_paths_must_exist_and_stay_inside`, `extension_leaves_are_checked_at_any_depth_with_indexed_labels`, `extension_symlink_escapes_are_detected_by_resolution`, `path_like_values_follow_the_original_prefixes` |
| 11 | `mcp.json` optional; invalid JSON; schema; `$schema` must equal the MCP schema id | `validate_plugin_dir` | T: `valid_mcp_configuration_passes_and_is_optional`, `mcp_requires_schema_and_servers`, `mcp_schema_const_and_explicit_schema_check_both_report` |
| 12 | `oneOf` server shapes, `env` property-name exclusion, `cwd` pattern (schema) | `jsonschema::validate` over the vendored schema | T: `mcp_server_shape_violations_report_one_of_and_the_explicit_rule`, `stdio_env_must_not_set_reserved_names`; jsonschema unit tests |
| 13 | server must be an object | `validate_server` | T: `mcp_server_shape_violations_report_one_of_and_the_explicit_rule` |
| 14 | stdio: `command` must be a string, no placeholders, bare name or `./` path, `./` commands contained | `check_stdio` | T: `stdio_command_rules`, `mcp_server_shape_violations_report_one_of_and_the_explicit_rule` |
| 15 | stdio `env` must be an object and must not set `PLUGIN_ROOT` / `PLUGIN_DATA` (spec §9.2) | `check_stdio` | T: `stdio_env_must_not_set_reserved_names` |
| 16 | stdio `args` / `cwd` (a list or a single truthy value): entries must be strings, only `${PLUGIN_ROOT}` / `${PLUGIN_DATA}` placeholders, path-like values contained (placeholder prefix replaced by `./`) | `check_stdio`, `check_runtime_path`, `find_placeholders` | T: `stdio_args_and_cwd_rules`, `stdio_args_and_cwd_entries_must_be_strings`, `placeholders_are_found_like_the_original_regular_expression` |
| 17 | http/sse: `url` must be a string, no `${...}`, no user info or fragment, HTTPS unless loopback host (`127.0.0.1`, `localhost`, `::1`) | `check_http`, `split_url` | T: `http_url_rules`, `http_url_must_be_a_string`, `urls_split_like_urlsplit` |
| 18 | headers: object, no duplicate names under different casing, no `${...}`, no `Authorization` (spec §7.2.1) | `check_http` | T: `http_header_rules` |
| 19 | `skills` present but not a directory -> `rel: skills exists but is not a directory (spec §6.2)` | `validate_plugin_dir` | T: `skills_that_is_not_a_directory_is_reported` |
| 20 | skills: `plugins/*/skills/*/SKILL.md` sorted, directly under `skills/` only; `Validating skills` always printed | `find_skills` | T: `only_direct_children_with_a_skill_file_are_validated_in_sorted_order`, `valid_repository_passes_with_progress_lines` |
| 21 | unreadable `SKILL.md` -> `cannot read as UTF-8: <reason>` | `validate_skill` | T: `unreadable_skill_files_are_findings` |
| 22 | frontmatter must start with `---\n` and end with `\n---\n` searched from offset 4 (empty block unterminated) -> `missing or unterminated YAML frontmatter` | `parse_frontmatter` | T: `frontmatter_must_exist_and_be_terminated`, `frontmatter_block_is_found_like_the_original` |
| 23 | YAML must parse and be a mapping -> `invalid YAML frontmatter: <reason>` / `frontmatter is not a mapping` | `parse_frontmatter` over core `parse_yaml` | T: `invalid_yaml_and_non_mappings_are_reported` |
| 24 | only `name description license allowed-tools metadata compatibility` -> `non-spec frontmatter field(s): a, b` (sorted) | `validate_skill` | T: `only_spec_fields_are_allowed` |
| 25 | `name` required (falsy = missing), at most 64 characters, lowercase, no leading/trailing or doubled `-`, alphanumeric or `-`, equal to the directory (NFKC-folded) | `check_skill_name`, `nfkc_lite` | T: `name_rules`, `name_length_is_bounded_in_characters`, `name_and_description_are_required`, `nfkc_fold_covers_fullwidth_ascii_only` |
| 26 | `description` required (falsy or blank = missing), at most 1024 characters | `validate_skill` | T: `description_length_rules`, `name_and_description_are_required` |
| 27 | `compatibility` at most 500 characters; `allowed-tools` must be a string, not a sequence | `validate_skill` | T: `compatibility_and_allowed_tools_rules` |
| 28 | CRLF / lone CR files accepted (Python text mode) | `universal_newlines` | T: `crlf_skill_files_are_accepted` |
| 29 | exit 0 / `All Agent Plugins v1.0.0 checks passed.` / exit 1 with summary | `validate_command` | T: `command_success_prints_progress_then_the_pass_line`, `command_failure_reports_errors_on_stderr_and_exits_one`, `command_usage_errors_exit_two`, `help_documents_the_command_and_it_is_registered`; B: `spec_validate_passes_on_a_valid_repository`, `spec_validate_reports_findings_on_stderr_and_exits_one`, `spec_validate_missing_schemas_and_bad_arguments` |
| 30 | the real tree conforms | whole repository validated | T: `real_repository_tree_passes`; B: `spec_validate_passes_on_the_real_repository_tree` |

The real-tree tests assert zero findings over this repository's 5 plugins and 76
plugin skills; a genuine finding would fail them and be listed in the Issue
handoff.

## Intentional changes (recorded)

1. **Own schema evaluator and regex engine** (see above). `jsonschema` was
   unpinned (`uv run --with jsonschema`), so its message wording is whatever the
   latest release printed; the Rust wording follows the current release for the
   keywords in use. Messages for keywords the vendored schemas do not use do not
   exist. A schema using an unsupported keyword or pattern construct fails the
   run (exit 1) instead of validating partially.
2. **YAML.** Frontmatter is parsed by the strict core YAML subset (#4374), not
   PyYAML `safe_load`. Consequences: anchors/aliases/tags, duplicate keys,
   multi-line plain or quoted scalars and tab indentation are rejected as
   `invalid YAML frontmatter` (PyYAML accepted most of these); YAML 1.1 words
   `yes`/`no`/`on`/`off` stay strings (PyYAML turned them into booleans, whose
   `str()` was `True`/`False`). Parse errors keep the prefix; the detail is the
   core message plus `(line N of the file)`. No real `SKILL.md` uses any of
   these.
3. **JSON.** Manifests are parsed by the strict core parser: duplicate keys, a
   byte-order mark, trailing commas, lone surrogates, NaN/Infinity and nesting
   deeper than 128 are `invalid JSON` (Python accepted duplicates, NaN and
   surrogates). Parse error detail is the core parser's text.
4. **Unicode normalization.** NFKC needs a normalization table the standard
   library lacks. `nfkc_lite` folds fullwidth ASCII (U+FF01..U+FF5E) and the
   ideographic space only; other compatibility characters (ligatures,
   superscripts, ...) compare unnormalized, so such a name may be judged
   differently from Python in the rare case where NFKC would make it equal to,
   or different from, the directory name. Adding a normalization crate is a
   dependency decision left to the manager.
5. **Inputs where Python crashed are findings; the run continues.** A
   `plugin.json`/`mcp.json`/`SKILL.md` that cannot be read (directory, non-UTF-8,
   larger than 1 MiB) is `rel: cannot read: <reason>` (SKILL.md keeps `cannot
   read as UTF-8: <reason>`); a non-object top-level value yields the schema type
   error and stops the member checks for that file; `mcpServers` that is not an
   object skips the server checks; an invalid IPv6 host in a URL is `url is not a
   valid URL (invalid IPv6 URL)`. The `validator raised <Type>: ...` wrapper
   message does not exist in Rust.
6. **Run-level failures** (missing root or `plugins/`, unreadable/invalid/
   unsupported schema) are `error: ...` with exit 1 instead of tracebacks.
7. **Symbolic links.** Files are read with `read_regular_file`, which refuses
   symlinks; a symlinked manifest or `SKILL.md` therefore reports `cannot read
   ... symbolic links are not followed` in addition to the symlink finding
   (Python read the target). Files over 1 MiB are rejected. Directory listing
   failures during the symlink scan are findings (`cannot list directory`), and
   nesting deeper than 64 levels under `plugins/` is a finding; `rglob` ignored
   both.
8. **Containment** resolves the longest existing prefix of the path with
   `canonicalize` and appends the rest (same helper as the manifests slice).
9. **URL splitting** follows `urlsplit` for scheme, netloc, user info, fragment
   and hostname (leading C0/space stripped, tab/CR/LF removed). Python 3.12's
   extra validation of bracketed hosts as IP addresses is not reproduced.
10. **Failure summary** is printed through the shared error report as `error:
    Spec validation failed with N error(s).` (no leading blank line); exit
    statuses are unchanged (0 / 1) and the command rejects arguments other than
    one optional ROOT with the shared usage error (exit 2).
11. **Output ordering.** Findings are printed after the run, progress lines on
    stdout and `  ERROR:` lines on stderr, each stream in order (the manifests
    command behaves the same).

## Shared-code changes (additive, called out)

- `crates/toolkit-cli/src/lib.rs`: `pub mod spec;`, one `COMMANDS` row
  (`spec validate`, no flags, one optional positional), the `USAGE` text.
- `crates/toolkit-cli/src/manifests.rs`: only visibility. `Report::events` and
  the helpers `py_repr`, `py_repr_opt`, `py_repr_str`, `py_strip`, `is_falsy`,
  `universal_newlines`, `read_failure` and `resolve_lenient` became
  `pub(crate)` so the spec module reuses the Python-compatible formatting and
  path resolution instead of copying it. No behaviour change.
- `toolkit-core`, `Cargo.toml`, `Cargo.lock`, the Makefile and `.rsi/*` are
  untouched (no new dependency).
