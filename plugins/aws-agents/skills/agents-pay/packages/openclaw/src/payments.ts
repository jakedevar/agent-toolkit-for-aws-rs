import { invokeAgentCoreBridge, type BridgeInvoker } from "./bridge.js";
import { getConfig } from "./config.js";

interface PaymentSession {
  createdAt?: string;
  expiryTimeInMinutes?: number;
  availableLimits?: {
    availableSpendAmount?: {
      value?: string;
    };
  };
}

export interface SessionStatus {
  usable: boolean;
  status: string;
  expired: boolean;
  minutes_left: number | null;
  remaining_usd: number | null;
}

export async function getPaymentSessionStatus(
  invoke: BridgeInvoker = invokeAgentCoreBridge,
): Promise<SessionStatus> {
  const config = getConfig();

  try {
    const response = await invoke({
      operation: "get_payment_session",
      region: config.region,
      payment_manager_arn: config.paymentManagerArn,
      payment_session_id: config.payment_session_id,
      user_id: config.userId,
      agent_name: "openclaw-aws-agents-pay",
    });
    const session = response.paymentSession as PaymentSession | undefined;
    if (!session) {
      return {
        usable: false,
        status: "not_found",
        expired: true,
        minutes_left: null,
        remaining_usd: null,
      };
    }

    let expired = false;
    let minutesLeft: number | null = null;
    const createdAt = session.createdAt
      ? Date.parse(session.createdAt)
      : Number.NaN;
    if (
      Number.isFinite(createdAt) &&
      typeof session.expiryTimeInMinutes === "number"
    ) {
      const expiresAt = createdAt + session.expiryTimeInMinutes * 60_000;
      expired = expiresAt <= Date.now();
      if (!expired) {
        minutesLeft = Math.max(
          0,
          Math.floor((expiresAt - Date.now()) / 60_000),
        );
      }
    }

    const rawRemaining = session.availableLimits?.availableSpendAmount?.value;
    const remainingUsd =
      rawRemaining === undefined ? null : Number.parseFloat(rawRemaining);
    const hasBudget =
      remainingUsd === null ||
      (Number.isFinite(remainingUsd) && remainingUsd > 0);
    const usable = !expired && hasBudget;

    return {
      usable,
      status: usable ? "ACTIVE" : expired ? "EXPIRED" : "DRAINED",
      expired,
      minutes_left: minutesLeft,
      remaining_usd: remainingUsd,
    };
  } catch {
    return {
      usable: false,
      status: "error",
      expired: true,
      minutes_left: null,
      remaining_usd: null,
    };
  }
}

export interface PaymentResult {
  signedPayload: Record<string, unknown>;
}

/**
 * Ask the Rust bridge to authorize and pay the offer TS selected. Rust
 * re-authorizes `accepted` against the same policy, builds the ProcessPayment
 * payload and derives the idempotency token (R-TOK), so neither is sent.
 */
export async function processPayment(
  version: string,
  accepted: Record<string, unknown>,
  resource: Record<string, unknown>,
  resourceUrl: string,
  invoke: BridgeInvoker = invokeAgentCoreBridge,
): Promise<PaymentResult> {
  if (version !== "2") {
    throw new Error("Only x402 v2 payment challenges are supported");
  }

  const config = getConfig();
  const response = await invoke({
    operation: "process_payment",
    region: config.region,
    payment_manager_arn: config.paymentManagerArn,
    payment_session_id: config.payment_session_id,
    payment_instrument_id: config.paymentInstrumentId,
    user_id: config.userId,
    agent_name: "openclaw-aws-agents-pay",
    version,
    request_url: resourceUrl,
    accepted,
    resource,
    policy: {
      networkPreferences: config.networkPreferences,
      allowedOrigins: config.allowedOrigins,
      allowedRecipients: config.allowedRecipients,
      allowAnyRecipient: config.allowAnyRecipient,
      allowedAssetsByNetwork: config.allowedAssetsByNetwork,
      maxPaymentAmountAtomic: config.maxPaymentAmountAtomic,
    },
  });

  const paymentOutput = response.paymentOutput as
    | { cryptoX402?: { payload?: unknown } }
    | undefined;
  if (
    !paymentOutput ||
    !paymentOutput.cryptoX402?.payload
  ) {
    throw new Error(
      "ProcessPayment did not return a signed cryptoX402 payload",
    );
  }

  const payload = paymentOutput.cryptoX402.payload;
  const normalized: unknown =
    typeof payload === "string" ? JSON.parse(payload) : payload;
  if (
    !normalized ||
    typeof normalized !== "object" ||
    Array.isArray(normalized)
  ) {
    throw new Error("ProcessPayment returned an invalid signed payload");
  }
  return { signedPayload: normalized as Record<string, unknown> };
}
