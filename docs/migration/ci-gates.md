# Development recipes and CI gates (#4378)

Slice `ci`, programme #4369. Depends on #4374 (shared contracts), #4375
(`toolkit manifests validate`), #4376 (`toolkit spec validate`) and #4377
(`toolkit sync plugin-skills|vendored`). Files: `mise.toml`,
`plugins/aws-startup-advisor/mise.toml`, `.github/workflows/build.yml`,
`.github/dependabot.yml`, the four `references/vendored/README.md` command
hints, and the contract test `crates/toolkit-cli/tests/ci_gates.rs`.

## Caller map (old to new)

| Caller | Old | New |
| --- | --- | --- |
| `mise run lint:manifests` | `python3 tools/validate.py` | `cargo run --locked --quiet -p toolkit-cli --bin toolkit -- manifests validate .` |
| `mise run lint:skill-parity` | `python3 tools/sync-plugin-skills.py --check` | `... toolkit -- sync plugin-skills --check --root .` |
| `mise run lint:spec` | `uv run --with jsonschema --with pyyaml python tools/validate_spec.py` | `... toolkit -- spec validate .` |
| `mise run test:tools` (removed) | `python3 -m unittest discover -s tools/tests -v` | `mise run test:rust` (`make test`, `cargo test --workspace --locked`); the Devin-plugin cases live in `crates/toolkit-cli/src/manifests/tests.rs` |
| root `lint:vendored-parity` (new) | only the startup-advisor workflow ran it | `... toolkit -- sync vendored --check --plugin-root plugins/aws-startup-advisor`, part of `validate` so the main build checks vendored parity |
| `plugins/aws-startup-advisor` `lint:vendored-parity` | `python3 tools/sync-vendored.py --check` | `cargo run --locked --quiet --manifest-path ../../Cargo.toml -p toolkit-cli --bin toolkit -- sync vendored --check --plugin-root .` |
| four `references/vendored/README.md` hints | `python3 plugins/aws-startup-advisor/tools/sync-vendored.py [--check]` | `cargo run --locked -p toolkit-cli --bin toolkit -- sync vendored --plugin-root plugins/aws-startup-advisor [--check]` |
| `build.yml` step "Validate manifests, skills, and plugin parity" | `mise run validate` (Python) | same task name, now Rust; preceded by "Install Rust toolchain" and "Build toolkit (locked)" |

The #4377 commands default to paths relative to the working directory
(`--root` to `.`, `--plugin-root` to `plugins/aws-startup-advisor`) and the
#4375/#4376 commands default `ROOT` to `.`. Every recipe passes its root
explicitly instead of relying on the default.

## What CI runs

`build.yml` (push to `main`, pull requests, merge queue, manual) now:

1. checks out, installs mise tools (node, gitleaks, markdownlint);
2. installs the Rust toolchain pinned in `rust-toolchain.toml` with `rustfmt`
   and `clippy` from the runner's rustup (no new third-party action);
3. `mise run build:toolkit`: one locked build of the `toolkit` binary; every
   validator task depends on it, so the later steps reuse the build;
4. `mise run lint:md` (markdownlint, retained);
5. `mise run lint:rust`: `make fmt` (`cargo fmt --all -- --check`) and
   `make clippy` (`cargo clippy --workspace --all-targets -- -D warnings`);
6. `mise run validate`: manifests, plugin-skill parity, vendored parity, spec
   conformance and `make test` (`cargo test --workspace --locked`), which
   includes the real-tree tests of all four validators;
7. `mise run security` (gitleaks, retained).

The job timeout rose from 10 to 30 minutes for the cold Rust build. Branch
protection, release workflows, `codeql.yml`, `scorecard-analysis.yml`,
secrets and the other workflows are unchanged. `startup-advisor.yml` still
runs `mise -C plugins/aws-startup-advisor run ci`; its vendored-parity task now
calls the Rust command.

## `fmt-patch`: an applicable rustfmt result (#5509)

