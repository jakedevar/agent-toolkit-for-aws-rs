#!/bin/sh
# run-rsid-test-shards.sh shard <NAME> [--filterset <EXPR>]: the body of an
# AgentSubmitJob `test` job with `params.shard` (#5472).
#
# Invocation contract (read from the argv builder embedded in the rsid binary,
# crates/rsid-jobs `test_command`; the same way #4865 read the at_commit one):
#   argv = <cargo-slot> env -u <STRIP_NAMESPACE_ENV> scripts/run-rsid-test-shards.sh \
#            shard <NAME> [--filterset <EXPR>]
#   cwd  = the caller's sandbox, a Cargo workspace (the script is run as the
#          relative path `scripts/run-rsid-test-shards.sh`);
#   NAME = params.shard, 1..200 bytes; <EXPR> = params.filterset, up to 32
#          nextest atoms `test(NAME)`, `test(=NAME)` or `test(/REGEX/)` joined
#          by ` | `, at most 1024 bytes;
#   env  = the stamped CARGO_TARGET_DIR, TMPDIR, RUSTC_WRAPPER (sccache) etc.
# The daemon does NOT take the artifact lock for shard jobs: this script holds
# `<CARGO_TARGET_DIR>/.rsid-test-shards.lock` shared, as every other job does.
# Exit: the test run's own status (nextest 100 = a test failed, 4 = nothing
# matched); 2 = bad arguments; 127 = cargo-nextest is not installed.
#
# What a shard is here. This workspace has no `test-shard-*` cargo features, so
# a shard is a unit of the workspace itself:
#   NAME = a workspace member package (e.g. toolkit-cli), run as `-p NAME`, or
#   NAME = workspace, every member (`--workspace`).
# --filterset narrows the shard with nextest's filterset language (`-E`), which
# is what makes a filtered workspace-wide run possible without the slower
# whole-workspace `workspace-test` recipe. Runs in the sandbox, in the stamped
# target, so the sandbox's own build is reused. Doctests are not run (nextest).
usage() {
  echo "usage: run-rsid-test-shards.sh shard <NAME|workspace> [--filterset <EXPR>]" >&2
  exit 2
}
[ "${1:-}" = shard ] || usage
shift
name="${1:-}"
[ -n "$name" ] || usage
shift
filterset=""
if [ $# -gt 0 ]; then
  [ "$1" = --filterset ] && [ $# -eq 2 ] && [ -n "$2" ] || usage
  filterset="$2"
fi
case "$name" in
  [A-Za-z0-9_]*) ;;
  *) echo "run-rsid-test-shards: bad shard name '$name'" >&2; exit 2 ;;
esac
case "$name" in
  *[!A-Za-z0-9_-]*) echo "run-rsid-test-shards: bad shard name '$name'" >&2; exit 2 ;;
esac

cargo nextest --version >/dev/null 2>&1 || {
  echo "run-rsid-test-shards: cargo-nextest is not installed (cargo install cargo-nextest --locked)" >&2
  exit 127
}

# Hold the artifact lock shared for the whole run (fd 9 survives the exec).
if command -v flock >/dev/null 2>&1; then
  target="${CARGO_TARGET_DIR:-target}"
  mkdir -p "$target" || exit 2
  exec 9>>"$target/.rsid-test-shards.lock" || exit 2
  flock -s 9 || exit 2
fi

if [ "$name" = workspace ]; then
  scope="--workspace"
else
  scope="-p $name"
fi
echo "run-rsid-test-shards: shard $name${filterset:+ filterset $filterset}"
# $scope is a fixed flag plus a validated token, so word splitting is intended.
if [ -n "$filterset" ]; then
  # shellcheck disable=SC2086
  exec cargo nextest run $scope --locked --no-fail-fast -E "$filterset"
fi
# shellcheck disable=SC2086
exec cargo nextest run $scope --locked --no-fail-fast
