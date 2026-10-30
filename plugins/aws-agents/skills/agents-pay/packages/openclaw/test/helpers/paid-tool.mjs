// The registered paid tool over injected seams (#4411 B4, #5179 ruling):
// index.ts registerTools(api, deps) with fake probe, replay and bridge
// functions. Production wiring (definePluginEntry) always passes the real
// probeUrl, replayWithHeader and invokeAgentCoreBridge.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import * as bridge from "../../dist/bridge.js";
import * as index from "../../dist/index.js";
import { PRESENT_BINARY, fakeSpawner } from "./fake-child.mjs";

const read = async (name) =>
  JSON.parse(await readFile(new URL(`../fixtures/${name}`, import.meta.url), "utf8"));

export const SECURITY = await read("v2-security-contract.json");
export const FINAL = await read("bridge-contract/final-output/tool-cases.json");
export const SIGNATURE = FINAL.signedPayload.signature;
export const HEADER = FINAL.header;

export const FAILED_SAFELY =
  "Payment flow failed safely without exposing provider or proof details";
const refusalPayload = { paid: false, refused: true, reason: FAILED_SAFELY };
export const LOCAL_REFUSAL = {
  content: [{ type: "text", text: JSON.stringify(refusalPayload, null, 2) }],
  details: refusalPayload,
};

export function probeFor(challenge) {
  return {
    status: 402,
    headers: {
      "payment-required": Buffer.from(JSON.stringify(challenge)).toString("base64"),
    },
    body: "",
    contentType: "application/json",
    bodySha256: "",
    bodyBytes: 0,
  };
}

export function finalCase(name) {
  const found = FINAL.cases.find((testCase) => testCase.name === name);
  assert.ok(found, name);
  return found;
}

// A sanitize bridge whose fake child runs `behave(child)`.
// `binary` may be given as undefined (an unsupported platform), so it is read
// by presence, not with a destructuring default.
export function childBridge(behave, options = {}) {
  const binary = Object.hasOwn(options, "binary") ? options.binary : PRESENT_BINARY;
  const limits = options.limits ?? bridge.BRIDGE_LIMITS;
  const spawner = fakeSpawner(behave);
  const invoke = (request) =>
    bridge.runBridge(binary, bridge.BRIDGE_ARGS, request, limits, { spawn: spawner.spawn });
  return { spawner, invoke };
}

// A sanitize bridge answering with one exact frame.
export function frameBridge(frame, code = 0) {
  return childBridge((child) => {
    child.out(frame);
    child.close(code);
  });
}

/**
 * Register the plugin tools against a fake host api and injected seams.
 * - probe: returns the security fixture challenge unless overridden;
 * - replay: `replay(header)` gives the merchant's paid response;
 * - bridge: process_payment answers with `signedPayload` (kept by reference so
 *   the test can see it cleared); sanitize_output goes to `sanitize`.
 */
export function paidTool({ returnBody, replay, sanitize, signedPayload, probe }) {
  const tools = new Map();
  const config = { ...structuredClone(SECURITY.config) };
  if (returnBody !== undefined) config.returnBody = returnBody;
  const api = {
    pluginConfig: config,
    registerTool(tool) {
      tools.set(tool.name, tool);
    },
  };
  const payload = structuredClone(signedPayload ?? FINAL.signedPayload);
  const calls = { probe: [], replay: [], process: [], sanitize: [], payloadAtSanitize: [], other: [] };
  const deps = {
    probe: async (url) => {
      calls.probe.push(url);
      return probe ? probe(url) : probeFor(SECURITY.challenge);
    },
    replay: async (url, header) => {
      calls.replay.push({ url, header });
      return replay(header);
    },
    invoke: async (request) => {
      if (request.operation === "process_payment") {
        calls.process.push(structuredClone(request));
        return { paymentOutput: { cryptoX402: { payload } } };
      }
      if (request.operation === "sanitize_output") {
        calls.sanitize.push(structuredClone(request));
        calls.payloadAtSanitize.push(JSON.stringify(payload));
        return sanitize(request);
      }
      calls.other.push(structuredClone(request));
      throw new Error("unexpected bridge operation");
    },
  };
  index.registerTools(api, deps);
  return {
    paid: (url = FINAL.requestUrl) => tools.get("get_paid_content").execute("call-1", { url }),
    tools,
    calls,
    payload,
  };
}

// Neither the exact proof header nor the signature reaches either channel.
export function assertNoProof(result) {
  const serialized = JSON.stringify(result);
  assert.ok(!serialized.includes(SIGNATURE), "signature echoed");
  assert.ok(!serialized.includes(HEADER), "header echoed");
  assert.ok(!result.content[0].text.includes(SIGNATURE));
  assert.ok(!JSON.stringify(result.details).includes(SIGNATURE));
}

// The proof object was cleared and only the checked result survives.
export function assertCleared(harness) {
  assert.deepEqual(Object.keys(harness.payload), []);
}

// Run one tool-cases.json case end to end through runBridge and a fake child
// that answers with the case's Rust-verified frame.
export async function runFinalCase(testCase) {
  const sanitizeBridge = frameBridge(testCase.frame);
  const harness = paidTool({
    returnBody: testCase.returnBody,
    replay: () => {
      if (testCase.replayThrows) throw new Error("socket hang up");
      return structuredClone(testCase.replay);
    },
    sanitize: sanitizeBridge.invoke,
  });
  const result = await harness.paid();

  assert.equal(harness.calls.process.length, 1);
  assert.equal(harness.calls.replay.length, 1);
  assert.equal(harness.calls.replay[0].header, HEADER);
  assert.equal(harness.calls.sanitize.length, 1);
  // TS built exactly the request the Rust test answers with this frame.
  assert.equal(JSON.stringify(harness.calls.sanitize[0]), JSON.stringify(testCase.request));
  assert.deepEqual(sanitizeBridge.spawner.children[0].written, [JSON.stringify(testCase.request)]);
  // The registry and the proof outlived replay and serialization.
  assert.equal(JSON.parse(harness.calls.payloadAtSanitize[0]).signature, SIGNATURE);
  assert.deepEqual(harness.calls.sanitize[0].secrets, [SIGNATURE, HEADER, SECURITY.config.payment_session_id]);

  const expected = JSON.parse(testCase.frame).result;
  assert.deepEqual(result, expected);
  assert.equal(result.content[0].text, expected.content[0].text);
  assert.deepEqual(JSON.parse(result.content[0].text), result.details);
  assertNoProof(result);
  assertCleared(harness);
  const candidate = testCase.request.candidate;
  if (testCase.outcome === "clean") {
    assert.equal(result.content[0].text, candidate.content[0].text);
    assert.deepEqual(result.details, candidate.details);
  } else if (testCase.outcome === "refuse") {
    assert.deepEqual(result.details, { paid: false, reason: FAILED_SAFELY, refused: true });
  } else {
    assert.equal(result.details.redacted, true);
    assert.match(result.details.body, /\[REDACTED\]/);
  }
  return { result, harness };
}
