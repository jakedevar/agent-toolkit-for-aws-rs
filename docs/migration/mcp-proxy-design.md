# MCP proxy replacement: research and design (#4413, phase 1)

Status: design, revision 2. It answers AC4 security pre-review #4994 (CHANGES_REQUESTED at
`1cf22ef`; findings #4995 to #5003, resolved in section 4 and mapped in section 7) and records
the PM and GM rulings from #4413 (section 6). No code changes. Programme #4369,
slice `mcp`, dependencies #4379 (aws-core boundaries) and #4374 (core). Related: #4582 (real
SigV4 signer and HTTPS transport for `toolkit asm exec`), #4463 (SDK adapter, closed), #4487
(staging a Rust binary inside a plugin), #4414 (cutover), #4415 (retire).

Evidence base, all read and never executed (no Python, `uvx`, proxy or live AWS call was run):

- this repository at `fa3fa13` (the allocated base);
- upstream `aws/mcp-proxy-for-aws` at tag **v1.7.0** (2026-09-15), the version the plugins pin:
  `mcp_proxy_for_aws/{cli,server,proxy,utils,sigv4_helper}.py` and
  `middleware/{initialize_middleware,tool_filter,tool_error_middleware,profile_switcher}.py`, plus
  the repository README. Revision 1 read `sigv4_helper.py` and `utils.py` on `main`. Review
  #4994 re-read both at v1.7.0 and confirmed the behaviour recorded here.
- for revision 2: the pinned dependency sources in the local cargo registry (aws-config 1.13.0,
  aws-smithy-http-client 1.5.1, rustls-native-certs 0.8.5, tokio 1.54.1) at the lines the
  findings cite, all re-checked.
- AWS documentation: *Setting up the AWS MCP Server* and *OAuth 2.1 authentication for AWS MCP
  Server* (Agent Toolkit user guide); the July 2026 "OAuth support for the AWS MCP Server"
  announcement.

There is no vendored copy of the proxy in the repository and no copy in the local `uv` cache.

## 1. AC1 inventory

### 1.1 Plugin runtime configuration: the six files AC5 is about

The six files have one shape. All launch `uvx` with the **same pinned package and endpoint**, the
same `--skip-auth` flag and a per-plugin attribution label. No file sets `env`, `cwd`, `timeout`
or `headers`.

| File | Format | Plugin | Server key | `command` | `args` | Endpoint (fixed) |
| --- | --- | --- | --- | --- | --- | --- |
| `plugins/aws-core/.mcp.json` | Claude Code / Cursor (`.cursor-plugin/plugin.json` → `./.mcp.json`); no `type` means stdio | aws-core | `aws-mcp` | `uvx` | `mcp-proxy-for-aws-cli==1.7.0`, endpoint, `--skip-auth`, `--metadata`, `INSTALL_SOURCE=agent-toolkit-core` | `https://aws-mcp.us-east-1.api.aws/mcp` |
| `plugins/aws-core/mcp.json` | agent-plugins.org 1.0.0 (`"type": "stdio"`) | aws-core | `aws-mcp` | `uvx` | same as above | same |
| `plugins/aws-data-analytics/.mcp.json` | Claude Code / Cursor | aws-data-analytics | `aws-mcp` | `uvx` | same, `INSTALL_SOURCE=agent-toolkit-data-analytics` | same |
| `plugins/aws-data-analytics/mcp.json` | agent-plugins.org 1.0.0 | aws-data-analytics | `aws-mcp` | `uvx` | same, `INSTALL_SOURCE=agent-toolkit-data-analytics` | same |
| `plugins/aws-startup-advisor/.mcp.json` | Claude Code / Cursor | aws-startup-advisor | `aws-mcp` | `uvx` | same, `INSTALL_SOURCE=agent-toolkit-startup-advisor` | same |
| `plugins/aws-startup-advisor/mcp.json` | agent-plugins.org 1.0.0 | aws-startup-advisor | `aws-mcp` | `uvx` | same, `INSTALL_SOURCE=agent-toolkit-startup-advisor` | same |

What these flags mean in v1.7.0 (section 2 has the detail):

- Signing service `aws-mcp` and region `us-east-1`, both parsed from the host
  `aws-mcp.us-east-1.api.aws`.
- `--skip-auth`: **sign when credentials resolve and send unsigned only when none resolve**. It is
  not "never sign". Users with no AWS account can still use the documentation tools this way,
  which is what `kiro-project-init.md` relies on.
- `_meta` on JSON-RPC requests gets `INSTALL_SOURCE=<label>` and an automatically computed
  `AWS_REGION` (section 2.5).
- Defaults: no retries, timeouts 180 s overall, 60 s connect, 120 s read, 180 s write and 300 s
  per tool call, log level `ERROR`, telemetry on, not read-only.

Two other plugin configs are already direct HTTP with no proxy, so they are out of AC5 scope.
They show the existing pattern for option (b).

| File | Plugin | Server | Transport and auth | Endpoint |
| --- | --- | --- | --- | --- |
| `plugins/aws-agents/.mcp.json`, `plugins/aws-agents/mcp.json` | aws-agents | `awsknowledge` | `http` / `streamable-http`, unauthenticated | `https://knowledge-mcp.global.api.aws` |
| `plugins/aws-agents-for-devsecops/.mcp.json` | aws-agents-for-devsecops | `aws-devops-agent` | `http` with `Authorization: Bearer ${DEVOPS_AGENT_TOKEN}`, client timeout 120 000 ms | `https://connect.aidevops.us-east-1.api.aws/mcp` |

### 1.2 Other launch references (shipped prose that a user or agent copies into a config or runs)

| File | Lines | What it launches | Endpoint / signing | Plugin |
| --- | --- | --- | --- | --- |
| `README.md` | 115-121, 146, 156-163; "You need uv installed" prerequisites at 137, 178, 190, which also state that credentials are not needed for documentation search (the `--skip-auth` behaviour) | Kiro JSON, `fx mcp add` command and fx JSON: `uvx mcp-proxy-for-aws-cli@latest <ep> --metadata AWS_REGION=us-west-2` | aws-mcp us-east-1; region metadata `us-west-2` | repo root |
| `plugins/aws-agents-for-devsecops/skills/setup-devops-agent/SKILL.md` | 46, 55, 118, 122, 170-180, 191-200, 259, 293 | stdin-piped `initialize`/`tools/list` check through `uvx mcp-proxy-for-aws-cli@latest https://connect.aidevops.${REGION}.api.aws/mcp --service aidevops --region $REGION`; generated SigV4 config (`--service aidevops --region <REGION>`, timeout 120 000); aws-mcp fallback config (`--metadata AWS_REGION=us-east-1`, timeout 100 000); `uvx` install and troubleshooting rows | aidevops (explicit service and region) and aws-mcp | aws-agents-for-devsecops |
| `plugins/aws-agents-for-devsecops/README.md` | 90 | auth table: "Local signing proxy via `mcp-proxy-for-aws-cli`" | aidevops | aws-agents-for-devsecops |
| `plugins/aws-startup-advisor/skills/operate-on-aws/references/connecting.md` | 119-147, 293 | SigV4 config `uvx mcp-proxy-for-aws@latest https://connect.aidevops.{region}.api.aws/mcp --service aidevops --region {region}` (note: the **library** package name, not `-cli`); "Requires `uvx` (Python)" | aidevops | aws-startup-advisor |
| `plugins/aws-startup-advisor/skills/operate-on-aws/SKILL.md` | 451-452 | "SigV4 through `mcp-proxy-for-aws`" | aidevops | aws-startup-advisor |
| `plugins/aws-startup-advisor/README.md` | 51 | describes `aws-mcp` as launched through `uvx mcp-proxy-for-aws-cli==1.7.0` | aws-mcp | aws-startup-advisor |
| `plugins/aws-startup-advisor/skills/prompt-library-for-startups/references/prompt-library/kiro-project-init.md` | 350-353, 440, 484-486 | Kiro config `uvx mcp-proxy-for-aws-cli@latest`; `timeout 15s uvx … --skip-auth` smoke test; `uv cache clean mcp-proxy-for-aws-cli` | aws-mcp, `--skip-auth` | aws-startup-advisor |
| `plugins/aws-startup-advisor/skills/prompt-library-for-startups/references/prompt-library/mvsp.md` | 351 | `uvx mcp-proxy-for-aws-cli@latest <ep> --skip-auth` | aws-mcp | aws-startup-advisor |
| `skills/specialized-skills/end-user-computing-skills/amazon-workspaces-agent-access/references/connection-setup.md` | 68-110 | customer **Python library** code (`aws_iam_streamablehttp_client`, service `agentaccess-mcp`, extra header `X-Amzn-AgentAccess-Streaming-Session-Url`, `metadata=` SAML `_meta`), and "run via `uvx mcp-proxy-for-aws`" | `agentaccess-mcp.{region}.api.aws` | skills catalog |
| `…/amazon-workspaces-agent-access/SKILL.md`, `references/session-lifecycle.md`, `references/tools-reference.md` | SKILL 11-13, 51; lifecycle 23; tools 3 | prose: proxy signs, issues the session `DELETE` on clean exit and handles the `agentaccess___` tool prefix | agentaccess-mcp | skills catalog |
| `plugins/aws-core/skills/aws-secrets-manager/SKILL.md` | 104 | states that asm-exec does *not* start the proxy (no launch) | — | aws-core |
| `plugins/aws-core/skills/aws-secrets-manager/references/asm-exec` | 109 | Python helper that mirrors the proxy's host parsing; owned by #4451/#4582 | aws-mcp | aws-core |
| `crates/toolkit-cli/src/asm_exec/mcp.rs` | 45 | Rust port of the same parsing (`signing_service_region`) | — | — |

