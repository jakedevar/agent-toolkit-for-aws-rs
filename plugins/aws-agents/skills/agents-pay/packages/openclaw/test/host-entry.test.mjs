import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import entry, { definePluginEntry, registerTools } from "../dist/index.js";

const fixture = JSON.parse(await readFile(new URL("./fixtures/v2-security-contract.json", import.meta.url), "utf8"));

function host(deps) {
  const tools = new Map();
  return {
    tools,
    api: {
      pluginConfig: { ...fixture.config, deps, probe: deps.probe, replay: deps.replay, invoke: deps.invoke },
      registerTool(tool) { tools.set(tool.name, tool); },
    },
  };
}

function trapDeps(calls) {
  return {
    probe: async () => { calls.push("probe"); return { status: 200 }; },
    replay: async () => { calls.push("replay"); throw new Error("unexpected replay"); },
    invoke: async () => { calls.push("invoke"); throw new Error("unexpected invoke"); },
  };
}

test("host exports ignore dependency-like runtime inputs; explicit test seam remains usable", async () => {
  for (const register of [definePluginEntry, entry]) {
    const calls = [];
    const deps = trapDeps(calls);
    const { api, tools } = host(deps);
    register(api);
    const result = await tools.get("get_paid_content").execute("call", {
      url: "http://127.0.0.1/", deps, ...deps,
    });
    assert.deepEqual(calls, [], "host entry must not use runtime dependencies");
    assert.equal(result.details.refused, true);
    assert.match(result.details.reason, /HTTPS/i);
  }

  const calls = [];
  const deps = trapDeps(calls);
  const { api, tools } = host(deps);
  registerTools(api, deps);
  const result = await tools.get("get_paid_content").execute("call", { url: "http://127.0.0.1/" });
  assert.deepEqual(calls, ["probe"]);
  assert.equal(result.details.status_code, 200);
});

test("host entry pins all frozen production dependencies", async () => {
  const source = await readFile(new URL("../src/index.ts", import.meta.url), "utf8");
  assert.match(source, /const PRODUCTION: PaidToolDeps = Object\.freeze\(\{\s*probe: probeUrl,\s*replay: replayWithHeader,\s*invoke: invokeAgentCoreBridge,\s*\}\);/);
  assert.match(source, /export function definePluginEntry\(api: any\) \{\s*registerTools\(api, PRODUCTION\);\s*\}\s*export default definePluginEntry;/);
});