`make fmt` (daemon recipe `fmt`) only checks, and prints rustfmt's own
`Diff in FILE:LINE:` hunks, which cannot be applied. Workers that may not run
cargo locally submit the daemon recipe `fmt-patch` (`make fmt-patch`,
`scripts/fmt-patch.sh`) instead. It copies the worktree (tracked and untracked,
non-ignored files, as on disk) to a `mktemp` scratch directory, runs
`cargo fmt --all` there and prints `git diff` of that copy as a standard
unified patch between the lines `===== BEGIN FMT-PATCH =====` and
`===== END FMT-PATCH =====` in the job log. It never writes the caller's
worktree and leaves `CARGO_TARGET_DIR` and `RUSTC_WRAPPER` untouched. It exits
0 when there is nothing to format (empty patch between the markers) and
nonzero only on a real failure (for example rustfmt cannot parse a file).
Copy the lines between the markers to a file and run `git apply FILE`, then
commit; `fmt` should then pass. `scripts/fmt-patch.selftest.sh` (run by
`make test-scripts`) covers it with a fake `cargo`. The daemon recipe `test-scripts` runs them all.

## No Python in the root recipes

The root `mise.toml` no longer installs `python` or `uv` and no task in it, or
in `build.yml`, runs Python. `tests/ci_gates.rs::no_recipe_installs_or_invokes_python`
enforces this. The plugin `mise.toml` now declares `python = "3.12"` and
`uv = "latest"` itself because its remaining tasks still run Python helpers
owned by other slices (`scripts/*.py`, `tests/`); mise merges the parent tools, so before
this change they came from the root. They go when those slices land.

## OpenClaw package node tests (daemon recipe, #5158)

The OpenClaw package (`plugins/aws-agents/skills/agents-pay/packages/openclaw`)
has TypeScript sources and `node --test` suites. The daemon runs them with the
recipe `openclaw-node-test` (`.rsi/jobs.toml`, `make openclaw-node-test`, i.e.
AgentSubmitJob `{"kind":"test","params":{"recipe":"openclaw-node-test"}}`).
`scripts/openclaw-node-test.sh run` does this:

1. It checks that every `package-lock.json` tarball this platform installs is in
   the offline npm cache, and that each one's sha512 equals the lock
   `integrity`. A missing cache, a missing tarball or wrong bytes fail closed
   (exit 3) and name the operator command below.
2. It copies the cache and the skill root (the package plus the canonical skill
   that `stage-skill` bundles, minus what the package `.gitignore` names) to a
   private temporary directory and runs there. The shared cache and the
   worktree are never written, so no `node_modules`, `dist` or staged skill is
   left for the real-tree validators (`spec validate`, `manifests validate`).
   It gives npm empty user and global npmrc files and `offline=true`.
3. In a fresh network namespace (`unshare --user --map-current-user --net`: one
   loopback, down) it runs `npm ci --offline --ignore-scripts`, then
   `npm test`. If the namespace cannot be created it exits 4 rather than run
   with network.
4. `python` and `python3` on `PATH` are shims that exit 127. Since #4411 B4
   the package `test` script names no Python (`npm run build`, `npm run
   stage-skill`, `node --test test/*.test.mjs`), so it runs as `npm test`
   itself. The runner still skips, and logs, a `python3 -m unittest` `&&` step
   in an older script; its self-test keeps that case.

The recipe first runs `scripts/openclaw-node-test.selftest.sh`, which uses a
throwaway package and cache. It checks the fail-closed cases (missing cache,
missing or tampered tarball, unset `HOME`), the skipped Python step, the Python
shim, that the network is unreachable, and that the shared cache and the
package directory stay untouched. `make test-scripts` also runs the self-test.

**Where the dependencies come from.** The cache lives outside the repository at
`$HOME/.cache/rsi/openclaw-npm-cache` (`OPENCLAW_NPM_CACHE` overrides it). The
committed `package-lock.json` pins its content: each tarball is addressed and
checked by its lock sha512, and `npm ci` checks it again. The tarballs are not
committed. For one platform they total about 10.8 MB, and 9.7 MB of that is the
native TypeScript 7 compiler (`@typescript/typescript-linux-x64` 7.0.2;
darwin-arm64 is 9.3 MB). Every TypeScript bump would add that again to the
history. The platform-independent tarballs (typescript, @types/node, typebox,
undici-types) are about 1.1 MB.

**One-time operator action per host (uses the network):**

```sh
make openclaw-npm-cache
```

It runs `npm ci` from the registry on a copy of `package.json` and
`package-lock.json` (npm verifies each tarball against the lock). It then checks
the result as in step 1 and moves it into place. Rerun it after any
`package-lock.json` change, for example B4's 1.1.0 identity bump if that
changes a dependency. Until then the recipe fails closed with the same
message. No daemon recipe uses the network.

