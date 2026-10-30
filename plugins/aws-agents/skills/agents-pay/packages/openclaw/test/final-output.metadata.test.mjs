// #4411 B4 R-OC-FINAL with returnBody false (metadata only): the merchant's
// Content-Type is untrusted even when the body is withheld. Runs in its own
// process: config.ts caches the first loaded config.
import assert from "node:assert/strict";
import test from "node:test";

import { finalCase, runFinalCase } from "./helpers/paid-tool.mjs";

test("T-JS-FINAL-CLEAN: a withheld body echo leaves the metadata output unchanged", async () => {
  const testCase = finalCase("meta-withheld-body-echo");
  assert.equal(testCase.returnBody, false);
  const { result } = await runFinalCase(testCase);
  assert.equal(result.details.content_returned, false);
  assert.equal("body" in result.details, false);
  assert.deepEqual(Object.keys(result.details).sort(), [
    "body_bytes",
    "body_sha256",
    "content_returned",
    "content_type",
    "paid",
    "refused",
    "status_code",
    "url",
  ]);
});

test("T-JS-FINAL-3: a Content-Type header echo with returnBody false refuses the result", async () => {
  const testCase = finalCase("meta-content-type-echo");
  assert.equal(testCase.returnBody, false);
  await runFinalCase(testCase);
});
