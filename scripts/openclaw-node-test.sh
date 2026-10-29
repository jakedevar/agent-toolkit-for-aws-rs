#!/bin/sh
# Offline node test route for the OpenClaw package (#5158, design OQ8).
#
#   sh scripts/openclaw-node-test.sh run        daemon route (make openclaw-node-test)
#   sh scripts/openclaw-node-test.sh check      verify the offline cache only
#   sh scripts/openclaw-node-test.sh populate   ONE-TIME OPERATOR ACTION, uses the
#                                               network (make openclaw-npm-cache)
#
# `run` never uses the network and never runs Python:
#   1. `check`: every package-lock.json tarball this platform installs must be in
#      the offline npm cache, and its bytes must match the lock's integrity. A
#      missing cache, a missing tarball or a tampered tarball fails closed (exit 3).
#   2. The cache is copied to a private temporary directory, so a run never
#      mutates the shared cache and the user's npmrc is never read.
#   3. Inside a fresh network namespace (unshare --net: no interface but a
#      down loopback), `npm ci --offline --ignore-scripts`, then the package's
#      `npm test` script. Exit 4 when the namespace cannot be created; there
#      is no fallback with network.
#   4. `python` and `python3` on PATH are shims that exit 127. While the test
#      script still has its pre-#4411-B4 `python3 -m unittest` step, that `&&`
#      step is left out (logged); every other step runs as written. When the
#      script names no Python, it runs unchanged.
#
# The cache lives outside the repository at $OPENCLAW_NPM_CACHE, default
# $HOME/.cache/rsi/openclaw-npm-cache (an npm cache directory: it holds
# _cacache/). `populate` fills it from the registry with `npm ci` on a copy of
# package.json and package-lock.json, so npm verifies each tarball against the
# lock, then `check` runs on the result before it replaces the old cache.
set -eu

repo="$(cd "$(dirname "$0")/.." && pwd -P)"
pkg="${OPENCLAW_PACKAGE_DIR:-$repo/plugins/aws-agents/skills/agents-pay/packages/openclaw}"
if [ -n "${OPENCLAW_NPM_CACHE:-}" ]; then
  cache="$OPENCLAW_NPM_CACHE"
elif [ -n "${HOME:-}" ]; then
  cache="$HOME/.cache/rsi/openclaw-npm-cache"
else
  echo "openclaw-node-test: HOME and OPENCLAW_NPM_CACHE are both unset; cannot locate the offline npm cache" >&2
  exit 3
fi
populate_hint="one-time operator action (network): make openclaw-npm-cache   (writes $cache)"

die() { # code, message
  echo "openclaw-node-test: $2" >&2
  exit "$1"
}

need_tools() {
  command -v node >/dev/null 2>&1 || die 3 "node is not on PATH"
  command -v npm >/dev/null 2>&1 || die 3 "npm is not on PATH"
}

# check CACHE_DIR: every lock tarball for this platform is present and intact.
check_cache() {
  [ -f "$pkg/package-lock.json" ] || die 3 "no package-lock.json in $pkg"
  [ -d "$1/_cacache/content-v2" ] || die 3 "offline npm cache missing: $1/_cacache/content-v2 does not exist; $populate_hint"
  node --input-type=module - "$pkg/package-lock.json" "$1" <<'NODE' || die 3 "offline npm cache at $1 is incomplete or does not match package-lock.json; $populate_hint"
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

const [lockPath, cacheDir] = process.argv.slice(2);
const lock = JSON.parse(readFileSync(lockPath, "utf8"));
if (lock.lockfileVersion !== 3 || typeof lock.packages !== "object") {
  console.error(`unsupported lockfile (want lockfileVersion 3): ${lockPath}`);
  process.exit(1);
}
const applies = (list, value) =>
  !Array.isArray(list) ||
  (list.some((entry) => entry === value) ||
    (list.every((entry) => entry.startsWith("!")) && !list.includes(`!${value}`)));
let bad = 0;
let checked = 0;
for (const [key, entry] of Object.entries(lock.packages)) {
  if (key === "" || entry.link) continue;
  if (!applies(entry.os, process.platform) || !applies(entry.cpu, process.arch)) continue;
  const match = /^sha512-([A-Za-z0-9+/]+={0,2})$/.exec(entry.integrity ?? "");
  if (!match) {
    console.error(`${key}: no sha512 integrity in the lock`);
    bad += 1;
    continue;
  }
  const hex = Buffer.from(match[1], "base64").toString("hex");
  const file = path.join(cacheDir, "_cacache", "content-v2", "sha512", hex.slice(0, 2), hex.slice(2, 4), hex.slice(4));
  if (!existsSync(file)) {
    console.error(`${key}@${entry.version}: missing from the offline cache`);
    bad += 1;
    continue;
  }
  const actual = createHash("sha512").update(readFileSync(file)).digest("hex");
  if (actual !== hex) {
    console.error(`${key}@${entry.version}: cached bytes do not match the lock integrity`);
    bad += 1;
    continue;
  }
  checked += 1;
}
if (bad > 0 || checked === 0) process.exit(1);
console.log(`offline cache ok: ${checked} tarballs match package-lock.json for ${process.platform}-${process.arch}`);
NODE
}

