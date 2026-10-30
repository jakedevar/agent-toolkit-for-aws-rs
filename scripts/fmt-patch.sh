#!/bin/sh
# fmt-patch (#5509): print the rustfmt result for the current worktree as an
# applicable unified patch. The worktree is copied (tracked and untracked,
# non-ignored files, as they are on disk) into a mktemp scratch directory;
# `cargo fmt --all` runs there and `git diff` of that copy is printed between
# marker lines in the job log. The caller's worktree is never written.
#
# Usage: sh scripts/fmt-patch.sh        (make fmt-patch; daemon recipe fmt-patch)
# Apply: copy the lines between the markers into a file, then `git apply FILE`.
# Exit:  0 when there is nothing to format (empty patch) or a patch was printed;
#        nonzero only on a real failure (copy, git, or cargo fmt itself).
# CARGO_TARGET_DIR and RUSTC_WRAPPER are left exactly as the caller set them.
set -eu
begin='===== BEGIN FMT-PATCH ====='
end='===== END FMT-PATCH ====='

root="$(git rev-parse --show-toplevel)" || { echo "fmt-patch: not in a git worktree" >&2; exit 2; }
scratch="$(mktemp -d "${TMPDIR:-/tmp}/fmt-patch.XXXXXX")" || exit 1
trap 'rm -rf "$scratch"' EXIT INT TERM

# Copy what the worktree holds now: tracked + untracked-not-ignored paths that
# still exist on disk (a tracked file deleted in the worktree stays deleted).
git -C "$root" ls-files -z --cached --others --exclude-standard \
  | (cd "$root" && xargs -0 sh -c 'for f; do if [ -e "$f" ] || [ -L "$f" ]; then printf "%s\0" "$f"; fi; done' sh) \
  | tar -C "$root" --null -T - -cf - \
  | tar -C "$scratch" -xf -

# The scratch copy is its own repository, baselined at the copied state.
g() { git -C "$scratch" -c user.name=fmt-patch -c user.email=fmt-patch@example.invalid \
  -c commit.gpgsign=false -c core.autocrlf=false -c core.safecrlf=false "$@"; }
g init -q
g add -A
g commit -q --allow-empty -m baseline

if ! (cd "$scratch" && cargo fmt --all); then
  echo "fmt-patch: cargo fmt --all failed in the scratch copy (see output above)" >&2
  exit 1
fi

g add -A
patch="$scratch/.fmt-patch.diff"
g diff --cached --no-color --no-ext-diff --binary >"$patch"
echo "$begin"
cat "$patch"
echo "$end"
if [ -s "$patch" ]; then
  echo "fmt-patch: rustfmt changed $(g diff --cached --name-only | wc -l | tr -d ' ') file(s); apply the lines between the markers with git apply"
else
  echo "fmt-patch: nothing to format (empty patch)"
fi
