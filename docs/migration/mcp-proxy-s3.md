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
  `annotations.readOnlyHint` is JSON `true`; every listed name is remembered
  with its hint. A `tools/call` for any other tool (including one never listed)
  is answered locally with upstream's text, `Tool '<name>' is not available in
  read-only mode.`, as a `CallToolResult` with `isError: true`, and never sent.
  A batch holding such a call is refused whole (its other requests get
  `-32600`), so nothing of it is sent. The filter trusts annotations: it is a
  convenience filter, not authorization (IAM is).
- **D1 on 401/403**: `SourceAuth::refresh` runs one forced rebuild
  (`GuardedCredentials::resolve(skip_auth, true)`), then the core re-signs and
  resends once. Every attempt resolves credentials once (T-CRED-1).
- **R-STICKY session boundary**: the core calls `Auth::initialize_started`
  before it signs an `initialize`, so the new session starts unsigned-capable
  and the `initialize` itself counts for it.
- **The proxy's own deadline** (tool or request timeout): besides answering
  the client, the core sends one signed `notifications/cancelled` with the
  request id and reason `request timed out`; its response is not read.
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
