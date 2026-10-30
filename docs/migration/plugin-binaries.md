# Per-plugin `./bin` binaries: build and stage pipeline (#4487)

Status: pipeline, contract test and design implemented on the #4487 branch. Nothing is
released. Integration round 21 (#5078) switched the live aws-core `hooks.json` to the
canonical secret-safety launcher (section 9, follow-up 1). The staged binaries travel with
the release (#4414).

## 1. Decision and scope

The GM ruling recorded on #4487, #4413 and #4993 chose option (i) of #4993: each plugin
ships its own platform binaries under `plugins/<plugin>/bin/`, and one pipeline stages
every such binary. It serves the secret-safety PreToolUse hook (`secret-safety-hook`
crate, bin `secret-safety`, #4412) now. Later it serves the `toolkit` binary that the
#4413 MCP proxy cutover uses.

The #5586 GM ruling adds a subprocess `toolkit` dispatcher to `aws-core`,
`aws-agents` and `aws-startup-advisor`, each built with exactly
`["ai-models-live", "docgraph-live"]` on every declared platform. The default
CLI and shared AWS crate features remain empty and SDK-free. The separately
distributed OpenClaw package retains its existing `agents-pay-live` build;
payment commands use that package's dispatcher. See
[the invocation inventory and default-build guard](shipped-toolkit.md).

Each plugin is installed into its own cache directory, and a plugin cannot reach a
sibling plugin's files. So a binary that two plugins need is staged into both. The
size cost is: binary size, times five platforms, times the plugins that ship it.

The pipeline has three parts:

| Part | Path | Committed |
| --- | --- | --- |
| Declaration: which binaries each plugin ships, from which package/bin/features, for which platforms, in which launcher role | `packaging/plugin-binaries.json` | yes (hand-edited, reviewed) |
| Staging tool (Rust) | `crates/plugin-stage` (bin `plugin-stage`) | yes |
| Staged binaries, dispatchers and manifest | `plugins/<plugin>/bin/` | no: generated and gitignored |

## 2. Target triples

| Platform key | Rust target | Linker used by `plugin-stage` | Built locally (Linux x86_64 host, zig 0.17.0, cargo-zigbuild 0.23.4) | Needs CI or another host |
| --- | --- | --- | --- | --- |
| `linux-x86_64` | `x86_64-unknown-linux-gnu`, glibc floor 2.17 | `cargo zigbuild --target x86_64-unknown-linux-gnu.2.17` | yes; highest glibc symbol needed: `GLIBC_2.16` | no |
| `linux-aarch64` | `aarch64-unknown-linux-gnu`, glibc floor 2.17 | `cargo zigbuild` | yes (ELF aarch64) | a run on real aarch64 Linux (no qemu here) |
| `macos-x86_64` | `x86_64-apple-darwin` | `cargo zigbuild` (no Apple SDK; zig ships the libSystem stubs) | yes (Mach-O x86_64) | a run on a Mac |
| `macos-aarch64` | `aarch64-apple-darwin` | `cargo zigbuild` | yes (Mach-O arm64, ad-hoc `LC_CODE_SIGNATURE` present) | a run on an Apple Silicon Mac |
| `windows-x86_64` | `x86_64-pc-windows-gnu` | `cargo zigbuild` | yes (PE32+); runs under wine | a run on Windows, under Git Bash and from a native launch |

On the host platform without a glibc floor (for example a native macOS build),
`plugin-stage` uses plain `cargo build`. Linux always uses zig, because zig links the
glibc 2.17 floor. A plain host build would require the build machine's glibc, which is
often newer than many user machines have.

Choices made, with the alternatives:

- **glibc 2.17 instead of musl.** The `*-linux-musl` targets would give fully static
  binaries, but their standard libraries are not installed here. Adding them needs
  `rustup target add`, which downloads them. A glibc 2.17 floor covers every
  mainstream glibc distribution released since 2012. On Alpine (musl) the glibc binary
  cannot start. The guard then fails closed (section 7). Switching a platform to musl
  is a one-line declaration change once the target is installed.
- **`windows-gnu` instead of `windows-msvc`.** MSVC linking needs either a Windows
  host or `cargo-xwin`, and neither is installed. The GNU target links with zig on
  Linux and needs no runtime DLL beyond the system ones.
- **Windows on ARM64** runs `windows-x86_64` under the OS x64 emulation, so it has no
  separate platform.

Feasibility evidence: on this branch (rolling `fa3fa137` plus this change, before it was committed), `plugin-stage stage --platform all`
built a probe binary for all five platforms in 46 s. The probe was `plugin-stage`
itself, which has the same dependency profile as `secret-safety-hook`: pure Rust plus
`toolkit-core`. It wrote a manifest that `plugin-stage verify` accepted. The Linux
x86_64 binary ran through its dispatcher, and the Windows binary ran under wine. The
`secret-safety-hook` crate is not on rolling yet, so it was not built here.

Not yet proven: whether `toolkit` with the `toolkit-aws/sdk` feature cross-builds with
zig. It needs `aws-lc-sys`, which builds C code with cmake through `zig cc`. #4413
must prove that before its S5 cutover.

No target needs a paid runner or a paid service. One Linux host with zig and
cargo-zigbuild builds all five. The steps marked "needs CI or another host" are only
runtime smoke runs on real hardware. They can run on an operator's own machines, or on
GitHub-hosted runners, which are free only for public repositories. No workflow file
was added, so no runner is started by this change.

## 3. Layout under each plugin

```text
plugins/<plugin>/bin/
  manifest.json              provenance manifest (generated)
  <name>                     POSIX dispatcher (generated, 0755)
  <name>.exe                 windows-x86_64 binary
  linux-x86_64/<name>
  linux-aarch64/<name>
  macos-x86_64/<name>
  macos-aarch64/<name>
```

Every staged file is written through a temporary file and a rename, with mode 0755, so
a launcher never runs a half-written binary. The Windows binary sits next to the
dispatcher, not in a platform directory, so that both Windows launch paths in
section 4 reach it.

### Package destinations (#4411 B3)

A declaration entry carries exactly one of `plugin` or `package_root`. A
`package_root` is a repository-relative path (plain `[A-Za-z0-9._-]` segments,
no `.`/`..`, no leading or trailing `/`) to an npm/ClawHub package inside the
source tree, such as `plugins/aws-agents/skills/agents-pay/packages/openclaw`.
It stages into `<package_root>/bin/` with exactly the layout, dispatcher,
manifest (`"package_root"` instead of `"plugin"`), sha256 and modes above.

- The entry's role must be `subprocess` (and only package entries may use it):
  code spawns the platform file directly, so `plugin-stage launcher` refuses it
  and the launcher allowlist and inventory of section 4 are unchanged.
- Every path component must be a real directory (no symlink). The directory must
  hold a regular `package.json` and a `.gitignore` with a `bin/` line (and no
  `!` negation line, since verify-package cannot evaluate patterns without git). It may not lie inside a `plugins/*/bin/` or another
  package destination's `bin/`. `verify`, `verify --complete` and `stage` check
  this before reading or writing anything.
- `check` also refuses any git-tracked file under a package destination's `bin/`.
  Package destinations are never force-added to a release tree. The package ships
  as a tarball (`npm pack`), not from git.
- `plugin-stage verify-package --package-root DIR --complete` is the npm
  `prepack` guard (R-PKG-4). It finds the declaration in the nearest ancestor of
  the current directory (or `--root`), refuses a `DIR` that is not a declared
  package destination, and runs the same per-destination verifier as
  `verify --complete` on that destination alone. It needs no git, Cargo or
  network.
- No test target stages the real declaration (R-PKG-7, T-PKG-NOLIVE). The
  OpenClaw entry builds `toolkit-cli` with `agents-pay-live`, so only
  `make plugin-binaries` (a release target, not a verification target) stages it.

## 4. Launchers: how hooks.json and .mcp.json pick the binary

Launchers never name a platform directory. They always call the dispatcher
`${CLAUDE_PLUGIN_ROOT}/bin/<name>`. The contract test rejects any launcher that reaches
into `bin/<platform>/`, names `bin/<name>.exe`, or passes the bare `bin` directory.

**The dispatcher** is a short POSIX `sh` script, generated byte for byte by
`launcher::dispatcher_script`:

1. It maps `uname -s` to `linux`, `macos` or `windows` (`MINGW*`, `MSYS*` or
   `CYGWIN*`).
2. It maps `uname -m` to `x86_64` (`x86_64`, `amd64` or `x64`) or `aarch64`
   (`aarch64` or `arm64`).
3. Any other OS or architecture (FreeBSD, `armv7l`, `i686`, an empty or failing
   `uname`) is unsupported: it writes `<name>: unsupported platform ...` to stderr and
   exits 127 before it looks at any file (#5025). No path is ever built from an
   unsupported value, so a hand-placed `bin/unsupported-x86_64/<name>` can never run.
4. It runs `bin/<os>-<arch>/<name>` with `exec`, passing its arguments and stdin
   through. On Windows (x86_64, or ARM64 through Windows' x86_64 emulation) it runs
   `bin/<name>.exe`.
5. If the file is missing or not executable, it writes
   `<name>: no staged binary for <os>-<arch> ...` to stderr and exits 127.

The `uname` output only chooses between fixed `case` branches and is never pasted into
a path, so it cannot inject anything. Under Rosetta, `uname -m` reports `x86_64` and
the x86_64 binary runs translated, which is correct. A 32-bit Git Bash (`i686`) is
refused rather than guessed at, so the guard denies there.

**Hooks (role `guard-hook`).** The command is the #4412 fail-closed launcher, unchanged
except for the path. `plugin-stage launcher aws-core/secret-safety` prints it:

```text
sh -c 'if out=$("$0" 2>/dev/null); then printf %s "$out"; else printf %s "{\"hookSpecificOutput\":{...\"permissionDecision\":\"deny\",...}}"; fi' "${CLAUDE_PLUGIN_ROOT}/bin/secret-safety"
```

A unit test (`the_guard_command_matches_the_4412_launcher_contract`) pins the string. It
was also compared byte for byte with the #4412 `hooks.json` command at `44c15ea9`,
where it differs only in the path. The contract requires a guard-hook launcher to be
exactly this canonical string and to be registered under `PreToolUse`.

**Windows `.exe` through `sh -c "$0"`.** Claude Code runs hook commands with Git Bash on
Windows, so `sh -c '... "$0" ...' ".../bin/secret-safety"` runs the dispatcher script.
`uname -s` reports `MINGW64_NT-...`, and the dispatcher then runs
`bin/secret-safety.exe`. The extensionless dispatcher file exists, so MSYS runs it and
does not go looking for the `.exe` itself; the dispatcher decides. Tests run this path
with a faked `uname` (`MINGW64_NT`, `MSYS_NT`, `CYGWIN_NT`). A real run on Windows is
still open.

**MCP servers (role `mcp-server`).** The server `command` must be exactly
`${CLAUDE_PLUGIN_ROOT}/bin/<name>` (or `./bin/<name>`). `sh -c` wrappers and extra
words in `command` are refused, and `args` follow the literal-data rule below.

- On Unix the host spawns the dispatcher script directly.
- On Windows the host launches `bin/<name>` without a shell. Windows process creation
  (and Node/libuv `spawn`) appends `.exe` to a name that has no extension, so it reaches
  `bin/<name>.exe` without needing `sh`.

That Windows behaviour must be verified on a Windows host before #4413 S5. If it fails,
the fallback is a host that runs MCP commands through `sh`, which Git for Windows
provides.

**Role `hook`** (non-guarding hooks, for example telemetry) may be followed by
`|| true`, because a missing binary there must not block the user. Its only templates
are `"${CLAUDE_PLUGIN_ROOT}/bin/<name>"` and `"${CLAUDE_PLUGIN_ROOT}/bin/<name>" || true`.

### Launcher allowlist (#5022, #5053, #5069, #5070, #5071, #5088)

Three review rounds found ways to spell a path into `bin/` that a classifier missed:

1. Quoting: `"${CLAUDE_PLUGIN_ROOT}"/bin/x || true`.
2. Repeated separators and continuations: `.//bin/x` and `b\<LF>in`.
3. Shell expansion: POSIX classes `b[[:lower:]]n`, brace ranges `{b..b}in` and
   positional construction `sh -c '"$0/$1$2/x"' "${CLAUDE_PLUGIN_ROOT}" b in`.

Following the PM ruling on #4487, the checker no longer detects bad shell. It accepts
only a closed set of templates and refuses everything else
(`crates/plugin-stage/src/launcher.rs`).

**Inventory.** `crates/plugin-stage/tests/fixtures/shipped-launchers.json` lists every
shipped launcher string with the template it matches. That covers every hook
`command`, MCP `command` and MCP `args` element in a scanned `hooks.json`, `.mcp.json`
or `mcp.json` on rolling: 12 hook commands, 6 MCP commands and 30 MCP args. The plugin
manifests only point at those files. Tests pin the inventory as complete
(`the_inventory_lists_every_shipped_launcher_string`) and pin each string to its
template (`shipped_strings_match_their_inventory_templates`).

**Shell-evaluated strings** (a hook `command`) must be one of these templates byte for
byte. Only the `<...>` parts vary:

| Template id | Shape |
| --- | --- |
| `guard-deny` | the canonical guard deny launcher above, for a declared `guard-hook` |
| `bin-hook` | `"${CLAUDE_PLUGIN_ROOT}/bin/<name>"`, for a declared `hook` |
| `bin-hook-or-true` | `"${CLAUDE_PLUGIN_ROOT}/bin/<name>" \|\| true`, for a declared `hook` |
| `plugin-file` | `"${CLAUDE_PLUGIN_ROOT}/<relpath>"` |
| `cat-plugin-file` | `cat "${CLAUDE_PLUGIN_ROOT}/<relpath>" 2>/dev/null \|\| true` |
| `python-guard` | `sh -c 'python3 "$0" 2>/dev/null \|\| python "$0" 2>/dev/null \|\| py -3 "$0"' "${CLAUDE_PLUGIN_ROOT}/<relpath>"` |
| `python-optional` | as `python-guard`, with ` 2>/dev/null \|\| true` after `py -3 "$0"` |
| `python-args`, `-reconcile`, `-session-end` | `sh -c 'python3 "$0" "$@" 2>/dev/null \|\| python "$0" "$@" 2>/dev/null \|\| py -3 "$0" "$@" 2>/dev/null \|\| true' "${CLAUDE_PLUGIN_ROOT}/<relpath>"`, then nothing, ` --reconcile` or ` --session-end` |
| `cursor-python-args-reconcile`, `-session-end` | `sh -c 'python3 "$0" "$@" 2>/dev/null \|\| python "$0" "$@" 2>/dev/null \|\| true' "${CURSOR_PLUGIN_ROOT}/<relpath>"`, then ` --reconcile` or ` --session-end` |

- `<name>` is a binary that the declaration declares for that plugin with that role.
- `<relpath>` matches `^[A-Za-z0-9_][A-Za-z0-9_.-]*(/[A-Za-z0-9_][A-Za-z0-9_.-]*)*$`.
  No segment may be `bin` in any case, with trailing dots ignored as on Windows (so
  `Bin.` counts as `bin`). The first-character class already excludes `.`, `..` and
  hidden segments. Any part of the relpath that exists in the plugin as a symlink is
  refused, so a plain spelling cannot lead into `bin/`.
- The python shapes are exactly as shipped and stay until #4412 activates the binary
  and #4415 retires the scripts.
- `CURSOR_PLUGIN_ROOT` twins exist only where the inventory has them (the Cursor
  telemetry launchers). A Cursor guard or binary-hook launcher would be a new shape.
- A guard is also checked to be registered under `PreToolUse`.

**MCP servers** are checked as a whole object, `command` and `args` together (#5088,
PM ruling after review #5018 round 4). Round 4 showed why one string at a time is not
enough: `env -C "${CLAUDE_PLUGIN_ROOT}/work" -S "sh -c '../b[[:lower:]]n/x || true'"`
passed, because `env` looked like a harmless program and the packed argument looked
like data, yet `env -S` splits that argument into a real shell invocation. The
`command` must be one of:

- **A canonical binary command** (above) of a declared `mcp-server`. Its `args` go to
  our own binary and are literal data (below), and the other keys of the object are
  scanned as literal data too.
- **Exactly a program of the shipped inventory.** The only one is `uvx` (`MCP_PROGRAMS`
  in `launcher.rs`). Its whole argv must match its template, and the server object may
  hold only `type` (`"stdio"`), `command` and `args`, so no `env` or `cwd` can steer it.

| Template id | `uvx` argument |
| --- | --- |
| `uvx-package` | first: `mcp-proxy-for-aws-cli==<digits>[.<digits>...]`, a package of the inventory with a pinned version; no `uvx` option (`--from`, `--with`, ...) can come before it |
| `uvx-url` | second: `https://<host>[/<path>]`; lowercase DNS labels (at least two), path segments `[A-Za-z0-9_-][A-Za-z0-9._-]*`, no user, port, query, fragment or `%` |
| `uvx-skip-auth` | then any number of: `--skip-auth` |
| `uvx-metadata`, `uvx-metadata-value` | or `--metadata` followed by `<KEY>=<value>`, `[A-Z][A-Z0-9_]*=[A-Za-z0-9][A-Za-z0-9._-]*` |

Every `uvx` argument must also pass the literal-data rules below, so none names the
plugin root or holds a `bin` segment. Everything else is refused, whatever its args:

- program launchers such as `env`, `xargs`, `nice`, `nohup`, `timeout`, `stdbuf`,
  `setsid`, `sudo`, `doas` and `busybox`;
- shells (`sh`, `bash`, `cmd`, `powershell`, `pwsh`, ...);
- interpreters (`python`, `node`, `ruby`, `perl`, ...);
- any other program name or spelling (`UVX`, `uvx.exe`, `/usr/bin/env`, `npx`);
- a non-string `command`, and `args` with no `command`.

A new MCP program or argument shape needs a reviewed template change and a new
inventory entry, just like a new hook shape.

**Every other string** (MCP `args`, `matcher`, `type`, `url`, headers and so on) is
literal data:

- It may name `CLAUDE_PLUGIN_ROOT` or `CURSOR_PLUGIN_ROOT` (in any case) only as exactly
  `${CLAUDE_PLUGIN_ROOT}/<relpath>`.
- It may not hold a `bin` path segment. Segments are split at every character outside
  `[A-Za-z0-9_.-]`.
- An MCP `args` element may not name a shell (a defence in depth for canonical binary
  servers; program servers are already held to their template).
- An exact canonical binary launcher outside a `command` key is refused ("may only
  appear in a launcher `command` field").

**Everything else is refused.** The message names the file and JSON key and never
echoes the string. When a refused `command` starts like a binary launcher, the message
says why: undeclared name, platform path, `.exe` or wrong role. A new hook shape needs
a reviewed template change and a new inventory entry. `plugin-stage check` reports a
declared guard with no registration as not enforced.

**Tests** (`crates/plugin-stage/tests/launcher_allowlist.rs` and
`packaging_contract.rs`):

- Every reproduction from #5022, #5053, #5069, #5070, #5071 and #5088 is rejected, for
  hooks under `PreToolUse` and `Stop` and for MCP command/args splits. For #5088 that
  covers the reviewer's exact JSON on `aws-core`, and split `-S`, attached `-Ssh`,
  `--split-string` and `--split-string=` wrappers around the fixture guard, MCP server
  and hook binaries. Each launcher, shell and interpreter the ruling names is refused
  with any args, and so are `uvx` argv bypasses (`--from`, an unpinned or foreign
  package, extra flags, a root or `bin` argument, extra `env` or `cwd` keys).
- A runtime test shows the round-3 forms fail open without the binary and reach a
  marker binary under `sh` and `bash`. The canonical guard denies.
- A runtime test (where `env` supports `-C` and `-S`) shows the #5088 split, attached
  and long `-S` forms exit without a deny when no binary is staged and run a marker
  `bin/guard` when one is.
- A deterministic exhaustive test inserts each of 31 shell metacharacters at every
  position of every template instance. That is 52,855 mutants since the #4412 switch
  (57,412 before it, when the inventory still held the aws-core `python-guard` string),
  each run under a guard event and a plain event. Shipped `guard-deny` instances name a
  binary that only aws-core declares, so the fixture plugin exercises its own declared
  guard instead. The same insertions into each MCP command (with the
  args it accepts) and into every element of each of the three distinct shipped `uvx`
  argvs (12,183 argv mutants) are refused too. No mutant is accepted.

**Residual.** An MCP server can no longer start an interpreter or a wrapper: its
command is `uvx` with that template, or our own binary. What remains is trust in what
those programs do with their arguments. The pinned `mcp-proxy-for-aws-cli` package and
the URL it connects to are a supply-chain matter, not a launcher spelling. Likewise, a
hook template that runs a plugin file (`plugin-file`, the python shapes) trusts that
file's content: a reviewed plugin script could itself call `bin/`, which is a
source-review matter.

## 5. Checksums and provenance

`plugin-stage stage` writes `bin/manifest.json`:

- `source_commit`: the full commit from `git rev-parse HEAD`.
- `source_dirty`: true when `git status --porcelain --untracked-files=all` reports
  anything: a tracked change, or any untracked file that is not ignored, anywhere in
  the checkout (#5027). Cargo builds untracked inputs without being told to: an
  auto-discovered `build.rs`, a new module, `.cargo/config.toml`,
  `rust-toolchain.toml`. A build with any of them present is therefore not a build of
  the named commit. Ignored files (`target/` and the generated `plugins/*/bin/`) stay
  permitted. The check covers the whole checkout, not only the staged packages' paths:
  path dependencies and workspace-root configuration are build inputs too, and a
  narrower list could miss one.
- `rustc`: the output of `rustc -V`.
- For each staged binary: `name`, `platform`, `triple`, `package`, `bin`, `features`,
  `builder` (`cargo` or `cargo-zigbuild`), `path`, `size` and `sha256`.
- For each dispatcher: its `sha256`.

The manifest has no timestamps, so restaging the same bytes produces the same manifest.

Rules the tool enforces:

- A dirty tree is refused unless `--allow-dirty` is passed. The manifest then records
  `source_dirty: true`, and the release gate refuses it.
- Stages merge only when the source commit and `rustc` match and neither tree was
  dirty. This lets a later run add platforms to an existing stage. Anything else must
  be replaced with `--clean`. One manifest therefore always describes one clean source.
- Ambient `RUSTFLAGS` and per-target config `rustflags` are cleared
  (`CARGO_ENCODED_RUSTFLAGS=""`). A developer's local linker flags (for example `mold`)
  therefore never reach a staged artifact.
- Builds use `--locked --release`. The workspace release profile strips symbols.

The sha256 pins the exact bytes that were staged. It does not prove a reproducible
build: rebuilding the same commit with another zig or rustc can produce different
bytes. The manifest records the compiler, so such a difference can be explained.

`plugin-stage verify --complete` is the release gate. It requires every declared
binary on every declared platform, staged from a clean commit, in addition to the
normal checks.

Symlinks (#5023). Verify, partial or complete, refuses a symlinked plugin directory,
`bin/`, platform directory or `manifest.json` before reading through it, using
`symlink_metadata`. The gate therefore certifies a self-contained package: a symlink
force-added to the release tree would not ship the verified bytes. `stage` refuses the
same layouts before it writes anything, with or without `--clean`, so it never writes
through a link out of the plugin.

## 6. Keeping staged binaries out of source review

- `.gitignore` excludes `/plugins/*/bin/` on every source branch, including rolling.
  Built binaries are never committed to the source tree.
- `.gitattributes` marks `plugins/*/bin/**` as `linguist-generated` and `binary`. If a
  release tree ever carries them, they are hidden from diffs and code review.
- Every file under `bin/` must be listed in the manifest. A hand-placed or hand-edited
  file fails `verify` as "not listed" or "sha256 differs", and an edited dispatcher
  fails as "differs from the generated dispatcher". The only hand-edited input is the
  reviewed declaration.

Why a release tree may carry binaries: plugin marketplaces install a plugin from the
git tree the marketplace entry points at (`"source": "./plugins/aws-core"`). Users
therefore receive binaries only if that tree contains them. The design keeps them off
every source branch. Only the operator-gated release step (#4414 packaging, then a
release or main publication) may add the staged `bin/` directories, with
`git add -f`, after `verify --complete` passes. This change does not do that, and no
release is published.

## 7. Fail-closed behaviour

| Situation | Guard hook (secret-safety) | MCP server |
| --- | --- | --- |
| `bin/` not staged (fresh source checkout) | launcher prints the deny | spawn fails; server unavailable |
| unsupported platform (for example `armv7l`, FreeBSD, `i686`), even with a file planted at an unsupported path | dispatcher exits 127 before any lookup, launcher denies | exits 127; server unavailable |
| supported platform with no staged binary | dispatcher exits 127, launcher denies | exits 127; server unavailable |
| binary not executable, wrong architecture, or glibc too old | `exec` fails, launcher denies | server unavailable |
| binary exits non-zero or is killed | launcher denies | server unavailable |
| `CLAUDE_PLUGIN_ROOT` unset | launcher denies | spawn fails |

Each guard row is exercised through real `sh` in
`tests/staging.rs::the_guard_launcher_forwards_the_binary_and_fails_closed`.

Until the binary is staged in the installed plugin, the switched hook denies every
guarded call (all Bash calls and AWS tool calls). That is the manager ruling on #4412,
and it is why the switch ships only together with staged binaries (section 9).

## 8. Packaging contract test

`crates/plugin-stage/tests/packaging_contract.rs` runs in the normal workspace tests
(`make test`, CI `test:rust`). It is offline and builds no cross target.

- **Launchers.** It walks every plugin's `hooks.json`, `.mcp.json` and `mcp.json`. It
  skips `skills/`, `fixtures/`, `tests/`, `node_modules/` and `bin/`. It also follows
  `hooks` and `mcpServers` in the plugin manifests (`plugin.json` and
  `.claude-plugin/`, `.codex-plugin/`, `.cursor-plugin/` and `.devin-plugin/`). A value
  may be a file path, an inline object, or a list of either (Devin's string-or-list
  `mcpServers`, #5024). Every listed file is resolved and scanned with its kind kept,
  and any other value or member type is refused. Every string is checked against
  the launcher allowlist (section 4). Each reference must name a binary that the
  declaration declares for that plugin, and must have the form its role requires.
  Today it scans 12 launcher files and finds two `bin/` references: the two aws-core
  PreToolUse registrations of `aws-core/secret-safety` (#4412 switch, #5078).
- **Staged files.** For every `plugins/*/bin/` that exists, the test checks:
  - the plugin declares binaries, and the manifest exists and parses;
  - each entry is declared, has the declared triple, package, bin and features, and
    sits at its layout path;
  - the plugin directory, `bin/`, platform directories and the manifest are real (not
    symlinks);
  - each file is a regular file (not a symlink), has the executable bit (Unix), and
    matches its size and sha256;
  - dispatchers are byte-identical to the generated script;
  - no unlisted file or directory exists.

  In CI nothing is staged, so only the declaration and launcher checks apply. On a
  developer machine that has staged binaries, they are checked too.
- **Fixtures** prove each violation is reported, and `tests/staging.rs` drives the real
  `install` path, tampering, the release gate, and the dispatcher on seven faked
  `uname` platforms.

`plugin-stage check` runs the same checks from the command line and lists declared
binaries that no launcher uses yet.

## 9. Usage and follow-ups

```text
cargo run --locked -p plugin-stage -- plan --platform all     # what would build here, or why not
cargo run --locked -p plugin-stage -- stage --platform all    # build, stage, write manifests, verify
cargo run --locked -p plugin-stage -- stage --only aws-core/secret-safety --platform host
cargo run --locked -p plugin-stage -- check                   # launcher contract + staged files
cargo run --locked -p plugin-stage -- verify --complete       # release gate
cargo run --locked -p plugin-stage -- launcher aws-core/secret-safety
cd plugins/aws-agents/skills/agents-pay/packages/openclaw && \
  plugin-stage verify-package --package-root . --complete   # npm prepack guard
```

`make plugin-binaries` and `make plugin-binaries-check` wrap the `stage --platform all`
and `check` commands.

`plan` and `stage` report every problem before building anything:

- a declared package that is not in the workspace, reported with its consumer.
- a Rust target that is not installed, with the remedy
  `rustup target add <triple>` or a native runner.
- a missing `zig` or `cargo-zigbuild`.

Follow-ups:

1. **#4412 switch.** Step 1 is done on the integration round 21 head (#5078); step 2
   belongs to release packaging (#4414):
   1. Replace both `PreToolUse` commands in
      `plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json` with the output of
      `plugin-stage launcher aws-core/secret-safety`.
   2. Run `stage --platform all` and `verify --complete`.

   The contract test then enforces the canonical fail-closed form. The switch commit
   and the staged binaries travel together into the release.
2. **#4413 S5.** Add one declaration entry per plugin whose `.mcp.json` or `mcp.json`
   moves to `toolkit`. Each entry needs role `mcp-server`, package `toolkit-cli`, bin
   `toolkit` and features `["toolkit-aws/sdk"]`. Prove that the `aws-lc-sys` zig cross
   build works. Verify the Windows native `.exe` launch.
3. **#4414.** The release packaging runs `stage --platform all` on the clean release
   commit, then `verify --complete`, then hands off to the operator-gated publication.
