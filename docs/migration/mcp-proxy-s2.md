# MCP proxy S2: protocol core (#5052, slice S2 of #4413)

This note records how S2 implements the accepted design
(`docs/migration/mcp-proxy-design.md` at `fd7e7ffd`, sections 4.5 and 4.6 and the
section 7 map), and the choices it made where the design left room. The code is
`crates/toolkit-cli/src/mcp_proxy*`. No command is registered yet; S3 adds
`toolkit mcp-proxy`, and no plugin configuration changes before S5.

## What S2 provides

- `Proxy::new(config, transport, auth, clock)` and `Proxy::run(stdin, stdout, diag) -> Exit`.
  These run over any `toolkit_aws::StreamingTransport` (S1) and an `Auth` seam.
- The stdio reader and the single stdout writer. The writer serves a bounded control queue
  first at every line boundary, then a bounded data queue.
- R-MEM: `Budget` and the RAII `Reservation` (`mcp_proxy/budget.rs`), a separate control
  reserve, and every bound in one module (`mcp_proxy/limits.rs`, `Limits::default` = section
  4.5).
- Framing:
  - every upstream message is parsed with `parse_json_with_limits` on its request's worker
    thread;
  - it is re-serialised compactly, with U+2028/U+2029 escaped;
  - anything that is not an object or a non-empty batch of objects is dropped with a `Diag`
    event.
- The bounded SSE parser (`mcp_proxy/sse.rs`).
- The session state machine:
  - `Mcp-Session-Id` (1-256 visible ASCII bytes) and `protocolVersion` (`YYYY-MM-DD`) are
    captured from `initialize` and sent on later requests;
  - the `GET` stream opens after `notifications/initialized` (a 405 ends it silently; it is
    reopened at most once per 30 s);
  - exactly one `DELETE` is sent at stdin EOF;
  - a 404 with a session gives the session-expired error.
- `_meta` injection with upstream precedence.
- The 16-request concurrency cap.
- Cancellation forwarding.
- `--retries` with fresh signatures.
- The fixed error templates (`mcp_proxy/templates.rs`).
- New `Diag` events: the closed `toolkit_aws::diag::ProxyEvent` set, made of fixed kinds plus
  counts and booleans.

## Decisions

1. **Threads, not tokio.** S1's `StreamingTransport` is synchronous. Each message gets a
   worker thread (`mcp-proxy-request-N`), so the default `toolkit-cli` build stays free of
   the SDK and async runtime.
   - Upstream messages are parsed on that worker, never on `mcp-proxy-reader` or
     `mcp-proxy-writer`; a test hook asserts this.
   - Client lines are parsed on the reader. They are the client's own bounded input
     (8 MiB, linear work).
2. **Peak charge.** The design's charge rule gives a message of `b` bytes a peak of
   `parse_charge(b) + b + 1` (the output line replaces the raw bytes after the parse).
   - `Limits::new` refuses bounds whose budget cannot hold the largest message.
   - It also refuses a control reserve smaller than one maximal 1 KiB control line.
   - So T-BUDGET runs with an 8 KiB budget and a 1,025 B reserve, not the design's
     illustrative 4 KiB and 512 B: those cannot hold one 1 KiB message's own charge
     (3,584 B of nodes + 2 KiB), or one maximal control line.
3. **Reservation timing.** A body chunk is reserved after it is pulled and before it is
   appended (S1's pull-only stream reads nothing ahead). The chunk in hand (at most 64 KiB)
   is per-connection fixed overhead.
4. **Waiting.** A budget wait happens only while the data queue holds bytes (the line in the
   writer's hand counts), and never past now + idle-read timeout. Otherwise it fails with
   `-32603 "proxy memory budget exhausted"`.
   - The stdin reader alone waits without a deadline: only the proxy's own producers block
     it, and they always finish or fail.
5. **Error mapping.**
   - For `tools/call`, failures of the call (HTTP status, credentials, connect, idle timeout,
     no valid response, tool timeout) are a `CallToolResult` with `isError: true`, as
     upstream does.
   - Failures of the proxy's bounds or of the session (too many requests, budget, response
     too large, session expired or refused) are JSON-RPC `-32603` for every method, per
     section 4.5.
   - A client line that is not JSON is `-32700`. One that is not a JSON-RPC message is
     `-32600` with `id: null`, or with its id when that id is valid.
   - An oversize line is `-32600 "Invalid Request: message too large"` with its id, which is
     recovered by a prefix scanner, or else gets a `Diag` line only.
6. **Ids.** Request ids must be strings or numbers of at most 256 compact bytes. A tool name
   quoted in a template is cut to 384 escaped bytes. Together these keep every control line
   within 1 KiB, and a unit test checks the worst case.
   - A duplicate in-flight id is refused (`-32600`, `id: null`).
7. **Body fidelity.** A client message whose `_meta` did not change is forwarded byte for
   byte, without a trailing CR. A changed one is re-serialised.
   - Upstream messages are always re-serialised. As the design accepts, integers outside
     `i64` become floats and floats print in Rust's shortest form.
8. **No valid response.** A request stream that ends without delivering this request's
   response gets one `-32603 "no valid response from the MCP server"`. For example, the only
   event was malformed. So the client never waits for its tool timeout.
9. **An HTTP error carrying a JSON-RPC error** for one of the message's own ids is forwarded
   (re-serialised). Any other error body is never read into any output.
10. **Concurrency.** The cap counts every upstream POST.
    - A request over the cap is refused at once.
    - A notification or a client response waits for a slot instead, as backpressure on the
      client. A `notifications/cancelled` acts locally before it waits.
    - A batch is one POST and one slot, and its requests use the request timeout.
11. **D1 seam.** On a 401/403, the core calls `Auth::refresh()` once. `true` means one
    re-signed resend; `false`, or a second 401/403, gives the credentials text.
    - `Auth::session_started()` is called for each new session id, for S3's R-STICKY session
      handling.
    - `SignerAuth` over the fake signer never refreshes.
12. **EOF.** stdin EOF sends the `DELETE` (only with a session), then aborts every task.
    Lines still queued are dropped.
    - A stdout write error aborts everything at once, with no `DELETE`.
    - `run` does not join the reader or the workers; their reservations are released as each
      one notices the shutdown.

## Deferred to S3

- The command itself:
  - CLI parsing and `McpTarget`;
  - the Q3 `User-Agent` (`ProxyConfig::user_agent` is `None` until then);
  - `--header` validation (R-HDR owned set);
  - `--retries` range checking (the core clamps to 10).
- An `Auth` over S1's `GuardedCredentials`, with R-ABS/R-SRC/R-PREC/R-STICKY:
  - `refresh()` = D1 `force_rebuild`;
  - `session_started()` = R-STICKY's session reset.
- Startup wiring, the exit status and the `toolkit mcp-proxy: …` line, and binary-level pipe
  tests.
- The T-SIG-2/4, T-CRED-1/5, T-LEAK*, T-EP, T-HDR wire, T-RO and T-CLI suites.
- Forwarding a `notifications/cancelled` upstream when the proxy's own tool timeout fires.
  Today the local task is aborted and the client gets the timeout result, but the server is
  not told.
- Follow-ups already listed in the design: re-initialise on 404, and multi-profile.
