# MCP proxy replacement: research and design (#4413, phase 1)

Status: design for the AC4 credential and network pre-review. No code changes. Programme #4369,
slice `mcp`, dependencies #4379 (aws-core boundaries) and #4374 (core). Related: #4582 (real
SigV4 signer and HTTPS transport for `toolkit asm exec`), #4463 (SDK adapter, closed), #4487
(staging a Rust binary inside a plugin), #4414 (cutover), #4415 (retire).

Evidence base, all read and never executed (no Python, `uvx`, proxy or live AWS call was run):

- this repository at `fa3fa13` (the allocated base);
- upstream `aws/mcp-proxy-for-aws` at tag **v1.7.0** (2026-09-15), the version the plugins pin:
  `mcp_proxy_for_aws/{cli,server,proxy,utils,sigv4_helper}.py` and
  `middleware/{initialize_middleware,tool_filter,tool_error_middleware,profile_switcher}.py`, plus
  the repository README. `sigv4_helper.py` and `utils.py` were read on `main`; v1.7.0 is the
  newest tag, so `main` may be ahead of it. Slice 1 re-checks both files at the tag.
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
- `toolkit_core::parse_json` with `MAX_JSON_DEPTH = 128`.
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
  plugin user.

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

AC5 cannot be met in shipped configs before the binary is distributed. The plan sequences the
cutover behind that (S5), and operator ruling Q1 covers it.

## 4. Credential and network boundary (for the AC4 pre-review)

Scope: `toolkit mcp-proxy` as one process launched by an MCP client, with stdio as its only
input and output. It **listens on no socket**.

### 4.1 Credentials read, and how

