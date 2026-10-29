#!/bin/sh
# at-commit-test.sh <sha> <cargo args...>: the body of an AgentSubmitJob `test`
# job with `params.at_commit` (#4865).
#
# Invocation contract (read from the daemon's embedded reference runner, #3410):
#   cwd  = the caller's sandbox, a git checkout (the script is run as the
#          relative path `scripts/at-commit-test.sh`);
#   $1   = the 40-hex lowercase commit id to test;
#   $2.. = the arguments for cargo, already built by the job runner, e.g.
#          `test -p <pkg> [--lib|--test T|--bin B] --no-fail-fast -- <names> --exact`;
#   env  = the stamped CARGO_TARGET_DIR, TMPDIR, RUSTC_WRAPPER (sccache) etc.
# Exit: cargo's own status; 2 means the commit could not be checked out (bad
# id, or not in this repository); 143 when terminated.
#
# The commit is checked out in a detached scratch worktree under TMPDIR and
# built into its OWN target directory `<CARGO_TARGET_DIR>/.rsi-tmp/at-commit.*`.
# A path crate has the same metadata hash in every checkout, so a past commit
# built into the caller's own target would leave stale workspace-member rlibs
# that the caller's next build accepts as fresh (#702). Both scratch paths are
# removed on every exit. The caller's working tree, index and branch are never
# touched: no checkout or switch happens outside the scratch worktree.
sha="${1:?usage: at-commit-test.sh <sha> <cargo args...>}"
shift
case "$sha" in
  *[!0-9a-f]* | "")
    echo "at-commit-test: $sha is not a lowercase hex commit id" >&2
    exit 2
    ;;
esac
[ "${#sha}" -eq 40 ] || {
  echo "at-commit-test: $sha is not a 40-hex commit id" >&2
  exit 2
}
repo="$(git rev-parse --show-toplevel)" || exit 2
have() { git -C "$repo" cat-file -e "${sha}^{commit}" 2>/dev/null; }
if ! have; then
  # Best effort: a commit only the remote has.
  timeout 60 git -C "$repo" fetch --quiet origin "$sha" 2>/dev/null || true
  have || {
    echo "at-commit-test: commit $sha is not in this repository" >&2
    exit 2
  }
fi

work=""
scratch_target=""
cleanup() {
  if [ -n "$work" ]; then
    git -C "$repo" worktree remove --force "$work/wt" >/dev/null 2>&1 || true
    rm -rf "$work"
  fi
  [ -z "$scratch_target" ] || rm -rf "$scratch_target"
  git -C "$repo" worktree prune >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 143' TERM INT HUP

work="$(mktemp -d "${TMPDIR:-/tmp}/at-commit-test.XXXXXX")" || exit 2
# Absolute, because cargo runs from the scratch worktree.
target_base="${CARGO_TARGET_DIR:-$repo/target}"
mkdir -p "$target_base/.rsi-tmp" || exit 2
target_base="$(cd "$target_base/.rsi-tmp" && cd .. && pwd)" || exit 2
scratch_target="$(mktemp -d "$target_base/.rsi-tmp/at-commit.XXXXXX")" || exit 2

# Hooks off: this checkout is scratch, a repository hook must not run in it.
git -C "$repo" -c core.hooksPath=/dev/null worktree add --detach --quiet \
  "$work/wt" "$sha" || exit 2
echo "at-commit-test: $(git -C "$work/wt" log -1 --format='%H %s' "$sha")"
echo "at-commit-test: target $scratch_target"
cd "$work/wt" || exit 2
# Not exec: the EXIT trap must remove the scratch paths after cargo returns.
CARGO_TARGET_DIR="$scratch_target" cargo "$@"
