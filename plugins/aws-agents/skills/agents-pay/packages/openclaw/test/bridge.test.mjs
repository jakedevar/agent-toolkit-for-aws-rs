// #4411 B4: the bridge.ts pipe contract over a deterministic fake child.
// T-JS-BR-1..8, T-JS-BR-CAP (shared T-BR-OUT-CAP frames), T-JS-BR-CLEAN-*
// (R-TS-CLEAN, #5164) and T-JS-PATH (R-TS-1). No process, network or Python.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import * as bridge from "../dist/bridge.js";
import {
  PRESENT_BINARY,
  allZero,
  fakeSpawner,
  ownership,
  settled,
  tick,
} from "./helpers/fake-child.mjs";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const contract = (name) =>
  readFile(new URL(`./fixtures/bridge-contract/${name}`, import.meta.url));

const SESSION_REQUEST = JSON.parse(
  (await contract("get_payment_session.request.json")).toString("utf8"),
);
const SESSION_OUTPUT = await contract("get_payment_session.bridge-output.json");

// Short enough to finish quickly, long enough never to fire in a test that
// settles by another path.
const LIMITS = Object.freeze({
  maxInputBytes: 128 * 1024,
  maxOutputBytes: 128 * 1024,
  maxStderrBytes: 16 * 1024,
  timeoutMs: 5_000,
});

// One runBridge call over a fresh fake child; resolves once spawned.
async function start({ limits = LIMITS, request = SESSION_REQUEST } = {}) {
  const spawner = fakeSpawner();
  const owner = ownership();
  let outcomes = 0;
  const promise = bridge.runBridge(
    PRESENT_BINARY,
    bridge.BRIDGE_ARGS,
    request,
    limits,
    owner.hooks(spawner.spawn),
  );
  promise.then(
    () => {
      outcomes += 1;
    },
    () => {
      outcomes += 1;
    },
  );
  const child = await spawner.next();
  return {
    child,
    spawner,
    owner,
    result: settled(promise),
    outcomes: () => outcomes,
  };
}

// Every Buffer the test handed in and every Buffer the bridge allocated is
// zero, and the bridge retains no stdout chunk.
function assertClean(run) {
  for (const [index, chunk] of run.child.sent.entries()) {
    assert.ok(allZero(chunk), `sent chunk ${index} was not zeroed`);
  }
  for (const [index, buffer] of run.owner.owned.entries()) {
    assert.ok(allZero(buffer), `owned buffer ${index} was not zeroed`);
  }
  assert.equal(run.owner.state.retained(), 0);
}

// The late events a killed or finished child can still deliver.
async function lateEvents(run) {
  run.child.out("late-stdout-after-settlement");
  run.child.err("late-stderr-after-settlement");
  run.child.emit("error", new Error("late"));
  run.child.stdin.emit("error", new Error("late"));
  run.child.close(0);
  run.child.out("late-stdout-after-close");
  run.child.close(1);
  await tick();
}

test("T-JS-BR-1: a success frame parses; spawn is the fixed binary with no shell", async () => {
  const run = await start();
  const [call] = run.spawner.calls;
  assert.equal(call.command, PRESENT_BINARY);
  assert.deepEqual(call.args, ["agents-pay", "bridge"]);
  assert.equal(call.options.shell, false);
  assert.equal(call.options.windowsHide, true);
  assert.deepEqual(call.options.stdio, ["pipe", "pipe", "pipe"]);
  assert.equal(path.resolve(call.options.cwd), path.resolve(packageRoot));
  // Exactly the inherited environment: the bridge adds no variable (the old
  // PYTHON* additions are gone) and removes none.
  assert.deepEqual(call.options.env, { ...process.env });
  assert.notEqual(call.options.env, process.env);
  assert.deepEqual(run.child.written, [JSON.stringify(SESSION_REQUEST)]);

  run.child.out(Buffer.from(SESSION_OUTPUT));
  run.child.close(0);
  const { value, error } = await run.result;
  assert.equal(error, undefined);
  assert.deepEqual(value, JSON.parse(SESSION_OUTPUT.toString("utf8")).result);
  assert.deepEqual(run.child.kills, []);
});

test("T-JS-BR-2: exits 1 and 2 give the fixed request-failed error", async () => {
  for (const code of [1, 2]) {
    const run = await start();
    run.child.out('{"ok":false,"error":"invalid bridge request"}\n');
    run.child.close(code);
    const { error } = await run.result;
    assert.equal(error?.message, "AgentCore bridge request failed");
  }
});