| Source | How | Notes |
| --- | --- | --- |
| SDK default chain (`SdkCredentialsProvider::default_chain`) | env (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`), shared `config`/`credentials` (profile from `--profile` or `AWS_PROFILE`, validated, #4473), SSO and `aws login` token cache, `credential_process`, assume-role, web identity, ECS/EKS container role, IMDS | same sources as botocore. The profile selects **credentials only**; the signing region comes from section 2.3 rules |
| `--header K=V` values | argv | treated as secrets (agentaccess streaming URL is a bearer credential) |
| nothing else | — | no flag accepts a key or token (`reject_credential_arguments`); no file is written |

Decisions:

- **D1. Refresh.** Upstream re-reads disk on every request. The SDK chain caches and refreshes
  before expiry. We keep the SDK cache and add one forced reload: on a 401/403 or a credentials
  failure, rebuild the provider chain once before failing. A user who runs `aws sso login`/`aws
  login` mid-session therefore recovers without a restart, as with upstream. Assumption: an
  account switch by editing the profile without expiring the old credentials applies after the
  next auth failure, not immediately. This is recorded as an intentional difference.
- **D2. `credential_process` stdin.** The child **must not inherit the MCP stdin**: it would
  consume or block protocol bytes, which is the upstream patch's reason. aws-config 1.13.0's
  `CredentialProcessProvider` must be checked for its stdin setting in S1. If it inherits, the
  proxy wraps `credential_process` in its own provider that spawns with `Stdio::null()`.
  Test T-CRED-4 is required either way.
- **D3. `--skip-auth`.** Only `CredentialFailure::NotLoaded` ("no provider found anything")
  downgrades to unsigned. Expired, provider-error, timed-out or invalid-config failures fail
  closed with the fixed credentials message. They never go unsigned, which matches upstream:
  provider errors raise.
- **D4. Role session name.** The SDK default is used. Upstream's `mcp-proxy-for-aws-<epoch>`
  CloudTrail label is not reproduced. This is recorded as a compatibility note (Q5).

### 4.2 Where credentials flow; no logging or echo

- Access key, secret and token exist only in `Credentials` (redacting `Debug`) and inside the SDK
  signer for the duration of one `sign()` call. The only bytes derived from them that leave the
  process are:
  - the `Authorization` header: access key id in the credential scope plus an HMAC signature;
  - `x-amz-security-token`;
  - `x-amz-date`.

  These go in a TLS request to the single validated endpoint.
- Nothing credential-derived is written to stdout. stdout carries only JSON-RPC messages
  **received from the endpoint or generated by the proxy from fixed templates**.
- stderr diagnostics:
  - each line passes through `redact_text` and is length-bounded with `snippet`;
  - signing and credential failures carry their kind only (#4594), never provider text, so a
    `credential_process` stderr is never shown;
  - `Headers`/`HttpRequest`/`HttpResponse` `Debug` already hides `authorization`,
    `x-amz-security-token`, cookies and API keys. The proxy adds every `--header` name to its
    hide list, which needs a small `toolkit-aws` extension: a per-instance sensitive-name set,
    as with upstream `register_sensitive_headers`.
- **Bodies are never logged** at any level. This is a deliberate tightening: upstream logs error
  bodies at `WARNING` and payloads at `DEBUG`. `--log-level DEBUG` logs method, JSON-RPC id and
  method name, status, byte counts and timing only.
- Error messages returned to the MCP client (section 4.6) are fixed templates plus the tool
  name. They never contain credentials, signed headers, or upstream bodies beyond what the
  server itself sent as a JSON-RPC error object.

### 4.3 TLS

- rustls with the aws-lc provider via `aws-smithy-http-client`. Certificates are always verified
  against the platform trust store (`rustls-native-certs` 0.8.5 in `Cargo.lock`). There is no
  `--insecure` / `verify=False` flag, and none will be added.
- https only (`require_https`), no userinfo, no redirects (a 3xx is an error, not followed).
- **No HTTP(S) proxy is used.** Upstream's `httpx` honours `HTTPS_PROXY`, `SSL_CERT_FILE` and
  `SSL_CERT_DIR`. Corporate users behind an intercepting proxy or a private CA will break unless
  this is ruled on (Q4). Proposal: support `HTTPS_PROXY` (CONNECT tunnel, TLS still verified
  end-to-end against the AWS host) and `AWS_CA_BUNDLE`/`SSL_CERT_FILE` as extra roots in a later
  slice; not in the first one.

### 4.4 Allowed endpoint set (no arbitrary URL forwarding)

- The endpoint is a positional argv value, validated once at start by `validate_endpoint(url,
  "endpoint", &EndpointPolicy::aws_public(), resolver)`:
  - https;
  - host suffix `.api.aws` or `.amazonaws.com`;
  - every DNS answer public;
  - re-checked at connect time and on the socket peer.
- The proxy sends requests to **exactly that URL**, path and query unchanged, and to nothing
  else:
  - no URL is ever taken from stdin, from a server response, from SSE `retry`/`id` fields or from
    a header such as `Location`;
  - server-supplied URLs inside tool results (for example `aws___get_presigned_url`) are opaque
    data passed to the client.
- Intentional narrowing vs upstream:
  - non-AWS hosts are refused;
  - `http://localhost` is refused;
  - `.amazonaws.com.cn` is refused (not in `DEFAULT_ALLOWED_SUFFIXES`);
  - private-address answers are refused, so VPC-endpoint (PrivateLink) DNS is refused.

  Q6 asks whether China partitions or PrivateLink must be supported.
- Methods: only `POST` (messages), `GET` (optional server stream, `Accept: text/event-stream`)
  and `DELETE` (session end) to that URL.

### 4.5 Size and resource bounds

The proposed constants, all in one module and tested at the boundary:

| Bound | Value | On violation |
| --- | --- | --- |
| stdin message (one line) | 8 MiB | JSON-RPC error `-32600` to the client if an `id` can be recovered, otherwise a stderr diagnostic; the line is discarded and the loop continues |
| HTTP `application/json` response body | 32 MiB, read incrementally | stream aborted; request answered with JSON-RPC error `-32603` "response too large" |
| one SSE event (`data:` lines joined) | 32 MiB | same |
| SSE line length (before the event bound) | 32 MiB | same |
| JSON nesting | 128 (`MAX_JSON_DEPTH`) | treated as malformed |
| `Mcp-Session-Id` | 1-256 bytes of visible ASCII 0x21-0x7E (MCP spec) | session refused, error to client |
| response header count and size | hyper's header limits; S1 pins the exact values with a test | connect error |
| concurrent in-flight requests | 16 | JSON-RPC error `-32603` "too many concurrent requests" (never silently queued without bound) |
| stdout queue | bounded channel (64 messages); the backpressure blocks readers of that upstream stream | — |
| connect / idle-read / write / overall request timeout | 60 / 120 / 180 / 180 s defaults, CLI-compatible flags, each 1 s-1 h (`Timeouts`) | timeout error |
| tool call | 300 s (`--tool-timeout`) | in-flight request cancelled; `CallToolResult{isError:true}` |

SSE streams with steady bytes but no end are bounded by `--tool-timeout` for `tools/call` and by
`--timeout` for other requests. The optional `GET` stream is bounded only by idle-read and is
reopened at most once per 30 s.

### 4.6 Threat model: malicious or compromised MCP server response

The endpoint is AWS-operated, but its responses are treated as untrusted input. They may also
carry attacker-influenced content, such as documentation text or the output of a resource the
user reads.

| Threat | Control |
| --- | --- |
| Exhaust memory with an endless body, a huge SSE event or deep JSON | section 4.5 bounds; incremental reading; depth limit |
| Break stdio framing: raw newline or control bytes in a message so one SSE event becomes two JSON-RPC lines, or a non-JSON line reaches the client | every message from upstream is **parsed and re-serialised compactly** (`to_compact_string`) before it reaches stdout; non-JSON or non-object-or-batch data is dropped with a diagnostic; one writer thread writes whole lines |
| Redirect the proxy to another host (3xx `Location`, SSE `retry`, links in results) | no redirects; the endpoint is fixed; no field of a response is used as a URL |
| Inject headers through `Mcp-Session-Id` (CR/LF) or exfiltrate via an oversized id | the session id is validated to the spec charset and length before it is stored, and refused otherwise (`Headers::insert` also refuses control characters) |
| Make the proxy sign a request of the server's choosing | the proxy signs only bodies it read from the client (plus `_meta` injection); a response never causes a new upstream request except a `GET` stream reopen to the same URL |
| Cookies or other state (`Set-Cookie`) | ignored; no cookie jar |
| Spoof responses to ids the client never sent, or answer a request twice | forwarded as-is. The client owns id correlation and the proxy keeps no state that a spoof could corrupt; the proxy's own pending-request table ignores unknown ids for timeout bookkeeping |
| Server-initiated requests (sampling, elicitation, roots/list) | forwarded to the client, which decides. The proxy has no capabilities of its own and cannot be made to act |
| Lie in `readOnlyHint` to get a write tool through `--read-only` | same as upstream: the filter trusts annotations and is a convenience, not an authorization boundary; IAM is the boundary (stated in `--help`) |
| Prompt injection in tool results | out of scope for a transport. The proxy passes content unchanged, as upstream does |
| Leak secrets through error text the proxy prints | stderr is `redact_text`-scrubbed and bounded; upstream bodies are never logged; JSON-RPC error objects from the server are forwarded only to the client, which already receives the server's data |
| Downgrade to plain HTTP or a private address (DNS rebinding) | https required at start and per request; DNS guard at connect; peer check after connect (#4593) |
| TLS interception | certificate verification is always on; no opt-out flag |

### 4.7 How offline tests prove each property

There is no live AWS and no network: the build and test jobs run offline. The fixtures are
recorded or hand-written HTTP exchanges served by a **fake `StreamingTransport`**. For TLS and
socket behaviour, an in-process **loopback test server** is used through `SdkTransport::with_connector`
and a test connector, as the #4463 tests already do. The proxy runs in-process with injected
stdin/stdout/stderr buffers and a fixed clock, and the binary-level tests drive `toolkit
mcp-proxy` through pipes against the fake transport selected by a test-only constructor.

| Property | Test |
| --- | --- |
| SigV4 correctness | T-SIG-1: AWS SigV4 test-suite known-answer vectors through `SigV4Signer` with a fixed clock (shared with #4582); T-SIG-2: a proxied `initialize` POST signed for `aws-mcp`/`us-east-1` gives a byte-exact `Authorization` header against a recorded fixture; T-SIG-3: session token gives `x-amz-security-token`; T-SIG-4: `_meta` injection happens **before** signing (the signed payload hash equals the hash of the sent body) |
| service and region resolution | T-SCOPE: table test of `signing_service_region` plus `--service`/`--region`/`AWS_REGION` precedence, including aidevops, agentaccess-mcp and bedrock-agentcore hosts, and the error when nothing yields a region |
| credential flow and refresh | T-CRED-1: fake provider counts loads: one load per attempt, and a reload after a recorded 403; T-CRED-2: `--skip-auth` and `NotLoaded` give an unsigned request with no `authorization` header; T-CRED-3: `--skip-auth` and an expired or provider error give a fail-closed credentials error, not unsigned; T-CRED-4: a fake `credential_process` script that reads stdin and sees EOF (proves `Stdio::null`); T-CRED-5: a credential flag in argv is refused |
| nothing logged or echoed | T-LEAK: run every fixture (success, 4xx with body, 5xx, timeout, malformed SSE, credential failure) with canary credentials (`AKIDCANARY…`, a secret and token canary, a `--header` canary) and `--log-level DEBUG`; assert no canary and no upstream body appears in stderr or in any stdout message the proxy generated |
| TLS verification | T-TLS-1: loopback TLS server with a self-signed certificate gives a connect failure and no request body read; T-TLS-2: `http://` endpoint refused at startup; T-TLS-3: 301/307 response is not followed (the fake transport records exactly one request) |
| endpoint allowlist | T-EP: refused: non-AWS host, `localhost`, IP literal, `.amazonaws.com.cn`, private DNS answer, userinfo, a different URL in a response `Location`, an SSE `retry`/`id`, a tool result. Accepted: the four documented host shapes. The fake transport asserts every request URL equals the configured URL |
| size bounds | T-SIZE: one test per row of section 4.5 at the limit (accepted) and limit+1 (refused, loop continues, next message still served) |
| framing | T-FRAME: SSE event whose JSON string contains `\n` escapes and ` `, multi-line `data:`, comments, CRLF, an event split across chunks, a batch array, and a non-JSON event; stdout always has one valid JSON object per line |
| session lifecycle | T-SESS: session id captured from `initialize`, sent afterwards, invalid id refused; stdin EOF sends exactly one `DELETE` with the id (405 tolerated); `GET` 405 tolerated silently; 404 on a later POST gives a session-expired error to the client |
| timeouts and errors | T-ERR: 401/403 on `tools/call` gives the fixed credentials text with `isError:true`; 5xx gives the fixed "Tool call '<name>' failed: HTTP 503 … Please retry."; non-tool request failure gives JSON-RPC `-32603`; tool timeout fires at the configured duration with a fake clock; `--retries N` retries connect and 5xx/429 with fresh signatures (the fake signer counts) and never more than N |
| read-only | T-RO: `tools/list` filtered by truthy `readOnlyHint`; a call to a filtered tool is refused with upstream's exact text and never sent upstream |
| CLI compatibility | T-CLI: each of the six plugin argument vectors and the documented aidevops vector parse to the expected config; unknown upstream flags (`--profile A B`, `--disable-telemetry` before support) give a usage error, not silent acceptance |
| config shape | the existing `toolkit manifests`/`toolkit spec` validation (stdio `command` is a bare name) plus a new contract test that no tracked `.mcp.json`/`mcp.json` uses `uvx`, `uv`, `python` or `mcp-proxy-for-aws` (AC5 gate) |

## 5. Implementation plan

Slices are ordered by dependency. Each slice keeps the Python/`uvx` configs untouched until S5.

- **S1: streaming seam and SDK wiring** (toolkit-aws; shares the signer and transport wiring with
  #4582)
  - Add a `StreamingTransport` trait to `toolkit-aws`: `open(&HttpRequest) -> ResponseStream`
    with status, headers and incremental chunks under an idle-read timeout and a byte cap.
  - Implement it on `SdkTransport` (same guards), plus a fake in `testing`.
  - Move `signing_service_region` from `asm_exec::mcp` into `toolkit-aws`.
  - Add a per-instance sensitive-header set.
  - Verify the aws-config `credential_process` stdin behaviour (D2) and add a null-stdin provider
    if needed.
  - Add a `toolkit-cli` feature (`aws-sdk`) that enables `toolkit-aws/sdk`, and swap asm exec's
    `Unavailable*` for the real adapters when it is on (closes #4582 item 1).
  - Tests: T-SIG-1, T-TLS-*, T-CRED-4, the transport parts of T-SIZE.
- **S2: protocol core** (toolkit-cli `mcp_proxy` module, no network)
  - The stdio reader and writer.
  - The JSON-RPC framing and re-serialisation.
  - The SSE parser.
  - The session state machine (`initialize` capture, `Mcp-Session-Id`, `GET` stream, `DELETE`
    on EOF).
  - `_meta` injection with upstream precedence.
  - The concurrency cap.
  - Error templates.
  - Tests: T-FRAME, T-SESS, T-SIZE, T-ERR against the fake streaming transport and fake signer.
- **S3: `toolkit mcp-proxy` command**
  - CLI parsing compatible with the used flags: `endpoint`, `--service`, `--region`, `--profile`
    (single), `--metadata`, `--header`, `--skip-auth`, `--read-only`, `--retries`, `--timeout`,
    `--connect-timeout`, `--read-timeout`, `--write-timeout`, `--tool-timeout`, `--log-level`.
  - `--disable-telemetry` and the User-Agent policy follow Q3.
  - Wires S1 and S2, plus the credential decisions D1-D3 and the read-only filter.
  - Tests: T-SCOPE, T-CRED-1/2/3/5, T-LEAK, T-EP, T-RO, T-CLI, and binary-level pipe tests over
    the fake transport.
- **S4: independent security review (AC4 completion)**
  - Opposite-family reviewer, exact SHA of S1-S3.
  - Checklist: section 4 of this document.
- **S5: cutover (blocked on Q1, distribution)**
  - Change the six files to `"command": "toolkit"` (or the staged `./bin/…` path Q1 picks),
    `"args": ["mcp-proxy", "https://aws-mcp.us-east-1.api.aws/mcp", "--skip-auth", "--metadata",
    "INSTALL_SOURCE=agent-toolkit-<plugin>"]`.
  - Rewrite the section 1.2 prose:
    - README Kiro/fx examples and prerequisites;
    - `setup-devops-agent` (check script, SigV4 and fallback configs, `uvx` rows);
    - `connecting.md` and `operate-on-aws/SKILL.md`;
    - startup-advisor README;
    - `kiro-project-init.md` and `mvsp.md`;
    - the devsecops README table.
  - Add the OAuth direct option for aws-mcp to the README and `connecting.md`.
  - Add the AC5 contract test.
  - The amazon-workspaces Python **library** example stays as customer code (it is a Python SDK
    integration sample, not a launch of this toolkit). The `uvx mcp-proxy-for-aws` CLI hint gets
    the `toolkit mcp-proxy … --service agentaccess-mcp --header …` equivalent.
  - Folded into or coordinated with cutover #4414.
- **S6: `iam-policy-autopilot` (manager ruling item b)**
  - Decide between a published non-Python binary and the documented AWS-native alternative (IAM
    Access Analyzer `ValidatePolicy`/policy generation through `call_aws`).
  - Rewrite the 22 occurrences in both mirrors.
  - Separate research is needed: its distribution and its behaviour were not examined here.
- **Follow-ups, not needed for AC1-AC5 with today's configs:**
  - multi-profile (`--profile A B`, `AWS_MCP_PROXY_PROFILES`, `aws_profile` injection);
  - `HTTPS_PROXY`/custom CA (Q4);
  - China partition / PrivateLink (Q6);
  - automatic re-initialise on 404 session expiry.

Test plan summary: everything in section 4.7 runs in the default offline daemon test job. The
S1 SDK paths need the `aws-sdk` feature compiled, linted and tested by a daemon check (the #4577 gap). No test
contacts AWS. An optional manual smoke test against the live endpoint is operator-owned (Q2) and
never part of CI.

## 6. Open questions needing a PM or operator ruling

| # | Question | Kind | Proposed default |
| --- | --- | --- | --- |
| Q1 | How is the `toolkit` binary delivered to plugin users? Options: (i) per-platform binaries staged inside each plugin and referenced as `./bin/toolkit` (the #4487 approach for the hook binary; three plugins times N platforms of binary size); (ii) a GitHub/package-manager release that users install on PATH; (iii) both. S5 cannot land, and AC5 cannot be met in shipped configs, before this is decided. | **release** | (i) for plugins, sharing #4487's build matrix; (ii) for README/manual setup |
| Q2 | May an operator run one manual smoke test (unsigned `initialize` + `tools/list`, and a signed run with the operator's own credentials) against `aws-mcp.us-east-1.api.aws` before S5, to confirm skip-auth docs access and signing parity? | **credentials** (live AWS identity; no spending expected) | yes, operator-run, outside CI, no results stored beyond pass/fail |
| Q3 | Telemetry parity: upstream sends the MCP client's name and version in `User-Agent` unless `--disable-telemetry`, and the configs send `INSTALL_SOURCE` attribution in `_meta`. Keep both, keep only `INSTALL_SOURCE`, or tie them to the fork's telemetry consent (#4398)? | product/privacy | keep `INSTALL_SOURCE` (it is explicit in shipped config); `User-Agent` without client info unless the PM wants parity |
| Q4 | Corporate networks: support `HTTPS_PROXY` and custom CA bundles (upstream does via `httpx` `trust_env`)? The current `SdkTransport` refuses both by design. | product/security | follow-up slice after S5; documented as a known difference until then |
| Q5 | Accept the intentional differences: SDK credential caching with a reload on auth failure (D1), SDK role session name (D4), AWS-only endpoint allowlist, no body logging, the backend's own `initialize` result forwarded (not a re-served "MCP Proxy for AWS" server identity) | product | accept |
| Q6 | Must `.amazonaws.com.cn` endpoints or PrivateLink/private-DNS endpoints work? | product/security | no for the first release |
| Q7 | Should the plugin configs offer OAuth direct (option b) instead of the proxy for clients that support it, accepting the AC3 change for those users? | product | no in plugin configs; documented as the alternative in README and skills |
| Q8 | `iam-policy-autopilot` replacement path (S6): published binary, or an AWS-native alternative through the MCP server | product | research slice first |

Spending: none of the planned work spends money. Q1 may imply release infrastructure (CI
runners and storage for per-platform builds) and Q2 uses live credentials. Both are marked above.
