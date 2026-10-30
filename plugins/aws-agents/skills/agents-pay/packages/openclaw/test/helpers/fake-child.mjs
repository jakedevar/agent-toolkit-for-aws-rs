// Deterministic fake child process for runBridge (#4411 B4, T-JS-BR-*,
// T-JS-BR-CLEAN-*, T-JS-BR-CAP, T-JS-FINAL-FAIL). No process is spawned and
// no network or Python is used: the test drives every stdout, stderr, stdin,
// error and close event itself, in the order it chooses, and keeps a
// reference to every Buffer it hands to the bridge so it can check that the
// bridge zeroed it.
import { EventEmitter } from "node:events";
import { execPath } from "node:process";

// An executable file that exists on every host running these tests. The fake
// spawn never runs it; runBridge only checks it with access(X_OK).
export const PRESENT_BINARY = execPath;

export class FakeChild extends EventEmitter {
  constructor() {
    super();
    this.stdout = new EventEmitter();
    this.stderr = new EventEmitter();
    this.stdin = new EventEmitter();
    this.written = [];
    this.kills = [];
    this.sent = [];
    this.stdin.end = (data) => {
      this.written.push(data);
    };
  }

  kill(signal) {
    this.kills.push(signal);
    return true;
  }

  // Emit one stdout chunk; the Buffer is retained for the zero checks.
  out(bytes) {
    const chunk = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
    this.sent.push(chunk);
    this.stdout.emit("data", chunk);
    return chunk;
  }

  err(bytes) {
    const chunk = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
    this.sent.push(chunk);
    this.stderr.emit("data", chunk);
    return chunk;
  }

  close(code) {
    this.emit("close", code);
  }
}

// A spawn replacement that records every call and hands out FakeChild
// objects. `next()` resolves with the child of the next spawn call.
export function fakeSpawner(onSpawn) {
  const calls = [];
  const children = [];
  const waiting = new Map();
  let handed = 0;
  const spawn = (command, args, options) => {
    const child = new FakeChild();
    calls.push({ command, args, options });
    children.push(child);
    const resolve = waiting.get(children.length - 1);
    if (resolve) {
      waiting.delete(children.length - 1);
      resolve(child);
    }
    if (onSpawn) queueMicrotask(() => onSpawn(child, calls.at(-1)));
    return child;
  };
  // The child of the next spawn call not yet handed out.
  const next = () => {
    const index = handed;
    handed += 1;
    if (children[index]) return Promise.resolve(children[index]);
    return new Promise((resolve) => waiting.set(index, resolve));
  };
  return { spawn, calls, children, next };
}

// Owned-Buffer observation: every Buffer runBridge allocates itself (the
// combined stdout) is reported here, and `retained()` reads how many stdout
// chunks it still holds.
export function ownership() {
  const owned = [];
  const state = { retained: () => -1 };
  return {
    owned,
    state,
    hooks(spawn) {
      return {
        spawn,
        own: (buffer) => owned.push(buffer),
        retained: (count) => {
          state.retained = count;
        },
      };
    },
  };
}

export function allZero(buffer) {
  return buffer.every((byte) => byte === 0);
}

export async function settled(promise) {
  try {
    return { value: await promise };
  } catch (error) {
    return { error };
  }
}

// Let queued promise callbacks (access(), the spawn) run.
export function tick() {
  return new Promise((resolve) => setImmediate(resolve));
}
