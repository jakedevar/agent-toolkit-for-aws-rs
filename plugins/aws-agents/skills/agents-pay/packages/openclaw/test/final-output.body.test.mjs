// #4411 B4 R-OC-FINAL with returnBody true: T-JS-FINAL-1..4, T-JS-FINAL-FAIL
// and T-JS-FINAL-CLEAN. The registered paid tool runs through injected
// probe/replay/bridge seams (#5179); the sanitize call goes through runBridge
// and a fake child answering with the Rust-verified tool-cases.json frames.
// Runs in its own process: config.ts caches the first loaded config.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import * as bridge from "../dist/bridge.js";
import {
  FINAL,
  LOCAL_REFUSAL,
  SECURITY,
  assertCleared,
  assertNoProof,
  childBridge,
  finalCase,
  frameBridge,
  paidTool,
  runFinalCase,
} from "./helpers/paid-tool.mjs";

const echoReplay = () => structuredClone(finalCase("body-echo").replay);

test("T-JS-FINAL-1: exact header and signature echoes in the body are redacted", async () => {
  await runFinalCase(finalCase("body-echo"));
});

test("T-JS-FINAL-2: encoded and escaped proof fragments in the body are redacted", async () => {
  await runFinalCase(finalCase("body-encoded"));
});

test("T-JS-FINAL-3: a Content-Type echo refuses the whole result, body hit included", async () => {
  await runFinalCase(finalCase("body-and-content-type"));
});

test("T-JS-FINAL-4: replay-402 and a thrown replay pass the final check unchanged", async () => {
  for (const name of ["body-replay-402", "body-replay-throws"]) {
    await runFinalCase(finalCase(name));
  }
});

test("T-JS-FINAL-CLEAN: an opt-in capped, truncated, untrusted body keeps its bytes", async () => {
  const { result } = await runFinalCase(finalCase("body-clean-truncated"));
  assert.equal(result.details.truncated, true);
  assert.equal(result.details.untrusted, true);
  assert.equal(result.details.body.length, 10240);
});

test("T-JS-FINAL-CLEAN: refusals before any proof keep their bytes and skip the scrub", async () => {
  const notPayable = paidTool({
    returnBody: true,
    probe: () => ({ status: 200, headers: {}, body: "", contentType: "", bodySha256: "", bodyBytes: 0 }),
    replay: echoReplay,
    sanitize: () => assert.fail("no sanitize before a proof"),
  });
  const payload = {
    paid: false,
    refused: true,
    status_code: 200,
    reason: "URL did not return an x402 payment challenge",
  };
  assert.deepEqual(await notPayable.paid(), {
    content: [{ type: "text", text: JSON.stringify(payload, null, 2) }],
    details: payload,
  });
  assert.equal(notPayable.calls.process.length, 0);

  const challenge = structuredClone(SECURITY.challenge);
  challenge.accepts = [challenge.accepts[0]];
  const blocked = paidTool({
    returnBody: true,
    probe: () => ({
      status: 402,
      headers: {
        "payment-required": Buffer.from(JSON.stringify(challenge)).toString("base64"),
      },
      body: "",
      contentType: "application/json",
      bodySha256: "",
      bodyBytes: 0,
    }),
    replay: echoReplay,
    sanitize: () => assert.fail("no sanitize before a proof"),
  });
  const refusal = await blocked.paid();
  assert.equal(refusal.details.refused, true);
  assert.match(refusal.details.reason, /No payment option satisfies/);
  assert.equal(refusal.content[0].text, JSON.stringify(refusal.details, null, 2));
  assert.equal(blocked.calls.process.length, 0);
  assert.equal(blocked.calls.sanitize.length, 0);
});

// T-JS-FINAL-FAIL: every failure of the final check is the local fixed
// refusal in both channels; the echoed proof never comes back raw.
async function assertFailsClosed(harness, label) {
  const result = await harness.paid();
  assert.deepEqual(result, LOCAL_REFUSAL, label);
  assertNoProof(result);
  assertCleared(harness);
  assert.equal(harness.calls.process.length, 1, `${label}: one ProcessPayment`);
  return result;
}

test("T-JS-FINAL-FAIL: a proof without a registrable signature refuses without a scrub", async () => {
  const harness = paidTool({
    returnBody: true,
    signedPayload: { authorization: { validAfter: "0", nonce: "proof-nonce-0001" } },
    replay: echoReplay,
    sanitize: () => assert.fail("no sanitize without a complete registry"),
  });
  await assertFailsClosed(harness, "missing signature");
  assert.equal(harness.calls.sanitize.length, 0);
});

test("T-JS-FINAL-FAIL: malformed sanitize responses refuse", async () => {
  const text = '{"paid":true}';
  for (const result of [
    { content: [{ type: "text", text }] },
    { content: [{ type: "text", text }], details: { paid: true }, extra: 1 },
    { content: [{ type: "text", text }, { type: "text", text }], details: { paid: true } },
    { content: [{ type: "image", text }], details: { paid: true } },
    { content: [{ type: "text", text: 1 }], details: { paid: true } },
    { content: [{ type: "text", text, extra: 1 }], details: { paid: true } },
    { content: [{ type: "text", text }], details: [] },
    { content: [{ type: "text", text }], details: null },
    { content: {}, details: { paid: true } },
  ]) {
    const sanitize = frameBridge(`${JSON.stringify({ ok: true, result })}\n`);
    const harness = paidTool({ returnBody: true, replay: echoReplay, sanitize: sanitize.invoke });
    await assertFailsClosed(harness, JSON.stringify(result));
    assert.equal(harness.calls.sanitize.length, 1);
  }
});

