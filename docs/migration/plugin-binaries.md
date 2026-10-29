# Per-plugin `./bin` binaries: build and stage pipeline (#4487)

Status: pipeline, contract test and design implemented on the #4487 branch. Nothing is
released. The live `hooks.json` still launches `secret-safety.py`; the switch to the Rust
binary happens once the #4412 crate is on rolling (section 9).

## 1. Decision and scope

The GM ruling recorded on #4487, #4413 and #4993 chose option (i) of #4993: each plugin
ships its own platform binaries under `plugins/<plugin>/bin/`, and one pipeline stages
every such binary. It serves the secret-safety PreToolUse hook (`secret-safety-hook`
crate, bin `secret-safety`, #4412) now. Later it serves the `toolkit` binary that the
#4413 MCP proxy cutover uses.

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

## 4. Launchers: how hooks.json and .mcp.json pick the binary

Launchers never name a platform directory. They always call the dispatcher
`${CLAUDE_PLUGIN_ROOT}/bin/<name>`. The contract test rejects any launcher that reaches
into `bin/<platform>/`, names `bin/<name>.exe`, or passes the bare `bin` directory.

**The dispatcher** is a 20-line POSIX `sh` script, generated byte for byte by
`launcher::dispatcher_script`:

1. It maps `uname -s` to `linux`, `macos` or `windows` (`MINGW*`, `MSYS*` or
   `CYGWIN*`), and anything else to `unsupported`.
2. It maps `uname -m` to `x86_64` (`x86_64`, `amd64` or `x64`) or `aarch64`
   (`aarch64` or `arm64`), and anything else to `unsupported`.
3. It runs `bin/<os>-<arch>/<name>` with `exec`, passing its arguments and stdin
   through. On Windows it runs `bin/<name>.exe`.
4. If the file is missing or not executable, it writes
   `<name>: no staged binary for <os>-<arch> ...` to stderr and exits 127.

The `uname` output only chooses between fixed `case` branches and is never pasted into
a path, so it cannot inject anything. Under Rosetta, `uname -m` reports `x86_64` and
the x86_64 binary runs translated, which is correct.

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
`${CLAUDE_PLUGIN_ROOT}/bin/<name>` (or `./bin/<name>`). `args` are free.
`sh -c` wrappers and extra words in `command` are refused.

- On Unix the host spawns the dispatcher script directly.
- On Windows the host launches `bin/<name>` without a shell. Windows process creation
  (and Node/libuv `spawn`) appends `.exe` to a name that has no extension, so it reaches
  `bin/<name>.exe` without needing `sh`.

That Windows behaviour must be verified on a Windows host before #4413 S5. If it fails,
the fallback is a host that runs MCP commands through `sh`, which Git for Windows
provides.

**Role `hook`** (non-guarding hooks, for example telemetry) may wrap the dispatcher in
any form, because a missing binary there must not block the user.

## 5. Checksums and provenance

`plugin-stage stage` writes `bin/manifest.json`:

- `source_commit`: the full commit from `git rev-parse HEAD`.
- `source_dirty`: true when `git status --porcelain --untracked-files=no` reports
  changes.
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
| no binary for this platform (for example `armv7l`, FreeBSD) | dispatcher exits 127, launcher denies | exits 127; server unavailable |
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
  `.claude-plugin/`, `.codex-plugin/`, `.cursor-plugin/` and `.devin-plugin/`), both
  file paths and inline objects. It recognises any `${CLAUDE_PLUGIN_ROOT}` or
  `${CURSOR_PLUGIN_ROOT}` `/bin/...` reference, braced or not, and any string starting
  with `./bin/` or `bin/`. Each reference must name a binary that the declaration
  declares for that plugin, and must have the form its role requires (section 4).
  Today it scans 12 launcher files and finds no `bin/` reference, because the
  secret-safety switch has not landed.
- **Staged files.** For every `plugins/*/bin/` that exists, the test checks:
  - the plugin declares binaries, and the manifest exists and parses;
  - each entry is declared, has the declared triple, package, bin and features, and
    sits at its layout path;
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
```

`make plugin-binaries` and `make plugin-binaries-check` wrap the `stage --platform all`
and `check` commands.

`plan` and `stage` report every problem before building anything:

- a declared package that is not in the workspace. Today `secret-safety-hook` is
  reported with its consumer, #4412.
- a Rust target that is not installed, with the remedy
  `rustup target add <triple>` or a native runner.
- a missing `zig` or `cargo-zigbuild`.

Follow-ups:

1. **#4412 switch.** When `crates/secret-safety-hook` lands on rolling:
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
