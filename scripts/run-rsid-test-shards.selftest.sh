#!/bin/sh
# Shell-level self-test for scripts/run-rsid-test-shards.sh (#5472). Runs the
# script in a throwaway directory with a fake `cargo` on PATH, so it needs no
# Rust build. Run: sh scripts/run-rsid-test-shards.selftest.sh   (make test-scripts)
set -u
script="$(cd "$(dirname "$0")" && pwd)/run-rsid-test-shards.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/shard-selftest.XXXXXX")" || exit 1
trap 'rm -rf "$tmp"' EXIT
fails=0
check() { # name, condition-exit-status
  if [ "$2" -eq 0 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
mkdir -p "$tmp/work" "$tmp/bin" "$tmp/target"

# Fake cargo: answers `nextest --version`, else records its argv.
cat >"$tmp/bin/cargo" <<'CARGO'
#!/bin/sh
if [ "$1 $2" = "nextest --version" ]; then
  [ -z "${FAKE_NO_NEXTEST:-}" ] || exit 101
  echo "cargo-nextest fake"; exit 0
fi
{ echo "args=$*"; echo "cwd=$(pwd -P)"; echo "target=$CARGO_TARGET_DIR"; } >"$FAKE_CARGO_LOG"
exit "${FAKE_CARGO_EXIT:-0}"
CARGO
chmod +x "$tmp/bin/cargo"

run() { # exit-code-of-cargo, script args...
  code="$1"; shift
  rm -f "$tmp/cargo.log"
  (cd "$tmp/work" && PATH="$tmp/bin:$PATH" CARGO_TARGET_DIR="$tmp/target" \
    FAKE_CARGO_LOG="$tmp/cargo.log" FAKE_CARGO_EXIT="$code" sh "$script" "$@") \
    >"$tmp/out" 2>"$tmp/err"
}

run 0 shard toolkit-cli
check "package shard exits 0" $?
grep -q '^args=nextest run -p toolkit-cli --locked --no-fail-fast$' "$tmp/cargo.log"
check "package shard runs nextest -p NAME over all its tests" $?
grep -q "^cwd=.*/work$" "$tmp/cargo.log" && grep -q "^target=$tmp/target$" "$tmp/cargo.log"
check "runs in the caller's cwd with the stamped CARGO_TARGET_DIR" $?
[ -f "$tmp/target/.rsid-test-shards.lock" ]
check "artifact lock file lives in the target dir" $?

run 0 shard toolkit-cli --filterset 'test(=a::b) | test(/c/)'
check "filterset run exits 0" $?
grep -q '^args=nextest run -p toolkit-cli --locked --no-fail-fast -E test(=a::b) | test(/c/)$' "$tmp/cargo.log"
check "filterset reaches nextest as one -E argument" $?

run 0 shard workspace
grep -q '^args=nextest run --workspace --locked --no-fail-fast$' "$tmp/cargo.log"
check "shard workspace runs every member" $?

run 100 shard toolkit-cli --filterset 'test(x)'
code=$?; [ "$code" -eq 100 ]
check "a failing test run exits with the runner's status (got $code)" $?

# Bad inputs exit 2 before cargo runs.
for bad in "" "nope" "shard" "shard ../x" "shard -p" "shard a/b" "shard ok --filterset" \
  "shard ok --bogus x" "shard ok --filterset x y"; do
  # shellcheck disable=SC2086
  run 0 $bad
  code=$?
  [ "$code" -eq 2 ] && [ ! -e "$tmp/cargo.log" ]
  check "bad args '$bad' exit 2 without running cargo (got $code)" $?
done
run 0 shard ok --filterset ""
code=$?; [ "$code" -eq 2 ]
check "empty filterset exits 2 (got $code)" $?

(cd "$tmp/work" && FAKE_NO_NEXTEST=1 PATH="$tmp/bin:$PATH" CARGO_TARGET_DIR="$tmp/target" \
  FAKE_CARGO_LOG="$tmp/cargo.log" sh "$script" shard x) >"$tmp/out" 2>"$tmp/err"
code=$?; [ "$code" -eq 127 ] && grep -q nextest "$tmp/err"
check "missing cargo-nextest exits 127 with a hint (got $code)" $?

[ "$fails" -eq 0 ] && echo "run-rsid-test-shards selftest: all passed" && exit 0
echo "run-rsid-test-shards selftest: $fails failed" >&2
exit 1
