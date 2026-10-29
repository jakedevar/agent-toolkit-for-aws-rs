#!/bin/sh
# Self-test for scripts/openclaw-node-test.sh (#5158). Uses a throwaway package
# whose one dependency is a local tarball placed in a throwaway cache, so it
# needs no network and no registry. Needs node, npm and unshare (as the route
# does). Run: sh scripts/openclaw-node-test.selftest.sh   (make test-scripts,
# and first in make openclaw-node-test)
set -u
script="$(cd "$(dirname "$0")" && pwd)/openclaw-node-test.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/openclaw-node-selftest.XXXXXX")" || exit 1
trap 'rm -rf "$tmp"' EXIT
fails=0
check() { # name, condition-exit-status
  if [ "$2" -eq 0 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
route() { # pkg, cache, mode -> exit status; output in $tmp/out
  OPENCLAW_PACKAGE_DIR="$1" OPENCLAW_NPM_CACHE="$2" sh "$script" "$3" >"$tmp/out" 2>&1
}

# One dependency tarball and its lock integrity.
mkdir -p "$tmp/src/package"
echo '{"name":"fake-dep","version":"1.0.0"}' >"$tmp/src/package/package.json"
tar -czf "$tmp/fake-dep.tgz" -C "$tmp/src" package
sri="$(node -e 'const c=require("crypto"),f=require("fs");process.stdout.write(c.createHash("sha512").update(f.readFileSync(process.argv[1])).digest("base64"))' "$tmp/fake-dep.tgz")"
hex="$(node -e 'process.stdout.write(Buffer.from(process.argv[1],"base64").toString("hex"))' "$sri")"
seed_cache() { # dir
  d="$1/_cacache/content-v2/sha512/$(echo "$hex" | cut -c1-2)/$(echo "$hex" | cut -c3-4)"
  mkdir -p "$d"
  cp "$tmp/fake-dep.tgz" "$d/$(echo "$hex" | cut -c5-)"
}

make_pkg() { # dir, test-script, extra-lock-entries
  mkdir -p "$1"
  cat >"$1/package.json" <<EOF
{"name":"fake","version":"1.0.0","private":true,"dependencies":{"fake-dep":"1.0.0"},"scripts":{"test":"$2"}}
EOF
  cat >"$1/package-lock.json" <<EOF
{"name":"fake","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{
"":{"name":"fake","version":"1.0.0","dependencies":{"fake-dep":"1.0.0"}},
"node_modules/fake-dep":{"version":"1.0.0","resolved":"https://registry.npmjs.org/fake-dep/-/fake-dep-1.0.0.tgz","integrity":"sha512-$sri"}$3}}
EOF
}
other_os=',"node_modules/far":{"version":"1.0.0","resolved":"https://registry.npmjs.org/far/-/far-1.0.0.tgz","integrity":"sha512-AAAA","optional":true,"os":["aix"],"cpu":["ppc64"]}'

# Usage and a missing cache fail closed with the operator command.
sh "$script" bogus >"$tmp/out" 2>&1
check "unknown mode is a usage error (exit 2)" "$([ $? -eq 2 ] && echo 0 || echo 1)"
make_pkg "$tmp/pkg" 'node -e 1' ''
route "$tmp/pkg" "$tmp/no-cache" check
st=$?
check "missing cache: check exits 3" "$([ $st -eq 3 ] && echo 0 || echo 1)"
grep -q "make openclaw-npm-cache" "$tmp/out"
check "missing cache names the operator command" $?
route "$tmp/pkg" "$tmp/no-cache" run
check "missing cache: run exits 3 before npm" "$([ $? -eq 3 ] && echo 0 || echo 1)"
env -u HOME -u OPENCLAW_NPM_CACHE OPENCLAW_PACKAGE_DIR="$tmp/pkg" sh "$script" check >"$tmp/out" 2>&1
check "no HOME and no override: exit 3" "$([ $? -eq 3 ] && echo 0 || echo 1)"

# A complete cache passes; another platform's optional tarball is not needed.
seed_cache "$tmp/cache"
make_pkg "$tmp/pkg-os" 'node -e 1' "$other_os"
route "$tmp/pkg-os" "$tmp/cache" check
check "complete cache passes; other-platform entries skipped" $?

# A missing or tampered tarball fails closed.
mkdir -p "$tmp/empty/_cacache/content-v2"
route "$tmp/pkg" "$tmp/empty" check
st=$?
check "missing tarball: exit 3" "$([ $st -eq 3 ] && echo 0 || echo 1)"
grep -q "fake-dep@1.0.0: missing from the offline cache" "$tmp/out"
check "missing tarball is named" $?
cp -R "$tmp/cache" "$tmp/tampered"
find "$tmp/tampered/_cacache/content-v2" -type f -exec sh -c 'echo x >>"$1"' _ {} \;
route "$tmp/pkg" "$tmp/tampered" run
st=$?
check "tampered tarball: run exits 3" "$([ $st -eq 3 ] && echo 0 || echo 1)"
grep -q "do not match the lock integrity" "$tmp/out"
check "tampered tarball is named" $?

# run: offline install, Python step skipped, network unreachable.
cat >"$tmp/net.mjs" <<'EOF'
import net from "node:net";
const socket = net.connect({ host: "192.0.2.1", port: 9, timeout: 3000 });
socket.on("connect", () => { console.error("NETWORK REACHABLE"); process.exit(1); });
socket.on("timeout", () => { console.error("NETWORK TIMEOUT (not isolated)"); process.exit(1); });
socket.on("error", (e) => { console.log(`network refused: ${e.code}`); process.exit(0); });
EOF
make_pkg "$tmp/run" "node -e \\\"require('fake-dep/package.json')\\\" && python3 -m unittest discover && node $tmp/net.mjs && touch ran-last" ''
route "$tmp/run" "$tmp/cache" run
st=$?
check "run: offline install and node steps pass" "$st"
[ $st -eq 0 ] || cat "$tmp/out"
grep -q "skipped Python step: python3 -m unittest discover" "$tmp/out"
check "run: the Python step is skipped and logged" $?
grep -q "network refused: ENETUNREACH" "$tmp/out"
check "run: the network is unreachable inside the route" $?
[ -f "$tmp/run/ran-last" ] && [ -d "$tmp/run/node_modules/fake-dep" ]
check "run: steps after the Python step ran" $?
[ ! -e "$tmp/cache/_logs" ] && [ ! -e "$tmp/cache/_cacache/index-v5" ]
check "run: the shared cache is not written" $?

# A Python-free script runs as npm test; Python reached indirectly is refused.
printf 'python3 -c "print(1)"\n' >"$tmp/indirect.sh"
make_pkg "$tmp/py" "sh $tmp/indirect.sh" ''
route "$tmp/py" "$tmp/cache" run
st=$?
check "run: Python reached through PATH fails the route" "$([ $st -ne 0 ] && echo 0 || echo 1)"
grep -q "Python is not allowed in this route" "$tmp/out"
check "run: the Python shim names the refusal" $?
grep -q "test steps: npm test" "$tmp/out"
check "run: a Python-free script runs as npm test" $?

# A test script the route cannot split safely is refused.
make_pkg "$tmp/bad" "node -e 1; python3 -V" ''
route "$tmp/bad" "$tmp/cache" run
check "run: a script with ; and Python is refused (exit 5)" "$([ $? -eq 5 ] && echo 0 || echo 1)"

if [ "$fails" -ne 0 ]; then
  echo "openclaw-node-test selftest: $fails failure(s)"
  exit 1
fi
echo "openclaw-node-test selftest: all checks passed"
