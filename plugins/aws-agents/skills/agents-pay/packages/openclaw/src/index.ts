import { Type } from "typebox";
import { invokeAgentCoreBridge, type BridgeInvoker } from "./bridge.js";
import { getConfig, loadConfig, type X402Config } from "./config.js";
import { getPaymentSessionStatus, processPayment } from "./payments.js";
import {
  PaymentBlocked,
  probeUrl,
  extractChallenge,
  buildPaymentPayloadEnvelope,
  sleepPastValidAfter,
  replayWithHeader,
  validateChallengePolicy,
} from "./x402.js";

type ToolResult = {
  content: { type: "text"; text: string }[];
  details: Record<string, unknown>;
};

function json(payload: unknown): ToolResult {
  return {
    content: [
      { type: "text" as const, text: JSON.stringify(payload, null, 2) },
    ],
    details: payload as Record<string, unknown>,
  };
}

function clearObject(value: Record<string, unknown>): void {
  for (const key of Object.keys(value)) {
    delete value[key];
  }
}

export const MAX_RETURNED_BODY_BYTES = 10240;

const FAILED_SAFELY =
  "Payment flow failed safely without exposing provider or proof details";

/**
 * Build the model-visible `get_paid_content` result from a successfully paid
 * replay. Pure and side-effect-free so the security contract around body
 * withholding can be tested directly, without mocking the network.
 *
 * Default/absent `config.returnBody` preserves the original behavior exactly:
 * metadata + hash only, never the paid body. Opt-in (`returnBody: true`)
 * additionally returns the body (capped, marked untrusted) — see
 * references/security-model.md ("Publisher content isolation"): unsanitized
 * paid content may carry prompt injection.
 */
export function buildPaidContentResult(
  result: {
    status: number;
    contentType: string;
    bodySha256: string;
    bodyBytes: number;
    url: string;
    body: string;
  },
  config: Pick<X402Config, "returnBody">,
): Record<string, unknown> {
  const response: Record<string, unknown> = {
    paid: result.status >= 200 && result.status < 300,
    refused: false,
    status_code: result.status,
    content_type: result.contentType,
    body_sha256: result.bodySha256,
    body_bytes: result.bodyBytes,
    url: result.url,
    content_returned: false,
  };

  if (config.returnBody === true) {
    const truncated = result.body.length > MAX_RETURNED_BODY_BYTES;
    response.content_returned = true;
    response.body = result.body.slice(0, MAX_RETURNED_BODY_BYTES);
    response.truncated = truncated;
    response.untrusted = true;
  }

  return response;
}

/**
 * The seams of the paid tool (#5179). definePluginEntry always passes
 * PRODUCTION; no environment variable, config value or tool parameter can
 * replace them. Tests call registerTools with fakes.
 */
export interface PaidToolDeps {
  probe: typeof probeUrl;
  replay: typeof replayWithHeader;
  invoke: BridgeInvoker;
}

const PRODUCTION: PaidToolDeps = Object.freeze({
  probe: probeUrl,
  replay: replayWithHeader,
  invoke: invokeAgentCoreBridge,
});

/**
 * The invocation-local proof registry (R-OC-FINAL): the signed payload's
 * signature and the exact PAYMENT-SIGNATURE header value, kept from the
 * moment each exists until the final output check. Never logged, global or
 * persisted.
 */
interface ProofRegistry {
  proof: Record<string, unknown> | null;
  complete: boolean;
  secrets: string[];
}

