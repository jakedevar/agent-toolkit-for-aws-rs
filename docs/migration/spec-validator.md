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

Schema evaluation and regular expressions come from maintained Rust crates
(review #4482 replaced the first-cut bespoke evaluator and regex engine, see
"Review repair" below). `crates/toolkit-cli/src/spec/engine.rs` is the adapter:

- **Engine.** `jsonschema` (draft 2020-12, `default-features = false`, so no
  HTTP or file resolver and no TLS; the validator is also built `offline`) with
  `fancy-regex` as its pattern engine, which supports the look-ahead in the
  `name` pattern like Python's `re`. Every match runs under an explicit
  backtracking limit (`BACKTRACK_LIMIT`, 100 000 steps).
- **Fail closed before evaluation.** `compile` refuses (runtime error
  `schema_unsupported`, exit 1) a `$ref` that is not `#` / `#/pointer`, one that
  does not resolve, one that points at a value that is not a schema, a recursive
  reference, `$dynamicRef`/`$recursiveRef`, a nested `$id`, percent-encoded
  references, and a quantifier applied to an anchor (`^*`, `$+`, `\b?`: Python
  rejects "nothing to repeat", `fancy-regex` alone does not). Invalid regular
  expressions and invalid schemas are rejected by the engine at build time.
- **Fail closed during evaluation.** A regex match the engine cannot finish
  (backtrack limit, engine failure) is *never* a match, a non-match or a finding:
  the crate re-evaluates `not`/`oneOf`/`anyOf`/`if` under both outcomes and
  reports only what every outcome agrees on; the adapter turns the remaining
  undecided case into `schema_evaluation_failed` (runtime error, exit 1, "schema
  evaluation failed for <label>: ..."). An error that holds under every outcome
  (for example `maxLength` beside an undecidable pattern) is still an ordinary
  finding.
- **Messages.** The Python wording is rebuilt from the typed error kind
  (`'x' is not of type 'string'`, `'name' is a required property`,
  `Additional properties are not allowed ('a', 'b' were unexpected)`, `'x' was
  expected`, `'' should be non-empty`, `... is too long`, `... does not match
  '<pattern>'`, `... is not valid under any of the given schemas`, `... is valid
  under each of ...`, `... should not be valid under ...`, `False schema does not
  allow ...`), with instance and schema values rendered as Python `repr` of the
  original documents so key order is kept. Findings are sorted by instance path
  then by position in the schema document, printed as `<label>: <message> (at
  <path>)` with `<root>` for the document itself.

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
`crates/toolkit-cli/src/spec/tests.rs` (`T:`) and
`crates/toolkit-cli/tests/cli.rs` (`B:`, real binary).

| # | Python check | Rust check | Tests |
| --- | --- | --- | --- |
| 1 | `reject_symlinks`: any symlink under `plugins/` -> `rel: symlinks are not allowed under plugins/`, sorted by path components, symlinked directories not descended | `reject_symlinks`, `scan_links` | T: `symlinks_under_plugins_are_rejected_in_component_order`, `symlink_directories_are_reported_but_not_descended`, `symlinked_skill_files_are_rejected_and_never_read`; B: `spec_validate_rejects_symlinks_under_plugins` |
| 2 | one entry per directory of `plugins/` (sorted, files skipped), `Validating plugin: plugins/P` | `validate_tree` | T: `plugins_are_visited_in_sorted_order_and_files_are_skipped`, `one_broken_plugin_does_not_stop_the_others` |
| 3 | missing root `plugin.json` -> `rel: missing required root plugin.json (spec §5.1)` | `validate_plugin_dir` | T: `missing_plugin_json_is_reported` |
| 4 | invalid JSON -> `rel/plugin.json: invalid JSON: <err>` | `read_json` (detail is the core parser message) | T: `invalid_plugin_json_is_reported`, `invalid_mcp_json_is_reported` |
| 5 | `plugin.json` against `plugin.schema.json` -> `label: message (at path)` | `schema_errors` + `engine::Schema::validate` | T: `plugin_json_must_be_an_object`, `empty_object_reports_required_members_and_explicit_checks`, `schema_violations_are_reported_sorted_by_path`, `name_pattern_and_length_come_from_the_vendored_schema` |
| 6 | `$schema` must equal the plugin schema id | `validate_plugin_dir` | T: `empty_object_reports_required_members_and_explicit_checks`, `schema_violations_are_reported_sorted_by_path`; positive: `valid_repository_passes_with_progress_lines` |
| 7 | `name` must equal the directory (`name 'x' != directory 'y'`, `None` when absent) | `validate_plugin_dir` | T: `name_must_equal_the_directory` |
| 8 | `extensions` must be an object (spec §8.1); JSON null counts as absent | `check_extensions` | T: `extensions_must_be_an_object_of_reverse_domain_namespaces` |
| 9 | extension namespaces need a `.` (spec §8) | `check_extensions` | same test |
| 10 | every string leaf of an extension, at any depth (`extensions.NS.a.b[1].c` labels), that looks like a path is checked: `\` forbidden, `./` required, `..` forbidden, resolved target must stay in the plugin root (symlinks resolved), `./` targets must exist | `check_extensions`, `collect_strings`, `looks_like_path`, `check_containment` | T: `extension_values_that_are_not_paths_or_that_exist_pass`, `extension_paths_must_exist_and_stay_inside`, `extension_leaves_are_checked_at_any_depth_with_indexed_labels`, `extension_symlink_escapes_are_detected_by_resolution`, `path_like_values_follow_the_original_prefixes` |
| 11 | `mcp.json` optional; invalid JSON; schema; `$schema` must equal the MCP schema id | `validate_plugin_dir` | T: `valid_mcp_configuration_passes_and_is_optional`, `mcp_requires_schema_and_servers`, `mcp_schema_const_and_explicit_schema_check_both_report` |
| 12 | `oneOf` server shapes, `env` property-name exclusion, `cwd` pattern (schema) | `engine::Schema::validate` over the vendored schema | T: `mcp_server_shape_violations_report_one_of_and_the_explicit_rule`, `stdio_env_must_not_set_reserved_names`, `schema_messages_keep_the_published_wording`, `findings_at_one_path_follow_the_schema_keyword_order` |
| 13 | server must be an object | `validate_server` | T: `mcp_server_shape_violations_report_one_of_and_the_explicit_rule` |
| 14 | stdio: `command` must be a string, no placeholders, bare name or `./` path, `./` commands contained | `check_stdio` | T: `stdio_command_rules`, `mcp_server_shape_violations_report_one_of_and_the_explicit_rule` |
| 15 | stdio `env` must be an object and must not set `PLUGIN_ROOT` / `PLUGIN_DATA` (spec §9.2) | `check_stdio` | T: `stdio_env_must_not_set_reserved_names` |
| 16 | stdio `args` / `cwd` (a list or a single truthy value): entries must be strings, only `${PLUGIN_ROOT}` / `${PLUGIN_DATA}` placeholders, path-like values contained (placeholder prefix replaced by `./`) | `check_stdio`, `check_runtime_path`, `find_placeholders` | T: `stdio_args_and_cwd_rules`, `stdio_args_and_cwd_entries_must_be_strings`, `placeholders_are_found_like_the_original_regular_expression` |
| 17 | http/sse: `url` must be a string, no `${...}`, no user info or fragment, HTTPS unless loopback host (`127.0.0.1`, `localhost`, `::1`) | `check_http`, `split_url` | T: `http_url_rules`, `http_url_must_be_a_string`, `urls_split_like_urlsplit`, `bracketed_hosts_must_be_ipv6_literals`, `bracketed_non_ip_hosts_never_get_the_loopback_exception`, `netlocs_are_checked_under_nfkc`, `nfkc_delimiter_netlocs_are_findings_in_the_command_not_passes` |
| 18 | headers: object, no duplicate names under different casing, no `${...}`, no `Authorization` (spec §7.2.1) | `check_http` | T: `http_header_rules` |
| 19 | `skills` present but not a directory -> `rel: skills exists but is not a directory (spec §6.2)` | `validate_plugin_dir` | T: `skills_that_is_not_a_directory_is_reported` |
| 20 | skills: `plugins/*/skills/*/SKILL.md` sorted, directly under `skills/` only; `Validating skills` always printed | `find_skills` | T: `only_direct_children_with_a_skill_file_are_validated_in_sorted_order`, `valid_repository_passes_with_progress_lines` |
| 21 | unreadable `SKILL.md` -> `cannot read as UTF-8: <reason>` | `validate_skill` | T: `unreadable_skill_files_are_findings` |
| 22 | frontmatter must start with `---\n` and end with `\n---\n` searched from offset 4 (empty block unterminated) -> `missing or unterminated YAML frontmatter` | `parse_frontmatter` | T: `frontmatter_must_exist_and_be_terminated`, `frontmatter_block_is_found_like_the_original` |
| 23 | YAML must parse and be a mapping -> `invalid YAML frontmatter: <reason>` / `frontmatter is not a mapping` | `parse_frontmatter` over core `parse_yaml` | T: `invalid_yaml_and_non_mappings_are_reported` |
| 24 | only `name description license allowed-tools metadata compatibility` -> `non-spec frontmatter field(s): a, b` (sorted) | `validate_skill` | T: `only_spec_fields_are_allowed` |
| 25 | `name` required (falsy = missing), at most 64 characters, lowercase, no leading/trailing or doubled `-`, alphanumeric or `-`, equal to the directory (exact NFKC of both) | `check_skill_name`, `nfkc` | T: `name_rules`, `name_length_is_bounded_in_characters`, `name_and_description_are_required`, `nfkc_is_exact_unicode_normalization`, `skill_names_compare_after_composition_and_expansion`, `skill_name_length_counts_normalized_characters`, `float_zero_is_a_missing_required_field`, `quoted_and_nonzero_floats_are_text` |
| 26 | `description` required (falsy or blank = missing; `0`, `0.0` and `-0.0` are falsy), at most 1024 characters | `validate_skill`, `is_falsy` | T: `description_length_rules`, `name_and_description_are_required`, `float_zero_is_a_missing_required_field`, `quoted_and_nonzero_floats_are_text` |
| 27 | `compatibility` at most 500 characters; `allowed-tools` must be a string, not a sequence | `validate_skill` | T: `compatibility_and_allowed_tools_rules` |
| 28 | CRLF / lone CR files accepted (Python text mode) | `universal_newlines` | T: `crlf_skill_files_are_accepted` |
| 29 | exit 0 / `All Agent Plugins v1.0.0 checks passed.` / exit 1 with summary | `validate_command` | T: `command_success_prints_progress_then_the_pass_line`, `command_failure_reports_errors_on_stderr_and_exits_one`, `command_usage_errors_exit_two`, `help_documents_the_command_and_it_is_registered`; B: `spec_validate_passes_on_a_valid_repository`, `spec_validate_reports_findings_on_stderr_and_exits_one`, `spec_validate_missing_schemas_and_bad_arguments` |
| 30 | the real tree conforms | whole repository validated | T: `real_repository_tree_passes`; B: `spec_validate_passes_on_the_real_repository_tree` |

The real-tree tests assert zero findings over this repository's 5 plugins and 76
plugin skills; a genuine finding would fail them and be listed in the Issue
handoff.

## Intentional changes (recorded)

1. **Engines.** Schema evaluation uses the `jsonschema` crate and patterns use
   `fancy-regex` (see above). `jsonschema` was unpinned in Python
   (`uv run --with jsonschema`), so its wording was whatever the latest release
   printed; the adapter reproduces the Python sentences for the keywords the
   vendored schemas use and falls back to the crate's own sentence for other
   keywords. Differences that follow from the engine:
   - Regular expressions use ECMA-262 `$` semantics (end of the string only).
     Python's `re.search` also let `$` match before a final `\n`, so a manifest
     `name` such as `"demo\n"` passed the `name` pattern in Python and is a
     schema finding now. Names with a trailing newline are not valid plugin
     names, so this only tightens the check.
   - `name`'s look-ahead and `cwd`'s alternation behave as in Python for every
     input without a newline.
   - Findings at one instance path keep schema keyword order, which matches
     Python's dictionary-order iteration.
   - A schema with a recursive `$ref` is refused (the vendored schemas have
     none); recursion without consuming the instance cannot be bounded.
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
4. **Unicode normalization is exact NFKC** (`unicode-normalization`). Names and
   directory names are normalized before every check: length counts normalized
   characters, lowercase / hyphen / alphanumeric tests and the directory
   comparison see the normalized text. The earlier fullwidth-only fold is gone.
   Unicode data versions of the crate and of the Python used by CI may differ for
   characters assigned very recently; no real skill is affected.
5. **Inputs where Python crashed are findings; the run continues.** A
   `plugin.json`/`mcp.json`/`SKILL.md` that cannot be read (directory, non-UTF-8,
   larger than 1 MiB) is `rel: cannot read: <reason>` (SKILL.md keeps `cannot
   read as UTF-8: <reason>`); a non-object top-level value yields the schema type
   error and stops the member checks for that file; `mcpServers` that is not an
   object skips the server checks; a URL that `urlsplit` rejects (unbalanced
   brackets, a bracketed host that is not an IPv6 literal, an IPvFuture
   mismatch, an NFKC-hidden delimiter) is `url is not a valid URL (<Python
   reason>)`, never a pass and never a loopback exemption. The `validator raised
   <Type>: ...` wrapper message does not exist in Rust.
6. **Run-level failures** (missing root or `plugins/`, unreadable/invalid/
   unsupported schema, a schema evaluation that cannot finish) are `error: ...`
   with exit 1 instead of tracebacks.
7. **Symbolic links.** Files are read with `read_regular_file`, which refuses
   symlinks; a symlinked manifest or `SKILL.md` therefore reports `cannot read
   ... symbolic links are not followed` in addition to the symlink finding
   (Python read the target). Files over 1 MiB are rejected. Directory listing
   failures during the symlink scan are findings (`cannot list directory`), and
   nesting deeper than 64 levels under `plugins/` is a finding; `rglob` ignored
   both.
8. **Containment** resolves the longest existing prefix of the path with
   `canonicalize` and appends the rest (same helper as the manifests slice).
9. **URL splitting** is a hand port of `urlsplit` (leading C0/space stripped,
   tab/CR/LF removed, scheme, netloc, user info, fragment, hostname) including
   its validation: unbalanced brackets (`Invalid IPv6 URL`), the bracketed-host
   check of Python 3.12 (an IPv6 literal with optional `%zone`, or an IPvFuture
   `v<hex>.<text>`; `[name]` and `[1.2.3.4]` are rejected with Python's
   wording) and `_checknetloc` (a non-ASCII netloc whose NFKC form gains one of
   `/ ? # @ :` is rejected). The reported reason text is Python's.
10. **Failure summary** is printed through the shared error report as `error:
    Spec validation failed with N error(s).` (no leading blank line); exit
    statuses are unchanged (0 / 1) and the command rejects arguments other than
    one optional ROOT with the shared usage error (exit 2).
11. **Output ordering.** Findings are printed after the run, progress lines on
    stdout and `  ERROR:` lines on stderr, each stream in order (the manifests
    command behaves the same).
12. **Python truthiness of float zero.** `0.0` and `-0.0` are falsy exactly like
    `0` (`str(fm.get(k, "") or "")`), so `description: 0.0` is a missing
    description and `name: 0.0` a missing name. A quoted `"0.0"` is text. A
    non-zero float is rendered with Python's `repr` (`1.0`, not `1`), so `name:
    1.0` is `'1.0'` and fails the character and directory checks as in Python.

## Review repair (#4482, changes requested on `8cb33298`)

| Finding | Resolution |
| --- | --- |
| B1 regex recursion limit is not a work bound; exhaustion could be a match; `^*` accepted | Bespoke `regex.rs` deleted. `fancy-regex` through `jsonschema` with a 100 000 step backtrack limit; patterns without look-around or back-references run on the linear `regex` engine. Exhaustion is `schema_evaluation_failed` (exit 1), never a match. `^*`-style patterns rejected up front (T: `invalid_regular_expressions_are_rejected`, `catastrophic_patterns_stay_bounded`, `backtracking_exhaustion_is_an_evaluation_failure_not_a_match`, `instance_violations_stay_distinct_from_evaluation_failures`, `exhaustion_while_validating_a_manifest_is_a_runtime_error`) |
| B2 external / missing / non-schema `$ref` could pass under `not` | Bespoke `jsonschema.rs` deleted. `compile` checks every `$ref` (local, resolving, schema-valued, acyclic) before the crate sees the schema, and the crate is built `offline`. Operational failures are `Err`, distinct from instance findings (T: `external_references_are_rejected_even_under_combinators`, `missing_non_schema_and_unsupported_references_are_rejected`, `recursive_references_are_rejected`, `a_replacement_schema_cannot_turn_failures_into_a_pass`, `unusable_roots_and_schemas_are_runtime_errors`) |
| B3 float zero accepted as present | `is_falsy` treats `0.0`/`-0.0` as falsy (shared helper, see below); floats render with Python `repr` (T: `float_zero_is_a_missing_required_field`, `quoted_and_nonzero_floats_are_text`) |
| B4 approximate NFKC | Exact NFKC with `unicode-normalization` for names and directories before all checks (T: `nfkc_is_exact_unicode_normalization`, `skill_names_compare_after_composition_and_expansion`, `skill_name_length_counts_normalized_characters`) |
| B5 URL splitter accepted `[name]`, `[IPv4]`, NFKC delimiters | Hand-ported `urlsplit` bracket and netloc validation (T: `bracketed_hosts_must_be_ipv6_literals`, `bracketed_non_ip_hosts_never_get_the_loopback_exception`, `netlocs_are_checked_under_nfkc`, `nfkc_delimiter_netlocs_are_findings_in_the_command_not_passes`) |

### Dependencies (`crates/toolkit-cli/Cargo.toml` only; `toolkit-core` is untouched)

| Crate | Features | Why it is needed | Why not less |
| --- | --- | --- | --- |
| `jsonschema` 0.60 | `default-features = false` (drops `resolve-http`, `resolve-file`, `tls-aws-lc-rs`, `idna`, so no `reqwest`, `rustls` or network stack) | Draft 2020-12 evaluation of the vendored plugin and MCP schemas (`oneOf`, `$ref`, `propertyNames`, `not`, ...) with typed errors and the "undecided regex match" handling that keeps combinators from inverting an engine failure. Brings `fancy-regex` as its pattern engine, which gives Python-compatible look-ahead | Writing and proving a second evaluator was the review's blocking finding |
| `serde_json` 1 | `default-features = false`, `std` | `jsonschema` takes a `serde_json::Value`; the toolkit's own `Json` is converted once per file (instances and schemas) | The conversion type is part of the crate's API; the adapter renders messages from the original `Json`, not from the `Value`, so key order is kept |
| `unicode-normalization` 0.1 | `default-features = false`, `std` | Exact NFKC for skill names, directories and URL authorities (B4, B5) | The standard library has no normalization tables |

`fancy-regex` is not a direct dependency; it is reached through `jsonschema`.
`cargo tree -p toolkit-cli -e normal` lists 79 packages, all pure Rust, none
with a build script that downloads or executes network content; `cargo generate-lockfile`
produced `Cargo.lock` (committed).

## Shared-code changes (called out)

- `crates/toolkit-cli/src/lib.rs`: `pub mod spec;`, one `COMMANDS` row
  (`spec validate`, no flags, one optional positional), the `USAGE` text.
- `crates/toolkit-cli/src/manifests.rs`: `Report::events` and the helpers
  `py_repr`, `py_repr_opt`, `py_repr_str`, `py_strip`, `is_falsy`,
  `universal_newlines`, `read_failure` and `resolve_lenient` are `pub(crate)` so
  the spec module reuses the Python-compatible formatting and path resolution.
  Behaviour changes (B3), both toward Python and covered by the manifests tests
  staying green: `is_falsy` now treats float zero as falsy (affects the
  marketplace `source` skip and the plugin `name` comparison, where Python also
  treats `0.0` as falsy), and `py_repr` renders floats with Python's `repr`
  (`py_float_repr`: `1.0`, `1e+16`, `inf`) instead of Rust's `Display`.
- `toolkit-core`, the Makefile and `.rsi/*` are untouched.
