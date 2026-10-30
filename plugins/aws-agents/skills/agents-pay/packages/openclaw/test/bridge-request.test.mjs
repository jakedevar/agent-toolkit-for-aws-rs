// #4411 B4 T-JS-REQ (R-TS-2): payments.ts sends requests byte-equal to the
// shared bridge-contract fixtures that the Rust bridge tests read. The bridge
// is runBridge over a fake child, so the bytes compared are the bytes written
// to the child's stdin. Runs in its own process: config.ts caches the first
// loaded config.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import * as bridge from "../dist/bridge.js";
import { loadConfig } from "../dist/config.js";
import * as payments from "../dist/payments.js";
import { PRESENT_BINARY, fakeSpawner } from "./helpers/fake-child.mjs";

const contract = async (name) =>
  (
    await readFile(new URL(`./fixtures/bridge-contract/${name}`, import.meta.url))
  ).toString("utf8");

const REQUEST_URL = "https://merchant.example/pay/weather?city=sydney";
const ACCEPTED = {
  scheme: "exact",
  network: "eip155:84532",
  amount: "1000",
  asset: "0x036cbd53842c5426634e7929541ec2318f3dcf7e",
  payTo: "0x1111111111111111111111111111111111111111",
  maxTimeoutSeconds: 60,
};

await loadConfig({
  region: "us-east-1",
  paymentManagerArn: "a".repeat(66),
  paymentInstrumentId: "i".repeat(34),
  userId: "user",
  payment_session_id: "s".repeat(31),
  networkPreferences: ["eip155:84532"],
  allowedOrigins: ["https://merchant.example"],
  allowedRecipients: ["0x1111111111111111111111111111111111111111"],
  allowedAssetsByNetwork: {
    "eip155:84532": ["0x036cbd53842c5426634e7929541ec2318f3dcf7e"],
  },
  maxPaymentAmountAtomic: "100000",
});

// A bridge whose fake child answers each operation with its fixture output.
function recordingBridge(outputs) {
  const spawner = fakeSpawner((child) => {
    const request = JSON.parse(child.written[0]);
    child.out(outputs[request.operation]);
    child.close(0);
  });
  const invoke = (request) =>
    bridge.runBridge(PRESENT_BINARY, bridge.BRIDGE_ARGS, request, bridge.BRIDGE_LIMITS, {
      spawn: spawner.spawn,
    });
  return { spawner, invoke };
}

test("T-JS-REQ: processPayment sends the fixture process_payment request", async () => {
  const { spawner, invoke } = recordingBridge({
    process_payment: await contract("process_payment.bridge-output.json"),
  });
  const payment = await payments.processPayment(
    "2",
    structuredClone(ACCEPTED),
    { url: REQUEST_URL },
    REQUEST_URL,
    invoke,
  );
  assert.equal(spawner.calls.length, 1);
  const [written] = spawner.children[0].written;
  assert.equal(written, await contract("process_payment.request.json"));
  const sent = JSON.parse(written);
  assert.equal("client_token" in sent, false);
  assert.equal("payload" in sent, false);
  assert.equal("allowAnyRecipient" in sent.policy, false);
  assert.equal("returnBody" in sent.policy, false);
  assert.notEqual(written, await contract("legacy_process_payment.request.json"));
  assert.deepEqual(payment.signedPayload, { signature: "synthetic-signature" });
});

test("T-JS-REQ: a non-v2 version is refused before the bridge", async () => {
  const { spawner, invoke } = recordingBridge({});
  await assert.rejects(
    payments.processPayment("1", structuredClone(ACCEPTED), { url: REQUEST_URL }, REQUEST_URL, invoke),
  );
  assert.equal(spawner.calls.length, 0);
});

test("T-JS-REQ: getPaymentSessionStatus sends the fixture get_payment_session request", async () => {
  const { spawner, invoke } = recordingBridge({
    get_payment_session: await contract("get_payment_session.bridge-output.json"),
  });
  const status = await payments.getPaymentSessionStatus(invoke);
  assert.equal(spawner.calls.length, 1);
  assert.equal(
    spawner.children[0].written[0],
    await contract("get_payment_session.request.json"),
  );
  // The fixture session was created 2026-08-07 with a 10-minute expiry.
  assert.deepEqual(status, {
    usable: false,
    status: "EXPIRED",
    expired: true,
    minutes_left: null,
    remaining_usd: null,
  });
});

test("T-JS-REQ: a bridge failure gives the existing status error", async () => {
  const spawner = fakeSpawner((child) => child.close(1));
  const status = await payments.getPaymentSessionStatus((request) =>
    bridge.runBridge(PRESENT_BINARY, bridge.BRIDGE_ARGS, request, bridge.BRIDGE_LIMITS, {
      spawn: spawner.spawn,
    }),
  );
  assert.deepEqual(status, {
    usable: false,
    status: "error",
    expired: true,
    minutes_left: null,
    remaining_usd: null,
  });
});
