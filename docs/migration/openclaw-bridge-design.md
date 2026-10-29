# OpenClaw AgentCore bridge and setup: Rust port design (#4411)

Status: design, revision 1. No implementation code. Programme #4369, slice `payments-bridge`,
wave 3. Dependencies: #4379 (shared AWS client seams, landed) and #4410 (x402 payments; design
accepted by pre-review #5091 at `07d6a0b9`, `docs/migration/x402-payments-design.md`, cited
below as **X402**). Packaging follows #4487 (plugin-stage binary pipeline, landed on rolling at
`c27549a6`, `docs/migration/plugin-binaries.md`, cited as **PB**) and feeds #4414 (cutover and
release packaging).

Evidence base. Everything below was read, and nothing was executed: no Python, no network, no
payment, no AWS call and no `npm` command.

- This repository at `07d6a0b9` (the accepted #4410 design commit on rolling `fa3fa137`):
  `plugins/aws-agents/skills/agents-pay/packages/openclaw/**` (every tracked file),
  `agents-pay/SKILL.md`, `references/openclaw-setup.md`, `references/operator-guide.md`,
  `scripts/agents_pay_admin.py` (`build_openclaw_config`, `cmd_setup_openclaw`) and
  `requirements.txt`.
- Rolling `c27549a6`, read with `git show` only: `docs/migration/plugin-binaries.md`,
  `packaging/plugin-binaries.json`, `docs/migration/ci-gates.md`, `docs/migration/telemetry.md`,
  the `Makefile` and `crates/toolkit-cli/src/registry.rs`.
- `docs/migration/aws-core-contracts.md` (#4379) for the shared seams.
- The botocore 1.43.108 AgentCore model, read as JSON data with `jq`
  (`botocore/data/bedrock-agentcore/2024-02-28/service-2.json`, sha256
  `6ae4e07026769e280e7e076cf3096f7291424e99da95e046599a621001f9a6c5`, the same file X402 read),
  and botocore's `utils.py` and `compat.py` (timestamp parsing), read as text.
- Issues #4411, #4410 (both handoffs), #5103, #4487, #4414 and #4379.
- Golden token vectors in section 4.7 were computed with coreutils `printf | sha256sum` from the
  material definitions. The method reproduces X402's first vector (`c4293dfd…`) exactly.

## 0. Summary

| Question | Decision |
| --- | --- |
| What stays in TypeScript | Everything the OpenClaw host loads: the plugin entry, both tools, config loading, the TS policy gate, SSRF checks, the merchant probe, the paid replay and the tool output. `x402.ts` and `config.ts` do not change |
| What moves to Rust | `runtime/agentcore_bridge.py`, the only Python on the runtime path, becomes the route `toolkit agents-pay bridge`. The Python venv, `pip` and the Python admin CLI leave the OpenClaw install and setup |
| The seam (AC2) | **A per-call subprocess with the same pipe contract** as the Python bridge: one JSON request on stdin, one JSON line on stdout, exit 0/1/2, nothing on stderr. `bridge.ts` keeps its caps, timeout, kill and zeroization, and changes only the program it spawns (section 3) |
| The spending path | The bridge **does not sign a payload chosen by the caller.** It re-authorizes the offer TS selected through #4410's `policy::authorize`. The only route to `ProcessPayment` stays `Approved` → `agentcore::process_payment` → `proof::verify`, so there is no second payment path and no new live constructor (section 4.4, R-BR-ONE) |
| Request, error and pipe contracts (AC1) | Preserved: framing, the 128 KiB limits, exact-key strictness, field rules, the `{"ok":…}` envelope, the two error texts and their exit codes, and the silent stderr. `get_payment_session` is unchanged on the wire. `process_payment` loses `payload` and `client_token` and gains `request_url`, `accepted`, `resource` and `policy` (D-OC2). Section 8 records every change |
| Idempotency (X402 Q14) | OpenClaw adopts R-TOK. TS `deriveClientToken` is removed, and the CLI and the plugin now derive the same token for the same purchase (D-OC4) |
| Packaging without Python (AC3) | The npm/ClawHub package ships `toolkit` (built with `agents-pay-live`) under `bin/`, staged by plugin-stage through a new **package destination** with role `subprocess`. Setup runs `bin/toolkit agents-pay admin setup-openclaw` (#4410 S6) instead of a venv (section 6) |
| Tests (AC3 of #4410, applied here) | All offline. They use fakes and the shared fixture namespace, and add no live constructor. A build without `agents-pay-live` cannot reach AWS (section 10) |
| Long-running stdio (#5103) | Not used now. The bridge protocol module is shaped so that #5103's `serve` can reuse its request validation and limits. `VerifiedProof::into_trusted_glue` is the one primitive both need (section 3.4) |

## 1. Inventory (AC1)

### 1.1 Files under `plugins/aws-agents/skills/agents-pay/packages/openclaw/`

| File | Lines | Role | Callers | Owner after the port |
| --- | --- | --- | --- | --- |
| `runtime/agentcore_bridge.py` | 149 | Bounded JSON bridge for `GetPaymentSession` and `ProcessPayment` through boto3 | `src/bridge.ts` (spawn), `test/test_agentcore_bridge.py` (import and subprocess) | **Rust**: `agents_pay/bridge/` (B1, B2). The Python file is retired by the programme retirement step (#4415 pattern) |
| `test/test_agentcore_bridge.py` | 189 | unittest: 5 tests of request mapping, strictness and the stdin error path | `npm test` (`python3 -m unittest discover`) | Rust tests T-BR-* (section 10.1) |
| `test/test_setup_openclaw.py` | 99 | unittest: 4 tests of `agents_pay_admin` (`usd_to_atomic`, `build_openclaw_config`, `discover_deployed`, `format_duration`), loaded from the **staged** copy `skills/agents-pay/scripts/` | `npm test` | Ported by **#4410 S6** (X402 Q12). #4411 cites them as T-S6-OC-1..4 and retires the file |
| `src/bridge.ts` | 167 | Spawns the helper with `shell: false`, enforces caps and the timeout, parses the envelope | `src/payments.ts` (2 call sites); `test/security.test.mjs` (`parseBridgeResponse`) | TS, adapted (R-TS-1) |
| `src/payments.ts` | 172 | `getPaymentSessionStatus` (status maths on the session), `deriveClientToken`, `processPayment` (request build, proof extraction) | `src/index.ts`; `test/security.test.mjs` (`deriveClientToken`) | TS, adapted (R-TS-2). `deriveClientToken` is removed (D-OC15) |
| `src/index.ts` | 166 | Plugin entry `definePluginEntry`; tools `get_payment_session_status` and `get_paid_content`; `paidFetch` orchestration; `buildPaidContentResult` | OpenClaw host (`openclaw.extensions: ./dist/index.js`); tests | TS, one call changes (R-TS-3) |
| `src/x402.ts` | 487 | SSRF guard, hardened HTTPS GET (64 KiB cap, 15 s, no redirects, DNS-answer check), challenge extraction, TS policy, envelope, `validAfter` wait, replay | `src/index.ts`; tests | TS, **unchanged** |
| `src/config.ts` | 180 | Trusted config: plugin settings, else a protected `~/.x402/config.json`; `normalizeConfig` | `src/index.ts`, `src/payments.ts`; tests | TS, **unchanged** |
| `test/security.test.mjs` | 518 | node:test, 20 tests: policy, SSRF, token, bridge envelope, header, manifest, package identity, bundled skill, lockfile, `returnBody` | `npm test` | JS, adapted (R-TS-6). The token test moves to Rust |
| `test/fixtures/v2-security-contract.json` | 50 | Shared challenge/config fixture | `security.test.mjs` | Also read by the Rust parity tests (T-OCP-PAR) |
| `scripts/stage-skill.mjs` | 29 | Copies `SKILL.md`, `requirements.txt`, `references/`, `scripts/` into `skills/agents-pay/` | `npm run stage-skill`, `prepack`, `npm test` | JS, adapted: stages `SKILL.md` and `references/` only (R-PKG-6) |
| `package.json` | 50 | Package identity 1.0.7, scripts, `files` (includes `runtime/agentcore_bridge.py`) | npm, ClawHub, tests | Adapted (R-PKG-5) |
| `openclaw.plugin.json` | 76 | Manifest: two tools, the bundled skill, `configSchema` | OpenClaw host; tests | **Unchanged** |
| `README.md`, `PUBLISHING.md` | 105, 102 | Product and release docs; README states the Python bridge | humans; README is pinned by a test | Adapted (B5) |
| `skills/agents-pay/SKILL.md` | 548 | Tracked, byte-identical staged copy | OpenClaw skill loader | Re-staged after #4410 S7 changes the canonical text |
| `package-lock.json`, `tsconfig.json`, `.gitignore` | — | Build inputs | npm, tsc | `.gitignore` gains `bin/` (R-PKG-3) |

Outside the package, the OpenClaw path is documented in `agents-pay/references/openclaw-setup.md`
(venv, `pip install -r requirements.txt` and `python scripts/agents_pay_admin.py …`), in
`SKILL.md` ("OpenClaw and other agent hosts", which runs `python3 scripts/agents_pay_admin.py preflight`) and
in `references/operator-guide.md` ("OpenClaw"). `agents_pay_admin.py setup-openclaw` and
`build_openclaw_config` are #4410 S6's. No other JS/TS caller of the bridge exists: `git grep`
for `agentcore_bridge`, `invokeAgentCoreBridge` and `.venv` outside the package finds only these
docs and `inventory-audit.md`/`python-inventory.json`.

### 1.2 The runtime call graph (current)

```
OpenClaw host
 └─ dist/index.js definePluginEntry(api)
     ├─ get_payment_session_status ── ensureConfig ── payments.getPaymentSessionStatus
     │                                                  └─ bridge.invokeAgentCoreBridge({operation:"get_payment_session",…})
     └─ get_paid_content(url) ── ensureConfig ── paidFetch(url)
          1 x402.probeUrl(url)                 TS HTTPS GET, SSRF, cap, no redirect
          2 status != 402 → refused metadata
          3 x402.extractChallenge(probe)       header (base64, raw) then body; v2 only
          4 x402.validateChallengePolicy(…)    TS policy: first approved entry
          5 x402.buildProcessPaymentPayload(accepted)
          6 payments.processPayment("2", payload, url)
               └─ deriveClientToken(session, url, payload) ; bridge process_payment
          7 x402.sleepPastValidAfter(signed)   ≤ 15 s
          8 x402.buildPaymentPayloadEnvelope(resource, accepted, signed)
          9 x402.replayWithHeader(url, envelope)   one replay with PAYMENT-SIGNATURE
         10 buildPaidContentResult(result, config) | refusal object
```

The Python bridge sits only on the two `invokeAgentCoreBridge` edges. The model-facing tool output
is built in TS and never contains the proof or the bridge's stdout.

### 1.3 The bridge request contract (`agentcore_bridge.py`)

The request is one JSON object.

| Rule | Python behaviour |
| --- | --- |
| Size | `stdin.buffer.read(131073)`. Empty, or more than 131072 bytes, → input error |
| Parse | `json.loads(bytes)`, so the encoding is detected (UTF-8, a UTF-8 BOM, UTF-16/32), duplicate keys take the last value, and `NaN` and `Infinity` are accepted. A non-object → input error |
| `operation` | `get_payment_session` or `process_payment`; anything else → input error |
| Exact keys | `set(request) == expected`, with no missing and no extra key. Common: `operation`, `region`, `payment_manager_arn`, `payment_session_id`, `user_id`, `agent_name`. `process_payment` adds `payment_instrument_id`, `client_token`, `version`, `payload` |
| String fields | `_required_string`: `str`, `value.strip()` non-empty (Python Unicode whitespace), `len(value)` (code points) ≤ the limit. Limits: `region` 128; `agent_name` 256; `user_id` 512; `version` 8; `client_token` 128; the rest 2048 |
| Order of checks | region, agent name, manager ARN, session id, user id, then for payments: version (must be `"2"`), client token (exactly 64 lower-case hex), payload (a `dict`), instrument id |
| Mapping | `boto3.client("bedrock-agentcore", region_name=region)`; `get_payment_session(paymentManagerArn, paymentSessionId, userId, agentName)`; `process_payment(… , paymentInstrumentId, clientToken, paymentType="CRYPTO_X402", paymentInput={"cryptoX402":{"version", "payload"}})` |

`agent_name` is caller-chosen; TS always sends `openclaw-aws-agents-pay`. boto3 validates
model `min`/`max` lengths client-side (for example `UserId` ≤ 120, `PaymentAgentName` ≤ 256), not
patterns. A violation raises `ParamValidationError`, which lands in the generic branch (exit 1).

### 1.4 The bridge result and error contract

| Outcome | stdout (one line; `print` appends `\n`) | Exit | stderr |
| --- | --- | --- | --- |
| Success | `{"ok":true,"result":<the boto3 response dict>}`, `json.dumps(separators=(",",":"))`, `ensure_ascii`; `datetime` → `isoformat()`, `Decimal` → `str`, `bytes` → base64 | 0 | empty |
| Input error (`BridgeInputError`, `JSONDecodeError`) | `{"ok":false,"error":"invalid bridge request"}` | 2 | empty |
| Any other exception: AWS, credentials, parameter validation, serialization | `{"ok":false,"error":"AgentCore request failed"}` | 1 | empty (`# AWS and credential details must not enter OpenClaw output or logs`) |
| `KeyboardInterrupt`, `SystemExit`, interpreter start failure | traceback or nothing | non-zero | possibly non-empty |

Details of the success result that matter for compatibility:

- botocore's rest-json parser builds each structure **in model member order** and drops unknown
  wire members. It also adds a `ResponseMetadata` member (request id, HTTP status, **all HTTP
  response headers**, retry count) to every response. The bridge returned all of it.
- `DateTimestamp` is `timestampFormat: iso8601`. botocore's `parse_timestamp` first tries
  `float(value)`; for an ISO string that fails, and `dateutil.parser.parse` keeps the wire offset.
  `Z` becomes `tzutc()`, so `isoformat()` prints `+00:00`. The fraction is kept to microseconds
  and printed only when non-zero (`.123000`). Only a numeric value would be read in `tzlocal`
  (`compat.get_tzinfo_options`).
- `process_payment` returned the whole `ProcessPaymentResponse`: ids, `status`, timestamps and
  `paymentOutput.cryptoX402{version, payload}`, where `payload` is the signed proof (a `PaymentDocument`,
  `"sensitive": true`). TS reads only `paymentOutput.cryptoX402.payload`.

### 1.5 The pipe ("streaming") contract

The bridge has no incremental streaming. Each call is one process: TS writes the whole request
and closes stdin, and Python writes one document at the end. The contract that `bridge.ts`
enforces, and that the Rust side must satisfy, is:

| `bridge.ts` rule | Value | On violation |
| --- | --- | --- |
| request size before spawn | 131072 bytes | `"AgentCore bridge request exceeded its size limit"`, no spawn |
| helper and interpreter present | `access(R_OK)` on the script, `access(X_OK)` on the venv python | the promise rejects (an `ENOENT` error object reaches `paidFetch`'s catch) |
| spawn | `spawn(python, [script])`, `cwd` the package root, `env` the full `process.env` plus `PYTHONDONTWRITEBYTECODE=1` and `PYTHONNOUSERSITE=1`, `shell: false`, `windowsHide` | `"AgentCore bridge could not start"` |
| stdout | ≤ 131072 bytes, buffered, zero-filled after use | `"…response exceeded its size limit"`, SIGKILL |
| stderr | ≤ 16384 bytes, discarded | `"…diagnostics exceeded their size limit"`, SIGKILL |
| wall clock | 30 s | `"AgentCore bridge timed out"`, SIGKILL |
| stdin write error | — | `"AgentCore bridge input failed"`, SIGKILL |
| exit code ≠ 0 | — | `"AgentCore bridge request failed"` (stdout discarded) |
| stdout not `{"ok":true,"result":<object>}` | — | `"AgentCore bridge returned an invalid response"` |

Every one of these is a TS-internal message. `payments.ts` turns a status failure into
`{usable:false,status:"error",…}`; `paidFetch` turns a payment failure into `{paid:false,
refused:true,reason:"Payment flow failed safely without exposing provider or proof details"}`.
So **neither bridge exit code nor its error text is model-visible**. They matter only for
diagnosis and for tests.

### 1.6 Setup and install (current, Python)

1. `openclaw plugins install clawhub:@aws/aws-agents-pay`. The package carries
   `runtime/agentcore_bridge.py` and the staged skill (with `scripts/*.py` and `requirements.txt`).
2. `cd ~/.openclaw/extensions/aws-agents-pay/skills/agents-pay`, then `python3.11 -m venv .venv` and
   `pip install -r requirements.txt` (22 pinned packages, including boto3 1.43.63 and
   bedrock-agentcore 1.19.0).
3. Run `python scripts/agents_pay_admin.py setup-openclaw --project-dir …` (or `init-config`,
   `create-instrument`, `new-session`). It prints the plugin JSON for `~/.openclaw/openclaw.json`.
4. At run time `bridge.ts` spawns `skills/agents-pay/.venv/bin/python` (on Windows,
   `.venv\Scripts\python.exe`). Without the venv every payment and status call fails closed.

## 2. The constraint #4410 puts on the bridge

X402's R-SPEND-ORDER makes spending a type property:

- `Approved` is built only in `policy::authorize`;
- `agentcore::process_payment(api, Approved)` consumes it by value;
- `ProcessPaymentRequest::from_approved` is the only request builder;
- `derive_client_token` is the only token producer on the runtime path (R-TOK-NOFRESH);
- the proof leaves `proof::verify` only as a `VerifiedProof`.

`syn` gates (T-ORDER-CTOR) and `compile_fail` tests pin all of this.

The Python bridge's `process_payment` takes a **caller-chosen** `payload` and `client_token` and
signs them. A Rust drop-in with that request shape would need a second request builder, or a
second way to make an `Approved`, from caller data. That is the second payment path the brief
forbids, and it would break T-ORDER-CTOR. **So the bridge request has to change for
`process_payment`.** The question is how far, which section 3 decides.

The OpenClaw policy (TS) and the CLI policy (Python semantics, R-POL) are close but not
identical: the cap is atomic units against USD, assets are per network with defaults, v2 only,
and the resource binding is mandatory. Section 4.3 maps one onto the other, so that the one Rust
gate can judge an OpenClaw request without a second policy implementation.

## 3. The JS/TS → Rust seam (AC2)

### 3.1 Options

| # | Seam | Spending path | JS/TS change | Contract change | Verdict |
| --- | --- | --- | --- | --- | --- |
| A | Drop-in `toolkit agents-pay bridge` with the Python request shape (`payload`, `client_token`) | **second path**: signs a caller payload outside `Approved` | `bridge.ts` path only | none | **Rejected**: forbidden by the brief and by R-SPEND-ORDER |
| B | `get_paid_content` shells out to `toolkit agents-pay fetch <url>` (X402 S5); TS keeps only the status tool | the one S5 path | **rewrite**: `paidFetch` and the TS policy, network and replay become dead code; the tool output must be re-mapped from the CLI JSON; config moves from plugin settings to `~/.agents-pay/config.json`, or is passed through | large: CLI replay loop (up to 5, 1 s apart), CLI output keys, CLI config file, `return_body` semantics | Rejected for now. It is the "one runtime" end state, but it rewrites working TS and changes published tool behaviour (AC2) |
| C | **Per-call subprocess `toolkit agents-pay bridge`, same pipe contract, re-authorizing**. TS sends the offer it selected plus its policy; Rust runs `policy::authorize` → `process_payment` → `proof::verify` and returns the verified proof | the one path, reached through `flow::prove` | `bridge.ts` spawn target; `payments.ts` request fields; `index.ts` passes `resource` (about 40 lines) | `process_payment` request fields (D-OC2); stricter outcomes (D-OC3) | **Recommended** |
| D | Long-running `toolkit agents-pay serve` over JSON lines (#5103), shared by the plugin process | the one path | `bridge.ts` becomes a supervisor: start, restart, request ids, per-request deadlines, backpressure | new framing; proofs live in a long-lived process | Not now: #5103 has no protocol design yet ("record the trusted-caller protocol and limits before implementation"), and per-call start-up cost is not a problem (3.3) |
| E | N-API addon (`napi-rs`) loaded into the OpenClaw process | the one path | native module loading in TS | in-process ABI | Rejected: new FFI and dependencies, one `.node` per Node ABI and platform, and a crash takes the gateway down |
| F | WebAssembly module | — | — | — | Rejected: no credential chain, no sockets |

### 3.2 Recommendation: C

C keeps every TS line that does not touch the bridge. It keeps the pipe contract `bridge.ts`
already enforces, so caps, timeout, SIGKILL and zeroization stay as they are. It makes the
#4410 gate the final authority on every `ProcessPayment`, giving two independent gates: TS, then
Rust. It also lets the plugin and the CLI share one token derivation. The trust model does not
change: the bridge's caller is the plugin process, and the policy comes from its trusted
config, exactly as today (B-OC2).

### 3.3 Why per call, and how #5103 fits

- **Cost.** A Rust process start is shorter than today's Python start plus boto3 import, and
  each call already resolves credentials (boto3 did too). The payment path makes at most two
  bridge calls per tool invocation.
- **Secret lifetime.** A per-call process holds the proof only for one call. A shared server
  would hold every proof of the gateway's lifetime in one address space.
- **Fit with #5103.** The bridge's pure layer is `agents_pay::bridge::protocol`: request
  limits, exact-key validation, Python string rules and the error vocabulary. #5103's `serve`
  can reuse it per line (one request object per line, each ≤ 128 KiB, one response line). Its
  proof hand-off uses the same `VerifiedProof::into_trusted_glue` (3.4). Switching OpenClaw to
  `serve` later changes `bridge.ts` only, behind its own review (OQ5).
- **B1 is unaffected.** OpenClaw never used the Python import API or the browser handle: it
  replays in TS. The bridge offers only OpenClaw's two operations. It does not offer `fetch`,
  `prepare_browser_payment` or `attach_browser_payment`, so the import API stays unreplaced
  until #5103 (X402 B1).

### 3.4 The one new primitive in #4410's modules

`proof::VerifiedProof::into_trusted_glue(self) -> GlueProof`, with visibility
`pub(in crate::agents_pay)`. `GlueProof` wraps the signed payload in `toolkit_aws::redact::Secret`
and has a redacted `Debug`. Its only consumer is `bridge::render`, which serializes it into
the one designated stdout field (R-BR-OUT-3). A future #5103 `attach` would be the second
consumer. A `syn` gate pins the consumers (T-BR-ONE-4). It consumes a `VerifiedProof`, so an
unverified proof can never reach the pipe, which is the bridge's counterpart of "the replay
header only from a `VerifiedProof`".

## 4. Bridge requirements (Rust)

### 4.1 Route and framing

- **R-BR-ROUTE.** `toolkit agents-pay bridge`, registered in the `agents-pay` slice (X402 8.1
  `mod.rs`). It takes no flags and no positionals; extra argv is a router usage error (exit 2;
  TS never sends any). argv passes through `reject_credential_arguments`. The help text marks
  it "trusted plugin glue, not for agents". `docs/migration/agents-pay.md` documents it
  (`every_slice_documents_its_commands`).
- **R-BR-IN.** Read at most 131073 bytes from stdin, until EOF or that count, whichever comes
  first. Empty, or more than 131072 bytes → input error. Parse with
  `toolkit_core::json::parse_json`. Duplicate keys, `NaN`/`Infinity`, a BOM or a non-UTF-8
  encoding → input error (D-OC18). A non-object → input error. The bridge reads nothing else:
  no file, no `PAYMENT_*` or `AWS_REGION`, no terminal (R-BR-WORLD).
- **R-BR-OUT-1.** Exactly one document followed by `\n`, written once, at the end, with
  compact separators and `ensure_ascii` escaping: every non-ASCII scalar becomes `\uXXXX`, with
  astral characters as surrogate pairs. Float members render as Python `repr`. The emitter is
  the existing Python-JSON emitter used by X402 R-ADM-WRITE (`aurora::pyfmt`), extended with a
  compact mode there if it lacks one, not a second emitter. A document over 131072 bytes is
  replaced by the AgentCore failure document (exit 1), so the TS stdout cap is never what ends the call (D-OC13).
- **R-BR-QUIET.** The route writes **nothing** to stderr on any path. No tracing, no telemetry
  and no panic message: the route installs a silent panic hook and wraps its handler in
  `catch_unwind`, and a panic becomes the AgentCore failure document with exit 1. Python's
  generic `except Exception` has the same effect.

### 4.2 Requests

Field rules (R-BR-STR) reproduce `_required_string` exactly: a JSON string; non-empty after
Python `str.strip()`, whose whitespace set is the Unicode `White_Space` property plus
U+001C..U+001F (`str.isspace`), with the table added to X402's `pyval.rs`; length counted in
**code points**; the same per-field limits as Python (1.3). The value is passed on unstripped,
as in Python.

**`get_payment_session`** keeps the exact Python key set:
`{operation, region, payment_manager_arn, payment_session_id, user_id, agent_name}`.

**`process_payment`** has the key set
`{operation, region, payment_manager_arn, payment_session_id, payment_instrument_id, user_id,
agent_name, version, request_url, accepted, resource, policy}`. The Python `payload` and
`client_token` keys are **not accepted**: a request that carries them fails the exact-key rule
(T-BR-REQ-LEG, D-OC2).

| Field | Rule (all failures → input error, exit 2, zero AWS calls) |
| --- | --- |
| `region` | R-BR-STR, ≤ 128, then `toolkit_aws` `is_valid_region` (D-OC10) |
| `agent_name` | R-BR-STR, ≤ 256, and **equal** to `openclaw-aws-agents-pay`, the runtime agent name X402 pins (D-OC9) |
| `payment_manager_arn`, `payment_session_id`, `payment_instrument_id` | R-BR-STR, ≤ 2048 |
| `user_id` | R-BR-STR, ≤ 512 |
| `version` | R-BR-STR, ≤ 8, equal to `"2"` (Python: `"unsupported x402 version"`) |
| `request_url` | R-BR-STR, ≤ 2048. Its https, host, userinfo and DNS checks are `policy::authorize`'s (R-ORIGIN) |
| `accepted` | a JSON object: the one entry TS selected (`authorized.accepted`, unmodified) |
| `resource` | a JSON object whose `url` is an R-BR-STR string ≤ 2048. TS requires one, so a request without it is outside the contract |
| `policy` | a JSON object; section 4.3 |

Model length limits (`UserId` ≤ 120, `PaymentAgentName` ≤ 256, `ClientToken` 33..=256, and the
other `min`/`max` of the four members sent) are checked after these rules and before any
network I/O. A violation gives the **AgentCore failure** (exit 1, zero calls), which is
boto3's `ParamValidationError` result (R-BR-PARAM). Patterns are not checked, as in boto3.

The checks run in the Python order of 1.3, field by field, so the first failing field decides
the outcome. Both outcomes are the same fixed document, so the order is only observable through
"zero calls", which T-BR-REQ asserts.

### 4.3 The OpenClaw policy profile (`policy/openclaw.rs`, pure)

`Policy::from_openclaw(&Json) -> Result<Policy, BridgeInputError>` produces the **same `Policy`
type** that `policy::authorize` consumes for the CLI. It constructs no `Approved` and touches no
I/O. Input keys ⊆ `{networkPreferences, allowedOrigins, allowedRecipients, allowAnyRecipient,
allowedAssetsByNetwork, maxPaymentAmountAtomic}`; any other key → input error. `returnBody` is
TS-only output policy and is never sent.

| OpenClaw field (TS semantics) | R-POL field | Mapping |
| --- | --- | --- |
| `maxPaymentAmountAtomic`, `^[1-9][0-9]*$`, required; TS: `amount <= BigInt(max)` | per-payment cap | `CapDecimal { digits: max, exp: −6 }` (R-AMT-REP). R-AMT-CMP then gives k = 0, comparing `A` with `max` as digit strings: **exactly** TS's `amount <= max` for every asset, because both sides are atomic units. The string must match the regex and be ≤ 1000 digits (the D6 mantissa bound) |
| `allowedRecipients` (non-empty list of strings) xor `allowAnyRecipient: true` | `allowed_recipients` / `allow_any_recipient` | direct. Presence-based mutual exclusivity and the at-least-one rule are the same in both. A non-boolean `allowAnyRecipient` → input error |
| `networkPreferences`, absent or `[]` → `["eip155:84532"]` | `allowed_networks` | the list, or the TS default when absent or empty |
| `allowedAssetsByNetwork[n]`, a non-empty list, else TS `DEFAULT_ASSETS_BY_NETWORK[n]` (Base Sepolia USDC), else none | `allowed_assets` | for each allowed network: the configured list when non-empty, else the default entry, else `[]`. Members must be strings (D3 text unreachable: input error) |
| (TS: scheme must be `exact`) | `allowed_schemes` | `["exact"]` |
| `allowedOrigins`, each `assertHttpsUrl(o).origin.toLowerCase()` | `allowed_origins` | each entry must be https with a host and no userinfo (else input error, which matches TS, where `assertHttpsUrl` throws). It is then replaced by its R-URLPARSE `canonical_origin`, lower-cased. This keeps TS's "an entry with a path still names its origin" behaviour, which raw R-POL comparison would refuse (T-OCP-ORIG) |

The challenge passed to `authorize` is assembled from the request as
`{"x402Version": 2, "accepts": [accepted], "resource": resource}`. Passing **only the TS-selected
entry** means the Rust gate either approves exactly the offer TS will put in its envelope, or
refuses. The two gates can never sign different offers.

### 4.4 The flow (R-BR-FLOW) and the single spending path (R-BR-ONE)

`process_payment`:

1. R-BR-IN and 4.2 (pure) → input error, exit 2.
2. `Policy::from_openclaw` → input error, exit 2.
3. `Deps::for_process()` (X402 G1b), the **same** constructor as every agents-pay route. A build
   without `agents-pay-live` gets `Unavailable`, then the bridge's D32 document (R-BR-D32).
4. X402 G3, through the same check S5 uses, over the request's manager ARN and `request_url`. Test identities in a release build →
   input error, exit 2.
5. `flow::prove(&deps, ProveInput { url, challenge, policy, ids, purchase_id: "", deadline })`, which:
   - runs `policy::authorize` (R-ORIGIN with `deps` resolver and R-DNS-ADMIT; R-POL; R-AMT; R-VER; R-RESBIND; R-TOK) → `Approved`;
   - then `agentcore::process_payment(deps.payments, approved)` → `Authorized`;
   - then `proof::verify(authorized, deps.clock)` → `VerifiedProof`.
   This is the post-probe half that S5's `fetch` and `--browser-handle` already run. If S5 lands
   with that half inlined in `fetch`, B2 first extracts it in a refactor commit that changes no S5
   test. Pre-authorization refusals → input error, exit 2. Post-authorization failures → AgentCore
   failure, exit 1 (R-BR-ERR).
6. `into_trusted_glue` → render (R-BR-OUT-3) → exit 0.

`get_payment_session`: steps 1, 3 and 4 (the ARN only), then S4's `GetPaymentSession` reader
with the request's ids, region and the fixed agent name. No DNS lookup or policy runs, and the
result is rendered as R-BR-OUT-2.

Rules that keep this one path (each pinned by T-BR-ONE):

- **R-BR-ONE-1.** `bridge/` never names `Approved`, `Approved::new`, `ProcessPaymentRequest`,
  `agentcore::process_payment`, `derive_client_token` or `PaymentsApi::process_payment`. It
  reaches spending only through `flow::prove`.
- **R-BR-ONE-2.** `flow::prove` is the only caller of `agentcore::process_payment` crate-wide.
  `fetch` and the browser handle call `prove`.
- **R-BR-ONE-3.** `live.rs` gains **no** item. Its public item list is pinned by a test, so the
  bridge cannot add a live constructor. If S4/S5 bind the AgentCore region when the client is
  built, B2 changes the `PaymentsApi` methods to take the region per call. That is a signature
  change inside S4, with the CLI passing its resolved region, not a new constructor. Assumption A2
  states this.
- **R-BR-ONE-4.** `VerifiedProof::into_trusted_glue` has exactly one caller (`bridge::render`).

### 4.5 Deadline (R-BR-DL)

The bridge's operation deadline is **25 s** of the injected clock from the start of the handler,
5 s inside `bridge.ts`'s 30 s kill. It covers R-DNS-ADMIT, the credential chain and the
`ProcessPayment` logical call, whose deadline is `min(60 s, remaining)` per X402 R-AWS-RETRY.

- The bridge **never sleeps**. `validAfter` gets the X402 window check only: a value more than
  15 s away → `proof::verify` refuses, an AgentCore failure with exit 1 (post-authorization). The
  wait itself stays in TS (`sleepPastValidAfter`, unchanged).
- If S5 places the wait inside `proof::verify`, B2 splits it into `verify` (checks) and a flow
  wait step, without changing S5's tests.
- A deadline that passes before authorization is a pre-authorization refusal (exit 2). After it, an AgentCore failure (exit 1).

A SIGKILL from TS after `ProcessPayment` was sent loses the proof, not money. The next tool call
derives the same token (R-TOK), and the service returns the stored authorization within its
retention window (X402 R-TOK-SCOPE).

### 4.6 Output

- **R-BR-OUT-2 (`get_payment_session`).** `{"ok":true,"result":{"paymentSession":{…}}}`:
  - members in **model order** (`paymentSessionId`, `paymentManagerArn`, `limits{maxSpendAmount{value,currency}}`, `userId`, `expiryTimeInMinutes`, `createdAt`, `availableLimits{availableSpendAmount{value,currency}, updatedAt}`, `updatedAt`); absent optional members omitted; unknown wire members dropped;
  - no `ResponseMetadata` (D-OC7);
  - timestamps re-rendered as Python `isoformat()` of the dateutil-parsed value: the wire offset kept, `Z` → `+00:00`, the fraction cut to 6 digits and printed only when non-zero. A timestamp that is not an ISO 8601 string → AgentCore failure (Python would have raised; D-OC7 notes the numeric edge);
  - a missing **required** member (model `required`) → AgentCore failure, following the X402 6.1 rule (D-OC8).
- **R-BR-OUT-3 (`process_payment`).** `{"ok":true,"result":{"paymentOutput":{"cryptoX402":{"payload":<signed payload>}}}}`
  and nothing else (D-OC7). The payload is the X402 S4-normalised object: a JSON string holding
  an object is parsed, exactly as TS did. It appears **once**, at that path.
- **R-BR-SCRUB.** Before writing, the rendered document is checked with X402 R-SCRUB's matcher
  against the invocation's live secrets (the proof header-equivalent and `signature`, the session
  id and any AWS secret). For `process_payment` the designated payload path is exempt from the
  proof's own match but nowhere else. For `get_payment_session` the session id is exempt only as
  the `paymentSessionId` member (TS reads the session; this is today's result). Any other hit →
  AgentCore failure (exit 1). This catches a service response that echoes a credential.

### 4.7 Errors, exit codes and tokens

**R-BR-ERR.** The error vocabulary is the Python one plus one value:

| Class | stdout | Exit |
| --- | --- | --- |
| R-BR-IN, 4.2 rules, the 4.3 profile, G3, every X402 **T-ORDER-PRE** class (R-ORIGIN incl. DNS non-public or unresolvable, R-POL, R-AMT, R-VER, R-RESBIND, D4, D11, the deadline before authorization) | `{"ok":false,"error":"invalid bridge request"}` | 2 |
| R-BR-PARAM, credentials missing, any AgentCore error response (allowlisted code or not), transport failure, response shape or size, `status` ≠ `PROOF_GENERATED`, echo mismatch, every X402 **T-ORDER-POST** proof failure (R-PP-VERIFY `to`/`value`/`validBefore`, the `validAfter` window), the deadline after authorization, R-BR-OUT-1 cap, R-BR-SCRUB hit, panic | `{"ok":false,"error":"AgentCore request failed"}` | 1 |
| R-BR-D32: build without `agents-pay-live`, after a **valid** request | `{"ok":false,"error":"payment support unavailable"}` | 1 |

No text names a field, a value, a service code or a reason, as in Python. The pre/post split
equals X402's, so a test can use the exit code to tell zero `ProcessPayment` calls (2) from at
most one logical call (1). TS maps every non-zero exit to the same internal error, so
`get_paid_content` and `get_payment_session_status` show exactly what they show today.

**R-BR-TOK.** The bridge's `clientToken` is X402 R-TOK with an empty purchase id. TS no longer
sends a token. Golden vectors for the bridge fixture: session `payment-session-TEST00000000001`,
URL `https://merchant.test/pay/weather?city=sydney`, network `eip155:84532`, asset
`0x036cbd53842c5426634e7929541ec2318f3dcf7e`, payTo `0x1111111111111111111111111111111111111111`:

- amount `1000` → `e36c6d851f2fc37ad61d084f213d568e5c6920da19a3aa2675babb3bcff5c81d`;
- amount `1001` → `617369d56a4f010acec1db48579ab07eaf78887e3464e044a68c04c5b6162b3d`;
- for D-OC4's record, the **legacy TS** material (session, origin+pathname, scheme, network,
  asset, payTo, amount) for amount `1000` gave
  `7e14e05230713b1372f18d39af58f0b415132ed8b742ecad5d870cf0cfd619d3`.
- Changing `extra.nonce` or `maxTimeoutSeconds` leaves the token equal (R-TOK excludes them).

### 4.8 World and AC3 custody (R-BR-WORLD, R-BR-AC3)

- The bridge's world is X402's `Deps`, from `Deps::for_process()`. It uses only `payments`,
  `resolver` and `clock`. It never calls `ConfigStore`, `MerchantHttp`, `Terminal`,
  `DeployedState` or `AdminApi`, and a recording fixture proves each is untouched (T-BR-NOCFG).
- Credentials come from the S1 guarded chain (the boto3 sources). `AWS_REGION` and
  `AWS_DEFAULT_REGION` have no effect, because the region is the request's (as `region_name` was).
  `AWS_ENDPOINT_URL*` and `AWS_USE_FIPS_ENDPOINT` are ignored (X402 D18). AgentCore TLS uses
  `PaymentRoots` (X402 D34).
- **Refinement of X402 G1c for this route.** The bridge reads and validates its stdin request
  **before** `Deps::for_process()`, the way other routes parse argv. That keeps the Python exit 2
  for malformed input in every build, and it lets the real binary's error path be tested
  (T-BR-ERR-BIN). T-AC3-INH for this route therefore asserts the following, replacing "read no stdin":
  - at most 131073 bytes of stdin were read;
  - a valid request in a non-live build gives the R-BR-D32 document;
  - the loopback recorder saw zero connections;
  - `HOME` and the working directory are byte- and mtime-identical.
- The G1a compile guards, the G2 fixture namespace, G3 and the G4 source gate apply unchanged.
  The bridge test crate root carries the G1a guard.

## 5. TS caller changes (AC2)

The goal is the smallest diff that switches the helper and the request. Each item is tested
in R-TS-6.

- **R-TS-1 `bridge.ts`.**
  - Replace `bridgePath` and `pythonPath` with a fixed binary table keyed by `process.platform`
    and `process.arch`:
    - `linux`/`x64` → `bin/linux-x86_64/toolkit`;
    - `linux`/`arm64` → `bin/linux-aarch64/toolkit`;
    - `darwin`/`x64` → `bin/macos-x86_64/toolkit`;
    - `darwin`/`arm64` → `bin/macos-aarch64/toolkit`;
    - `win32`/`x64` and `win32`/`arm64` → `bin/toolkit.exe` (ARM64 through Windows' x64 emulation, as PB section 2).
  - Any other pair rejects with `"AgentCore bridge could not start"`, and no process is spawned.
    No `uname`, no shell and no dispatcher are used. The table mirrors PB's dispatcher `case`
    table, so the two cannot drift (T-JS-PATH, T-PKG-LAYOUT).
  - `access(binary, X_OK)` replaces the two `access` calls; the error behaviour is unchanged.
  - `spawn(binary, ["agents-pay", "bridge"], { cwd: packageRoot, env: { ...process.env }, shell: false, stdio, windowsHide })`. The two `PYTHON*` variables are dropped.
  - The `BridgeRequest` type changes for `process_payment` (4.2).
  - Every limit, message, kill, timer and zeroization line stays.
  - For tests only, the spawn logic moves into an exported `runBridge(binary, args, request, limits)`. `invokeAgentCoreBridge` calls it with the fixed binary and the production limits. No environment variable or config can override the binary path.
- **R-TS-2 `payments.ts`.**
  - `processPayment(version, accepted, resource, resourceUrl)` sends `request_url: resourceUrl`, `accepted`, `resource` and `policy` (the six policy fields of the loaded config, with `undefined` members dropped by `JSON.stringify`).
  - It no longer sends `payload` or `client_token`.
  - `deriveClientToken` and the `node:crypto` import are removed (D-OC15).
  - The proof extraction (`paymentOutput.cryptoX402.payload`, string-or-object) and `getPaymentSessionStatus` are unchanged.
- **R-TS-3 `index.ts`.** `paidFetch` calls `processPayment(authorized.version, authorized.accepted, authorized.resource, url)`. It passes the **selected entry itself** instead of `buildProcessPaymentPayload(...)`, because Rust builds the `ProcessPayment` payload (D-OC5). `buildProcessPaymentPayload` stays exported in `x402.ts`, unused by `index.ts`, so `x402.ts` does not change. B4 may delete it if the reviewer prefers (OQ12). Nothing else changes, including the envelope, which still uses `authorized.accepted` and `authorized.resource`.
- **R-TS-4.** `x402.ts`, `config.ts`, `openclaw.plugin.json` and both tool schemas and outputs are unchanged.
- **R-TS-5 `package.json`.**
  - `files` replaces `runtime/agentcore_bridge.py` with `bin`.
  - `test` drops `python3 -m unittest discover …`.
  - `prepack` adds `node scripts/check-staged-bin.mjs` (R-PKG-4).
  - The version is bumped (OQ9). No `bin` (PATH) entry and no install script are added.
- **R-TS-6 tests (`security.test.mjs` and new files).**
  - The `deriveClientToken` test is removed. Its three properties (nonce excluded, amount changes it, session changes it) are asserted in Rust on R-TOK (T-BR-TOK).
  - "public package and runtime identities stay aligned": `files` includes `bin`, excludes `runtime/agentcore_bridge.py`, and the version moves to the new value.
  - "bundled skill is exact": the staged entries become `SKILL.md` and `references`; the `requirements.txt` pin assertion becomes "no `requirements.txt` and no `*.py` is bundled".
  - New tests: T-JS-* (section 10.2).

## 6. Installer and packaging without Python (AC3)

### 6.1 Package layout

```text
@aws/aws-agents-pay/
  dist/…                      tsc output (unchanged)
  openclaw.plugin.json, README.md
  skills/agents-pay/SKILL.md, references/…      (no scripts/, no requirements.txt)
  bin/manifest.json           plugin-stage provenance manifest (PB section 5)
  bin/toolkit                 PB POSIX dispatcher (for humans running setup)
  bin/toolkit.exe             windows-x86_64
  bin/linux-x86_64/toolkit    bin/linux-aarch64/toolkit
  bin/macos-x86_64/toolkit    bin/macos-aarch64/toolkit
```

The runtime (`bridge.ts`) spawns the platform file directly (R-TS-1). Humans running setup use
the dispatcher (`./bin/toolkit …`), or `.\bin\toolkit.exe` from PowerShell.

### 6.2 plugin-stage: a package destination (R-PKG-1, R-PKG-2)

PB's declaration is plugin-scoped (`plugin` → `plugins/<plugin>/bin/`). The OpenClaw package is
an npm/ClawHub artifact inside the `aws-agents` plugin's source tree, not a plugin. B3 extends
the declaration and the tool:

- **R-PKG-1. Declaration.** An entry carries exactly one of `plugin` or `package_root`
  (both or neither → refused). `package_root` must:
  - be a repository-relative path to a real directory, with no symlink component and no `..`;
  - contain `package.json`;
  - have a `.gitignore` that ignores `bin/`;
  - not lie inside any `plugins/*/bin/`, or inside another destination's `bin/`.

  The entry for this slice is:

  ```json
  { "package_root": "plugins/aws-agents/skills/agents-pay/packages/openclaw",
    "name": "toolkit", "package": "toolkit-cli", "bin": "toolkit",
    "features": ["agents-pay-live"], "role": "subprocess", "consumer": "#4411" }
  ```

  `agents-pay-live` implies `aws-sdk` (X402 G1).
- **R-PKG-2. Role `subprocess`.** The binary is spawned by code, not by a launcher string.
  - `plugin-stage launcher` refuses the role.
  - The launcher allowlist and inventory (PB section 4) are unaffected: no new template, and no
    shipped launcher string changes.
  - `stage`, the manifest, the dispatcher, `check` and `verify --complete` treat a package
    destination exactly like a plugin `bin/`: the same layout, sha256, modes, no unlisted files
    and the same symlink refusals.
  - `check` additionally asserts that no file under a package destination's `bin/` is tracked by git.
- **R-PKG-3.** `packages/openclaw/.gitignore` gains `bin/`. The top-level `/plugins/*/bin/` rule
  does not match this depth.
- **R-PKG-4. `scripts/check-staged-bin.mjs`** (Node, no dependencies), run by `prepack`. It
  refuses to pack unless:
  - `bin/manifest.json` exists and parses;
  - `source_dirty` is false;
  - every declared platform file and the dispatcher are present;
  - each listed file's size and sha256 match (Node `crypto`);
  - no unlisted file exists under `bin/`.

  This is a pack-time guard; the release gate is still `plugin-stage verify --complete` (PB section 5).
- **R-PKG-5.** `package.json` `files` includes `bin`. `npm pack` keeps the executable bit of files that have it.
- **R-PKG-6.** `stage-skill.mjs` stages `SKILL.md` and `references/` only. The admin CLI is now
  the binary, so the bundled skill needs no scripts. The canonical `scripts/*.py` stay in the source tree until the programme retirement step.
- **R-PKG-7. No test builds the live binary.** plugin-stage tests use fixture declarations and
  probe packages only (PB section 2). A test asserts that the real declaration's `agents-pay-live`
  entry is never staged by a test target, and T-AC3-MAKE's scope is noted: `make plugin-binaries`
  is a release target, not a verification target (X402 Q24).

### 6.3 Release flow (with #4414)

1. Run `plugin-stage stage --platform all` on the clean release commit. This stages the plugin
   binaries and the OpenClaw package destination together.
2. Run `plugin-stage verify --complete`.
3. Run the PUBLISHING.md flow from the package directory:
   - `npm ci`;
   - `npm test`;
   - `clawhub package validate --runtime --allow-execute`;
   - `npm pack`, whose `prepack` runs R-PKG-4;
   - `clawhub package pack` and `verify --sha256`;
   - the smoke install against `openclaw@2026.3.24`.
4. **Package destinations are never `git add -f`'d.** PB section 6 force-adds plugin `bin/`
   directories to a release tree because marketplaces install from git. The OpenClaw package
   ships as a tarball, and adding five `toolkit` binaries to the `aws-agents` plugin's git install would only add size.
5. Publication stays operator-gated: no publish, session or spend without explicit approval
   (PUBLISHING.md).

Prerequisite shared with #4413 S5 (PB section 2, "Not yet proven"): `toolkit-cli` with `aws-sdk`
must cross-build (`aws-lc-sys` through `zig cc`) for all five platforms. A platform that cannot be
built fails `verify --complete`. OQ13 covers shipping fewer platforms.

### 6.4 Setup without Python

| Step | Before | After |
| --- | --- | --- |
| install | `openclaw plugins install clawhub:@aws/aws-agents-pay` | unchanged |
| AgentCore project | `npm install -g @aws/agentcore`, `agentcore create/add/deploy` | unchanged (Node) |
| environment | `python3.11 -m venv .venv`, `pip install -r requirements.txt` | **none** |
| wizard | `python scripts/agents_pay_admin.py setup-openclaw --project-dir …` | `cd ~/.openclaw/extensions/aws-agents-pay && ./bin/toolkit agents-pay admin setup-openclaw --project-dir "$AGENTCORE_PROJECT_DIR"` (#4410 S6; TTY gates, D33) |
| manual setup | `… init-config / create-instrument / new-session` | `./bin/toolkit agents-pay admin init-config / create-instrument / new-session` |
| preflight | `python3 scripts/agents_pay_admin.py preflight` | `./bin/toolkit agents-pay admin preflight` |
| runtime | venv python + `agentcore_bridge.py` | `bin/<platform>/toolkit agents-pay bridge` |

Upgrading from ≤ 1.0.7 works as follows:

- OpenClaw replaces the extension directory, so the old `.venv` goes with it. If an install method keeps it, it is unused.
- `~/.openclaw/openclaw.json`, `~/.x402/config.json` and the admin file `~/.agents-pay/config.json` are read unchanged (S6 reads the same file).
- Sessions and instruments are untouched.
- One behaviour crosses the upgrade, the token change (D-OC4). The release note states it.

## 7. Reuse of #4410 (S1-S5) and #4379

| Needed | Reused from | Added by #4411 |
| --- | --- | --- |
| policy gate, amounts, URL parsing, token | S1 `policy`, `amount`, `urlparse`, `pyval`, `challenge` (R-TOK, R-AMT-REP/CMP) | `policy/openclaw.rs` (a pure input adapter to the existing `Policy`); the `str.isspace` table in `pyval.rs` |
| AgentCore calls, request bytes, response checks, error allowlist, endpoint, SigV4 | S4 `agentcore.rs` over #4379 `toolkit_aws` (`endpoint::Endpoint::for_service`, `SigV4Signer`, `RetryPolicy`, `redact::Secret`, `recorded`, guarded credentials, `testing` fakes) | the model-order renderer of `GetPaymentSession` (R-BR-OUT-2); the client-side length check (R-BR-PARAM) over S4's model excerpt |
| orchestration, proof checks, deadline, scrub, error classes | S5 `flow::prove`, `proof::verify`, R-DNS-ADMIT, R-SCRUB matcher, T-ORDER classes | `VerifiedProof::into_trusted_glue`; the bridge handler; the `prove` extraction if S5 inlined it |
| world, live custody | S5 `deps.rs` (`Deps`, `for_process`, fixtures), `live.rs`, `agents-pay-live`, the G1a guards, G3, the G4 gate | nothing in `live.rs` (R-BR-ONE-3); the bridge route in the T-AC3-INH route list |
| JSON | `toolkit_core::json::parse_json`, X402's `aurora::pyfmt` emitter | a compact separator mode for that emitter, if missing |
| admin and setup | S6 `admin/*` (`setup-openclaw`, `build_openclaw_config`, `usd_to_atomic`, `discover_deployed`, `format_duration`) | nothing: docs only (B5) |

Assumptions for the S1, S4 and S5 reviewers (each is a precondition of B1/B2, and fixed in that
slice if false):

- **A1.** `policy::authorize` takes a `Policy` value and resolved `PaymentIds` without reading the
  config file itself. S2's `config.rs` produces them for the CLI.
- **A2.** The AgentCore region can be chosen per call. Otherwise B2 changes the `PaymentsApi`
  methods to take it (R-BR-ONE-3).
- **A3.** `proof::verify` checks the `validAfter` window without sleeping. Otherwise B2 splits it (R-BR-DL).

## 8. Intentional behaviour changes (D-OC)

None of these is visible in either tool's output: TS maps every bridge failure to its existing
texts. Each is listed with its effect and its test.

| ID | Change | Effect | Test |
| --- | --- | --- | --- |
| D-OC1 | The runtime helper is the Rust `toolkit agents-pay bridge` in `bin/`; no venv, boto3 or Python | Installs without Python; Windows uses `bin\toolkit.exe` | T-JS-PATH, T-PKG-* |
| D-OC2 | `process_payment` request: `payload` and `client_token` removed; `request_url`, `accepted`, `resource` and `policy` added. The old shape is refused (exit 2) | A stale TS with a new binary fails closed | T-BR-REQ-LEG |
| D-OC3 | The Rust gate re-authorizes each payment with the OpenClaw profile. It is stricter where #4410 is: (a) D11 (`maxTimeoutSeconds` 1..=3600 integer, `extra` ≤ 4 KiB object); (b) D4 (`eip155` only); (c) conflicting `amount`/`maxAmountRequired` refused (TS used `amount`); (d) D2 (a non-scalar field makes the entry ineligible); (e) R-RESBIND compares the raw netloc, case-sensitively, so `https://Merchant.test/x` or an explicit `:443` in `resource.url` no longer matches a lower-case request URL (TS compared WHATWG origins); (f) R-ORIGIN resolves the host at authorization time against the wider D8 table; (g) D1 duplicate keys | Some challenges TS would have paid are now refused, before `ProcessPayment`. All fail closed | T-OCP-DIV-a..g |
| D-OC4 | `clientToken` is R-TOK (Python material, empty purchase id) instead of TS `deriveClientToken` (X402 Q14 adopted) | The same purchase now derives one token in the CLI and the plugin. A purchase started on ≤ 1.0.7 and retried after the upgrade gets a new token, so the service may authorize again within the cap and the session budget (the R-TOK-SCOPE class) | T-BR-TOK |
| D-OC5 | The `ProcessPayment` payload is X402 PP-PAYLOAD: the vetted entry with all members in order, `amount` last and `maxAmountRequired` removed. It replaces TS's fixed 7-member object | For x402 v2 entries (`scheme, network, amount, asset, payTo, maxTimeoutSeconds, extra`) the member set is identical and only the order differs. Extra members are now forwarded. [inferred, X402 Q4] Live run L-OC-3 | T-BR-SVC-2 |
| D-OC6 | The proof is verified (R-PP-VERIFY) before it leaves Rust, and a `validAfter` beyond 15 s is refused in Rust | A mismatched proof never reaches TS | T-BR-POST-PROOF |
| D-OC7 | Results are reduced: `process_payment` returns only `paymentOutput.cryptoX402.payload` (always an object). `get_payment_session` has no `ResponseMetadata` (request id and HTTP headers no longer cross the pipe). A numeric timestamp, which botocore would read in local time, is refused | TS reads nothing that was removed | T-BR-OUT-* |
| D-OC8 | A missing required member (for example `createdAt`) is an AgentCore failure. Python passed partial data, and TS then treated a missing `createdAt` as **not expired**, so `usable: true` | `get_payment_session_status` reports `status: "error"` instead of failing open. TS's `"not_found"` branch becomes unreachable, because a response without `paymentSession` is now an error too | T-BR-OUT-REQ |
| D-OC9 | `agent_name` must equal `openclaw-aws-agents-pay` | TS always sends it | T-BR-REQ-AGENT |
| D-OC10 | An invalid `region` is an input error (exit 2) before any I/O. It was a boto3 failure (exit 1) | — | T-BR-REQ-REGION |
| D-OC11 | The AWS environment, inherited from #4410: endpoint overrides ignored (D18); `SSL_CERT_*` and `AWS_CA_BUNDLE` ignored for AgentCore (D34); D20 retry and timeout defaults, within the 25 s deadline. Proxy variables follow #4410's AgentCore transport [to confirm in S4] | A TLS-inspecting proxy, or an egress-proxy-only host, may lose AgentCore access (X402 B4; OQ14) | inherited S4/P-ROOTS tests; T-BR-DL |
| D-OC12 | New error value `"payment support unavailable"` (exit 1) in builds without `agents-pay-live` | Only non-release builds | T-BR-D32 |
| D-OC13 | The bridge caps its own output at 128 KiB (exit 1) | Deterministic instead of a TS kill | T-BR-OUT-CAP |
| D-OC14 | The package no longer bundles `scripts/`, `requirements.txt` or `runtime/agentcore_bridge.py`, and `npm test` runs no Python | — | T-JS-PKG |
| D-OC15 | `deriveClientToken` is no longer exported from `dist/payments.js` | It was internal; no documented API | T-JS-PKG |
| D-OC16 | X402 G3: test-fixture identities (`111111111111`, `123456789012`, `.test`/`.example`/`.invalid`/`.localhost`) are refused in release builds | — | inherited G3 tests, run on bridge inputs |
| D-OC17 | The bridge resolves the merchant host (R-ORIGIN) before `ProcessPayment` | One DNS lookup more per payment; an unresolvable host is refused earlier | T-OCP-DIV-f |
| D-OC18 | The input JSON must be UTF-8 without a BOM, with no duplicate keys or non-finite literals (D1) | TS's `JSON.stringify` never produces these | T-BR-REQ-JSON |

## 9. Documented boundaries

- **B-OC1. The merchant plane stays in TS.**
  - The OpenClaw probe and replay keep Node's TLS roots, `x402.ts`'s narrower SSRF table, its
    64 KiB cap, its 15 s timeout and one replay.
  - X402's R-NET guarantees (peer check, payment roots, the replay policy) do not cover OpenClaw's merchant traffic.
  - The bridge's authorize-time DNS check is a second check, not a pin. A host that resolves
    publicly at authorization and privately at replay is caught only by TS's connect-time check.
- **B-OC2. Policy authority is the plugin process.** The bridge trusts the `policy` and the ids in
  its request, as the Python bridge trusted `payload` and the token. It reads no operator file. The
  model reaches only the tools' `url` parameter, and OpenClaw's trusted config supplies the rest. A
  process that can run the binary with arbitrary stdin as the user can already call AgentCore with
  the user's credentials (X402 B5).
- **B-OC3. The proof crosses the bridge pipe to trusted glue, as today.** It never reaches tool
  output (TS contract, tested). R-SCRUB exempts only its designated field.
- **B-OC4. One runtime at a time.** The "choose one runtime path" guidance stays. The shared
  token (D-OC4) reduces the double-authorization risk when both are used, but does not remove it
  (different policies can approve different offers).
- **B-OC5. Unsupported platforms fail closed.** On musl (Alpine) Linux, 32-bit and other OS/arch
  combinations, `bridge.ts` rejects before spawning, or the spawn fails, and the tools report their existing
  failures. The same holds if an installer drops the executable bit (OQ11).
- **B-OC6.** X402 B2 (no exactly-once across changed challenges), B3 (the DNS residual) and B5 (IAM) carry over unchanged.

## 10. Tests (all offline)

No test makes a live payment, creates a wallet or session, calls AWS, or opens a non-loopback
socket. Fixtures use X402 G2 (account `111111111111`, `.test` hosts, `payment-session-TEST…` ids),
except the parity tests, which read the existing JS fixture as is (`.example`, also reserved).
Every fake records calls, and every test asserts exact counts.

### 10.1 Rust (`crates/toolkit-cli`, unit tests and `tests/agents_pay_bridge.rs` with the G1a guard)

| ID | Asserts |
| --- | --- |
| T-BR-REQ-SIZE | 0 bytes, 131072 bytes (accepted if valid) and 131073 bytes → exit 2; at most 131073 bytes are read from a 1 MiB stream (reader recorder) |
| T-BR-REQ-JSON | non-JSON, array, string, a BOM, UTF-16, duplicate keys, `NaN` → exit 2, exact document, zero calls |
| T-BR-REQ-KEYS | for each op: every missing key, one extra key (the Python `unexpected` case), each key of the other op → exit 2, zero calls |
| T-BR-REQ-LEG | the exact Python `process_payment` request (with `payload` and `client_token`) → exit 2, zero calls |
| T-BR-REQ-STR | per field: non-string, `""`, `"   "`, `"　"`, `"\x1c"` (Python whitespace) → refused; `"​"` (not whitespace) → accepted; the limit by code points (2048 × `é` = 4096 bytes accepted, 2049 refused); the value forwarded unstripped |
| T-BR-REQ-AGENT, -REGION, -VER | another agent name; `us-east-1x`, `""`; `version` `"1"`, `"02"`, `2` (number) → exit 2, zero calls |
| T-BR-REQ-PAY | `accepted` a list or string, `resource` without `url`, `policy` with an unknown key, `maxPaymentAmountAtomic` `"0"`/`"01"`/`"1.0"`/1001 digits, both recipient modes, neither → exit 2, zero calls (the Python `test_bridge_rejects_invalid_payment_inputs_before_aws` successor) |
| T-BR-PARAM | `user_id` of 121 code points → exit 1, **zero** calls |
| T-BR-SVC-1 | `get_payment_session` over `FakePaymentsApi`: golden request bytes (path, both headers, body) for the request's region and endpoint; one call (`test_get_payment_session_maps_exact_service_request` successor) |
| T-BR-SVC-2 | `process_payment` on the bridge fixture: one call whose body is the PP-PAYLOAD of the fixture entry, `paymentType` `CRYPTO_X402`, version `"2"` and token `e36c6d85…`; region recorded (`test_process_payment_maps_exact_service_request` successor) |
| T-BR-TOK | the 4.7 vectors through the bridge; nonce and `maxTimeoutSeconds` changes keep the token; amount and session changes change it (moved JS properties) |
| T-OCP-MAP | each row of 4.3: defaults (absent and `[]` networks; per-network asset default), cap boundaries (`max−1`, `max` allowed; `max+1` refused) for `max` = `"1"`, `"100000"` and a 40-digit value, and the equality with TS `amount <= max` |
| T-OCP-ORIG | `allowedOrigins` `["https://merchant.test/some/path"]`, `["https://MERCHANT.test"]`, `["https://merchant.test:443"]` → allowed for `https://merchant.test/x`; `["http://merchant.test"]`, `["https://u:p@merchant.test"]` → exit 2 |
| T-OCP-PAR | every policy case of `security.test.mjs` replayed through the Rust gate on `test/fixtures/v2-security-contract.json` with a `FixedResolver` (public): the hostile first offer is skipped when the full challenge is given; each of the 6 mutations refused; the origin mutation refused; `allowAnyRecipient` keeps the other 5 controls; amounts `"0"`, `"-1"`, `"1.0"`, `"1e3"`, `" 1000 "` refused. Same outcome as TS for every case, and zero `ProcessPayment` on each refusal |
| T-OCP-DIV-a..g | one case per D-OC3 item: Rust refuses (exit 2, zero calls) where TS accepted |
| T-BR-PRE | every X402 T-ORDER-PRE class reachable through the bridge → exit 2, **zero** `ProcessPayment` |
| T-BR-POST | AgentCore error codes (each allowlisted code, an unknown code, a 64 KiB+1 body), `status` mismatch, echo mismatch, missing payload, payload over 16 KiB → exit 1, one logical call, no second token (`ConflictException` not retried), and stdout contains no proof byte |
| T-BR-POST-PROOF | R-PP-VERIFY `to`, `value` and an expired `validBefore`; `validAfter` 16 s ahead → exit 1, proof absent from stdout. `validAfter` 10 s ahead → exit 0 and `FakeClock` recorded **no sleep** |
| T-BR-OUT-1 | success documents byte-exact: compact separators, trailing `\n`, `é` → `é`, U+1F600 → `😀` |
| T-BR-OUT-2 | session rendering: model member order from a shuffled wire body; unknown member dropped; no `ResponseMetadata`; `2026-08-07T00:00:00Z` → `2026-08-07T00:00:00+00:00`; `…00.5Z` → `…00.500000+00:00`; `…00.1234567Z` → `…00.123456+00:00`; `+05:30` kept; a numeric timestamp → exit 1 |
| T-BR-OUT-3 | payment result is exactly the reduced document; a JSON-string payload is normalized to an object; the signature occurs exactly once |
| T-BR-OUT-REQ | a session without `createdAt` → exit 1 |
| T-BR-OUT-CAP | a session that renders over 131072 bytes (through `\u` escapes) → exit 1 |
| T-BR-SCRUB | the fake service echoes the session id inside `limits.maxSpendAmount.currency` → exit 1; the echo inside `paymentSessionId` → exit 0 |
| T-BR-DL | a stalled fake AWS → exit 1 at ≤ 25 s of fake time; a deadline already passed at authorization → exit 2, zero calls |
| T-BR-NOCFG | a hostile `HOME` (`~/.agents-pay/config.json` with another policy, `~/.x402/config.json`) and `PAYMENT_*`/`AWS_REGION` set: request bytes and decisions depend only on stdin; the `ConfigStore`, `MerchantHttp`, `Terminal`, `DeployedState` and `AdminApi` recorders see zero calls; files unchanged |
| T-BR-QUIET | every case above, run in-process with a captured stderr: zero bytes. A fake that panics → exit 1, the AgentCore document, zero stderr bytes |
| T-BR-ERR-BIN | through `CARGO_BIN_EXE_toolkit` in both verification builds: `{"operation":"not-supported"}` → exit 2, the exact document, **empty stderr** (Python `test_invalid_stdin_fails_without_diagnostics` successor); a valid request → R-BR-D32 document, exit 1, empty stderr |
| T-BR-D32 / T-AC3-INH-BR | X402 T-AC3-INH for the bridge route, with the R-BR-AC3 assertions (≤ 131073 stdin bytes, zero recorder connections, files unchanged) |
| T-BR-ONE-1..4 | `syn` gates for R-BR-ONE-1..4, and `live.rs`'s pinned public items |
| T-PKG-LAYOUT | the bridge.ts table (read as text from `src/bridge.ts`) equals the PB dispatcher's platform map and file names |
| T-PKG-TS | text gates over `src/*.ts` and `package.json`: no `python`, `.venv`, `agentcore_bridge` or `PYTHON` string; `spawn` gets `shell: false` and an args array equal to `["agents-pay", "bridge"]` |
| T-S6-OC-1..4 | (in #4410 S6) `usd_to_atomic`, `build_openclaw_config`, `discover_deployed` with `--project-dir`, `format_duration`: the `test_setup_openclaw.py` successors that #4411 cites to retire it |

Shared cross-language fixtures in `packages/openclaw/test/fixtures/bridge-contract/`:

- `get_payment_session.request.json` and `process_payment.request.json`: the exact requests TS
  builds from a fixture config;
- `legacy_process_payment.request.json`;
- `get_payment_session.wire.json` and `process_payment.wire.json`: AWS response bodies;
- `*.bridge-output.json`: the exact expected stdout.

The Rust tests read them through `CARGO_MANIFEST_DIR`-relative paths. The JS tests (T-JS-REQ)
assert that TS builds byte-equal requests. So the two sides cannot drift without a test failing on
at least one of them.

### 10.2 JS (`node --test`, no network, no Python, no real binary)

| ID | Asserts |
| --- | --- |
| T-JS-BR-1..8 | `runBridge` against a fake child (`process.execPath` running `test/fixtures/fake-bridge.mjs` in a mode chosen by argv): success parse; exits 1 and 2 → `"AgentCore bridge request failed"`; stdout over 128 KiB → the size error, child killed; stderr over 16 KiB → the diagnostics error; a short test timeout → `"timed out"` and SIGKILL; a missing binary → rejects without spawning; invalid JSON → the invalid-response error; a request over 128 KiB → refused before spawn; the stdout buffers are zero-filled (inspected through a test hook) |
| T-JS-PATH | the platform table for all 6 supported pairs and 3 unsupported ones (`linux`/`ia32`, `freebsd`/`x64`, `linux`/`arm`) → rejection without spawn |
| T-JS-REQ | `processPayment` and `getPaymentSessionStatus` (with `runBridge` stubbed) send requests byte-equal to the `bridge-contract` fixtures; no `client_token` or `payload` key; `policy` omits `undefined` fields |
| T-JS-PKG | the R-TS-6 identity and bundling tests; `check-staged-bin.mjs` refuses a missing manifest, a dirty manifest, a size or sha256 mismatch, a missing platform and an unlisted file (temp-dir fixtures with tiny fake files) |
| (kept) | every other existing `security.test.mjs` test, unchanged |

The daemon's verification runs cargo only (`make test`, scoped_test). `ci-gates.md` lists no
`npm test`. So the JS tests need a verification route of their own (OQ8). The Rust-side
T-PKG-TS, T-PKG-LAYOUT and fixture tests run in every scoped test regardless.

### 10.3 plugin-stage (`crates/plugin-stage/tests/`)

| ID | Asserts |
| --- | --- |
| T-PKG-DECL | `plugin` xor `package_root`; `package_root` refusals: absolute, `..`, a symlink component, no `package.json`, a `.gitignore` without `bin/`, nested in a `plugins/*/bin/` or another destination; role `subprocess` refused by `launcher` |
| T-PKG-STAGE | a fixture package destination stages to `<root>/bin/` with the PB layout, manifest, dispatcher and modes (host platform, probe package) |
| T-PKG-VERIFY | `verify` and `verify --complete` cover package destinations: unlisted file, sha256 drift, symlinked `bin/`, missing platform; `check` refuses a tracked file under a package `bin/` |
| T-PKG-INV | the launcher inventory and allowlist results are byte-identical with and without the new entry |
| T-PKG-NOLIVE | no test target stages the real declaration (R-PKG-7) |

### 10.4 Operator live run (gate: real AWS, a real wallet and testnet spend; not part of any test)

On a disposable account with a release build of the package (`agents-pay-live`) and a
`0.10` budget on Base Sepolia, run after X402 section 11 steps 1-4:

| Step | Check |
| --- | --- |
| L-OC-1 | `npm pack`, `clawhub package pack`, `openclaw plugins install <tgz>` into a temporary home. The executable bit is kept, the two tools are listed, `./bin/toolkit agents-pay admin preflight` passes |
| L-OC-2 | `get_payment_session_status` → `usable: true`, with `minutes_left` and `remaining_usd` matching the console |
| L-OC-3 | `get_paid_content` on the sandbox URL → `paid: true` and exactly one on-chain transfer. This confirms D-OC5 (PP-PAYLOAD accepted by `ProcessPayment`, X402 Q4) and the TS envelope with a Rust-verified proof |
| L-OC-4 | repeat at once → no second transfer (same R-TOK token); compare with `toolkit agents-pay fetch` on the same URL: same token, no second transfer (D-OC4) |
| L-OC-5 | an above-cap URL → refused; CloudTrail shows no `ProcessPayment` |
| L-OC-6 | macOS (arm64), Linux arm64 and Windows (native and Git Bash) runs of L-OC-2 (PB section 2's hardware smoke runs) |
| L-OC-7 | roll back to 1.0.7 and its config (PUBLISHING.md step 6) |

## 11. Slices (dependency order, tests first)

Each slice writes failing tests first, then the code. Each needs a daemon `scoped_test` and an
independent cross-family review at the exact SHA.

| Slice | Content | Tests | Prerequisites |
| --- | --- | --- | --- |
| **B0** | this design | review | — |
| **B1** bridge protocol (pure) | `agents_pay/bridge/protocol.rs` (R-BR-IN, 4.2, R-BR-STR, R-BR-PARAM, the error vocabulary), `bridge/render.rs` (R-BR-OUT-1/2/3, R-BR-OUT-CAP), `policy/openclaw.rs` (4.3), the `pyval` whitespace table, the compact emitter mode; the `bridge-contract` fixtures | T-BR-REQ-*, T-BR-PARAM (pure part), T-BR-OUT-*, T-OCP-MAP, T-OCP-ORIG, T-OCP-PAR, T-OCP-DIV, T-BR-TOK (policy part) | X402 **S1** (policy, `Policy`, R-TOK) and **S4** (model excerpt and response readers) on rolling |
| **B2** bridge route | `bridge/mod.rs` handler (R-BR-ROUTE, R-BR-FLOW, R-BR-DL, R-BR-QUIET, R-BR-SCRUB, R-BR-D32, R-BR-WORLD, R-BR-AC3); `flow::prove` (extracted if needed); `VerifiedProof::into_trusted_glue`; the A2/A3 edits if needed; registry line and `agents-pay.md` section | T-BR-SVC-*, T-BR-PRE, T-BR-POST*, T-BR-DL, T-BR-NOCFG, T-BR-QUIET, T-BR-ERR-BIN, T-BR-D32/T-AC3-INH-BR, T-BR-ONE-* | B1; X402 **S5** (with S2-S4 and P-ROOTS) on rolling |
| **B3** packaging | plugin-stage `package_root` and role `subprocess` (R-PKG-1/2/7), the declaration entry, `.gitignore`, `check-staged-bin.mjs` | T-PKG-DECL, -STAGE, -VERIFY, -INV, -NOLIVE; T-JS-PKG (check script part) | #4487 (on rolling `c27549a6`); the `agents-pay-live` feature from S5 |
| **B4** TS adaptation | R-TS-1..6; `stage-skill.mjs`; `package.json`; the version | T-JS-*, T-PKG-LAYOUT, T-PKG-TS | B2, B3 |
| **B5** cutover docs | `README.md` (both runtime paragraphs and the getting-started text), `PUBLISHING.md` (6.3), `references/openclaw-setup.md` (6.4), the OpenClaw subsections of `SKILL.md` and `references/operator-guide.md` (re-staged copy), `docs/migration/agents-pay.md` bridge section, inventory rows for the three Python files → `ported` | the README test in `security.test.mjs`; X402 R-PORT over the new commands; `every_slice_documents_its_commands` | B4; X402 **S6** and **S7** (both edit `SKILL.md` and `references/`; B5 changes only the OpenClaw parts, on top of S7) |

The Python files `runtime/agentcore_bridge.py`, `test/test_agentcore_bridge.py` and
`test/test_setup_openclaw.py` are deleted only by the programme retirement step (#4415 pattern)
after B5 is verified, citing T-BR-* and T-S6-OC-*. From B4 on, no caller references them.

B1 can start once S1 and S4 are on rolling. B3 can start as soon as S5's feature name is fixed,
and in parallel with B1 and B2. B4 waits for B2 and B3.

## 12. Open decisions (each with a recommendation)

- **OQ1. The seam.** **Recommend C** (section 3): a per-call subprocess with Rust
  re-authorization. B is the long-term "one runtime" option and can follow as its own reviewed
  change once #5103 exists.
- **OQ2. Token adoption (X402 Q14).** **Recommend adopt R-TOK** (D-OC4). R-TOK-NOFRESH forbids
  a caller-supplied token, and sharing tokens with the CLI is a gain. **Product choice:** accept the
  one-time upgrade boundary and state it in the release note. An alternative is a compatibility
  mode that keeps the TS material, but it would need a second token producer, which #4410 forbids.
- **OQ3. PP-PAYLOAD versus the TS 7-member payload.** **Recommend** #4410's single builder
  (D-OC5), confirmed by L-OC-3. If the service rejects it, the fix lands in S4 for both runtimes
  (a P1 per X402 Q4), never as a bridge-specific builder.
- **OQ4. A distinct exit code for policy refusals.** **Recommend no.** Exit 2 and `invalid bridge
  request` already separate pre-authorization from post-authorization outcomes, and TS shows neither.
- **OQ5. Long-running stdio (#5103).** **Recommend not now.** Share `bridge::protocol` and
  `into_trusted_glue` with #5103, and revisit OpenClaw's transport after #5103 is designed and verified.
- **OQ6. One tarball with five platforms, or per-platform packages.** **Recommend one tarball**:
  it works with any installer, the way PB's plugin `bin/` does. B3 records the staged sizes. If
  ClawHub limits or user cost rule it out, per-platform optional packages (`os`/`cpu` fields)
  need proof that `openclaw plugins install` resolves them. That is unknown.
- **OQ7. Where to stage.** **Recommend the `package_root` destination** (R-PKG-1). Staging into
  `plugins/aws-agents/bin/` and copying at pack time was rejected: the copy would sit outside
  plugin-stage's verified manifest.
- **OQ8. Verifying the JS tests.** **Recommend** a manager-provided daemon recipe that runs
  `npm ci --offline && npm test` in the package from a pre-filled npm cache. Until then, rely on
  the Rust fixture and text gates (T-PKG-TS, T-PKG-LAYOUT, the shared fixtures), with publisher-run
  `npm test` recorded per PUBLISHING.md. A node test result from outside the daemon is not daemon
  evidence.
- **OQ9. Package version.** **Recommend `1.1.0`**: the tool contract and config are unchanged,
  while install, setup and the internal bridge change. **Product choice** (a reviewer may prefer
  `2.0.0` for the setup change).
- **OQ10. The 25 s bridge deadline.** **Recommend** a fixed constant tied to `bridge.ts`'s 30 s
  (both changed together, with a test pinning the 5 s margin).
- **OQ11. The executable bit through ClawHub.** **Recommend** treating it as an L-OC-1 gate.
  Do not add an install script, which would be install-time code execution. If ClawHub drops
  the bit, the tools fail closed (B-OC5), and the fix is a packaging change, not a `chmod` at run time.
- **OQ12. Dead `buildProcessPaymentPayload`.** **Recommend keeping it** in B4 so that `x402.ts`
  stays untouched. A follow-up can remove it once a reviewer accepts that edit.
- **OQ13. Fewer than five platforms.** **Recommend** blocking the release until `verify
  --complete` passes for all five, as PB does. **Product choice** if `aws-lc-sys` cross-builds fail
  on a platform.
- **OQ14. Proxies on the AgentCore plane.** boto3 honoured `HTTPS_PROXY` and `NO_PROXY`.
  **Recommend** inheriting whatever S4 decides, and having S4 state it, because a gateway that
  reaches AWS only through a proxy would lose payments. This is to be resolved in the S4 review,
  not in the bridge.

## 13. Review checklist

1. **One spending path.** No bridge code can build `Approved`, a `ProcessPaymentRequest` or a
   token, or call `process_payment` except through `flow::prove`, and `live.rs` gains nothing
   (R-BR-ONE, T-BR-ONE, X402 T-ORDER-CTOR).
2. **Contracts (AC1).** The framing, limits, exact keys, string rules, envelope, error texts,
   exit codes and silent stderr match section 1, except the D-OC rows (T-BR-REQ, -OUT, -ERR-BIN, -QUIET).
3. **Order.** Pre-authorization refusals make zero `ProcessPayment` calls (exit 2), and
   post-authorization failures make one logical call and send no proof (exit 1) (T-BR-PRE, T-BR-POST).
4. **TS minimality (AC2).** Only `bridge.ts`, `payments.ts`, one `index.ts` call,
   `package.json`, `stage-skill.mjs` and tests change, and the tool outputs are byte-identical (section 5).
5. **No Python (AC3).** The package, the install, the setup and `npm test` need no Python. The
   binary comes from the PB pipeline with a verified manifest, and no test builds the live binary (section 6, T-PKG-*).
6. **Custody.** Every verification build and every test lacks `agents-pay-live`, and an inherited
   hostile environment causes no connection or file change (R-BR-AC3, T-AC3-INH-BR).
7. **Changes recorded.** Every change of behaviour is listed in section 8 with a test.