## Retired Python (callers proven gone)

| File | Replacement | Proof |
| --- | --- | --- |
| `tools/validate.py` | `toolkit manifests validate` | no executable reference remains: a repository grep for `validate.py`, `validate_spec`, `sync-plugin-skills` and `test:tools` finds only code comments, migration documents, the `codeql.yml` comment and the inventory baseline |
| `tools/tests/test_validate.py` | `crates/toolkit-cli/src/manifests/tests.rs` | only caller was `test:tools` (removed) |
| `tools/validate_spec.py` | `toolkit spec validate` | only caller was `lint:spec` (repointed) |
| `tools/sync-plugin-skills.py` | `toolkit sync plugin-skills` | only caller was `lint:skill-parity` (repointed) |

`tools/schemas/1.0.0` stays: `toolkit spec validate` loads it.
`docs/migration/python-inventory.json` is the baseline and is not edited.

## Python kept because another open slice still needs it

| File | Why it stays |
| --- | --- |
| `plugins/aws-startup-advisor/tools/sync-vendored.py` | still executed as a subprocess by `plugins/aws-startup-advisor/tests/test_generation_ai_contracts.py` and `plugins/aws-startup-advisor/tests/test_estimation_ai_contracts.py` (`SYNC_VENDORED_PATH ... --check`), which run under the plugin's `test` task; `test_ai_capability_catalog_contracts.py` also names it in an assertion message |
| `plugins/aws-startup-advisor/tools/test_sync_vendored.py` | tests the file above; retire both together |

Retire them in the slice that ports those Python contract tests (they should
call `toolkit sync vendored --check --plugin-root .` or assert through the Rust
tests). The Rust command is already the CI gate; the Python script is no longer
reachable from any recipe or workflow.

## Baseline and generated artifacts (AC4)

- `Cargo.lock` is committed and every recipe and CI step builds with `--locked`;
  `.github/dependabot.yml` gained a weekly grouped `cargo` ecosystem next to
  `github-actions`, so dependency bumps arrive as reviewed lockfile changes.
- `tools/schemas/1.0.0` (vendored Agent Plugins schemas) is read by
  `toolkit spec validate`; nothing regenerates it.
- Plugin skill copies and `references/vendored` trees are generated from the
  canonical sources and checked byte-for-byte by `lint:skill-parity` and
  `lint:vendored-parity`; the write mode (`toolkit sync ... ` without
  `--check`) is the only way to refresh them.
- `scripts/artifact-contracts-baseline.json` belongs to a still-Python slice and is
  untouched; `tools/skill-refs-baseline.json` is read by `toolkit startup skill-refs`
  (#4407).

## Merge resolutions (base stack)

The sandbox started at the accepted #4376 head `81cc6189` (contains #4375
`505a4043`), then merged #4374 `98957f87` (clean) and #4377 `3a4c08d5`.
The #4377 merge conflicted in `crates/toolkit-cli/src/lib.rs` and
`crates/toolkit-cli/README.md`; every accepted change from both sides is kept:

- module list: `manifests`, `spec` (public) and `sync` (private);
- `USAGE`: all four command lines, all four command descriptions, one merged
  options block (`--plugin NAME` documents both meanings), and an exit-code
  line naming validation failures and sync drift;
- `COMMANDS`: `skills list`, `manifests validate`, `spec validate`,
  `sync plugin-skills`, `sync vendored`;
- value flags: both slices added the same idea. The branches disagreed on
  edge cases: #4375 takes the next word whatever it looks like
  (`--plugin -x` is the value `-x`), #4377 rejects an empty value and a value
  starting with `-` (`--plugin-root --check` is a usage error). Both test
  suites encode their rule, so `CommandSpec` gained `strict_values`: `false`
  for `manifests validate` and the unchanged commands, `true` for both `sync`
  commands. `Invocation::set_value` (last value wins) is kept and the duplicate
  `Invocation::value` accessor dropped;
- README: the `manifests validate`, `spec validate` and `sync` sections are all
  kept, with a short merge note on `strict_values`.

`Cargo.toml`/`Cargo.lock` merged without conflict (#4377 adds no dependency).

## Intentional changes

- `mise run test:tools` is gone; `mise run test:rust` replaces it.
- `lint:vendored-parity` is now part of the root `validate`, so it runs on every
  main build rather than only on startup-advisor pull requests.
- Root `mise.toml` no longer declares `python` and `uv`.