Not proxy launches, recorded so that AC1 is closed:
`skills/core-skills/launch-with-aws/SKILL.md:134` and its aws-core mirror pass an aws-mcp URL to
`launch_with_aws.py resolve-region` (owned by #4382).

### 1.3 Other `uvx` and `uv` references that the manager ruling attached to #4413

The PM ruling on #4413 (handoff 1) adds `uvx iam-policy-autopilot@latest` and the prose `uvx`
prerequisites. They are not the MCP proxy, so they are inventoried here and planned as their own
slice (S6).

| File | Count | Reference |
| --- | --- | --- |
| `plugins/aws-core/skills/aws-iam/references/aws-iam-policy-generation.md` and mirror `skills/core-skills/aws-iam/references/aws-iam-policy-generation.md` | 16 lines with `uvx` and 15 with `iam-policy-autopilot` in each copy (`grep -c` at `fa3fa13`; the PM ruling counts 22 occurrences) | `uvx iam-policy-autopilot@latest generate-policies`, "You MUST verify `uvx` is available" |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/SKILL.md:94-97`, `heroku-to-aws/SKILL.md:100-104` | 2 + 2 | `uvx --version` gate for the llm-to-bedrock and agent-advisor scripts (owned by those script slices: #4403-#4405, #4409, #4611) |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/phases/generate/generate-assemble.md:48`, `agents/generic-phase-worker-rwx.md:26,43` | 1 + 2 | `python3`/`uvx` checker and `Bash(uvx:*)` permission prose (owned by the startup slices) |
| `crates/toolkit-cli/tests/ci_gates.rs:266` | 1 | the CI gate that bans `uvx` in recipes; keep it |

## 2. What the proxy does (v1.7.0, from source)

### 2.1 Architecture

`mcp-proxy-for-aws-cli` is a pinned-dependency wrapper around the `mcp-proxy-for-aws` library. It
is a **FastMCP re-serving proxy**, not a byte forwarder:

- The local side is a FastMCP stdio server: `FastMCPProxy(...).run_async(transport='stdio')`.
- The upstream side is one cached `AWSMCPProxyClient`, a `StatefulProxyClient` over FastMCP's
  `StreamableHttpTransport` with a signing `httpx` client.
- It has a middleware chain:
  - `InitializeMiddleware`: on the client's `initialize`, connects upstream and copies the
    backend's `capabilities` and `instructions` into the local `InitializeResult`. Kiro CLI and
    Q Dev CLI are excluded from this eager connect because of a JSON-RPC error bug.
  - `ToolFilteringMiddleware` (`--read-only`).
  - `ToolErrorMiddleware` (tool timeout and error text).
  - `ProfileOverrideMiddleware`: only with two or more profiles.
  - `RetryMiddleware`: only with `--retries > 0`.
  - `LoggingMiddleware`: payloads logged at `DEBUG`.
- Upstream connect retries up to 3 times (`max_connect_retry=3`), forcing a disconnect each time,
  on `RuntimeError`. An HTTP error whose body is a JSON-RPC error becomes `McpError`. There is no
  explicit 404 (expired session) handling.
- Server-to-client traffic (notifications, progress, sampling, elicitation, roots) is forwarded
  by FastMCP's `StatefulProxyClient`. The MCP streamable-HTTP client also opens the optional
  `GET` SSE stream and sends `DELETE` on close. `sigv4_helper._handle_error_response` downgrades
  "405 on GET/DELETE" and "404 on POST" to `DEBUG` as expected session behaviour. The Amazon
  WorkSpaces skill depends on that `DELETE` (`X-Amzn-AgentAccess-Expire-Streaming-Session-On-Delete`).

### 2.2 Transport

- Client side: stdio, newline-delimited JSON-RPC (the MCP stdio transport).
- Upstream: MCP Streamable HTTP. Each JSON-RPC message is a `POST` to the endpoint with
  `Accept: application/json, text/event-stream`.
  - The reply is either `application/json` or an SSE stream (`text/event-stream`) carrying one or
    more JSON-RPC messages.
  - `Mcp-Session-Id` comes from the `initialize` response and is sent on later requests.
  - An optional long-lived `GET` SSE stream carries server-initiated messages.
  - `DELETE` ends the session.
- The client stack does not use legacy HTTP+SSE.

### 2.3 SigV4 signing (`sigv4_helper.py`)

- botocore `SigV4Auth(credentials, service, region).add_auth(AWSRequest(method, url, data=body,
  headers))`. This uses header signing with the payload SHA-256 computed by botocore.
  `x-amz-security-token` is added for temporary credentials.
- It runs as the **last** `httpx` request hook, so metadata injection is covered by the signature.
  `Content-Length` is set before signing and `connection` is dropped from the signed headers.
- **Credentials are resolved per request**: `boto3.Session(profile_name=profile).get_credentials()`
  runs on every request, so account switches and refreshes apply immediately. This uses the
  standard botocore chain: env, shared credentials and config including SSO, `aws login`,
  `credential_process`, assume-role and web identity, container, then IMDS.
- Two botocore patches are applied at import:
  - `credential_process` children get `stdin=DEVNULL`, so they cannot read the MCP stdio pipe.
  - The assume-role session name is fixed to `mcp-proxy-for-aws-<epoch>` for the life of the
    process.
- No credentials found:
  - with `--skip-auth`, the request is sent unsigned (logged at debug level);
  - otherwise `ValueError` "… configure credentials or use `--skip-auth`".
  - Errors from a provider are raised, not downgraded to unsigned.
- Service: `--service`, else parsed from the host (`utils.get_service_name_and_region_from_endpoint`):
  - `…bedrock-agentcore.<region>.amazonaws.com` gives `bedrock-agentcore`;
  - `<service>.<region>.api.aws` gives `<service>`;
  - otherwise the first label.
  - Empty gives `ValueError`.
- Signing region: `--region`, then the region parsed from the endpoint, then `AWS_REGION`. None
  gives `ValueError`.

### 2.4 Endpoint validation

`validate_endpoint_url` accepts any `https://` URL. It also accepts `http://` to
`localhost`/`127.0.0.1`/`::1`. **There is no domain allowlist.** The endpoint comes only from the
command line, never from MCP traffic. `follow_redirects=False`;
`httpx.Limits(max_keepalive_connections=1, max_connections=5)`. TLS uses `httpx` defaults:
`verify=True` with certifi, and `trust_env`, so `HTTPS_PROXY`, `SSL_CERT_FILE` and
`SSL_CERT_DIR` are honoured.

### 2.5 Headers and metadata

- Fixed headers:
  - `Accept: application/json, text/event-stream`;
  - `User-Agent: python-httpx/<v> mcp-proxy-for-aws/<v>`, plus `<client name>/<client version>`
    from `initialize.clientInfo` unless `--disable-telemetry`.
- `--header K=V` (repeatable) adds headers that are signed and registered as sensitive (redacted
  in logs). `authorization`, `date`, `x-amz-date` and `x-amz-security-token` are refused.
- `--metadata K=V…` is merged into `params._meta` of every JSON-RPC body that has a `params`
  object. Existing keys win, a conflict logs a warning, and parse errors are skipped.
- `AWS_REGION` is added automatically from `determine_aws_region` and user metadata overrides it.
  The order is:
  1. the boto3 session's region (profile config);
  2. `AWS_REGION`;
  3. the endpoint's region.

  The server uses this metadata value as the default region for AWS operations. Without it, the
  server defaults to `us-east-1`.

### 2.6 Retries, timeouts, errors

- `--retries 0..10` (default 0) uses FastMCP `RetryMiddleware` with exponential backoff. It wraps
  whole MCP operations, **including `tools/call`**.
- Timeouts: `httpx.Timeout(180, connect=60, read=120, write=180)`. `read` is a per-chunk idle
  timeout, so a long SSE stream survives while bytes keep arriving. `--tool-timeout 300` wraps each
  `tools/call` in `anyio.fail_after`.
- Error mapping (`ToolErrorMiddleware`):
  - any tool-call failure becomes `ToolError("Tool call '<name>' failed: <e>. Please retry.")`.
    The raw upstream text is included.
  - a 401/403 or a credentials `ValueError` gives a fixed message: "failed due to expired or
    invalid AWS credentials. Please refresh your credentials and retry. The proxy will
    automatically use the new credentials on the next request."
  - FastMCP turns a `ToolError` into a `CallToolResult` with `isError: true`.
- HTTP error responses are logged at `WARNING` **with the response body**. This is not
  redacted.
- Process exit: `main()` returns 1 on a launch error, but `__main__` does not `sys.exit` it.

### 2.7 Read-only and tool filtering

`--read-only` is the only tool filter:

- `on_list_tools` keeps only tools whose `annotations.readOnlyHint` is truthy. A tool without the
  hint is hidden.
- `on_call_tool` refuses others with `Tool '<name>' is not available in read-only mode.` The
  check is skipped when the FastMCP context is absent.

Multi-profile (`--profile A B …` or `AWS_MCP_PROXY_PROFILES`) adds an `aws_profile` enum parameter
to five auth-requiring tools. It routes calls to per-profile clients and validates the profile
name against the configured set. **No file in this repository uses `--read-only`, `--profile` or
multi-profile.**

### 2.8 Logging

Logging goes to stderr. Default level is `ERROR`; the README says `INFO`, but the code governs.
`authorization`, `x-amz-security-token`, `x-amz-date` and `--header` values are redacted. At info
level `_meta` contents are logged, and at debug level whole payloads. Upstream error bodies are
logged at `WARNING`.

## 3. Options

### (a) Rust `toolkit mcp-proxy`

This is a stdio ↔ Streamable HTTPS bridge with SigV4, CLI-compatible with the
`mcp-proxy-for-aws-cli` flags this repository uses. It reuses what exists:

- `toolkit_aws::sdk::{SdkCredentialsProvider, SigV4Signer, SdkTransport}` (#4463, closed):
  - SDK default credential chain;
  - `aws-sigv4` header signing, a fresh signature per attempt, `x-amz-security-token`;
  - rustls (aws-lc) HTTPS with no redirects, no proxy, https only, and a connect-time public-DNS
    guard plus a peer-address check (#4593);
  - redaction by failure kind (#4594).
- `validate_endpoint` with `EndpointPolicy::aws_public()`.
- `redact_text`, `Headers` (header-injection refusal, redacting `Debug`), `Timeouts` and
  `RetryPolicy`.
- `toolkit_core::parse_json` with `MAX_JSON_DEPTH = 128`, after S0's linear-work duplicate check
  and node cap (section 4.5, #4497).
- `asm_exec::mcp::signing_service_region`: the same host rules as upstream, already ported. It
  moves to `toolkit-aws` so asm exec and the proxy share it.

It needs some new pieces:

- a **streaming** response seam: the current `Transport` buffers the whole body under one read
  timeout, which cannot carry a multi-minute SSE stream;
- the stdio loop, the session handling and the SSE parser;
- a `toolkit-cli` cargo feature that turns on `toolkit-aws/sdk`.

The same signer and transport wiring is what #4582 needs for asm exec. The two should land
together or share one slice.

Trade-offs:

- (+) It keeps today's authentication behaviour exactly: SigV4 from the local chain, the
  `--skip-auth` unsigned fallback, `AWS_REGION`/`INSTALL_SOURCE` metadata, and explicit
  `--service`/`--region` for aidevops.
- (+) It keeps the non-OAuth endpoints working: aidevops and agentaccess-mcp support SigV4 or a
  bearer token, not OAuth. It also keeps `--read-only` and multi-profile as options.
- (+) It is tighter than upstream: an AWS-domain allowlist, a public-address guard, size bounds
  and no body logging.
- (−) It adds new security-sensitive code: a long-lived network bridge holding credentials.
- (−) The SDK feature adds build cost: aws-config, hyper, rustls, tokio.
- (−) **It needs a distribution channel for the `toolkit` binary, and none exists**
  (`docs/migration/telemetry.md` "Cutover"; same blocker for msk, bedrock-report and #4487).
  Until there is one, switching the six files to `toolkit` breaks the MCP server for every
  plugin user. The GM decided the channel (Q1, section 6.1): per-plugin staged `./bin` binaries
  from the pipeline shared with #4487, which S5 waits on.

### (b) Direct remote endpoint, no proxy

Since July 2026 the AWS MCP Server supports OAuth 2.1 through AWS Sign-in. Clients connect to
`https://aws-mcp.<region>.api.aws/mcp` directly with `type: http`.

- Claude Code, Kiro CLI 2.11+, Devin and Claude Code for Web take the plain URL.
- Cursor, Claude Desktop, Kiro IDE, VS Code, Gemini CLI, Codex and Copilot need
  `?oauth=initialize`.
- Prerequisite IAM: `signin:AuthorizeOAuth2Access` and `signin:CreateOAuth2Token`
  (`AWSMCPSignInOAuthAccessPolicy`).
- Tokens: 1 h access tokens, refresh up to 12 h.
- A headless `CreateOAuth2TokenWithIAM` path gives a bearer token with no refresh.
- No current MCP client signs SigV4 natively (no first-party evidence was found).

Trade-offs:

- (+) No local binary, no Python, nothing to distribute. The plugin config is a URL. It is the
  smallest change and AWS-supported.
- (−) It **changes authentication**, which conflicts with AC3:
  - from local credentials to a browser consent flow with new IAM permissions that some
    organisations block;
  - `--skip-auth` unsigned docs access probably goes away. Whether the server serves
    unauthenticated `initialize` and documentation tools to a plain HTTP client is not
    documented; it needs a live probe.
  - no `AWS_REGION` default-region metadata, so operations default to `us-east-1`;
  - no `INSTALL_SOURCE` attribution (`_meta` cannot be set from a URL config);
  - no read-only mode and no multi-profile, both of which AWS lists as SigV4-only.
- (−) It covers only aws-mcp. aidevops and agentaccess-mcp have no OAuth. The devops plugin
  already uses a bearer token, but the documented SigV4 alternative in
  `setup-devops-agent`/`connecting.md` would have no replacement.
- (−) One `.mcp.json` is shared by Claude Code and Cursor (`.cursor-plugin/plugin.json` →
  `./.mcp.json`), and they disagree on `?oauth=initialize`.

### (c) Another non-Python, non-`uvx` distribution of the proxy

- **Docker** `public.ecr.aws/mcp-proxy-for-aws/mcp-proxy-for-aws` is AWS-published. It still runs
  Python inside the container (AGENTS.md: no Python execution) and it needs Docker plus credential
  mounts (`~/.aws`, env) into the container. AWS documents no non-Python build.
- **Third-party ports**: `ajbeck/go-aws-mcp-proxy` (Go, early) and npm `aws-sigv4-mcp-proxy` (needs
  Node ≥ 20). Their maintainers are unverified and they would handle credentials.

Trade-offs: no code to write, but either AC5's intent fails (Python in a container) or the
credential boundary is handed to an unreviewed third party. A pinned upstream binary would need
the same distribution work as (a) anyway. **Rejected.**

### Recommendation: (a), with (b) documented as the zero-install alternative

The six plugin configs and the shipped SigV4 configs move to `toolkit mcp-proxy`, with the same
arguments minus the package spec. The README and `connecting.md` gain an OAuth section for aws-mcp.

- **AC2** (Rust or supported direct replacement passes integration fixtures): `toolkit
  mcp-proxy` is driven end to end by offline fixtures (section 5). Option (b) has nothing local
  to test beyond config shape. Option (c) is not Rust and not testable offline.
- **AC3** (retain authentication behaviour): only (a) keeps SigV4 from the local chain, the
  `--skip-auth` fallback, the default-region metadata, explicit `--service`/`--region` for
  aidevops, and header-carried session credentials for agentaccess. Option (b) swaps in a
  different auth model with different IAM prerequisites, and only for aws-mcp.
- **AC5** (no `uvx` runtime dependency): (a) and (b) both meet it. (c) moves Python into Docker
  or to a third party.

AC5 cannot be met in shipped configs before the binary is distributed. The GM decided Q1:
per-plugin staged `./bin` binaries through the pipeline shared with #4487 (section 6.1). The plan
sequences the cutover (S5) behind that pipeline and the operator's signed-run to-do.

## 4. Credential and network boundary (for the AC4 pre-review)

Scope: `toolkit mcp-proxy` is one process launched by an MCP client, with stdio as its only
input and output. It **listens on no socket**.

Revision 2 answers pre-review #4994 (CHANGES_REQUESTED at `1cf22ef`). Each of its nine
findings, #4995 to #5003, is resolved by a named requirement (`R-…`) below. Section 7 maps each
finding to its requirement, its tests and the slice that owns it.

### 4.0 Two planes, and which guarantees apply to each (#5003)

The guarantees in this section belong to one of two planes. They are not process-wide claims.

- **MCP forwarding plane.** This covers every HTTP request the proxy makes to carry MCP
  messages: `POST`, `GET` and `DELETE` to the endpoint. It goes through `SdkTransport` and its
  streaming extension. The fixed endpoint (4.4), signed-headers-only credential flow (4.2),
  public-address DNS and peer guard, header ownership (R-HDR) and size bounds (4.5) apply to this
  plane.
- **Credential provider plane.** This covers the requests aws-config's credential providers
  make while resolving credentials (STS, SSO, SSO-OIDC, AWS Sign-in, the ECS/EKS container
  endpoint, IMDS) and the `credential_process` child. These use aws-config's own SDK clients and
  HTTP client, not `SdkTransport`. Section 4.1.2 sets the rules for this plane. The MCP plane's
  AWS-suffix policy and public-address guard **do not apply here**; the metadata endpoints are
  link-local by design.

### 4.1 Credentials read, and how

| Source | How | Notes |
| --- | --- | --- |
| SDK default chain (`SdkCredentialsProvider::default_chain`, `sdk.rs:144-151`) | env (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`); shared `config`/`credentials` (profile from `--profile` or `AWS_PROFILE`, validated, #4473); the SSO token cache; the `aws login` session (`login_session`, which needs aws-config's `credentials-login` feature, enabled in S1 by R-LOGIN, #4997); `credential_process`; assume-role; web identity; ECS/EKS container role; IMDS | Same sources as botocore. The chain is built only from the startup environment and files (R-PROV). The profile selects **credentials only**; the signing region follows the section 2.3 rules |
| `--header K=V` values | argv | Treated as secrets: the agentaccess streaming URL is a bearer credential |
| nothing else | — | No flag accepts a key or token (`reject_credential_arguments`) |

**Files.** The proxy writes no file. The SDK's SSO and login providers may rewrite their own
token caches (`~/.aws/sso/cache`, `~/.aws/login/cache`, or `AWS_LOGIN_CACHE_DIRECTORY`) when they
refresh, as the AWS CLI and botocore do.

**Decisions:**

- **D1. Refresh.** Upstream re-reads disk on every request. The SDK chain caches credentials and
  refreshes them before expiry. We keep the SDK cache and add one forced rebuild of the provider
  chain, before failing, when either happens:
  - a 401/403 response;
  - a credential failure at precedence 2 or 3 (R-PREC).

  A user who runs `aws sso login` or `aws login` mid-session therefore recovers without a
  restart, as with upstream. This includes switching from an absent configuration to a
  configured one, because the cached profile view is rebuilt. Assumption: an account switch made
  by editing the profile, without the old credentials expiring, applies after the next auth
  failure rather than immediately. This is an intentional difference, accepted under Q5.
- **D2. `credential_process` stdin: mandatory null stdin.** Review #4994 confirmed the child
  inherits the MCP stdin:
  - aws-config 1.13.0 `credential_process.rs:112-114` uses tokio `Command::output`;
  - tokio 1.54.1 `process/mod.rs:1065-1071` forces only stdout and stderr to pipes.

  S1 must therefore ship a null-stdin mechanism. The workspace forbids `unsafe`, so S1 picks one
  of these:
  1. At startup, move the MCP stdin to a private descriptor and put the null device on fd 0
     through a reviewed safe wrapper, for example `rustix::stdio::dup2_stdin`. This is a new
     dependency, and S4 reviews it.
  2. Replace the profile chain's `credential_process` step with a toolkit provider that spawns
     with `Stdio::null()`.

  Option 1 is preferred because it also covers every future child. Test T-CRED-4 uses a Rust
  fixture executable that reads stdin and must see EOF at once.
- **D3. `--skip-auth`.** Replaced by R-ABS and R-PREC (4.1.1). The revision-1 rule "only
  `NotLoaded` downgrades" was insufficient (#4995).
- **D4. Role session name.** The SDK default is used. Upstream's `mcp-proxy-for-aws-<epoch>`
  CloudTrail label is not reproduced (Q5, accepted).
- **R-LOGIN (#4997).** S1 adds `credentials-login` to the `aws-config` features of
  `toolkit-aws`'s optional `sdk` feature (`crates/toolkit-aws/Cargo.toml:28` today has `rustls`,
  `rt-tokio`, `sso` and `credentials-process`).
  - **Why:** without it, aws-config 1.13.0 `profile/credentials/exec.rs:121-140` turns a
    `login_session` profile into `ProfileFileError::FeatureNotEnabled`. The chain maps that to
    `InvalidConfiguration`, so `aws login` users would fail closed even for documentation tools,
    which would regress AC3.
  - **Cost:** the feature adds `aws-sdk-signin` and its crypto dependencies (`p256`, `sha2`,
    `rand`, `zeroize`, …), and no other service SDK. That keeps service dependencies scoped
    (AGENTS.md).
  - **Fetch risk:** `aws-sdk-signin` is not in `Cargo.lock` at `fa3fa13`. If S1's daemon build
    cannot fetch it, that is a technical impasse to report, not a reason to drop the source.
  - **Scope:** asm exec gets the same source, which is the parity it also needs.
  - Proposed ruling R-6 (section 6).

#### 4.1.1 `--skip-auth`: unsigned only on semantic absence (#4995)

The SDK's `CredentialsNotLoaded` does not mean "nothing is configured". At aws-config 1.13.0 it
is returned in three cases that do not mean absence:

- an access key without a secret (`environment/credentials.rs:27-39,111-115`);
- a missing named profile (`profile/credentials.rs:186-188`);
- an incomplete SSO token configuration (`TokenProviderConfig`, same lines).

After any of these the default chain tries the next provider (`meta/credentials/chain.rs:104-116`).
It can then end with **a different identity**, such as an instance role, or with `NotLoaded`.
`toolkit-aws` maps `NotLoaded` to `AwsError::NoCredentials` (`sdk.rs:120-128`,
`binding.rs:72-80`). The SDK result alone therefore cannot decide whether to send unsigned.

**R-ABS. Credential-source preflight.** Before every credential load (each attempt, including
the D1 rebuild), the proxy classifies the configuration. The preflight reads only:

- whether each listed environment variable is set and non-blank;
- the selected profile name;
- the **key names**, never the values, of the selected profile in the shared `config` and
  `credentials` files.

The files are parsed with aws-config's own public profile parser (`aws_config::profile::load`),
so file semantics match the chain. It never reads, matches or logs provider error text. It gives
one of three classes:

| Preflight class | Condition | Credential result → action |
| --- | --- | --- |
| **Invalid** | Any of: exactly one of `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (or `SECRET_ACCESS_KEY`) is non-blank; `AWS_SESSION_TOKEN` is set without both; exactly one of `AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN` is set; an **explicitly selected** profile (`--profile`, else non-blank `AWS_PROFILE`) exists in neither file; `AWS_CONFIG_FILE` or `AWS_SHARED_CREDENTIALS_FILE` names a path that does not exist; a profile file exists but cannot be read or parsed | **Fail closed** with the fixed credentials message. The chain is **not called**, so it cannot fall through to another identity. This applies with and without `--skip-auth` |
| **Configured** | Not Invalid, and any of: a complete static env pair; both web-identity variables; `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` or `AWS_CONTAINER_CREDENTIALS_FULL_URI`; the selected profile has any credential-bearing key (`aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`, `role_arn`, `source_profile`, `credential_source`, `credential_process`, `web_identity_token_file`, `sso_session`, `sso_start_url`, `sso_account_id`, `sso_role_name`, `login_session`) | Chain success: sign. **Any** chain error, `NotLoaded` included, fails closed (D1 rebuild first). Never unsigned |
| **Absent** | Neither of the above. No credential environment variable is set, and the selected profile is either the implicit `default` and missing, or exists (default or explicit) with no credential-bearing key | Chain success (IMDS on an EC2 host): sign. `NotLoaded`: with `--skip-auth` send **unsigned**, otherwise the `NoCredentials` message. Any other error: fail closed |

**Parity with upstream.** botocore raises on an access key without a secret
(`PartialCredentialsError`) and on a missing named profile (`ProfileNotFound`). It returns no
credentials for a profile without credential keys. The table matches all three. The preflight is
stricter than botocore in four cases that botocore ignores and falls through:

- a secret key without an access key id;
- a session token without both keys;
- one half of the web-identity pair;
- a config-file variable that names a missing file.

These are refused, as the review requires for partial configuration. Proposed ruling R-1
(section 6).

**R-PREC. Error precedence.** Each MCP request makes at most two credential attempts: the
load, then D1's one rebuild. Precedence, highest first:

1. an Invalid preflight;
2. a non-`NotLoaded` SDK error (`ProviderTimedOut`, `ProviderError`, `InvalidConfiguration`,
   unhandled), or `CredentialsExpired`/`PartialCredentials` from `adopt_credentials`;
3. `NotLoaded` under a Configured preflight;
4. `NotLoaded` under an Absent preflight.

How a request resolves:

- A successful attempt signs the request.
- Otherwise the request fails with the highest-precedence failure seen on **any** of its
  attempts.
- Unsigned is possible only when **every** attempt in the request ended at level 4. A rebuild
  can therefore never turn an earlier level 1-3 failure into an anonymous request. For example,
  if the first attempt was Configured and the user deletes the configuration before the
  rebuild, the request still fails closed.
- Only levels 2-3 and 401/403 trigger the D1 rebuild. Level 1 needs the user to fix the
  configuration, and level 4 has nothing to refresh.

**R-STICKY. A signed session never silently becomes anonymous.** Once any request in the
current `Mcp-Session-Id` session has been signed, a later level-4 outcome fails closed with the
credentials message, until the session ends. Upstream would continue unsigned. The other
direction is allowed: an unsigned session may start signing when credentials appear, as
upstream does. Proposed ruling R-2.

**Offline verification, T-ABS (S1 preflight unit tests; S3 end to end).**

Unit tests cover every row and every listed key name, through the preflight's pure function
over an injected environment map and file texts.

End-to-end cases run the **real** `SdkCredentialsProvider::default_chain_with_http` (4.1.2) set
up as follows:

- one subprocess per case, because the SDK reads the process environment;
- `env_clear()`, with `HOME`, `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` pointing at
  synthetic files in a temp dir;
- `AWS_EC2_METADATA_DISABLED=true`, except in the IMDS case;
- the T-PROV recorder as the provider HTTP client.

| Case | Expected |
| --- | --- |
| Genuine absence: no env, no files | One request to the fake MCP transport, unsigned (no `authorization`) |
| Absence with a `default` profile that holds only `region` | Unsigned |
| IMDS enabled; the recorder refuses the IMDSv2 token `PUT` as a dispatch failure | `NotLoaded`, unsigned; the recorder saw only the IMDS token request |
| `--profile` typo, `AWS_PROFILE` typo | No MCP transport call; credentials message |
| Access key without secret **while a valid static `default` profile exists** | No transport call. This proves there is no fall-through to the profile identity |
| Session token alone; secret alone; web-identity half pair; `AWS_CONFIG_FILE` naming a missing file | No transport call |
| Profile `sso_session` naming a missing section (`TokenProviderConfig`) | No transport call |
| Profile `credential_process` fixture that exits 1 | No transport call |
| `credential_process` fixture returning an `Expiration` in the past | `CredentialsExpired`; no transport call |
| `role_arn` + `source_profile` profile whose STS call the recorder answers with a timeout connector error | Provider failure; no transport call |
| Precedence: first attempt Configured with a provider error, then the rebuild finds Absent | Fails closed, not unsigned |
| Precedence: first attempt Configured `NotLoaded`, then the rebuild succeeds | Signed |
| R-STICKY: a signed request, then the configuration is removed, then a level-4 outcome | Fails closed |

Every case runs with `--skip-auth`, and the fail-closed cases also run without it. A bare stderr
canary is asserted absent (T-LEAK-SDK). The fake `NotLoaded`/`Provider` variant tests from
revision 1 remain as unit tests only. They are not evidence for this property.

#### 4.1.2 Credential provider plane: egress and trust (#5003)

| Provider | Egress | Notes |
| --- | --- | --- |
| Environment, static profile keys | none | — |
| `credential_process` | a local child: the user's configured command, with null stdin (D2) | A trusted local extension. Its own I/O and network use are outside the proxy's control, as with the AWS CLI |
| SSO (`sso_session` or legacy keys) | HTTPS to the SSO portal and SSO-OIDC regional endpoints (role credentials, token refresh); the cached token is sent as a bearer token | The SDK SSO and SSO-OIDC clients |
| `aws login` (`login_session`) | HTTPS to the AWS Sign-in service for token refresh (DPoP-bound) | Needs R-LOGIN |
| Assume-role (`role_arn` + `source_profile`/`credential_source`) | HTTPS STS `AssumeRole`, signed with the source credentials (`exec.rs:43-54` builds its own STS client) | — |
| Web identity | HTTPS STS `AssumeRoleWithWebIdentity`; the token is in the operation payload (`web_identity_token.rs:269`) | — |
| ECS/EKS container | plain HTTP to the link-local container endpoint (`169.254.170.2`; EKS Pod Identity `169.254.170.23` / `fd00:ec2::23`) or `AWS_CONTAINER_CREDENTIALS_FULL_URI`, with `AWS_CONTAINER_AUTHORIZATION_TOKEN[_FILE]` | aws-config's own host checks for the full URI |
| IMDS | plain HTTP to `169.254.169.254` (or `AWS_EC2_METADATA_SERVICE_ENDPOINT`): an IMDSv2 token `PUT`, then a `GET` | Off with `AWS_EC2_METADATA_DISABLED` |

**How the provider plane behaves:**

- These requests use aws-config's own clients. They do **not** go through `SdkTransport`. They
  are not subject to the MCP plane's AWS-suffix policy, public-address DNS guard or peer check,
  and they cannot be: IMDS and the container endpoints are link-local.
- HTTPS calls on this plane are verified by the SDK's default rustls client (aws-config's
  `rustls` feature turns on `default-https-client`). Its native-root behaviour is the same as in
  4.3.
- Provider-plane token exchanges send tokens to their own AWS endpoints, as the SDK designs.
  This is outside the "credentials leave only as signed headers" guarantee, which is scoped to
  the MCP plane (4.2).
- **Who controls it:** only trusted launch configuration. That means:
  - the process environment;
  - the AWS `config` and `credentials` files;
  - the SSO and login caches;
  - any service-endpoint overrides aws-config honours (`AWS_ENDPOINT_URL_*`, profile
    `endpoint_url`, `AWS_EC2_METADATA_SERVICE_ENDPOINT`).

  This is the trust a user already gives the AWS CLI. The proxy documents it and does not narrow
  it, because narrowing it would break the normal chain.

**R-PROV. MCP traffic cannot reach the provider plane.**

- **Inputs:** the provider chain is built only from the startup environment and files, at start
  and on each D1 rebuild. No stdin message, server response, response header, SSE field or
  `_meta` value is passed to a provider builder, an environment variable or a file.
- **No environment mutation:** the proxy never mutates its environment. A source gate bans
  `std::env::set_var` and `std::env::remove_var` in the proxy module and in `toolkit-aws`.
- **One provider-HTTP seam:** S1 adds `SdkCredentialsProvider::default_chain_with_http(runtime,
  config, http_client)`, and `default_chain` calls it with the SDK default HTTPS client.
  `ProviderConfig::with_http_client` is public in aws-config 1.13.0. The IMDS, ECS, STS, SSO,
  SSO-OIDC and Sign-in clients are all built from that `ProviderConfig`
  (`default_provider/credentials.rs:181-195`), so this one injection point covers every provider
  HTTP request.

**Offline verification, T-PROV (S1 seam; S3 end to end).**

- **Recorder.** Every T-ABS, T-CRED and T-LOGIN case injects a recording `HttpClient` through
  the seam. The recorder answers from fixtures or refuses with a dispatch failure. It records
  method, host and path, never headers or bodies.
- **Destinations.** Each test asserts the exact destination list:
  - empty for env and static cases;
  - the IMDS token `PUT` for the IMDS case;
  - STS for assume-role;
  - Sign-in for a login refresh.
- **Source gate.** `DefaultCredentialsChain::builder` is called only inside
  `default_chain_with_http`, which always calls `.configure(…with_http_client(…))`.
- **R-PROV case.** Client messages and server responses carry `AWS_PROFILE`, `AWS_REGION`,
  `role_arn` and URLs in `_meta`, in headers and in SSE fields. The test then forces a D1
  rebuild. It asserts that the chain-builder inputs (profile name, region) equal the startup
  values and that the recorder's destination list is unchanged.
- The fake MCP `StreamingTransport` is **not** evidence for the provider plane; the recorder is.

### 4.2 Where credentials flow; no logging or echo

The guarantees here are about the MCP plane (4.0).

- Access key, secret and token exist only in `Credentials` (redacting `Debug`) and inside the SDK
  signer for the duration of one `sign()` call. The only bytes derived from them that leave the
  process **on the MCP plane** are:
  - the `Authorization` header: the access key id in the credential scope, plus an HMAC
    signature;
  - `x-amz-security-token`;
  - `x-amz-date`.

  These go in a TLS request to the single validated endpoint (4.4).
- Nothing credential-derived is written to stdout. stdout carries only JSON-RPC messages
  **received from the endpoint or generated by the proxy from fixed templates**.
- **Bodies are never logged** at any level. This is a deliberate tightening: upstream logs error
  bodies at `WARNING` and payloads at `DEBUG`.
- Error messages returned to the MCP client (4.6) are fixed templates plus the tool name. They
  never contain credentials, signed headers, or upstream bodies beyond what the server itself sent
  as a JSON-RPC error object.
- `Headers`/`HttpRequest`/`HttpResponse` `Debug` already hides `authorization`,
  `x-amz-security-token`, cookies and API keys. The proxy adds every `--header` name to a
  per-instance sensitive-name set (a small `toolkit-aws` extension, as with upstream
  `register_sensitive_headers`).

**R-LOG. The logging boundary: dependency events are dropped before formatting (#4996).**

aws-config emits provider errors through `tracing` **before** `toolkit-aws` discards their text:

- `meta/credentials/chain.rs:105,109` records `DisplayErrorContext(&err)` at `DEBUG` and `WARN`;
- a `credential_process` error carries the child's stderr verbatim (`credential_process.rs:124-130`).

Pattern redaction cannot recognise a bare secret in that text (`sdk.rs:571-574`), so
scrub-after-format is not a control. The rules:

1. **No subscriber, no logger.** The `toolkit` binary installs no `tracing` subscriber and no
   `log` logger, in any mode, at any `--log-level`, whatever `RUST_LOG` says. With no dispatcher
   set, `tracing` reports every callsite as disabled. An event's field values, such as
   `%DisplayErrorContext(&err)`, are then never evaluated or formatted. Today's tree satisfies
   this: `tracing` 0.1.44 is built without its `log` feature, and there is no
   `tracing-subscriber`, `tracing-log`, `env_logger` or other `log` backend (`Cargo.lock` at
   `fa3fa13`).
2. **Proxy diagnostics do not use `tracing`.** They go through the proxy's own `Diag` sink: a
   closed enum of events with allowlisted structured fields only:
   - the JSON-RPC method, from the fixed MCP method set, else `other`;
   - whether an id was present, and its type;
   - HTTP status;
   - byte counts and durations;
   - failure kinds (`CredentialFailure`, `TransportFailure`, bound names).

   No `Diag` variant carries a provider string, a header value, a body, a URL query (refused
   anyway, R-URL) or any error's `Display`. Each rendered line is length-bounded and then passed
   through `redact_text`, which is a second line of defence, not the control.
3. **stderr only.** `Diag` writes only to stderr. stdout is written only by the single JSON-RPC
   writer.
4. **Gate.** A CI source and dependency test fails if any of these happens:
   - `toolkit-cli`'s normal dependency tree gains `tracing-subscriber`, `tracing-log`,
     `env_logger` or any `log` implementation;
   - `tracing`'s `log` or `log-always` feature is enabled;
   - the workspace calls `tracing::subscriber::set_global_default`,
     `tracing::dispatcher::set_global_default`, `set_default`, `log::set_logger` or
     `log::set_boxed_logger`.
5. **Future change rule.** If a later slice needs dependency tracing, it must install a filter
   whose `register_callsite` returns `Interest::never` for every target outside the proxy's own.
   Dependency events are then rejected before any field is recorded. That slice must update this
   allowlist and T-LEAK-SDK in the same change.

**Offline verification (S3).**

- **T-LEAK.** Run every fixture (success, 4xx with body, 5xx, timeout, malformed SSE,
  credential failure) with canary credentials, a `--header` canary and an upstream-body canary,
  under `--log-level DEBUG`. Assert no canary and no upstream body appears in stderr or in any
  stdout message the proxy generated.
- **T-LEAK-SDK.** Binary-level: run the production `toolkit mcp-proxy` entry point, with the same
  logging setup as shipped, under `--log-level DEBUG` with each of `RUST_LOG=trace`,
  `RUST_LOG=aws_config=trace` and `RUST_LOG=debug`. The credential sources are real SDK chains
  over synthetic profiles:
  1. a profile `credential_process` that is a Rust fixture executable printing a **bare, random,
     unlabelled canary** on stderr and exiting 1. This is a chain `WARN` event, because it is a
     provider error;
  2. the same, reached after an environment provider `NotLoaded`, so a `DEBUG` source-chain event
     precedes it;
  3. a `role_arn` profile whose `source_profile` is that failing process.

  Assert the canary appears nowhere in stderr or stdout, and that the client receives the fixed
  credentials message.
- **T-LEAK-GATE.** Runs the gate in rule 4. It also checks in-process that
  `tracing::dispatcher::has_been_set()` is false after proxy startup.

### 4.3 TLS (#5001)

**MCP plane:**

- **Stack:** rustls with the aws-lc provider through `aws-smithy-http-client` 1.5.1
  (`sdk.rs:336-342`).
- **Verification** is always on. No flag disables it, and none will be added.
- **Scheme and redirects:** https only (`require_https`), no userinfo, no redirects (a 3xx is an
  error, not followed).
- **No HTTP(S) proxy.** The connector is built with no proxy configuration, so `HTTPS_PROXY`,
  `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` have no effect. This is a separate, verified property
  (T-TLS-8).

**Trust anchors (corrects revision 1).** Revision 1 said "the platform trust store". That was
inaccurate:

- Smithy loads the roots once per process into a `LazyLock` (`rustls_provider.rs:117-129`)
  through `rustls_native_certs::load_native_certs()`.
- In rustls-native-certs 0.8.5 (`lib.rs:48-57,118-123`), when `SSL_CERT_FILE` or `SSL_CERT_DIR`
  is set, roots come **only** from those locations, and the platform store is not consulted.

**R-TLS. v1 behaviour, consistent with the PM's Q4 ruling:**

- **`SSL_CERT_FILE` and `SSL_CERT_DIR` are honoured** as inherited, trusted launch
  configuration. They replace the platform roots, exactly as rustls-native-certs defines; they
  do not add to them.
  - This matches upstream, whose `httpx` honours the same two variables (`trust_env`).
  - Anyone who can set the proxy's environment can already choose its credentials and its
    binary, so this adds no new trust.
  - The proxy never reads, unsets or mutates these variables. Environment mutation is unsound in
    a threaded process (R-PROV gate).
- **Empty or broken root configuration fails closed.** A configured file or directory that is
  empty, missing or unreadable gives an empty or partial root set. Every handshake then fails
  with the fixed TLS connect error. There is no fallback to the platform store and no unverified
  mode.
- **`AWS_CA_BUNDLE` and the profile `ca_bundle` key are not honoured.** Neither aws-config 1.13.0
  nor aws-smithy-http-client 1.5.1 references them; checked in both sources. Upstream's `httpx`
  also ignores them for MCP traffic, and only botocore's own calls use them. This is a documented
  difference.
- **Still deferred under Q4:** `HTTPS_PROXY`/CONNECT tunnelling, and an explicit
  additional-roots option such as `--ca-bundle` (adding roots rather than replacing them). The
  deferral is safe:
  - no unverified fallback exists;
  - private-CA users already have a working path through `SSL_CERT_FILE`/`SSL_CERT_DIR`.

  So the deferral costs only intercepting-proxy users, who fail closed. Proposed ruling R-5
  records the corrected premise.
- **Provider plane:** the SDK's default HTTPS client loads roots the same way, so the same
  variables apply to STS, SSO and Sign-in calls.

**Offline verification, T-TLS (S1).** These tests run through the **production rustls
verification layer**.

**Harness:**

- `SdkTransport` gains a test-only constructor that uses the same `Connector` builder and TLS
  provider as production. It replaces only the DNS resolver, so that a synthetic hostname
  resolves to a loopback TLS server. The public-address guard is tested separately (T-EP).
- Certificates are minted at test time (`rcgen` as a dev-dependency).
- **Each case runs in its own subprocess**, because the root list is a process-wide `LazyLock`.
  The test binary re-executes itself with a case argument, and the environment is set with
  `Command::env`, never `set_var`.

**Cases:**

| Case | Expected |
| --- | --- |
| T-TLS-1: certificate from a CA not in the roots | Handshake failure; the server reads no request bytes |
| T-TLS-2: trusted CA (via `SSL_CERT_FILE`), certificate for another hostname | Failure |
| T-TLS-3: `SSL_CERT_FILE` holding the synthetic CA | Success |
| T-TLS-4: `SSL_CERT_DIR` holding the synthetic CA file | Success |
| T-TLS-5: `SSL_CERT_FILE` empty file; T-TLS-6: nonexistent path; T-TLS-7: `SSL_CERT_DIR` empty directory | Handshake failure; the server reads no request bytes |
| T-TLS-11 (replacement, not addition): in a subprocess with `SSL_CERT_FILE` set to the synthetic CA, read the root set the production path loads (`load_native_certs`, the same call Smithy's `LazyLock` makes) | It equals exactly the configured file's certificates (same count and DER). This proves the platform store is not merged in, which a handshake against a synthetic-CA server alone cannot show |
| T-TLS-8: `HTTPS_PROXY=http://127.0.0.1:<recorder>` set | The recorder sees no connection; the request reaches the server directly |
| T-TLS-9: `http://` endpoint | Refused at start |
| T-TLS-10: 301/307 response | Not followed; exactly one request recorded |

### 4.4 Allowed endpoint, request target and owned headers (#5002, #5000)

#### 4.4.1 R-URL. Endpoint grammar and the exact request target (#5002)

The endpoint is the single positional argv value. It is validated once at start by
`validate_endpoint(url, "endpoint", &EndpointPolicy::aws_public(), resolver)`. The accepted
grammar is the subset that validator accepts:

```text
endpoint = "https://" host [ ":" port ] path
host     = label *( "." label )      ; ASCII letters, digits, "-"; each label 1-63 bytes,
                                     ; no leading or trailing "-"; compared and emitted lowercase;
                                     ; equals or ends with an EndpointPolicy::aws_public()
                                     ; suffix (".api.aws", ".amazonaws.com"); every DNS answer public
port     = decimal 1-65535
path     = *( "/" segment )          ; may be empty
segment  = *pchar                    ; no control, whitespace or "\"; never ".."; "%" kept verbatim
```

**Refused:**

- a query (`?`), a fragment (`#`) or userinfo (`@`) (`endpoint.rs:245-248` and the userinfo check
  above it);
- an IP literal, `http`, a non-AWS host and anything else `validate_endpoint` refuses.

**Query rejection is an intentional supported-subset change.** Upstream v1.7.0
`validate_endpoint_url` accepts any https URL. No shipped or documented proxy endpoint has a
query. `?oauth=initialize` belongs to option (b)'s direct-HTTP client configs, which never go
through the proxy. Proposed ruling R-4. The refusal message is fixed text and never echoes the
URL, so a secret in a query is not printed.

**Representation.** `validate_endpoint`'s `Endpoint.path` trims trailing slashes
(`endpoint.rs:295-299`), so the proxy does **not** rebuild the request URL from `Endpoint`. It
uses `Endpoint` only for the host and the policy, and stores `McpTarget { authority, path }`:

- `authority` is the lowercase host, plus `":" port` when a port other than 443 is given. An
  explicit `:443` is dropped.
- `path` is the argv path **verbatim**, or `/` when empty.

The request URL is `https://` + `authority` + `path`, the same for `POST`, `GET` and `DELETE`:

- `/mcp` and `/mcp/` stay distinct;
- percent-encoding is neither decoded nor re-encoded;
- the global `EndpointPolicy::aws_public()` is **not** changed for this command.

**What the proxy never sends to:** no URL is ever taken from stdin, from a server response, from
SSE `retry`/`id` fields or from a header such as `Location`. Server-supplied URLs inside tool
results (for example `aws___get_presigned_url`) are opaque data passed to the client.

**Intentional narrowing vs upstream (Q6, ruled):**

- non-AWS hosts are refused;
- `http://localhost` is refused;
- `.amazonaws.com.cn` is refused;
- private-address answers are refused, and with them PrivateLink.

#### 4.4.2 R-HDR. Owned and rejected headers (#5000)

`Headers::insert` (`transport.rs:120-146`) refuses control characters, but it accepts `Host`,
framing and session headers. `sdk.rs:391-394` copies them to the SDK request, and aws-sigv4
1.6.3 `canonical_request.rs:425-431` signs a supplied `Host` instead of deriving one from the
URI. URL equality alone does not establish the emitted or signed authority. This is a
trusted-argv correctness gap: MCP traffic cannot set `--header`.

**Owned headers.** These are set only by the proxy or its HTTP stack. A `--header` naming one is
a **usage error at start**, compared ASCII case-insensitively:

| Group | Names |
| --- | --- |
| Authority and framing (hop-by-hop included) | `host`, `content-length`, `transfer-encoding`, `te`, `trailer`, `connection`, `keep-alive`, `proxy-connection`, `upgrade`, `expect` |
| Proxy | `proxy-authorization` |
| SigV4 | `authorization`, `x-amz-date`, `x-amz-security-token`, `x-amz-content-sha256`, `date` |
| MCP protocol and session | `mcp-session-id`, `mcp-protocol-version`, `last-event-id`, `accept`, `content-type` |
| Identity | `user-agent` (Q3 policy) |

**Other refusals and what is allowed:**

- The same name given twice, case-insensitively, is also a usage error.
- Any other name that passes `Headers::insert` is allowed. That includes the agentaccess
  headers (`X-Amzn-AgentAccess-Streaming-Session-Url`,
  `X-Amzn-AgentAccess-Expire-Streaming-Session-On-Delete`), which are forwarded, signed and
  redacted.

**How the owned values are produced:**

- **Host.** The proxy never sets `Host`. With no `Host` in `HttpRequest.headers`, aws-sigv4
  derives the signed host from the URL authority, and hyper writes `Host` from the same URI. The
  signed and emitted authority therefore both come from `McpTarget`.
- **Content-Length.** hyper derives it from the final body bytes, after `_meta` injection, and
  never uses `Transfer-Encoding` for these requests.
- **Mcp-Session-Id** comes only from the validated session state (4.5).
- **Mcp-Protocol-Version** comes from the negotiated `initialize` result, validated as
  `YYYY-MM-DD`.

**Defence in depth in `toolkit-aws` (S1).**

- **Transport check:** `SdkTransport` and the streaming transport refuse, before sending, any
  request whose headers include `host`, `content-length`, `transfer-encoding`, `te`, `trailer`,
  `connection`, `keep-alive`, `proxy-connection` or `upgrade`.
- **Signer check:** `SigV4Signer` refuses to sign such a request.
- Both checks return `InvalidConfig` without echoing values. This protects asm exec too.
- S1 confirms that no existing caller sets these headers. The `content-length` handling in
  `asm_exec/loopback.rs` is a test-side parser, not a request builder.

**Offline verification, T-HDR (S1 transport and signer refusal; S3 CLI and wire).**

| Case | Expected |
| --- | --- |
| CLI: `--header Host=evil.example`, `HOST=…`, `hOsT=…`, `Content-Length=0`, `Transfer-Encoding=chunked`, `Connection=close`, `Mcp-Session-Id=x`, `MCP-PROTOCOL-VERSION=x`, `Authorization=…`, `X-Amz-Date=…`, a duplicate name | Usage error; no request |
| Wire, on the T-TLS loopback server recording the raw request head | Exactly one `Host`, equal to `McpTarget.authority`. `Content-Length` equals the received body length and the post-`_meta` length. No `Transfer-Encoding`. The signed-headers list contains `host`, and the signature re-verifies against the received request with the fixture credentials |
| `X-Amzn-AgentAccess-Streaming-Session-Url` canary | Forwarded and signed; absent from stderr |
| URL target cases (#5002) | `/mcp` → `POST /mcp`; `/mcp/` → `POST /mcp/`; empty path and `/` → `POST /`; `/a%2Fb/mcp` → `POST /a%2Fb/mcp`, and the SigV4 canonical URI for it matches a known-answer fixture; `:443` → `Host` without a port; `:8443` → `Host: <host>:8443` and the same signed host |
| `?x=1`, a bare `?`, `#f`, `/mcp?token=CANARY` | Refused at start; `CANARY` appears in no output |

### 4.5 Size, work and memory bounds (#4998, #4999)

All constants live in one module. Tests inject smaller values.

| Bound | Value | On violation |
| --- | --- | --- |
| stdin message (one line) | 8 MiB | JSON-RPC error `-32600` to the client if an `id` can be recovered, otherwise a `Diag` line; the line is discarded and the loop continues |
| upstream message: one `application/json` body or one SSE event (`data:` lines joined) | **8 MiB** (revision 1: 32 MiB), read incrementally | Stream aborted; JSON-RPC error `-32603` "response too large" |
| SSE line length | 8 MiB | Same |
| JSON nesting | 128 (`MAX_JSON_DEPTH`) | Malformed |
| JSON nodes per message (each value and each object member counts) | **262,144** | Malformed ("too many JSON values") |
| object duplicate-key detection | indexed, linear expected (R-WORK, S0) | — |
| shared process byte budget | **96 MiB** (R-MEM) | Wait, then fail (R-MEM) |
| control reserve | 1 MiB, separate from the budget | — |
| `Mcp-Session-Id` | 1-256 bytes of visible ASCII 0x21-0x7E (MCP spec) | Session refused, error to the client |
| response header count and size | hyper's header limits; S1 pins the exact values with a test | Connect error |
| concurrent in-flight requests | 16 | `-32603` "too many concurrent requests" on the control lane; never queued without bound |
| stdout data queue | 64 lines **and** within the byte budget | Backpressure (R-MEM) |
| stdout control queue | 64 lines of at most 1 KiB each | The stdin reader pauses |
| connect / idle-read / write / overall request timeout | 60 / 120 / 180 / 180 s defaults, CLI-compatible flags, each 1 s to 1 h (`Timeouts`) | Timeout error |
| tool call | 300 s (`--tool-timeout`) | In-flight request cancelled; `CallToolResult{isError:true}` |

**Stream lifetime.** An SSE stream with steady bytes but no end is bounded by `--tool-timeout`
for `tools/call` and by `--timeout` for other requests. The optional `GET` stream is bounded
only by idle-read and is reopened at most once per 30 s.

**Why 8 MiB and 262,144 nodes.** aws-mcp tool results are documentation pages and AWS API
responses. 8 MiB is far above what a client places in a model's context. Upstream has no bound,
and a larger message now fails with a clear error instead of exhausting memory. A `tools/list`
with 100 tools uses about 10^4 nodes. These numbers are proposed ruling R-3.

**R-WORK. Parser work is linear in input bytes (#4998).**

`toolkit-core/src/json.rs:337` checks each new key against every earlier key in the object, so a
shallow, wide object costs n(n-1)/2 comparisons. Tens of thousands of short keys fit in well
under 1 MiB, inside both the depth and byte bounds. A network timer cannot pre-empt that
synchronous loop. Kaizen #4497 already tracks exactly this defect, from review #4488. Its fix
becomes slice **S0**:

- **Indexed duplicate check.** `parse_json` keeps an index of the object's keys next to the
  ordered `Vec`. The index is `std::collections::HashSet` with the default `RandomState`, so it
  is keyed SipHash with per-process random keys and an adversary cannot precompute collisions.
  `Json::Object` keeps insertion order, and the duplicate-key error and its position are
  unchanged. An object of at most 8 members may keep the linear scan, which is bounded by a
  constant.
- **Limits entry point.** S0 adds `parse_json_with_limits(input, &JsonLimits { max_depth,
  max_nodes })`. `parse_json` keeps its current limits, so its other callers are unaffected apart
  from the faster check.
- **Work bound.** Each byte is examined a bounded number of times, each key costs one hash and an
  expected O(1) probe, and `to_compact_string` is linear in the tree. Parse and re-serialise
  work per message is therefore O(message bytes), at most 8 MiB.
- **Documentation.** S0 also qualifies the O(input) wording in
  `docs/migration/security-hooks-review.md:63-65`, as #4497 requires.
- **Scheduling and cancellation.** Upstream messages are parsed and re-serialised on a blocking
  worker (`tokio::task::spawn_blocking`). That never happens on the stdin reader, the stdout
  writer or the runtime's I/O threads, so a parse cannot stall the protocol loop or other
  requests. The parser does not yield. A cancellation or timeout that fires during a parse takes
  effect when the parse returns, which is bounded by the linear work above. The message is then
  dropped and its budget released.

**Verification, T-WORK (S0; reused by S2).** Deterministic, with no timing or load:

- **Probe count.** A test-visible `parse_json_with_stats` counts key comparisons and index
  probes. An object of n = 20,000 distinct short keys (about 200 KiB) must parse with at most 2n
  probes. The old loop needs about 2×10^8 comparisons.
- **Behaviour kept:** a duplicate placed last is still rejected at the same error position, and
  key order is preserved.
- **Node cap:** with an injected `max_nodes`, the boundary value is accepted and boundary+1 is
  refused.
- **Nesting:** the same wide object nested inside arrays at depth 100 is covered.

**R-MEM. One shared byte budget with backpressure (#4999).**

Revision 1 bounded only message counts. 64 queued messages × 32 MiB is about 2 GiB, and 16
producers add 512 MiB more, before tree amplification. Revision 2 accounts bytes. One
process-wide `Budget` (96 MiB, injectable) covers every variable-size buffer: stdin line
buffers, upstream body and SSE buffers, the parse charge, serialised output lines and queued
stdout lines.

**Reservations:**

- **Reserve before allocating.** A reader reserves each chunk before appending it to its buffer.
  Before parsing, the task reserves a parse charge of `NODE_CHARGE × min(max_nodes, bytes/2 + 1)`
  plus `bytes` for owned strings. A message of b bytes cannot hold more than b/2+1 nodes.
  `NODE_CHARGE = 2 × size_of::<(String, Json)>()` covers `Vec` growth. It is a compile-time
  constant, 112 B with today's layout, and a test asserts its value. The compact output is
  reserved chunk by chunk as it is written.
- **Worst-case peak.** A largest message peaks at about 8 MiB raw + 28 MiB tree + 8 MiB strings.
  The raw buffer is released after the parse, so the peak stays at about 44 MiB. The 96 MiB
  budget therefore admits two worst-case messages at once, and smaller ones alongside.
- **Release is automatic.** Each reservation is an RAII `Reservation` guard released on drop. A
  dropped buffer, a delivered line, a rejection, an error, a cancellation, a timeout or a
  disconnect therefore always releases it; no code path can skip the release.

**Backpressure without deadlock:**

- A reservation that does not fit waits only while the stdout data queue holds bytes, because
  draining them will release budget. The wait is also capped by the idle-read timeout.
- If the queue is empty, or the wait times out, the request fails at once with `-32603` "proxy
  memory budget exhausted" and releases everything it holds.
- Partial holders therefore cannot deadlock one another: progress depends only on the writer.
- A waiting reader stops reading its socket, so TCP flow control pushes back on the server.

**Control lane:**

- Proxy-generated JSON-RPC errors and cancellation results are fixed templates of at most 1 KiB.
  They are reserved from the separate 1 MiB control reserve and go on the control queue, which
  the writer serves first at every line boundary.
- Each in-flight request produces at most one control message, and requests beyond 16 in flight
  are refused with one. If the control queue is ever full, the stdin reader pauses until it
  drains. This pushes back on the client and never grows without bound.
- Errors, timeouts and cancellation results therefore reach the client ahead of queued data as
  soon as stdout moves.

**Cancellation and disconnect:**

- A client `notifications/cancelled` for an in-flight id is forwarded upstream and aborts the
  local stream task, which releases its reservations. Its response is dropped.
- stdin EOF sends `DELETE` (4.6) and then aborts all tasks.
- A stdout write error (EPIPE) aborts all tasks and exits.

**Unaccounted fixed overhead:**

- hyper and rustls per-connection buffers: hyper's default maximum read buffer is about 408 KiB,
  for at most 17 connections (16 in flight plus the `GET` stream), so about 7 MiB;
- the 1 MiB control reserve.

The process's variable memory is therefore bounded at about 104 MiB, whatever the server sends
and however slowly the client reads.

**Verification, T-BUDGET (S2).** The test injects small limits: budget 4 KiB, message 1 KiB,
nodes 32, control reserve 512 B. It uses a fake streaming transport with four concurrent
producers and a fake stdout sink that can be paused. No large allocation and no network.

| Case | Expected |
| --- | --- |
| Boundary | A message whose total charge equals the remaining budget is delivered. One byte more waits while the queue holds bytes, and fails at once when the queue is empty |
| Backpressure | With the sink paused, producers stop at the budget. A test hook asserts `budget.in_use() ≤ limit` after every step. When the sink resumes, control errors reach it before queued data |
| Cancellation | Cancelling a waiting request lowers `in_use` by exactly its charge |
| Release | `in_use` returns to 0 after each of: reject (oversize, malformed, node cap), timeout (fake clock), cancellation, stdin EOF and stdout EPIPE |
| Recovery | After the sink resumes, waiting producers finish and new requests succeed |
| No deadlock | Producers holding partial reservations with an empty queue fail within the fake-clock bound instead of hanging |

### 4.6 Threat model: malicious or compromised MCP server response

The endpoint is AWS-operated, but its responses are treated as untrusted input. They may also
carry attacker-influenced content, such as documentation text or the output of a resource the
user reads.

| Threat | Control |
| --- | --- |
| Exhaust memory with an endless body, a huge SSE event, deep JSON or many small queued messages while the client reads slowly | Section 4.5 bounds; the R-MEM shared byte budget with reserve-before-allocate and backpressure; the node cap; incremental reading |
| Exhaust CPU with a shallow, wide object (quadratic duplicate check) | R-WORK: indexed duplicate detection (S0, #4497); linear work per message; parsing off the protocol loop |
| Break stdio framing: raw newline or control bytes in a message so one SSE event becomes two JSON-RPC lines, or a non-JSON line reaches the client | Every message from upstream is **parsed and re-serialised compactly** (`to_compact_string`) before it reaches stdout. Non-JSON data, and anything that is not an object or a batch, is dropped with a diagnostic. One writer writes whole lines |
| Redirect the proxy to another host (3xx `Location`, SSE `retry`, links in results) | No redirects; the endpoint is fixed (R-URL); no field of a response is used as a URL |
| Reach or reconfigure the credential provider plane through traffic | R-PROV: provider builders take only startup environment and files; no environment mutation |
| Inject headers through `Mcp-Session-Id` (CR/LF), or exfiltrate through an oversized id | The session id is validated to the spec charset and length before it is stored, and refused otherwise. `Headers::insert` also refuses control characters. `mcp-session-id` is an owned header (R-HDR) |
| Authority or framing confusion via headers (trusted argv) | R-HDR: owned headers are refused at start; the transport and signer refuse authority and framing headers; `Host` and `Content-Length` are derived from `McpTarget` and the final body |
| Make the proxy sign a request of the server's choosing | The proxy signs only bodies it read from the client, plus `_meta` injection. A response never causes a new upstream request, except a `GET` stream reopen to the same URL |
| Cookies or other state (`Set-Cookie`) | Ignored; no cookie jar |
| Spoof responses to ids the client never sent, or answer a request twice | Forwarded as-is. The client owns id correlation, and the proxy keeps no state a spoof could corrupt. The proxy's pending-request table ignores unknown ids for timeout and budget bookkeeping |
| Server-initiated requests (sampling, elicitation, roots/list) | Forwarded to the client, which decides. The proxy has no capabilities of its own and cannot be made to act |
| Lie in `readOnlyHint` to get a write tool through `--read-only` | Same as upstream: the filter trusts annotations. It is a documented convenience filter, never authorization; IAM is the boundary (stated in `--help`; confirmed acceptable by #4994) |
| Prompt injection in tool results | Out of scope for a transport. The proxy passes content unchanged, as upstream does |
| Leak secrets through error text the proxy prints | R-LOG: no tracing subscriber; allowlisted `Diag` fields only; bodies never logged. JSON-RPC error objects from the server go only to the client, which already receives the server's data |
| Downgrade to plain HTTP or a private address (DNS rebinding) | https required at start and per request; the DNS guard at connect time prevents sending to a non-public address; the peer check after connect is a backstop (#4593) |
| TLS interception | Certificate verification is always on, with no opt-out. Trust anchors come from the platform store, or from `SSL_CERT_FILE`/`SSL_CERT_DIR` when set (R-TLS) |

### 4.7 How offline tests prove each property

There is no live AWS and no network: the build and test jobs run offline. The fake transports
are each used only where they count as evidence:

- **Protocol behaviour** (framing, sessions, errors, budget) uses recorded or hand-written HTTP
  exchanges served by a **fake `StreamingTransport`**. The proxy runs in-process with injected
  stdin, stdout and stderr, and a fixed clock.
- **Credential properties** use the **real SDK default chain** in subprocesses, over synthetic
  files, with the T-PROV recorder as the only provider HTTP client.
- **TLS, endpoint target and header framing** use the **production rustls connector** against an
  in-process loopback TLS server, in subprocesses where a process-wide cache matters.

| Property | Test (owning slice) |
| --- | --- |
| SigV4 correctness | T-SIG-1: AWS SigV4 test-suite known-answer vectors through `SigV4Signer` with a fixed clock (S1, shared with #4582). T-SIG-2: a proxied `initialize` POST signed for `aws-mcp`/`us-east-1` gives a byte-exact `Authorization` header against a recorded fixture (S3). T-SIG-3: a session token gives `x-amz-security-token` (S1). T-SIG-4: `_meta` injection happens **before** signing; the signed payload hash equals the hash of the sent body (S3) |
| service and region resolution | T-SCOPE: table test of `signing_service_region` plus `--service`/`--region`/`AWS_REGION` precedence, including aidevops, agentaccess-mcp and bedrock-agentcore hosts, and the error when nothing yields a region (S3) |
| credential refresh and stdin | T-CRED-1: a fake provider counts loads: one load per attempt, and a rebuild after a recorded 403 (S3). T-CRED-4: a Rust fixture `credential_process` reads stdin and sees EOF at once (S1, D2). T-CRED-5: a credential flag in argv is refused (S3) |
| unsigned only on semantic absence; error precedence | T-ABS (S1 preflight units, S3 end to end), section 4.1.1 table |
| `aws login` source | T-LOGIN (S1), R-LOGIN, below |
| provider-plane egress and isolation | T-PROV (S1 seam, S3), section 4.1.2 |
| nothing logged or echoed | T-LEAK, T-LEAK-SDK, T-LEAK-GATE (S3), section 4.2 |
| TLS verification and root sources | T-TLS-1 to T-TLS-11 (S1), section 4.3 |
| endpoint allowlist | T-EP (S3). Refused: non-AWS host, `localhost`, IP literal, `.amazonaws.com.cn`, private DNS answer, userinfo; a different URL in a response `Location`, an SSE `retry`/`id` or a tool result is never requested. Accepted: the four documented host shapes. Every request URL equals `McpTarget`'s |
| request target and header framing | T-HDR, including the #5002 URL target cases (S1 refusal, S3 CLI and wire), section 4.4 |
| size bounds | T-SIZE: one test per row of section 4.5 at the limit (accepted) and limit+1 (refused, loop continues, next message still served) (S1 transport rows, S2) |
| parser work | T-WORK (S0, S2), section 4.5 |
| memory budget and backpressure | T-BUDGET (S2), section 4.5 |
| framing | T-FRAME: an SSE event whose JSON string contains `\n` escapes and U+2028, multi-line `data:`, comments, CRLF, an event split across chunks, a batch array, and a non-JSON event; stdout always has one valid JSON object per line (S2) |
| session lifecycle | T-SESS: session id captured from `initialize` and sent afterwards; invalid id refused; stdin EOF sends exactly one `DELETE` with the id (405 tolerated); `GET` 405 tolerated silently; a 404 on a later POST gives a session-expired error to the client (S2) |
| timeouts and errors | T-ERR: 401/403 on `tools/call` gives the fixed credentials text with `isError:true`; 5xx gives the fixed "Tool call '<name>' failed: HTTP 503 … Please retry."; a non-tool request failure gives JSON-RPC `-32603`; the tool timeout fires at the configured duration with a fake clock; `--retries N` retries connect and 5xx/429 with fresh signatures (the fake signer counts) and never more than N times (S2) |
| read-only | T-RO: `tools/list` filtered by truthy `readOnlyHint`; a call to a filtered tool is refused with upstream's exact text and never sent upstream (S3) |
| CLI compatibility | T-CLI: each of the six plugin argument vectors and the documented aidevops vector parse to the expected config; unknown upstream flags (`--profile A B`, `--disable-telemetry` before support) give a usage error, not silent acceptance; owned `--header` names refused (S3) |
| config shape | The existing `toolkit manifests`/`toolkit spec` validation, plus a new contract test that no tracked `.mcp.json`/`mcp.json` uses `uvx`, `uv`, `python` or `mcp-proxy-for-aws` (the AC5 gate, S5) |

**T-LOGIN (S1, R-LOGIN).** Each case runs in a subprocess with:

- `AWS_LOGIN_CACHE_DIRECTORY` set to a temp dir;
- a synthetic profile with `login_session = <synthetic ARN>`;
- a synthetic cache file in the format aws-config's `login/cache.rs` reads. The fixture shape
  is copied from aws-config's own tests, and the DPoP key is generated at test time. No real
  token is used.

| Case | Expected |
| --- | --- |
| Unexpired cached session | A signed request with the fixture access key id; the recorder saw no Sign-in call |
| Expired session; the recorder answers the Sign-in token refresh from a fixture | Signed with the refreshed key, and the cache file is updated |
| Refresh failure (recorder 400, then a dispatch error) | Fails closed, with no MCP transport call, even with `--skip-auth` (the preflight is Configured) |
| Recovery: after the failure, the test rewrites the cache (simulating `aws login`) | The next request succeeds through the D1 rebuild |
| The unexpired case without the feature | Fails closed (`FeatureNotEnabled` → `InvalidConfiguration`). The valid-cache case is the proof the feature is on; the fake-provider success of revision 1 is not |

## 5. Implementation plan

Slices are ordered by dependency. Each slice leaves the Python/`uvx` configs untouched until S5.

- **S0: bounded JSON parser work** (toolkit-core; closes kaizen #4497; it has no dependency on
  S1 and may land first)
  - Indexed duplicate-key detection that preserves member order and the duplicate-key error.
  - `parse_json_with_limits` with a node cap.
  - A test-visible probe counter.
  - Qualify `security-hooks-review.md:63-65`.
  - Tests: T-WORK, and the existing `parse_json` suite unchanged.
- **S1: streaming seam, SDK wiring, credential and TLS boundary** (toolkit-aws; shares the signer
  and transport wiring with #4582)
  - Add a `StreamingTransport` trait: `open(&HttpRequest) -> ResponseStream`, with status,
    headers and incremental chunks under an idle-read timeout and a byte cap. Implement it on
    `SdkTransport` with the same guards, plus a fake in `testing`.
  - The R-HDR transport and signer refusal of authority and framing headers.
  - The per-instance sensitive-header set.
  - Move `signing_service_region` from `asm_exec::mcp` into `toolkit-aws`.
  - R-LOGIN: add `credentials-login` to the `sdk` feature's aws-config features.
  - The R-ABS preflight classifier: a pure function, plus the `aws_config::profile::load`
    adapter.
  - The R-PROV seam `default_chain_with_http`, and the env-mutation source gate.
  - D2's null-stdin mechanism, which is now mandatory.
  - The T-TLS loopback harness, with its test-only resolver constructor.
  - A `toolkit-cli` feature (`aws-sdk`) that enables `toolkit-aws/sdk`, and the swap of asm
    exec's `Unavailable*` for the real adapters when it is on (closes #4582 item 1).
  - Tests: T-SIG-1/3, T-TLS-1 to T-TLS-11, T-CRED-4, T-LOGIN, the T-ABS preflight units, the
    T-PROV seam and gate, the T-HDR refusal, the transport rows of T-SIZE.
- **S2: protocol core** (toolkit-cli `mcp_proxy` module, no network)
  - The stdio reader and writer, with separate data and control queues.
  - The R-MEM `Budget` and its `Reservation` guards.
  - The JSON-RPC framing and re-serialisation through `parse_json_with_limits`, on blocking
    workers.
  - The SSE parser.
  - The session state machine (`initialize` capture, `Mcp-Session-Id`, `Mcp-Protocol-Version`,
    the `GET` stream, `DELETE` on EOF).
  - `_meta` injection with upstream precedence.
  - The concurrency cap.
  - Cancellation forwarding.
  - Error templates.
  - Tests: T-FRAME, T-SESS, T-SIZE, T-ERR, T-BUDGET, and T-WORK at message level, against the
    fake streaming transport and fake signer.
- **S3: `toolkit mcp-proxy` command**
  - CLI parsing compatible with the flags in use: `endpoint`, `--service`, `--region`,
    `--profile` (single), `--metadata`, `--header` (R-HDR owned set), `--skip-auth`,
    `--read-only`, `--retries`, `--timeout`, `--connect-timeout`, `--read-timeout`,
    `--write-timeout`, `--tool-timeout`, `--log-level`.
  - R-URL's `McpTarget`.
  - The Q3 User-Agent policy.
  - The R-LOG `Diag` sink and the dependency/source gate.
  - Wiring of S1 and S2, plus D1, R-ABS, R-PREC, R-STICKY and the read-only filter.
  - Tests: T-SCOPE, T-SIG-2/4, T-CRED-1/5, T-ABS end to end, T-PROV end to end, T-LEAK,
    T-LEAK-SDK, T-LEAK-GATE, T-EP, T-HDR CLI and wire, T-RO, T-CLI, and binary-level pipe tests
    over the fake transport.
- **S4: independent security review (AC4 completion)**
  - Opposite-family reviewer, at the exact SHA of S0-S3.
  - Checklist: section 4 of this document and the section 7 map.
- **S5: cutover** (Q1 decided by the GM; waits on the operator's signed-run to-do)
  - **Distribution (GM ruling on Q1).** Per-plugin staged binaries: `"command": "./bin/toolkit"`,
    built and staged by the one pipeline shared with #4487.
  - **Operator gates.** The pipeline's design and implementation are tracked on #4487. Two
    things stay operator gates:
    - publishing a release, or anything touching `main`;
    - any paid CI runner or paid service, which stops the work and reports the exact cost line
      up.
  - **Configs.** The six files become `"args": ["mcp-proxy", "https://aws-mcp.us-east-1.api.aws/mcp",
    "--skip-auth", "--metadata", "INSTALL_SOURCE=agent-toolkit-<plugin>"]`.
  - **Live checks (GM ruling on Q2).**
    - Agents may run the **unsigned** `initialize` + `tools/list` check against
      `https://aws-mcp.us-east-1.api.aws/mcp` themselves, because it needs no credentials.
    - The **signed** run is an operator action. Once S1-S3 are on rolling and the binary builds,
      the PM files one operator to-do (an exact copy-paste build plus signed-call command,
      linked to #4413) and waits on it.
    - S5's config cutover waits on that to-do, or on a GM ruling if it stays open for more than
      48 h. S0-S4 do not wait on it.
  - **Prose.** Rewrite the section 1.2 prose:
    - the README Kiro/fx examples and prerequisites;
    - `setup-devops-agent` (the check script, the SigV4 and fallback configs, the `uvx` rows);
    - `connecting.md` and `operate-on-aws/SKILL.md`;
    - the startup-advisor README;
    - `kiro-project-init.md` and `mvsp.md`;
    - the devsecops README table.
  - **OAuth.** Add the OAuth direct option for aws-mcp to the README and `connecting.md`
    (documented only, Q7).
  - **AC5.** Add the AC5 contract test.
  - **Amazon WorkSpaces.** The Python **library** example stays as customer code: it is a Python
    SDK integration sample, not a launch of this toolkit. The `uvx mcp-proxy-for-aws` CLI hint
    gets the `toolkit mcp-proxy … --service agentaccess-mcp --header …` equivalent.
  - Folded into, or coordinated with, cutover #4414.
- **S6: `iam-policy-autopilot`.** Out of #4413's scope by the PM's Q8 ruling. It gets its own
  research Issue.
- **Follow-ups, not needed for AC1-AC5 with today's configs:**
  - multi-profile (`--profile A B`, `AWS_MCP_PROXY_PROFILES`, `aws_profile` injection);
  - `HTTPS_PROXY` and an additional-roots option (Q4);
  - China partition / PrivateLink (Q6);
  - automatic re-initialise on 404 session expiry.

**Test plan summary.** Everything in section 4.7 runs in the default offline daemon test job.
The S1 SDK paths need the `aws-sdk` feature compiled, linted and tested by a daemon check (the
#4577 gap). No test contacts AWS. The only live checks are the Q2 ones above, and they are never
part of CI.

## 6. Rulings and open questions

### 6.1 Recorded rulings (from #4413's handoffs)

| # | Question | Ruling |
| --- | --- | --- |
| Q1 | How the `toolkit` binary reaches plugin users | **GM (5d050b3a, relayed by PM 8cd977f6):** (i) per-plugin staged `./bin` binaries, one staging pipeline shared with #4487; proceed with its design and implementation there. Operator gates remain: releases or anything touching `main`, and any real spending (stop and report the exact cost line) |
| Q2 | Live smoke test | **GM:** split. Agents may run the unsigned `initialize`/`tools/list` against `https://aws-mcp.us-east-1.api.aws/mcp`. The signed run is one operator to-do the PM files once S1-S3 are on rolling and the binary builds; S5 waits on it (or a GM ruling after 48 h). S1-S4 are not blocked |
| Q3 | Telemetry parity | **PM:** keep `INSTALL_SOURCE` in `_meta`; drop the MCP client name and version from `User-Agent` |
| Q4 | `HTTPS_PROXY`, custom CA bundles | **PM:** later follow-up; TLS always verified and no proxy in v1. R-5 below corrects the factual premise without changing the deferral |
| Q5 | Intentional differences (D1, D4, AWS-only allowlist, no body logging, forwarded `initialize` result) | **PM:** accepted |
| Q6 | `.amazonaws.com.cn`, PrivateLink | **PM:** not now; follow-up |
| Q7 | OAuth direct in plugin configs | **PM:** documented only |
| Q8 | `iam-policy-autopilot` | **PM:** out of #4413's scope; its own research Issue later |

### 6.2 Proposed rulings for revision 2 (PM)

Each is the design's choice, and the design is written as if it were accepted. A different
ruling changes only the named requirement.

| # | Proposal | Requirement | Why |
| --- | --- | --- | --- |
| R-1 | The preflight refuses every partial credential configuration, including four that botocore ignores and falls through: a secret key without an access key id, a session token without both keys, one half of the web-identity pair, and `AWS_CONFIG_FILE`/`AWS_SHARED_CREDENTIALS_FILE` naming a missing file | R-ABS | #4995: partial configuration must fail closed rather than fall through to another identity or to anonymous |
| R-2 | Once a session has signed a request, a later absence fails closed instead of going unsigned (upstream would go unsigned) | R-STICKY | An identity-bound session should not silently continue anonymously |
| R-3 | Upstream message bound 8 MiB (was 32 MiB), 262,144 JSON nodes per message, shared 96 MiB byte budget, 1 MiB control reserve | R-MEM, R-WORK | #4999: about 104 MiB worst-case variable memory, instead of more than 2.5 GiB |
| R-4 | Endpoint URLs with a query or fragment are refused: an intentional supported subset of upstream's validator | R-URL | No proxy endpoint uses a query; this avoids a second URL representation and logging a secret query |
| R-5 | `SSL_CERT_FILE`/`SSL_CERT_DIR` are honoured as inherited trusted configuration (replacement roots); `AWS_CA_BUNDLE` is not honoured; Q4's deferral of `HTTPS_PROXY` and additional roots stands | R-TLS | #5001: this states the real behaviour, which matches upstream's `httpx`; no unverified fallback exists |
| R-6 | Enable aws-config `credentials-login` in `toolkit-aws`'s `sdk` feature (asm exec gets it too) | R-LOGIN | #4997: keeps the `aws login` source required by AC3 |

### 6.3 Spending

None of the planned work spends money. The #4487 build matrix may need paid runners. If so, it
stops and reports the exact cost line up, as the GM ruling requires.

## 7. Pre-review #4994 findings: resolution map

| Finding | Requirement (section) | Verification | Slice |
| --- | --- | --- | --- |
| #4995 (P1) NotLoaded does not prove absence | R-ABS preflight, R-PREC precedence across D1 attempts, R-STICKY (4.1.1); proposed R-1, R-2 | T-ABS: preflight unit table; the real SDK chain in subprocesses over synthetic env/config with the T-PROV recorder; genuine absence → unsigned; typo'd profiles, partial env (with a valid fallback profile present), invalid/SSO-incomplete profiles, process failure, expiry, provider timeout, precedence and stickiness cases → no transport call | S1 (preflight), S3 (wiring, end to end) |
| #4996 (P1) SDK tracing before redaction | R-LOG (4.2): no subscriber or logger ever; allowlisted `Diag` sink; dependency/source gate; `Interest::never` rule for any future subscriber | T-LEAK-SDK: the production entry point, `RUST_LOG` attempts, a real profile chain over a Rust fixture `credential_process` printing a bare canary (`WARN` and `DEBUG` chain paths, assume-role source chain); T-LEAK-GATE | S3 (S1 provides the fixture executable) |
| #4997 (P2) `aws login` needs `credentials-login` | R-LOGIN (4.1); proposed R-6 | T-LOGIN: synthetic `login_session` and cache, injected Sign-in transport; success, expiry refresh, refresh failure fail-closed, recovery after a cache rewrite | S1 |
| #4998 (P1) quadratic object parsing | R-WORK (4.5): indexed duplicate check (kaizen #4497), `parse_json_with_limits` node cap, parsing on blocking workers, cancellation after a bounded parse | T-WORK: probe count ≤ 2n for 20,000 keys, order and duplicate error preserved, node-cap boundary | **S0** (toolkit-core), used by S2 |
| #4999 (P2) aggregate memory | R-MEM (4.5): shared 96 MiB budget, reserve-before-allocate RAII reservations, node charge, deadlock-free wait, control lane, release on every exit path; proposed R-3 | T-BUDGET with injected small limits: boundary, paused sink, control priority, release on reject/timeout/cancel/EOF/EPIPE, recovery, no deadlock | S2 |
| #5000 (P2) Host and framing headers | R-HDR (4.4.2): owned set refused at start, case-insensitively; duplicates refused; transport and signer refusal; `Host`/`Content-Length` derived from `McpTarget` and the final body | T-HDR: CLI refusals (mixed case, framing, session); on the wire, exactly one `Host` = authority, `Content-Length` = post-`_meta` body, no TE, signature re-verifies; agentaccess header kept and redacted | S1 (refusal), S3 (CLI, wire) |
| #5001 (P2) native roots honour `SSL_CERT_*` | R-TLS (4.3): corrected statement; `SSL_CERT_FILE`/`DIR` honoured as trusted configuration; `AWS_CA_BUNDLE` not; fail closed on empty roots; no proxy; Q4 deferral kept; proposed R-5 | T-TLS-1 to T-TLS-11 through the production rustls layer, one subprocess per case (`LazyLock`): untrusted, wrong host, each root variable, empty/missing roots, `HTTPS_PROXY` ignored, http refused, no redirects | S1 |
| #5002 (P2) query and trailing-path contradiction | R-URL (4.4.1): stated grammar; query/fragment refused (proposed R-4); `McpTarget` keeps the path verbatim; no rebuild from `Endpoint`; global policy unchanged | T-HDR URL target cases: `/mcp` vs `/mcp/`, root, percent-encoded, ports, query/fragment refusal with no canary echo; SigV4 canonical-URI fixture | S3 (S1 harness) |
| #5003 (P2) provider-chain egress | Section 4.0 scoping; 4.1.2 provider-plane egress and trust table; R-PROV (no traffic into provider inputs, one provider-HTTP seam, no env mutation) | T-PROV: the recorder injected through `default_chain_with_http` in every credential case with exact destination lists; source gate on chain construction; the traffic-cannot-reconfigure case | S1 (seam, gate), S3 (end to end) |

Review #4994 confirmed the rest of section 4 as it stands: D1's single rebuild, D2 being
necessary, D4, the signer, the endpoint policy, the DNS guard, the read-only stance, framing by
re-serialisation, session-id validation and the T-SIG/T-SESS/T-ERR/T-RO plans. Those parts are
unchanged except where the requirements above touch them.