test("T-JS-BR-3: stdout over 128 KiB is refused and the child killed", async () => {
  const run = await start();
  run.child.out(Buffer.alloc(128 * 1024, 0x61));
  run.child.out(Buffer.from("b"));
  const { error } = await run.result;
  assert.equal(error?.message, "AgentCore bridge response exceeded its size limit");
  assert.deepEqual(run.child.kills, ["SIGKILL"]);
});

test("T-JS-BR-4: stderr over 16 KiB is refused and the child killed", async () => {
  const run = await start();
  run.child.err(Buffer.alloc(16 * 1024, 0x65));
  run.child.err(Buffer.from("e"));
  const { error } = await run.result;
  assert.equal(error?.message, "AgentCore bridge diagnostics exceeded their size limit");
  assert.deepEqual(run.child.kills, ["SIGKILL"]);
});

test("T-JS-BR-5: a short timeout kills the child", async () => {
  const run = await start({ limits: { ...LIMITS, timeoutMs: 20 } });
  const { error } = await run.result;
  assert.equal(error?.message, "AgentCore bridge timed out");
  assert.deepEqual(run.child.kills, ["SIGKILL"]);
});

test("T-JS-BR-6: a missing binary rejects without spawning", async () => {
  const spawner = fakeSpawner();
  const { error } = await settled(
    bridge.runBridge(
      path.join(packageRoot, "bin", "no-such-platform", "toolkit"),
      bridge.BRIDGE_ARGS,
      SESSION_REQUEST,
      LIMITS,
      { spawn: spawner.spawn },
    ),
  );
  assert.equal(error?.code, "ENOENT");
  assert.equal(spawner.calls.length, 0);
});

test("T-JS-BR-7: invalid JSON and non-success envelopes are invalid responses", async () => {
  for (const frame of [
    "not-json\n",
    '{"ok":true,"result":[]}\n',
    '{"ok":true,"result":null}\n',
    '{"result":{}}\n',
  ]) {
    const run = await start();
    run.child.out(frame);
    run.child.close(0);
    const { error } = await run.result;
    assert.equal(error?.message, "AgentCore bridge returned an invalid response");
  }
});

test("T-JS-BR-8: a request over 128 KiB is refused before spawn", async () => {
  // {"pad":"…"} encodes to 10 bytes plus the padding.
  const atLimit = { pad: "x".repeat(128 * 1024 - 10) };
  assert.equal(Buffer.byteLength(JSON.stringify(atLimit)), 128 * 1024);
  const accepted = await start({ request: atLimit });
  assert.equal(accepted.spawner.calls.length, 1);
  accepted.child.close(1);
  await accepted.result;

  const spawner = fakeSpawner();
  const { error } = await settled(
    bridge.runBridge(
      PRESENT_BINARY,
      bridge.BRIDGE_ARGS,
      { pad: "x".repeat(128 * 1024 - 9) },
      LIMITS,
      { spawn: spawner.spawn },
    ),
  );
  assert.equal(error?.message, "AgentCore bridge request exceeded its size limit");
  assert.equal(spawner.calls.length, 0);

  // A multi-byte request is counted in UTF-8 bytes, not code units.
  const { error: wide } = await settled(
    bridge.runBridge(
      PRESENT_BINARY,
      bridge.BRIDGE_ARGS,
      { pad: "é".repeat(64 * 1024) },
      LIMITS,
      { spawn: spawner.spawn },
    ),
  );
  assert.equal(wide?.message, "AgentCore bridge request exceeded its size limit");
  assert.equal(spawner.calls.length, 0);
});

test("T-JS-BR: spawn and stdin failures give their fixed errors", async () => {
  const failedStart = await start();
  failedStart.child.emit("error", new Error("spawn ENOENT"));
  assert.equal((await failedStart.result).error?.message, "AgentCore bridge could not start");

  const failedInput = await start();
  failedInput.child.stdin.emit("error", new Error("EPIPE"));
  assert.equal((await failedInput.result).error?.message, "AgentCore bridge input failed");
  assert.deepEqual(failedInput.child.kills, ["SIGKILL"]);
});

test("T-JS-BR: production limits and argv are fixed", () => {
  assert.deepEqual({ ...bridge.BRIDGE_LIMITS }, {
    maxInputBytes: 131072,
    maxOutputBytes: 131072,
    maxStderrBytes: 16384,
    timeoutMs: 30000,
  });
  assert.ok(Object.isFrozen(bridge.BRIDGE_LIMITS));
  assert.deepEqual([...bridge.BRIDGE_ARGS], ["agents-pay", "bridge"]);
  assert.ok(Object.isFrozen(bridge.BRIDGE_ARGS));
});