async function paidOutcome(
  url: string,
  deps: PaidToolDeps,
  registry: ProofRegistry,
): Promise<Record<string, unknown>> {
  try {
    const probe = await deps.probe(url);
    if (probe.status !== 402) {
      return {
        paid: false,
        refused: true,
        status_code: probe.status,
        reason: "URL did not return an x402 payment challenge",
      };
    }

    const config = getConfig();
    const parsed = extractChallenge(probe);
    const authorized = validateChallengePolicy(parsed, url, config);
    const payment = await processPayment(
      authorized.version,
      authorized.accepted,
      authorized.resource,
      url,
      deps.invoke,
    );
    registry.proof = payment.signedPayload;
    const signature = payment.signedPayload.signature;
    if (typeof signature === "string" && signature !== "") {
      registry.secrets.push(signature);
    } else {
      registry.complete = false;
    }

    await sleepPastValidAfter(payment.signedPayload);
    const header = buildPaymentPayloadEnvelope(
      authorized.resource,
      authorized.accepted,
      payment.signedPayload,
    );
    registry.secrets.push(header);
    const result = await deps.replay(url, header);
    if (result.status === 402) {
      return {
        paid: false,
        refused: false,
        status_code: 402,
        reason:
          "The merchant still returned 402 after the idempotent paid replay",
      };
    }

    return buildPaidContentResult(result, config);
  } catch (error) {
    return {
      paid: false,
      refused: true,
      reason:
        error instanceof PaymentBlocked
          ? error.message
          : FAILED_SAFELY,
    };
  }
}

function isToolResult(value: Record<string, unknown>): value is ToolResult {
  const keys = (object: object) => Object.keys(object).sort().join(",");
  const content = value.content;
  const details = value.details;
  if (keys(value) !== "content,details" || !Array.isArray(content)) {
    return false;
  }
  if (content.length !== 1) return false;
  const item: unknown = content[0];
  return (
    Boolean(item) &&
    typeof item === "object" &&
    !Array.isArray(item) &&
    keys(item as object) === "text,type" &&
    (item as { type?: unknown }).type === "text" &&
    typeof (item as { text?: unknown }).text === "string" &&
    Boolean(details) &&
    typeof details === "object" &&
    !Array.isArray(details)
  );
}

/**
 * R-OC-FINAL: once a proof exists, the complete tool result is checked by the
 * pure Rust sanitize_output operation before it is returned, over both the
 * text and the details channel. Any failure returns the fixed refusal; the
 * raw candidate is never returned.
 */
async function finalOutput(
  candidate: ToolResult,
  registry: ProofRegistry,
  deps: PaidToolDeps,
): Promise<ToolResult> {
  if (registry.proof === null) return candidate;
  if (!registry.complete) return json(refusal());
  try {
    const checked = await deps.invoke({
      operation: "sanitize_output",
      candidate,
      secrets: [...registry.secrets, getConfig().payment_session_id],
    });
    return isToolResult(checked) ? checked : json(refusal());
  } catch {
    return json(refusal());
  }
}

function refusal(): Record<string, unknown> {
  return { paid: false, refused: true, reason: FAILED_SAFELY };
}

async function paidFetch(url: string, deps: PaidToolDeps): Promise<ToolResult> {
  const registry: ProofRegistry = { proof: null, complete: true, secrets: [] };
  try {
    const payload = await paidOutcome(url, deps, registry);
    return await finalOutput(json(payload), registry, deps);
  } finally {
    // Drop the registry's references and clear the proof object. The JS
    // strings themselves cannot be erased (R-TS-CLEAN).
    registry.secrets.length = 0;
    if (registry.proof) clearObject(registry.proof);
    registry.proof = null;
  }
}

/** Register both tools against `deps`; definePluginEntry passes PRODUCTION. */
export function registerTools(api: any, deps: PaidToolDeps): void {
  let configLoaded = false;
  async function ensureConfig() {
    if (!configLoaded) {
      await loadConfig(api.pluginConfig);
      configLoaded = true;
    }
  }

  api.registerTool({
    name: "get_payment_session_status",
    description:
      "Return whether the operator-provisioned payment session is usable. " +
      "The runtime cannot create or replace payment sessions.",
    parameters: Type.Object({}),
    async execute(_toolCallId: string) {
      await ensureConfig();
      return json(await getPaymentSessionStatus(deps.invoke));
    },
  });

  api.registerTool({
    name: "get_paid_content",
    description:
      "Fetch and pay an approved x402 v2 URL, returning response metadata only. " +
      "Publisher content and signed proofs never enter model-visible output.",
    parameters: Type.Object({
      url: Type.String({ description: "HTTPS x402 v2 resource URL" }),
    }),
    async execute(_toolCallId: string, params: { url: string }) {
      await ensureConfig();
      return paidFetch(params.url, deps);
    },
  });
}

export function definePluginEntry(api: any) {
  registerTools(api, PRODUCTION);
}

export default definePluginEntry;
