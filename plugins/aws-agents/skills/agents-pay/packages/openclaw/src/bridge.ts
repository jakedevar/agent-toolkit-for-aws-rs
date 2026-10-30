import { spawn as spawnProcess } from "node:child_process";
import { constants } from "node:fs";
import { access } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

export interface BridgeLimits {
  readonly maxInputBytes: number;
  readonly maxOutputBytes: number;
  readonly maxStderrBytes: number;
  readonly timeoutMs: number;
}

/** The production pipe limits (design 1.5). The Rust bridge's 25 s deadline sits inside the 30 s kill. */
export const BRIDGE_LIMITS: BridgeLimits = Object.freeze({
  maxInputBytes: 128 * 1024,
  maxOutputBytes: 128 * 1024,
  maxStderrBytes: 16 * 1024,
  timeoutMs: 30_000,
});

/** The only argv the bridge binary is ever given. */
export const BRIDGE_ARGS: readonly string[] = Object.freeze(["agents-pay", "bridge"]);

export type BridgeRequest =
  | {
      operation: "get_payment_session";
      region: string;
      payment_manager_arn: string;
      payment_session_id: string;
      user_id: string;
      agent_name: string;
    }
  | {
      operation: "process_payment";
      region: string;
      payment_manager_arn: string;
      payment_session_id: string;
      payment_instrument_id: string;
      user_id: string;
      agent_name: string;
      version: string;
      request_url: string;
      accepted: Record<string, unknown>;
      resource: Record<string, unknown>;
      policy: Record<string, unknown>;
    }
  | {
      operation: "sanitize_output";
      candidate: {
        content: { type: "text"; text: string }[];
        details: Record<string, unknown>;
      };
      secrets: string[];
    };

export type BridgeInvoker = (
  request: BridgeRequest,
) => Promise<Record<string, unknown>>;

const packageRoot = fileURLToPath(new URL("../", import.meta.url));

// The staged plugin-stage layout (design 6.1; mirrors the PB dispatcher, pinned
// by the Rust T-PKG-LAYOUT test). Windows on ARM64 runs the x86_64 binary.
const BRIDGE_BINARIES: Readonly<Record<string, string>> = Object.freeze({
  "linux/x64": "bin/linux-x86_64/toolkit",
  "linux/arm64": "bin/linux-aarch64/toolkit",
  "darwin/x64": "bin/macos-x86_64/toolkit",
  "darwin/arm64": "bin/macos-aarch64/toolkit",
  "win32/x64": "bin/toolkit.exe",
  "win32/arm64": "bin/toolkit.exe",
});

/**
 * The staged bridge binary for an OS/architecture pair, or undefined when the
 * pair is unsupported. No environment variable or config can change it.
 */
export function bridgeBinaryFor(
  platform: string,
  arch: string,
): string | undefined {
  const key = `${platform}/${arch}`;
  if (!Object.prototype.hasOwnProperty.call(BRIDGE_BINARIES, key)) {
    return undefined;
  }
  return path.join(packageRoot, ...BRIDGE_BINARIES[key].split("/"));
}

export function parseBridgeResponse(serialized: string): Record<string, unknown> {
  const parsed: unknown = JSON.parse(serialized);
  if (
    !parsed ||
    typeof parsed !== "object" ||
    Array.isArray(parsed) ||
    !("ok" in parsed) ||
    parsed.ok !== true ||
    !("result" in parsed) ||
    !parsed.result ||
    typeof parsed.result !== "object" ||
    Array.isArray(parsed.result)
  ) {
    throw new Error("AgentCore bridge returned an invalid response");
  }
  return parsed.result as Record<string, unknown>;
}

/**
 * Test seams for runBridge only. Production calls (invokeAgentCoreBridge)
 * pass none: the real child_process spawn runs the fixed binary.
 * - spawn: a replacement for child_process.spawn (a fake child);
 * - own: observes each Buffer runBridge allocates itself;
 * - retained: receives a reader of how many stdout chunks are held.
 */
export interface BridgeHooks {
  spawn?: typeof spawnProcess;
  own?: (buffer: Buffer) => void;
  retained?: (count: () => number) => void;
}

/**
 * Run one bridge call: one JSON request on stdin, one JSON frame on stdout.
 *
 * R-TS-CLEAN (#5164): settlement is idempotent. Every owned stdout Buffer is
 * zeroed and dropped on every exit; a chunk that arrives after settlement is
 * zeroed at once and never kept; the chunk that overflows the cap is zeroed
 * before settling; stderr chunks are counted, then zeroed. This covers owned
 * Buffers only: the immutable strings decoded from them cannot be erased.
 */
export async function runBridge(
  binary: string | undefined,
  args: readonly string[],
  request: BridgeRequest | Record<string, unknown>,
  limits: BridgeLimits,
  hooks: BridgeHooks = {},
): Promise<Record<string, unknown>> {
  if (binary === undefined) {
    throw new Error("AgentCore bridge could not start");
  }
  const serialized = JSON.stringify(request);
  if (Buffer.byteLength(serialized) > limits.maxInputBytes) {
    throw new Error("AgentCore bridge request exceeded its size limit");
  }

  await access(binary, constants.X_OK);

  const spawn = hooks.spawn ?? spawnProcess;
  return new Promise((resolve, reject) => {
    const child = spawn(binary, [...args], {
      cwd: packageRoot,
      env: { ...process.env },
      shell: false,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdout: Buffer[] = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let settled = false;
    hooks.retained?.(() => stdout.length);

    const clearStdout = () => {
      for (const chunk of stdout) chunk.fill(0);
      stdout.length = 0;
    };

    const finishWithError = (message: string) => {
      if (settled) {
        clearStdout();
        return;
      }
      settled = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      clearStdout();
      reject(new Error(message));
    };

    const timer = setTimeout(() => {
      finishWithError("AgentCore bridge timed out");
    }, limits.timeoutMs);

    child.on("error", () => {
      finishWithError("AgentCore bridge could not start");
    });
    child.stdout.on("data", (chunk: Buffer) => {
      if (settled) {
        chunk.fill(0);
        return;
      }
      stdoutBytes += chunk.length;
      if (stdoutBytes > limits.maxOutputBytes) {
        chunk.fill(0);
        finishWithError("AgentCore bridge response exceeded its size limit");
        return;
      }
      stdout.push(chunk);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderrBytes += chunk.length;
      chunk.fill(0);
      if (stderrBytes > limits.maxStderrBytes) {
        finishWithError("AgentCore bridge diagnostics exceeded their size limit");
      }
    });
    child.stdin.on("error", () => {
      finishWithError("AgentCore bridge input failed");
    });
    child.on("close", (code) => {
      if (settled) {
        clearStdout();
        return;
      }
      settled = true;
      clearTimeout(timer);
      if (code !== 0) {
        clearStdout();
        reject(new Error("AgentCore bridge request failed"));
        return;
      }
      const combined = Buffer.concat(stdout);
      hooks.own?.(combined);
      try {
        resolve(parseBridgeResponse(combined.toString("utf8")));
      } catch {
        reject(new Error("AgentCore bridge returned an invalid response"));
      } finally {
        combined.fill(0);
        clearStdout();
      }
    });

    child.stdin.end(serialized);
  });
}

/** The production bridge call: the fixed binary for this platform and the production limits. */
export async function invokeAgentCoreBridge(
  request: BridgeRequest,
): Promise<Record<string, unknown>> {
  return runBridge(
    bridgeBinaryFor(process.platform, process.arch),
    BRIDGE_ARGS,
    request,
    BRIDGE_LIMITS,
  );
}
