# MCP proxy S3: the `toolkit mcp-proxy` command (#5079, slice S3 of #4413)

This note records how S3 implements the accepted design
(`docs/migration/mcp-proxy-design.md` at `fd7e7ffd`, section 5 "S3" and the
section 7 map) on top of S1 (#5017) and S2 (#5052, `mcp-proxy-s2.md`). No
plugin configuration changes before S5.

## Usage

```
toolkit mcp-proxy ENDPOINT [--service NAME] [--region REGION] [--profile NAME]
    [--metadata KEY=VALUE]... [--header NAME=VALUE]... [--skip-auth] [--read-only]
    [--retries N] [--timeout S] [--connect-timeout S] [--read-timeout S]
    [--write-timeout S] [--tool-timeout S] [--log-level LEVEL]
```

The command needs a build with `--features aws-sdk`; a default build refuses
every start with `toolkit mcp-proxy: this toolkit build has no AWS SDK
support; rebuild it with --features aws-sdk` (exit 2).

Exit status: 0 when stdin ends (after the session `DELETE`), 1 when stdout
fails, 2 for any refused start. Argument problems are ordinary usage errors
(`error: invalid arguments: ...` plus the usage text). Later refusals are one
line, `toolkit mcp-proxy: <fixed text>`, with nothing on stdout.

## Start-up order

1. Arguments (`mcp_proxy/cli.rs`): R-URL (`toolkit_aws::McpTarget`), the R-HDR
   owned header set, one `--profile`, `--retries` 0-10, timeouts 1-3600 s,
   upstream's `--log-level` names. No refused value is ever echoed.
2. The signing scope (`mcp_proxy/scope.rs`, T-SCOPE): `--service`, else the
   host's service; `--region`, then the host's region, then `AWS_REGION`.
3. R-ROOTS once (`ValidatedRoots::from_process`), before stdin is read and
   before any connector, provider or DNS lookup.
4. D2: `isolate_stdin` (Unix). See "Non-Unix targets" below.
5. The endpoint host must resolve to public addresses only.
6. The MCP transport (`SdkTransport` over the validated roots), the
   provider-plane client (`ProviderHttp::validated`, the same roots) and S1's
   `GuardedCredentials` (R-ABS, R-SRC, R-PREC, D1, R-STICKY) are built from the
   startup environment and files only; then the S2 core runs.

`main` holds the standard stdout and stderr locks for the whole run, so the
proxy writes through private duplicates of descriptors 1 and 2 (std's safe
`try_clone_to_owned`).

## Behaviour added in S3

- **`--read-only`** (T-RO): a `tools/list` result keeps only tools whose
  `annotations.readOnlyHint` is JSON `true`. A `tools/call` for any tool the
  current session has not approved (including one never listed) is answered
  locally with upstream's text, `Tool '<name>' is not available in read-only
  mode.`, as a `CallToolResult` with `isError: true`, and never sent. Every
  `tools/call` object is gated, whatever its framing: one without an id
  cannot be answered, so it is dropped (never forwarded) with
  `read-only-tool-refused`. A batch holding a refused call is refused whole
  (its other requests get `-32600`), so nothing of it is sent. The filter
  trusts annotations: it is a convenience filter, not authorization (IAM is).
  Approvals (repair 1, #5110/#5111):
  - they belong to one session: a new `initialize` clears them, and a
    listing sent in an earlier session never approves anything later;
  - a **fresh listing** (`tools/list` without `params.cursor`) replaces the
    approved set, so names it omits are revoked; a **page** (with
    `params.cursor`) extends the current set. Every listing is numbered at
    dispatch, and a listing older than the one that started the current
    snapshot is ignored, so a late response cannot restore approvals;
  - a listing changes approvals only after its response is claimed (a
    response that already timed out or was cancelled changes nothing);
  - only approved names are kept, charged to the R-MEM budget at
    `name bytes + 64` each, and capped at one message (`max_message`) in
    total. A listing that would pass the cap, or that the budget cannot
    hold, revokes every approval (fail closed) with
    `read-only-catalogue-refused`; the charge is released on replacement and
    on a new session.
- **D1 on 401/403**: `SourceAuth::refresh` runs one forced rebuild
  (`GuardedCredentials::resolve(skip_auth, true)`), then the core re-signs and
  resends once; a second 401/403 fails with the credentials text. Every
  attempt resolves credentials once (T-CRED-1). This applies to every MCP
  request the proxy owns (repair 1, #5113): the `POST`s, the `GET` stream
  open (after a second refusal the stream is retried at the next reopen
  interval) and the session `DELETE`. **Method-specific contract**: the
  proxy's own `notifications/cancelled` (below) is best effort: signed once,
  no D1 refresh, no retry, its response unread.
- **R-STICKY session boundary** (repair 1, #5106): sessions are numbered
  generations. When an `initialize` is dispatched, the current session is
  retired and the next generation starts atomically: no session id, no
  protocol version, no read-only approvals. Only that bookkeeping happens
  under the proxy's locks (repair 2, #5136): R-STICKY's credential side,
  `Auth::initialize_started` (the old session's signing is forgotten, so
  the `initialize` itself counts for the new one), may wait on credential
  I/O, so the initialize worker calls it before signing, with no proxy lock
  held. stdin, deadlines, cancels and shutdown never wait on a provider. A
  boundary lock orders these calls, and a boundary for a generation that is
  already retired or superseded is skipped, so a slow earlier boundary never
  resets a later session's signing state. The fresh `initialize` never carries the old
  `Mcp-Session-Id`. Work bound to a retired generation is never (re)sent:
  each attempt checks its generation when the request is built and again
  after signing, and a retry of an old-session request fails with the
  session-expired text. So no request into a signed session is ever sent
  unsigned, even when the re-`initialize` fails. A late `initialize`
  response of a retired generation commits nothing. The retired session is
  not sent a `DELETE` (it simply ends; the server expires it).
- **The `GET` stream** (repair 1, #5108): one worker serves every
  generation in turn, so at most one `GET` stream is ever open. When its
  session is retired the stream stops at its next chunk (or its idle read
  timeout), its messages are dropped, it is never reopened, and the worker
  then opens the new session's stream. A message already waiting for budget
  or stdout room when its session is retired is dropped too (repair 2,
  #5137): it is never queued into the next session, and the worker is not
  held by stdout backpressure. Lines already queued before the retirement
  are written.
- **The proxy's own deadline** (tool or request timeout): besides answering
  the client, the core queues one signed `notifications/cancelled` with the
  request id and reason `request timed out`. Repair 1 (#5107) bounds this
  control work: one control worker sends the queue in order; the queue
  holds at most the request cap (`max_in_flight`), each queued body is
  charged to the R-MEM budget, and a cancel waits at most the request
  timeout. A full queue, an exhausted budget, a passed deadline, a retired
  session or a worker thread the system refuses drops the cancel with
  `timeout-cancel-dropped`; the supervisor never blocks or panics on it.
  The deadline is absolute (repair 2, #5138). It is checked when the cancel
  is dequeued and again after signing, which may wait on credential I/O,
  together with shutdown and the session. The transport's connect and read
  timeouts are capped at the time left. A cancel refused at any of these
  points, or by the credentials, is dropped with `timeout-cancel-dropped`.
- **Admission** (repair 1, #5107): each request of a message takes one
  concurrency slot (a message without requests takes one), so a batch holds
  at most `max_in_flight` requests; a message whose requests do not fit is
  refused whole (each request gets the too-many-requests error) and nothing
  of it is sent.
- **`User-Agent`** (Q3): the fixed token `agent-toolkit-mcp-proxy/<version>`;
  the client's name and version are never added.
- **`AWS_REGION` metadata**: `AWS_DEFAULT_REGION`, then the selected profile's
  `region` (the boto3 session region), then `AWS_REGION`, then the host's
  region; `--metadata AWS_REGION=...` overrides it and client `_meta` keys win.
- **`--log-level`** sets the threshold of the allowlisted `Diag` sink only;
  the default is `ERROR`. No `tracing` subscriber or `log` logger is installed
  (R-LOG); `RUST_LOG` has no effect.

## Non-Unix targets (D2)

S1's null-stdin isolation exists on Unix only. On other targets the proxy
starts without it, and S1's guarded credentials refuse, per request and as an
Invalid (level 1) outcome, any selected **profile** source that could run a
`credential_process`: the profile has the key itself, or it has a
`source_profile` and some profile in the files has the key (key names only;
the chain is assumed to reach it). The refusal is the fixed credentials
message to the client and `credential-failure
kind=credential-process-needs-null-stdin` on stderr. There is no fallback to
another source (the profile row is refused, not skipped). Environment, web
identity, container and default-chain sources are unaffected. The rule is
`toolkit_aws::preflight::guard_credential_process`, unit-tested on every
target with the platform flag injected, plus a `cfg`-gated test of the
platform flag.

## Decisions and differences

1. **`--metadata` and `--header` take one pair per flag** (repeatable).
   Upstream's `--metadata A=1 B=2` leaves `B=2` as a second positional: a usage
   error, not a silent change. A repeated `--metadata` key keeps its last value.
2. **`--write-timeout`** is accepted and range-checked for compatibility; the
   transport has no separate write timer, and the request (or tool) deadline
   bounds the body write.
3. **`--disable-telemetry`** and multi-profile stay unsupported (usage errors);
   with Q3 the client name is never sent anyway.
4. **Per-request credentials**: as upstream (a new boto3 session per request),
   each attempt runs the guarded source, so a `credential_process` or
   assume-role runs per request. Caching is a possible follow-up.
5. **`readOnlyHint`** must be JSON `true`; truthy strings or numbers hide the
   tool (fail closed).
6. **Process-level tests** re-run the test binary as a child that calls the
   production `serve` over its real stdin, stdout and stderr pipes, with only
   the transport (and, in the default build, the credential source) replaced.
   The real `toolkit` binary is driven for every start-up refusal
   (`tests/mcp_proxy_cli.rs`); a live MCP exchange through it needs network.
7. **T-HDR on the wire** runs in `toolkit-aws` (the S1 loopback harness is
   test-only there): the proxy's request shape, signed by `SigV4Signer`, sent
   by the production connector; the server sees one `Host` equal to
   `McpTarget::authority`, `Content-Length` equal to the body, no
   `Transfer-Encoding`, and the signature re-verifies over the received
   request. The proxy side of the same property (the core hands the transport
   no `Host` or `Content-Length`, the URL is `McpTarget::url`, the body is the
   post-`_meta` body) is T-SIG-4.
8. **T-TLS-21** runs in `toolkit-aws` too: an assume-role profile whose STS
   endpoint (`AWS_ENDPOINT_URL_STS`) is a loopback TLS fixture succeeds with
   the fixture CA in `SSL_CERT_FILE` and fails the handshake, sending nothing,
   with a different valid CA.
9. **The provider-plane recorder** is now a public test double
   (`toolkit_aws::sdk::recorder`), so the end-to-end T-ABS, T-PROV and
   T-LEAK-SDK cases run the real aws-config providers from `toolkit-cli`.
10. **A static test root** (`crates/toolkit-cli/tests/data/mcp_proxy/test-root.pem`,
    a public self-signed certificate; its key was discarded) gives the child
    processes deterministic configured-mode roots.

## Bare `-`

`mcp-proxy` lists no pair in `STDIO_DASH` or `LITERAL_DASH`: a bare `-` is
refused for the endpoint and every value flag (see `cli-bare-dash.md`).

## Repair 1 (S4 review #5105)

The S4 security review requested changes at `9696df86`; all eight findings
are fixed before the stack lands (PM ruling, no carry). Tests are in
`crates/toolkit-cli/src/mcp_proxy/tests/repair.rs` unless named otherwise.

| Finding | Fix | Regression |
| --- | --- | --- |
| #5106 (P1) R-STICKY across sessions | generations, retirement at `initialize` dispatch, generation checks before and after signing | `r_sticky_a_failed_reinitialize_never_sends_the_signed_session_unsigned`, `r_sticky_an_old_session_request_is_not_resent_after_its_session_is_retired` |
| #5107 (P1) unbounded cancel threads | one control worker, queue capped at `max_in_flight`, budget-charged, deadline; per-request admission | `bounded_control_a_batch_needs_one_slot_per_request`, `bounded_control_timeout_cancellations_use_one_bounded_queue` |
| #5108 (P2) `GET` workers across sessions | one `GET` worker, generation-owned streams | `get_stream_one_stream_per_session_generation_and_old_messages_are_dropped` |
| #5109 (P2) id-less `tools/call` | gate every `tools/call` object | `read_only_a_tools_call_without_an_id_is_never_forwarded` |
| #5110 (P2) approvals survive sessions | per-session, versioned snapshots, claim first | `read_only_approvals_do_not_survive_a_new_session`, `read_only_a_fresh_listing_replaces_pages_extend_and_late_listings_are_ignored` |
| #5111 (P2) unbounded retained names | approved names only, charged and capped, fail closed | `read_only_retained_approvals_are_charged_capped_and_released` |
| #5113 (P2) D1 for `GET`/`DELETE` | one shared `open_signed` with D1 for every method; cancels documented best effort | `d1_a_refused_get_is_rebuilt_and_reopened_once`, `d1_a_refused_session_delete_is_rebuilt_and_resent_once` |
| #5112 (P3) unbounded build log | `BuildLog` off by default, opt-in (`with_build_recording`) and capped at 64 | `toolkit-aws` `sdk::source::tests::the_build_log_is_off_by_default_and_bounded_when_enabled` |

## Repair 2 (S4 review #5105, round 2)

Round 2 at `05a51ad9` confirmed that all eight repair-1 findings are
closed. It found three defects that repair 1 itself introduced; all three
are fixed. Tests are in `crates/toolkit-cli/src/mcp_proxy/tests/repair2.rs`.

| Finding | Fix | Regression |
| --- | --- | --- |
| #5136 (P1) retirement held `pending`/`session` across credential I/O | retirement is bookkeeping only; `Auth::initialize_started` runs in the initialize worker with no proxy lock held, ordered by a boundary lock that skips retired or superseded generations | `credential_io_a_held_session_boundary_never_blocks_stdin_deadlines_or_shutdown` |
| #5137 (P2) a retired `GET` delivery survived stdout backpressure | the delivery's cancellation includes its generation's retirement, in budget and stdout waits | `get_stream_a_retired_delivery_waiting_on_stdout_is_dropped_and_frees_the_worker` |
| #5138 (P2) a cancel sent after its deadline when signing was slow | deadline, shutdown and session checked again after signing; transport timeouts capped by the time left; every refusal logs `timeout-cancel-dropped` | `bounded_control_a_cancel_is_checked_again_after_signing` |
