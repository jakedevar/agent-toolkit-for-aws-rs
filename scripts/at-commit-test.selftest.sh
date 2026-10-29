#!/bin/sh
# Shell-level self-test for scripts/at-commit-test.sh (#4865). Runs the script
# against a throwaway git repository with a fake `cargo` on PATH, so it needs
# no Rust build. Run: sh scripts/at-commit-test.selftest.sh   (make test-scripts)
set -u
script="$(cd "$(dirname "$0")" && pwd)/at-commit-test.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/at-commit-selftest.XXXXXX")" || exit 1
trap 'rm -rf "$tmp"' EXIT
fails=0
check() { # name, condition-exit-status
  if [ "$2" -eq 0 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}

repo="$tmp/repo"
mkdir -p "$repo" "$tmp/bin" "$tmp/tmp" "$tmp/target"
git -C "$repo" init -q
git -C "$repo" config user.email t@example.invalid
git -C "$repo" config user.name t
git -C "$repo" config commit.gpgsign false
echo old >"$repo/marker"
git -C "$repo" add marker
git -C "$repo" commit -q -m old
old="$(git -C "$repo" rev-parse HEAD)"
echo new >"$repo/marker"
git -C "$repo" commit -q -am new
new="$(git -C "$repo" rev-parse HEAD)"

# Fake cargo: records what it saw, exits with $FAKE_CARGO_EXIT.
cat >"$tmp/bin/cargo" <<'CARGO'
#!/bin/sh
{
  echo "args=$*"
  echo "cwd=$(pwd -P)"
  echo "target=$CARGO_TARGET_DIR"
  echo "marker=$(cat marker)"
} >"$FAKE_CARGO_LOG"
exit "${FAKE_CARGO_EXIT:-0}"
CARGO
chmod +x "$tmp/bin/cargo"

run() { # exit-code-of-cargo, sha, args...
  code="$1"; shift
  (cd "$repo" && PATH="$tmp/bin:$PATH" TMPDIR="$tmp/tmp" CARGO_TARGET_DIR="$tmp/target" \
    FAKE_CARGO_LOG="$tmp/cargo.log" FAKE_CARGO_EXIT="$code" sh "$script" "$@") \
    >"$tmp/out" 2>"$tmp/err"
}

# Past commit: cargo sees the old tree, with the exact argv, in a scratch dir.
run 0 "$old" test -p demo --lib --no-fail-fast -- t::one --exact
check "exit 0 passes through" $?
grep -q '^args=test -p demo --lib --no-fail-fast -- t::one --exact$' "$tmp/cargo.log"
check "cargo receives the runner argv without the sha" $?
grep -q '^marker=old$' "$tmp/cargo.log"
check "cargo runs on the requested (old) commit" $?
grep -q "^target=$tmp/target/.rsi-tmp/at-commit\\." "$tmp/cargo.log"
check "scratch target is under the stamped CARGO_TARGET_DIR/.rsi-tmp" $?
grep -q "^cwd=.*$(basename "$tmp")/tmp/at-commit-test\\." "$tmp/cargo.log"
check "scratch worktree is under the stamped TMPDIR" $?

# The caller's tree, branch and index are untouched; scratch is gone.
[ "$(cat "$repo/marker")" = new ] && [ "$(git -C "$repo" rev-parse HEAD)" = "$new" ] &&
  [ -z "$(git -C "$repo" status --porcelain)" ]
check "caller tree, HEAD and index untouched" $?
[ -z "$(ls -A "$tmp/tmp")" ] && [ -z "$(ls -A "$tmp/target/.rsi-tmp")" ]
check "scratch worktree and target removed on exit" $?
[ "$(git -C "$repo" worktree list | wc -l)" -eq 1 ]
check "no worktree registration left behind" $?

# A failing test run: cargo's exit code passes through, cleanup still happens.
run 101 "$new" test -p demo
code=$?
[ "$code" -eq 101 ]
check "cargo exit 101 passes through (got $code)" $?
grep -q '^marker=new$' "$tmp/cargo.log"
check "HEAD commit is checked out too" $?
[ -z "$(ls -A "$tmp/tmp")" ] && [ -z "$(ls -A "$tmp/target/.rsi-tmp")" ]
check "scratch removed after a failing run" $?

# Bad inputs exit 2 before cargo runs.
rm -f "$tmp/cargo.log"
run 0 "NOTHEX" test
code=$?; [ "$code" -eq 2 ]
check "non-hex id exits 2 (got $code)" $?
run 0 "abc123" test
code=$?; [ "$code" -eq 2 ]
check "short id exits 2 (got $code)" $?
run 0 "0000000000000000000000000000000000000001" test
code=$?; [ "$code" -eq 2 ]
check "unknown commit exits 2 (got $code)" $?
grep -q "not in this repository" "$tmp/err"
check "unknown commit says so" $?
[ ! -e "$tmp/cargo.log" ]
check "cargo never ran for bad inputs" $?

[ "$fails" -eq 0 ] && echo "at-commit-test selftest: all passed" && exit 0
echo "at-commit-test selftest: $fails failed" >&2
exit 1