// T-JS-BR-CAP (#5162 TS half): the exact T-BR-OUT-CAP frames Rust pins.
for (const variant of ["ascii", "bmp", "astral"]) {
  test(`T-JS-BR-CAP ${variant}: a 131072-byte frame including LF parses`, async () => {
    const frame = await contract(`output-frame-cap/${variant}-131072.json`);
    assert.equal(frame.length, 131072);
    assert.equal(frame.at(-1), 0x0a);
    const expected = JSON.parse(frame.toString("utf8")).result;
    for (const split of [false, true]) {
      const run = await start();
      if (split) {
        run.child.out(Buffer.from(frame.subarray(0, -1)));
        run.child.out(Buffer.from("\n"));
      } else {
        run.child.out(Buffer.from(frame));
      }
      run.child.close(0);
      const { value, error } = await run.result;
      assert.equal(error, undefined, `split=${split}`);
      assert.deepEqual(value, expected);
      assertClean(run);
    }
  });

  test(`T-JS-BR-CAP ${variant}: a 131073-byte frame triggers the cap and kill`, async () => {
    const frame = await contract(`output-frame-cap/${variant}-131073.json`);
    assert.equal(frame.length, 131073);
    for (const split of [false, true]) {
      const run = await start();
      if (split) {
        // The final LF alone is the byte over the cap.
        run.child.out(Buffer.from(frame.subarray(0, -1)));
        run.child.out(Buffer.from("\n"));
      } else {
        run.child.out(Buffer.from(frame));
      }
      const { error } = await run.result;
      assert.equal(error?.message, "AgentCore bridge response exceeded its size limit");
      assert.deepEqual(run.child.kills, ["SIGKILL"]);
      run.child.close(0);
      await tick();
      assertClean(run);
    }
  });
}

test("T-JS-BR-CAP: Rust's fixed replacement frame never parses as success", async () => {
  const replacement = await contract("output-frame-cap/replacement.json");
  assert.equal(replacement.toString("utf8"), '{"ok":false,"error":"AgentCore request failed"}\n');
  for (const [code, message] of [
    [0, "AgentCore bridge returned an invalid response"],
    [1, "AgentCore bridge request failed"],
  ]) {
    const run = await start();
    run.child.out(Buffer.from(replacement));
    run.child.close(code);
    assert.equal((await run.result).error?.message, message);
    assertClean(run);
  }
});

// T-JS-BR-CLEAN-* (R-TS-CLEAN, #5164).
test("T-JS-BR-CLEAN-SUCCESS: success zeroes every chunk and the combined buffer", async () => {
  const run = await start();
  run.child.out(Buffer.from(SESSION_OUTPUT.subarray(0, 40)));
  run.child.out(Buffer.from(SESSION_OUTPUT.subarray(40)));
  run.child.close(0);
  assert.equal((await run.result).error, undefined);
  assert.ok(run.owner.owned.length >= 1, "the combined buffer was reported");
  assertClean(run);
  await lateEvents(run);
  assertClean(run);
  assert.equal(run.outcomes(), 1);
});

test("T-JS-BR-CLEAN-INVALID: invalid JSON and nonzero close zero their chunks", async () => {
  for (const [frame, code] of [
    ["{not json", 0],
    ['{"ok":true,"result":{"paymentOutput":{"cryptoX402":{"payload":{"signature":"0xsecret"}}}}}\n', 1],
  ]) {
    const run = await start();
    run.child.out(frame);
    run.child.close(code);
    assert.ok((await run.result).error);
    assertClean(run);
    await lateEvents(run);
    assertClean(run);
    assert.equal(run.outcomes(), 1);
  }
});

test("T-JS-BR-CLEAN-SPAWN: a spawn error zeroes later chunks", async () => {
  const run = await start();
  run.child.emit("error", new Error("spawn EACCES"));
  assert.equal((await run.result).error?.message, "AgentCore bridge could not start");
  await lateEvents(run);
  assertClean(run);
  assert.equal(run.outcomes(), 1);
});

test("T-JS-BR-CLEAN-TIMEOUT: queued and late stdout after a timeout is zeroed", async () => {
  const run = await start({ limits: { ...LIMITS, timeoutMs: 20 } });
  run.child.out("queued-proof-prefix-");
  run.child.out("queued-proof-more");
  assert.equal((await run.result).error?.message, "AgentCore bridge timed out");
  assertClean(run);
  await lateEvents(run);
  assertClean(run);
  assert.deepEqual(run.child.kills, ["SIGKILL"]);
  assert.equal(run.outcomes(), 1);
});

