# Repository manifest and skill-frontmatter validator (#4375)

Slice `manifests`, wave 1 of programme #4369. Built on the #4374 core contracts
(commit `c208205a`, frozen foundation `e5670754`). Verification evidence lives
in the Issue #4375 handoff, not in this file.

`toolkit manifests validate [ROOT] [--plugin NAME]` is the Rust replacement for
`python3 tools/validate.py [--plugin X]`. ROOT defaults to the current
directory (the Python tool derived the root from its own location). The legacy
Python files are untouched: callers (`mise.toml` task `lint:manifests` and
`test:tools`, `.github/workflows/build.yml` via `mise run validate`) are
repointed and the Python is retired by the CI slice #4378 once this command is
verified and landed.

## Slice audit (#4371 U-10)

`docs/migration/python-inventory.json` assigns exactly two files to `manifests`:
`tools/validate.py` (toolkit-helper) and `tools/tests/test_validate.py`
(test-or-diagnostic). Both belong: the test imports `validate.py` by path and
exercises only its Devin-manifest checks. Neighbouring files are correctly
elsewhere: `tools/validate_spec.py` is `spec` (#4376) and
`tools/sync-plugin-skills.py` is `sync` (#4377). No misassignment found.

## Output and exit behaviour

| Behaviour | Python | Rust |
| --- | --- | --- |
| Progress | `Validating ...` lines on stdout | same lines, same order, stdout |
| Finding | `  ERROR: <msg>` on stderr, run continues | same text on stderr, run continues |
| All passed | blank line, `All validations passed.`, exit 0 | same |
| Failed | blank line, `Validation failed with N error(s).` on stderr, exit 1 | `error: Validation failed with N error(s).` on stderr (shared error report, no blank line), exit 1 |
| Unknown `--plugin` | `Plugin not found: X` on stderr, exit 1 | `error: Plugin not found: X`, exit 1 |
| Bad arguments | argparse message, exit 2 | shared usage error, exit 2 |

## Rule parity

Messages are identical to Python unless a row says otherwise. `rel` is the
repository-relative `/` path. Tests are in `crates/toolkit-cli/src/manifests/tests.rs`
(`T:` names) and `crates/toolkit-cli/tests/cli.rs` (`B:`, real binary).

| # | Python check | Rust check | Tests |
| --- | --- | --- | --- |
| 1 | `validate_json`: file missing -> `Missing file: rel` | `Validator::validate_json` | T: `missing_marketplaces_are_reported_in_order`, `missing_claude_manifest_skips_devin_parity`, `codex_manifest_requires_all_keys` |
| 2 | invalid JSON -> `Invalid JSON in rel: <err>` | same prefix; detail is the core parser message (see changes 1-3) | T: `marketplace_invalid_json_is_reported` |
| 3 | missing required key -> `Missing key 'k' in rel` | same, in declared order | T: `marketplace_missing_keys_are_reported`, `codex_manifest_requires_all_keys`, `mcp_structure_errors_are_reported` |
| 4 | marketplace: plugin without `name` -> `Plugin missing 'name' in rel` | same (non-object entries included) | T: `marketplace_plugin_without_name_is_reported` |
| 5 | marketplace: `source` (string or `{path}` object) must be an existing directory from the repo root -> `Plugin source 'S' does not exist for 'N'`; empty/absent source skipped | same | T: `marketplace_source_must_exist_as_a_directory`, `marketplace_empty_or_absent_source_is_not_checked`, `marketplace_source_resolves_from_the_repository_root` |
| 6 | Claude manifest requires `name` | `validate_plugin` | T: `claude_manifest_missing_name_is_reported_with_parity_detail` |
| 7 | Codex manifest requires `name version description author interface` | `validate_plugin` | T: `codex_manifest_requires_all_keys` |
| 8 | Cursor manifest optional; `name` must equal the directory -> `Name 'X' does not match directory 'D' in rel` | `check_cursor` | T: `cursor_manifest_is_optional_and_name_must_match` |
| 9 | Devin manifest optional; `name` must be a non-empty string; must equal the directory | `check_devin` | T: `devin_manifest_is_optional`, `devin_name_must_be_a_non_empty_matching_string`, `devin_missing_name_key_is_reported` |
| 10 | Devin `name`/`version`/`description` must equal the Claude manifest (`'k' in D does not match C: 'a' != 'b'`, no detail for description); skipped without a Claude manifest | `check_devin` with Python-style `repr` of the values | T: `matching_devin_metadata_passes`, `devin_version_mismatch_fails`, `devin_description_mismatch_fails_without_detail`, `devin_missing_version_fails_with_none_detail`, `missing_claude_manifest_skips_devin_parity` |
| 11 | Devin `skills`/`mcpServers` (string or list) entries: non-string -> `'k' entry X is not a path string in rel`; absolute, `..` component or resolving outside the plugin -> `must stay inside the plugin directory`; missing -> `does not exist` | `check_devin_paths`, `check_inside` (canonicalizes the longest existing prefix so symlink escapes are caught) | T: `devin_declared_paths_must_stay_inside_and_exist`, `devin_valid_declared_paths_pass`, `devin_non_string_path_entries_are_reported`, `devin_symlink_leaving_the_plugin_escapes`, `check_inside_classifies_paths` |
| 12 | `.mcp.json` optional; requires `mcpServers`; stdio (default type) needs `command`; http needs `url` | `check_mcp`, `check_mcp_server` (messages carry no path, as before) | T: `mcp_valid_servers_pass`, `mcp_stdio_server_needs_a_command`, `mcp_http_server_needs_a_url`, `mcp_file_is_optional` |
| 13 | `${` in an MCP `url` string rejected | `check_mcp_server` | T: `mcp_url_interpolation_is_rejected` |
| 14 | plugin skills: each direct child directory of `plugins/P/skills` with a `SKILL.md` is validated, others skipped, sorted | `check_plugin_skills` | T: `plugin_skills_only_check_direct_children_with_a_skill_file`, `plugin_skill_errors_name_the_plugin_path` |
| 15 | top-level `skills/**/SKILL.md` found recursively, sorted as path component lists, `Validating skill: <dir>` per skill | `validate_top_level_skills`, `collect_skill_files` (never follows directory symlinks, as `rglob`) | T: `top_level_skills_are_found_recursively_in_component_order`, `top_level_skill_errors_use_repository_relative_paths`, `missing_skills_directory_is_fine`, `top_level_walk_never_follows_symlinks` |
| 16 | frontmatter must start with `---\n` -> `Missing YAML frontmatter in rel` | `validate_skill_frontmatter` | T: `frontmatter_must_exist_and_be_terminated` |
| 17 | closing `\n---\n` searched from offset 4 -> `Unterminated frontmatter in rel` (empty frontmatter is unterminated) | same | T: `frontmatter_must_exist_and_be_terminated` |
| 18 | simple `key: value` reader (strip, quote stripping, `>`/`\|` indicators dropped, two-space continuation lines, `splitlines` semantics, text-mode newline translation) | `parse_simple_frontmatter`, `py_splitlines`, `universal_newlines` | T: `frontmatter_values_follow_the_simple_reader`, `crlf_and_lone_cr_line_endings_are_accepted`, `simple_frontmatter_reader_edge_cases`, `splitlines_follows_python`, `universal_newlines_translate_like_text_mode` |
| 19 | `name` required / kebab-case regex / at most 64 chars / equals directory | `is_kebab` and the ordered checks | T: `name_is_required_kebab_case_bounded_and_matches_the_directory`, `kebab_matches_the_original_regular_expression` |
| 20 | `description` required / at least 20 characters | same | T: `description_is_required_and_at_least_twenty_characters`, `both_name_and_description_problems_are_reported_together` |
| 21 | `--plugin X` validates only that plugin, no marketplaces, no top-level skills; unknown -> exit 1 | `validate_tree(root, Some(name))` | T: `single_plugin_skips_marketplaces_and_top_level_skills`, `unknown_or_unsafe_plugin_names_are_runtime_errors`, `command_plugin_option_selects_one_plugin`; B: `manifests_validate_plugin_selection_and_usage_errors` |
| 22 | plugin directories visited in sorted order, non-directories skipped | `validate_tree` | T: `plugins_are_visited_in_sorted_order_and_files_are_skipped` |
| 23 | exit 0 / `All validations passed.` / exit 1 with summary | `validate_command` | T: `command_success_prints_progress_then_the_pass_line`, `command_failure_reports_errors_on_stderr_and_exits_one`; B: `manifests_validate_passes_on_a_valid_repository`, `manifests_validate_reports_errors_on_stderr_and_exits_one` |
| 24 | `TestRepoPlugins.test_every_devin_plugin_passes` | whole repository and each Devin plugin validated | T: `real_repository_tree_passes`, `every_real_devin_plugin_passes_individually`; B: `manifests_validate_passes_on_the_real_repository_tree` |

The real-tree tests above assert zero findings over this repository's 5 plugins,
3 marketplaces and every `SKILL.md`; a genuine finding would fail them and be
listed in the Issue handoff.

## Intentional changes (recorded)

1. **JSON parser.** Manifests are parsed with the strict `toolkit-core` parser,
   not Python's `json.loads`: duplicate keys, a byte-order mark, trailing
   commas, lone surrogates, control characters, NaN/Infinity and nesting deeper
   than 128 are `Invalid JSON` errors (Python silently accepted duplicates,
   NaN and surrogates). No real manifest uses any of these.
2. **Parse error detail.** `Invalid JSON in rel: <detail>` keeps the prefix; the
   detail is the core parser's `JSON parse error at line L, column C: ...`
   text instead of Python's `JSONDecodeError` text.
3. **Files Python crashed on are reported errors.** A manifest or `SKILL.md`
   that is unreadable, not UTF-8, larger than 1 MiB, a directory or a symbolic
   link yields `Cannot read rel: <reason>` and the run continues (Python raised
   a traceback). A top-level JSON value that is not an object yields the usual
   `Missing key` errors plus `Top-level value must be a JSON object in rel`.
   Other Python crashes also become errors: `'plugins' must be an array in rel`,
   `Plugin source for 'N' must be a string in rel`, `'mcpServers' must be an
   object in rel`, `MCP server 'N' must be an object in rel`.
4. **No symbolic links in content.** Manifests and `SKILL.md` are read through
   `read_regular_file`, which refuses symbolic links (Python followed them). The
   repository contains none. Directory symlinks under `skills/` are not
   descended into (as `rglob`). Devin path containment resolves symlinks of
   existing path prefixes; a dangling symlink reports `does not exist` even if
   it points outside the plugin.
5. **`--plugin` names** must be a single path component: `..`, `.`, empty names
   and names with a separator are `Plugin not found` (Python joined them and
   could validate directories outside `plugins/`). `--plugin=X` is accepted and
   a repeated `--plugin` keeps the last value (argparse behaviour); argparse
   prefix abbreviations (`--plug`) are not supported.
6. **Root.** `ROOT` is an explicit optional argument defaulting to the current
   directory; a missing root or a non-directory is `error: root does not exist:`
   / `root is not a directory:` (exit 1).
7. **Failure summary** is printed through the shared error report as
   `error: Validation failed with N error(s).` (no leading blank line, `error:`
   prefix); `Plugin not found` gets the same `error:` prefix. Exit statuses are
   unchanged (0 / 1 / 2).
8. **Python-style values in messages.** `repr` of the compared values is
   reproduced for strings, numbers, booleans, `None`, lists and dicts; floats
   use Rust formatting.
9. **Deep directory trees** under `skills/` stop at depth 64 with an error.

## Shared-code changes (additive, called out)

Only `crates/toolkit-cli/src/lib.rs` changed outside the new module:

- `CommandSpec` gains `value_flags` (flags taking a value as `--name v` or
  `--name=v`; `Invocation::value`, last value wins, a missing value is a usage
  error). The existing `skills list` row gets `value_flags: &[]`.
- `Output` gains `with_diagnostics` / `diagnostic` so a handler can write
  stderr lines before returning; `run` now builds the handler's `Output` with the
  stderr writer. `Output::new` and every existing behaviour are unchanged.
- `USAGE` documents the command; the registry/usage drift test also covers value
  flags.
- `COMMANDS` registers `manifests validate`.

`toolkit-core`, `Cargo.toml` and `Cargo.lock` are untouched (no new dependency).