# `npm test` when the package's test script names no Python; otherwise that
# script with each `&&` step that names Python removed.
node_test_script() {
  node --input-type=module - "$pkg/package.json" <<'NODE'
import { readFileSync } from "node:fs";

const script = JSON.parse(readFileSync(process.argv[2], "utf8")).scripts?.test;
if (typeof script !== "string" || script.trim() === "") {
  console.error("package.json has no test script");
  process.exit(1);
}
const steps = script.split("&&").map((step) => step.trim());
const kept = steps.filter((step) => !/\bpython[0-9.]*\b/.test(step));
for (const step of steps) if (!kept.includes(step)) console.error(`skipped Python step: ${step}`);
if (kept.length === 0 || kept.some((step) => step === "" || /[;|`]|\$\(/.test(step))) {
  console.error(`refusing to split the test script: ${JSON.stringify(script)}`);
  process.exit(1);
}
// A Python-free script runs as `npm test` itself, with npm's lifecycle environment.
process.stdout.write(kept.length === steps.length ? "npm test" : kept.join(" && "));
NODE
}

run() {
  need_tools
  check_cache "$cache"
  command -v unshare >/dev/null 2>&1 || die 4 "unshare is not on PATH; refusing to run without network isolation"
  unshare --user --map-current-user --net true 2>/dev/null ||
    die 4 "cannot create a network namespace (unshare --user --map-current-user --net); refusing to run with network"

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/openclaw-node-test.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/cache" "$tmp/shim" "$tmp/logs"
  cp -R "$cache/_cacache" "$tmp/cache/_cacache"
  : >"$tmp/user-npmrc"
  : >"$tmp/global-npmrc"
  for name in python python3; do
    printf '#!/bin/sh\necho "openclaw-node-test: Python is not allowed in this route ($0 $*)" >&2\nexit 127\n' >"$tmp/shim/$name"
    chmod +x "$tmp/shim/$name"
  done
  script="$(node_test_script)" || die 5 "cannot derive a Python-free test script from $pkg/package.json"

  echo "openclaw-node-test: package $pkg"
  echo "openclaw-node-test: node $(node --version), npm $(npm --version), $(uname -sm)"
  echo "openclaw-node-test: network namespace: unshare --net (loopback down, no other interface)"
  echo "openclaw-node-test: test steps: $script"
  cd "$pkg"
  env \
    PATH="$tmp/shim:$PATH" \
    npm_config_cache="$tmp/cache" \
    npm_config_userconfig="$tmp/user-npmrc" \
    npm_config_globalconfig="$tmp/global-npmrc" \
    npm_config_logs_dir="$tmp/logs" \
    npm_config_offline=true \
    npm_config_audit=false \
    npm_config_fund=false \
    npm_config_update_notifier=false \
    OPENCLAW_TEST_SCRIPT="$script" \
    unshare --user --map-current-user --net sh -euc '
      npm ci --offline --ignore-scripts --no-audit --no-fund
      PATH="$PWD/node_modules/.bin:$PATH" sh -euc "$OPENCLAW_TEST_SCRIPT"
    '
  echo "openclaw-node-test: ok"
}

populate() {
  need_tools
  stage="$(mktemp -d "${TMPDIR:-/tmp}/openclaw-npm-cache.XXXXXX")"
  trap 'rm -rf "$stage"' EXIT
  mkdir -p "$stage/pkg"
  cp "$pkg/package.json" "$pkg/package-lock.json" "$stage/pkg/"
  (
    cd "$stage/pkg"
    npm ci --cache "$stage/cache" --ignore-scripts --no-audit --no-fund --prefer-online
  )
  rm -rf "$stage/cache/_logs" "$stage/cache/_update-notifier-last-checked"
  check_cache "$stage/cache"
  mkdir -p "$(dirname "$cache")"
  rm -rf "$cache.old"
  if [ -e "$cache" ]; then mv "$cache" "$cache.old"; fi
  mv "$stage/cache" "$cache"
  rm -rf "$cache.old"
  echo "openclaw-node-test: populated $cache"
}

case "${1:-}" in
  run) run ;;
  check) need_tools && check_cache "$cache" ;;
  populate) populate ;;
  *) die 2 "usage: openclaw-node-test.sh run|check|populate" ;;
esac