test("T-JS-BR-CLEAN-STDERR: stderr overflow zeroes queued stdout and every stderr chunk", async () => {
  const run = await start();
  run.child.out("queued-proof-prefix");
  run.child.err(Buffer.alloc(8 * 1024, 0x65));
  run.child.err(Buffer.alloc(8 * 1024 + 1, 0x65));
  assert.equal(
    (await run.result).error?.message,
    "AgentCore bridge diagnostics exceeded their size limit",
  );
  assertClean(run);
  await lateEvents(run);
  assertClean(run);
  assert.equal(run.outcomes(), 1);
});

test("T-JS-BR-CLEAN-STDIN: stdin failure zeroes queued and late stdout", async () => {
  const run = await start();
  run.child.out("queued-proof-prefix");
  run.child.stdin.emit("error", new Error("EPIPE"));
  assert.equal((await run.result).error?.message, "AgentCore bridge input failed");
  assertClean(run);
  await lateEvents(run);
  assertClean(run);
  assert.equal(run.outcomes(), 1);
});

test("T-JS-BR-CLEAN-OVERFLOW: the overflow trigger, prefix and later chunks are zeroed", async () => {
  const run = await start();
  const prefix = run.child.out(Buffer.alloc(100_000, 0x70));
  const trigger = run.child.out(Buffer.alloc(40_000, 0x71));
  assert.equal(
    (await run.result).error?.message,
    "AgentCore bridge response exceeded its size limit",
  );
  assert.ok(allZero(prefix), "prefix");
  assert.ok(allZero(trigger), "overflow-triggering chunk");
  run.child.out(Buffer.alloc(1_000, 0x72));
  run.child.close(0);
  await tick();
  assertClean(run);
  await lateEvents(run);
  assertClean(run);
  assert.equal(run.outcomes(), 1);
});

test("T-JS-BR-CLEAN-ORDER: close before late error and data settles once", async () => {
  const run = await start();
  run.child.out("partial");
  run.child.close(1);
  run.child.emit("error", new Error("late"));
  run.child.out("after-close");
  assert.equal((await run.result).error?.message, "AgentCore bridge request failed");
  await lateEvents(run);
  assertClean(run);
  assert.deepEqual(run.child.kills, []);
  assert.equal(run.outcomes(), 1);
});

// T-JS-PATH (R-TS-1): the fixed platform table.
test("T-JS-PATH: six supported pairs map to the staged binaries", () => {
  const expected = {
    "linux/x64": ["bin", "linux-x86_64", "toolkit"],
    "linux/arm64": ["bin", "linux-aarch64", "toolkit"],
    "darwin/x64": ["bin", "macos-x86_64", "toolkit"],
    "darwin/arm64": ["bin", "macos-aarch64", "toolkit"],
    "win32/x64": ["bin", "toolkit.exe"],
    "win32/arm64": ["bin", "toolkit.exe"],
  };
  for (const [pair, parts] of Object.entries(expected)) {
    const [platform, arch] = pair.split("/");
    assert.equal(
      bridge.bridgeBinaryFor(platform, arch),
      path.join(packageRoot, ...parts),
      pair,
    );
  }
});

test("T-JS-PATH: unsupported pairs reject without spawning", async () => {
  for (const [platform, arch] of [
    ["linux", "ia32"],
    ["freebsd", "x64"],
    ["linux", "arm"],
    ["linux", "toString"],
    ["__proto__", "x64"],
  ]) {
    assert.equal(bridge.bridgeBinaryFor(platform, arch), undefined, `${platform}/${arch}`);
    const spawner = fakeSpawner();
    const { error } = await settled(
      bridge.runBridge(
        bridge.bridgeBinaryFor(platform, arch),
        bridge.BRIDGE_ARGS,
        SESSION_REQUEST,
        LIMITS,
        { spawn: spawner.spawn },
      ),
    );
    assert.equal(error?.message, "AgentCore bridge could not start");
    assert.equal(spawner.calls.length, 0);
  }

  const descriptor = Object.getOwnPropertyDescriptor(process, "platform");
  Object.defineProperty(process, "platform", { ...descriptor, value: "freebsd" });
  try {
    const { error } = await settled(bridge.invokeAgentCoreBridge(SESSION_REQUEST));
    assert.equal(error?.message, "AgentCore bridge could not start");
  } finally {
    Object.defineProperty(process, "platform", descriptor);
  }
});

test("T-JS-PATH: the production call uses the fixed table and fails closed without a staged binary", async () => {
  // The package under test has no staged bin/, so access() refuses before any spawn.
  const binary = bridge.bridgeBinaryFor(process.platform, process.arch);
  assert.ok(binary === undefined || binary.startsWith(path.join(packageRoot, "bin")));
  const { error } = await settled(bridge.invokeAgentCoreBridge(SESSION_REQUEST));
  assert.ok(error);
  assert.doesNotMatch(String(error?.message), /python|venv/i);
});