test("T-JS-FINAL-FAIL: bridge failures of the sanitize call refuse", async () => {
  const replacement = await readFile(
    new URL("./fixtures/bridge-contract/output-frame-cap/replacement.json", import.meta.url),
  );
  const cases = {
    "missing binary": childBridge(() => assert.fail("no spawn"), {
      binary: path.join(tmpdir(), "openclaw-b4-no-such-dir", "toolkit"),
    }),
    "unsupported platform": childBridge(() => assert.fail("no spawn"), { binary: undefined }),
    timeout: childBridge(() => {}, { limits: { ...bridge.BRIDGE_LIMITS, timeoutMs: 20 } }),
    "stdin failure": childBridge((child) => child.stdin.emit("error", new Error("EPIPE"))),
    "stderr overflow": childBridge((child) => child.err(Buffer.alloc(16 * 1024 + 1, 0x65))),
    "spawn error": childBridge((child) => child.emit("error", new Error("EACCES"))),
    "oversized response": childBridge((child) => child.out(Buffer.alloc(128 * 1024 + 1, 0x20))),
    "scrub failure exit 1": frameBridge(replacement, 1),
    "input refusal exit 2": frameBridge('{"ok":false,"error":"invalid bridge request"}\n', 2),
    "failure frame exit 0": frameBridge(replacement, 0),
    "invalid JSON": frameBridge("{not json\n", 0),
  };
  for (const [label, sanitize] of Object.entries(cases)) {
    const harness = paidTool({ returnBody: true, replay: echoReplay, sanitize: sanitize.invoke });
    await assertFailsClosed(harness, label);
  }
  assert.equal(cases["missing binary"].spawner.calls.length, 0);
  assert.equal(cases["unsupported platform"].spawner.calls.length, 0);
  assert.deepEqual(cases.timeout.spawner.children[0].kills, ["SIGKILL"]);
});

test("T-JS-FINAL-FAIL: an oversized sanitize request is refused before spawn", async () => {
  // 10240 control characters: escaped in details and escaped twice in the
  // text channel, the request exceeds the 131072-byte bridge input cap.
  const replay = () => ({
    ...echoReplay(),
    body: "\u0001".repeat(10240),
    bodyBytes: 10240,
  });
  const sanitize = childBridge(() => assert.fail("no spawn"));
  const harness = paidTool({ returnBody: true, replay, sanitize: sanitize.invoke });
  await assertFailsClosed(harness, "oversized request");
  assert.equal(harness.calls.sanitize.length, 1);
  assert.ok(Buffer.byteLength(JSON.stringify(harness.calls.sanitize[0])) > 131072);
  assert.equal(sanitize.spawner.calls.length, 0);
});

test("T-JS-FINAL-FAIL: a throwing sanitize seam refuses", async () => {
  const harness = paidTool({
    returnBody: true,
    replay: echoReplay,
    sanitize: () => {
      throw new Error("boom");
    },
  });
  await assertFailsClosed(harness, "throwing sanitize");
});

test("T-JS-FINAL: the status tool uses the injected bridge and is not scrubbed", async () => {
  const harness = paidTool({
    returnBody: true,
    replay: echoReplay,
    sanitize: () => assert.fail("status output is not a paid result"),
  });
  const result = await harness.tools.get("get_payment_session_status").execute("call-2");
  // The fake bridge refuses get_payment_session: the existing error status.
  assert.deepEqual(result.details, {
    usable: false,
    status: "error",
    expired: true,
    minutes_left: null,
    remaining_usd: null,
  });
  assert.equal(result.content[0].text, JSON.stringify(result.details, null, 2));
  assert.equal(harness.calls.other.length, 1);
  assert.equal(harness.calls.other[0].operation, "get_payment_session");
});

// T-JS-FINAL-CLEAN source gate: only the R-TS-3 caller and final-boundary
// changes; x402.ts and config.ts byte-identical to the accepted sources.
test("T-JS-FINAL-CLEAN: x402.ts and config.ts are unchanged; index.ts keeps to R-TS-3", async () => {
  const source = async (name) => readFile(new URL(`../src/${name}`, import.meta.url));
  const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
  assert.equal(
    sha256(await source("x402.ts")),
    "63a673899675c21e92eb043b51c89b8d6b60a690ee4fb66b8b786d86fdb15afb",
  );
  assert.equal(
    sha256(await source("config.ts")),
    "50c699b3bc45200b72867d14092c939f8c4236f83595e67f1ae02402902e92d8",
  );
  assert.equal(
    sha256(await readFile(new URL("../openclaw.plugin.json", import.meta.url))),
    "ac9ed6b45b5c0c7a44c071755c81c3c78013abea6204ce64ac62cdbaa6250425",
  );

  const index = (await source("index.ts")).toString("utf8");
  const imports = [...index.matchAll(/^import .* from "([^"]+)";$/gm)].map((m) => m[1]);
  const multiline = [...index.matchAll(/^} from "([^"]+)";$/gm)].map((m) => m[1]);
  assert.deepEqual([...imports, ...multiline].sort(), [
    "./bridge.js",
    "./config.js",
    "./payments.js",
    "./x402.js",
    "typebox",
  ]);
  assert.doesNotMatch(index, /node:(http|https|net|tls|dns|fs|child_process)/);
  assert.doesNotMatch(index, /buildProcessPaymentPayload/);
  assert.match(
    index,
    /processPayment\(\s*authorized\.version,\s*authorized\.accepted,\s*authorized\.resource,\s*url,/,
  );
  assert.match(index, /validateChallengePolicy\(parsed, url, config\)/);
  assert.match(index, /buildPaidContentResult\(result, config\)/);
  // OQ12: the dead builder stays exported from the unchanged x402.ts.
  const x402 = await import("../dist/x402.js");
  assert.equal(typeof x402.buildProcessPaymentPayload, "function");
  assert.equal(FINAL.cases.length, 8);
});
