#!/bin/sh
# Shell-level self-test for scripts/fmt-patch.sh (#5509). Runs the script on a
# throwaway git repository with a fake `cargo` on PATH, so it needs no Rust
# build. Run: sh scripts/fmt-patch.selftest.sh   (make test-scripts)
set -u
script="$(cd "$(dirname "$0")" && pwd)/fmt-patch.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/fmt-patch-selftest.XXXXXX")" || exit 1
trap 'rm -rf "$tmp"' EXIT
fails=0
check() { # name, condition-exit-status
  if [ "$2" -eq 0 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}

repo="$tmp/repo"
mkdir -p "$repo/src" "$tmp/bin" "$tmp/tmp"
git -C "$repo" init -q
git -C "$repo" config user.email t@example.invalid
git -C "$repo" config user.name t
git -C "$repo" config commit.gpgsign false
printf 'fn  ugly() {}\n' >"$repo/src/bad.rs"     # fake rustfmt squeezes the double space
printf 'fn tidy() {}\n' >"$repo/src/good.rs"
printf 'fn  gone() {}\n' >"$repo/src/gone.rs"
printf 'target/\n' >"$repo/.gitignore"
git -C "$repo" add -A
git -C "$repo" commit -q -m base
rm "$repo/src/gone.rs"                            # tracked, deleted in the worktree
printf 'fn  fresh() {}\n' >"$repo/src/new.rs"     # untracked, not ignored
mkdir -p "$repo/target"
printf 'fn  ignored() {}\n' >"$repo/target/ign.rs" # ignored

# Fake cargo: `fmt --all` squeezes "fn  " to "fn " in every .rs file it can
# see, and records cwd and the stamped environment. FAKE_CARGO_EXIT forces a
# failure.
cat >"$tmp/bin/cargo" <<'CARGO'
#!/bin/sh
{
  echo "args=$*"
  echo "cwd=$(pwd -P)"
  echo "target=${CARGO_TARGET_DIR-}"
  echo "wrapper=${RUSTC_WRAPPER-}"
} >"$FAKE_CARGO_LOG"
[ "${FAKE_CARGO_EXIT:-0}" -eq 0 ] || exit "$FAKE_CARGO_EXIT"
[ "$1 $2" = "fmt --all" ] || exit 64
for f in $(find . -name "*.rs"); do sed "s/fn  /fn /" "$f" >"$f.new" && mv "$f.new" "$f"; done
CARGO
chmod +x "$tmp/bin/cargo"

run() { # exit-code-of-cargo
  (cd "$repo" && PATH="$tmp/bin:$PATH" TMPDIR="$tmp/tmp" CARGO_TARGET_DIR="$tmp/stamped-target" \
    RUSTC_WRAPPER=sccache-stamp FAKE_CARGO_LOG="$tmp/cargo.log" FAKE_CARGO_EXIT="$1" sh "$script") \
    >"$tmp/out" 2>"$tmp/err"
}
patch_of() { # print the lines between the markers
  sed -n '/^===== BEGIN FMT-PATCH =====$/,/^===== END FMT-PATCH =====$/p' "$tmp/out" | sed '1d;$d'
}

before="$(git -C "$repo" status --porcelain=v1 -uall; cat "$repo/src/bad.rs" "$repo/src/new.rs")"

# Misformatted tree: a patch is printed, exit 0, and `git apply --check` takes it.
run 0
code=$?; [ "$code" -eq 0 ]
check "misformatted tree exits 0 (got $code)" $?
grep -q '^===== BEGIN FMT-PATCH =====$' "$tmp/out" && grep -q '^===== END FMT-PATCH =====$' "$tmp/out"
check "BEGIN and END marker lines are printed" $?
patch_of >"$tmp/p.diff"
[ -s "$tmp/p.diff" ]
check "patch between the markers is not empty" $?
(cd "$repo" && git apply --check "$tmp/p.diff")
check "git apply --check accepts the patch on the caller's worktree" $?
grep -q '^+fn ugly() {}$' "$tmp/p.diff" && grep -q '^+fn fresh() {}$' "$tmp/p.diff"
check "patch covers tracked and untracked-not-ignored files" $?
! grep -q 'ign.rs\|gone.rs\|good.rs' "$tmp/p.diff"
check "patch omits ignored, deleted and already-formatted files" $?
(cd "$repo" && git apply "$tmp/p.diff") && grep -q '^fn ugly() {}$' "$repo/src/bad.rs"
check "git apply really formats the file" $?
(cd "$repo" && git apply -R "$tmp/p.diff")

# The caller's worktree was not written by the script.
after="$(git -C "$repo" status --porcelain=v1 -uall; cat "$repo/src/bad.rs" "$repo/src/new.rs")"
[ "$before" = "$after" ]
check "caller's worktree is unchanged after the run" $?
[ -z "$(ls -A "$tmp/tmp")" ]
check "scratch directory under TMPDIR is removed" $?

# cargo ran in a scratch copy, with the stamped environment untouched.
grep -q '^args=fmt --all$' "$tmp/cargo.log"
check "cargo is run as 'fmt --all' (no --check)" $?
! grep -q "^cwd=$(cd "$repo" && pwd -P)\$" "$tmp/cargo.log" && grep -q "^cwd=.*$(basename "$tmp")/tmp/fmt-patch\\." "$tmp/cargo.log"
check "cargo runs in a mktemp scratch dir, not the worktree" $?
grep -q "^target=$tmp/stamped-target\$" "$tmp/cargo.log" && grep -q '^wrapper=sccache-stamp$' "$tmp/cargo.log"
check "CARGO_TARGET_DIR and RUSTC_WRAPPER reach cargo untouched" $?

# Clean tree: exit 0 and an empty patch between the markers.
(cd "$repo" && git apply "$tmp/p.diff")
git -C "$repo" add -A
git -C "$repo" commit -q -m formatted
run 0
code=$?; [ "$code" -eq 0 ]
check "clean tree exits 0 (got $code)" $?
grep -q '^===== BEGIN FMT-PATCH =====$' "$tmp/out" && grep -q '^===== END FMT-PATCH =====$' "$tmp/out"
check "clean tree still prints both markers" $?
[ -z "$(patch_of)" ]
check "clean tree prints an empty patch" $?

# A real failure is nonzero and prints no patch.
printf 'fn  again() {}\n' >"$repo/src/again.rs"
run 3
code=$?; [ "$code" -eq 1 ]
check "cargo fmt failure exits 1 (got $code)" $?
! grep -q 'BEGIN FMT-PATCH' "$tmp/out"
check "failure prints no patch markers" $?
grep -q 'cargo fmt --all failed' "$tmp/err"
check "failure says so on stderr" $?
[ -z "$(ls -A "$tmp/tmp")" ]
check "scratch directory is removed after a failure" $?

# Outside a git worktree: exit 2.
mkdir "$tmp/nogit"
(cd "$tmp/nogit" && GIT_CEILING_DIRECTORIES="$tmp" sh -c 'cd "$1" && sh "$2"' sh "$tmp/nogit" "$script") >/dev/null 2>"$tmp/err"
code=$?; [ "$code" -ne 0 ]
check "non-git directory fails (got $code)" $?

[ "$fails" -eq 0 ] && echo "fmt-patch selftest: all passed" && exit 0
echo "fmt-patch selftest: $fails failed" >&2
exit 1
