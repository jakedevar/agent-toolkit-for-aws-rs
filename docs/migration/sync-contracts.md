# Synchronization contracts (#4377, slice `sync`)

Rust replacements for the three Python files of slice `sync`
(`docs/migration/python-inventory.json`, `.slice == "sync"`; all three belong:
two toolkit helpers and the test file of one of them, confirmed against the
#4371 U-10 note). Python stays in the tree until the CI wiring (`mise.toml`) is
moved in #4378; nothing here deletes it.

| Python | Rust |
| --- | --- |
| `tools/sync-plugin-skills.py` | `toolkit sync plugin-skills` (`toolkit_core::plugin_skills`) |
| `plugins/aws-startup-advisor/tools/sync-vendored.py` | `toolkit sync vendored` (`toolkit_core::vendored`) |
| `plugins/aws-startup-advisor/tools/test_sync_vendored.py` | unit tests in `vendored.rs`, `crates/toolkit-cli/tests/sync.rs` |

Shared primitives (byte-exact compare, symlink-refusing listing, atomic copy,
staged directory replacement) live once in `toolkit_core::treesync`; neither
command re-implements them, and the telemetry bundle gate (#4398) is meant to
build on them too.

## Commands

```
toolkit sync plugin-skills [--root DIR] [--plugin NAME] [--check]
toolkit sync vendored [--plugin-root DIR] [--check]
```

`--root` (default `.`) is the repository root holding `skills/` and
`plugins/`; `--plugin-root` (default `plugins/aws-startup-advisor`, relative to
the current directory) is the plugin directory. Values may be written
`--flag VALUE` or `--flag=VALUE`. Run from the repository root.

Without `--check` both commands write; with `--check` they only report and
never write. Exit 0 = in sync (or synced), 1 = drift / refusal / runtime error,
2 = usage error.

## Parity table

| Behavior | Python | Rust | Status |
| --- | --- | --- | --- |
| plugin-skills: canonical index | every `skills/**/SKILL.md` parent dir by name (rglob; last duplicate wins by filesystem order) | same discovery by directory name; duplicates are an error only when a bundled skill uses the ambiguous name | **changed** (deterministic) |
| plugin-skills: match | same file set and `filecmp` deep bytes, files only | same (empty directories are not compared) | same |
| plugin-skills: sync | `rmtree` + `copytree` (non-atomic) | staged copy swapped in, then old copy removed; permissions kept | **changed** (atomic) |
| plugin-skills: order and text | plugins sorted; `Checking|Syncing plugin: X`, `  synced N <- skills/..`, blank line, `All plugin skills are in sync.` | identical stdout | same |
| plugin-skills: failure | stderr `\nN error(s):`, lines, check hint `Run 'python3 tools/sync-plugin-skills.py' to fix.`, exit 1 | stderr `error: N error(s):` + same lines + hint `Run 'toolkit sync plugin-skills' to fix.`, exit 1 | **changed** (prefix, command name) |
| plugin-skills: `--plugin X` missing | stderr `Plugin not found: X`, exit 1 | stderr `error: Plugin not found: X`, exit 1; names with `/`, `..`, empty are rejected (`Invalid plugin name`) | **changed** (safer) |
| plugin-skills: missing `plugins/` | traceback | runtime error exit 1 | **changed** |
| vendored: managed files | files under `references/vendored/`, not `README.md` (any depth), not under top-level `telemetry/` | same | same |
| vendored: README table | regex on `` | `a` | `b` | `` rows, last duplicate wins; rows must exist, files must have rows, canonical must be `skills/shared/<path>` | same rules and messages (command hint now `toolkit sync vendored`); rows are parsed line by line | same, wording of hint **changed** |
| vendored: write | restore missing listed copies, overwrite drifted copies, never delete; orphans stay as errors | same, each write atomic (temp file + rename) | same |
| vendored: output | `PASS — N vendored file(s) byte-identical to skills/shared/ and matching their README tables`; `OK — N vendored file(s) checked, M synced`; `FAIL — N vendored file(s) out of contract:` + `  - e` on **stdout**, exit 1; N counted before any write | identical stdout; additionally one stderr line `error: N vendored file(s) out of contract` | same (+ stderr summary) |
| vendored: displayed paths | plugin-relative, absolute for trees elsewhere | always relative to `--plugin-root` | **changed** (deterministic) |
| vendored: missing plugin root | passes (no `skills/`) | root that is not a directory is an error; a root with no `skills/` still passes | **changed** |
| vendored: README row paths | trusted (`vend / rel`, so `../` could write outside) | must already be normalized relative paths; others are reported and never touched | **changed** (traversal closed) |
| vendored: rows for `README.md` / `telemetry/` paths | would be restored from `skills/shared` if missing | never restored (reported as missing, as before) | **changed** (telemetry stays with #4398) |
| vendored: symlinks | `is_file()`/copy follow links (read and write through them) | never followed; a vendored link is drift and is replaced as a link; a canonical link or a link in `skills/shared` is "no canonical source" | **changed** (safety) |
| vendored: unlisted-file error order | `sorted()` of strings | same | same |

## Safe deletion and symlink boundaries (AC2, AC3)

- The only recursive deletion in either command is the *old copy* of a bundled
  skill directory that has just been replaced: a real directory (never a
  link), a direct child of `plugins/<plugin>/skills/`, with a canonical
  counterpart. `remove_dir_all` unlinks symlinks it meets instead of following
  them, so link targets inside a bundled copy are never touched. `vendored`
  deletes nothing at all.
- Backup and staging names are reserved, never assumed free. The staging
  directory (`.<skill>.toolkit-sync-new-<pid>-<n>`) and the moved-aside backup
  (`.<skill>.toolkit-sync-old-<pid>-<n>`) are each created exclusively
  (`create_dir`; `AlreadyExists` retries with a fresh suffix, bounded). The
  `rename(dest, backup)` therefore only replaces the empty placeholder this
  call created, so a pre-existing sibling with a would-be name (including an
  empty directory, which POSIX `rename` would otherwise silently replace)
  survives untouched. Fixture: `replace_tree_never_clobbers_a_pre_existing_backup_name`.
- Read-only old trees (review #4479). Before the final `remove_dir_all`, the
  moved-aside backup is walked with `symlink_metadata` (links are never
  followed, nothing outside the backup is touched) and owner `rwx` is added to
  each directory inside it; a bundled skill directory with mode `0555` and
  files therefore syncs and leaves no backup. Tests run unprivileged; the
  read-only test probes whether directory modes are enforced and skips with a
  message when running as root. If the cleanup still fails the **new tree
  stays installed**, the other skills are still processed and the command exits 1 with the
  `cleanup_failed` error (listed as `<plugin>: <skill>: installed the new copy ...`) naming the leftover backup path
  (`installed the new copy of <dest> but could not remove the replaced copy
  (<cause>); remove it manually: <backup>`); the leftover is the previous
  bundled copy and can be deleted by hand. Test:
  `replace_tree_keeps_the_new_tree_and_names_the_backup_when_cleanup_fails`
  (failure injected, so it needs no privileges).
- Staging cleanup on every error path (review #4479 re-review, #4495). The
  staging copy reproduces source directory modes, so a nonempty `0555` source
  leaves a stage that plain `remove_dir_all` cannot empty. Every error path
  after staging is created (populate failure, backup-name reservation failure,
  move-aside failure, install failure) removes this call's own stage with the
  same permission-aware, link-safe cleanup as the old copy (owner `rwx` added
  to real directories inside the stage only, links never followed). The
  original destination is untouched on these paths. If that cleanup itself
  fails, the returned error keeps its original code and message and appends
  `additionally could not remove the staging directory (<cause>); remove it
  manually: <stage>`. Fixtures: `backup_exhaustion_after_staging_removes_a_read_only_stage`
  (unprivileged, skipped with a message when running as root: nonempty `0555`
  source, one successful staging suffix, then 1000 colliding backup suffixes;
  the destination and the collision sibling survive and this call's stage is
  gone) and `stage_cleanup_failure_names_the_leftover_stage_and_keeps_the_error`
  (failure injected).
- Symbolic links are never followed: a link as plugin, `skills/` or bundled
  skill is an error; a link inside a canonical source stops that skill (nothing
  is copied, everything else continues); a link inside a bundled copy or a
  vendored file is drift and is replaced as a link (`rename` replaces the link,
  it never writes through it).
- Threat model is the one of `toolkit_core::paths`: static symlinks are always
  refused; concurrent mutation of the tree while the tool runs is not defended
  against (no handle-anchored traversal).

## Telemetry seam (#4398)

`toolkit sync vendored` skips the top-level `telemetry/` vendored directory
through `toolkit_core::vendored::SKIP_TOP_LEVEL_DIRS` (value
`TELEMETRY_BUNDLE_DIR`). The telemetry port adds its own bundle sync on
`toolkit_core::treesync` and changes that constant in the same change, so the
two gates never both own the bundle. The plugin-skills command copies a skill's
whole directory byte for byte (including any `references/vendored/telemetry`
inside it); it has no telemetry-specific rule.

## Core changes (minimal, additive)

- `toolkit_core::paths::open_regular_file`: the symlink/identity-checked open
  that `read_regular_file` already did, extracted so byte-exact comparison can
  reuse it without the text size limit (`read_regular_file` now calls it;
  behavior and tests unchanged).
- `toolkit_cli::CommandSpec::value_flags` and `Invocation::value`: long flags
  that take one value (`--flag VALUE` / `--flag=VALUE`; missing, empty or
  `-`-prefixed value is a usage error). Same minimal shape as the sibling #4375
  branch; whichever lands second reconciles the field.
- `toolkit_core::{treesync, plugin_skills, vendored}` modules; `toolkit sync`
  routes and usage text.

## Verification evidence

Unit tests port every case of `test_sync_vendored.py` and add negative cases for
links, traversal, duplicates and refusals. `crates/toolkit-cli/tests/sync.rs`
runs the binary end to end and runs both commands in **check mode on the real
repository tree** (read-only). At the base commit the real tree has no drift
for either command.
