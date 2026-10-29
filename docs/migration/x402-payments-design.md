# x402 payments: Rust port design and boundary pre-review (#4410, phase 1)

Status: design, revision 1. No implementation code. Programme #4369, slice `payments-policy`,
wave 3, dependency #4379 (aws-core). AC1 of #4410 requires the payments, credential and network
semantics to be reviewed before implementation. This document is the thing to review.
Sibling: #4411 (`payments-bridge`, the OpenClaw AgentCore bridge, which depends on this slice).

Evidence base. Everything below was read and nothing was executed: no Python, no network, no
payment and no AWS call was made.

- This repository at `fa3fa13` (the allocated base, equal to `origin/rolling` at writing time):
  `plugins/aws-agents/skills/agents-pay/scripts/*.py`, `plugins/aws-agents/skills/agents-build/scripts/*.py`,
  `agents-pay/SKILL.md` and `references/`, `agents-build/references/payments.md`, the OpenClaw
  package (`packages/openclaw/src/{x402,payments,bridge,config}.ts`, `runtime/agentcore_bridge.py`,
  `test/test_setup_openclaw.py`) and `skills/core-skills/amazon-bedrock/references/agentcore-payments*.md`.
- The accepted designs whose patterns this one reuses: #4413 MCP proxy
  (`docs/migration/mcp-proxy-design.md` at `fd7e7ffd`) for credential source selection (R-SRC),
  TLS roots (R-ROOTS, R-TLS), streaming transport with byte caps, and redaction. #4382 Launch with
  AWS (`docs/migration/launch-with-aws-design.md` at `e03640d1`) for deadlines (R-API-BODY,
  TA-STREAM-DL), the error-code allowlist and known-secret scrub (R-API-CODE), and file safety
  (R-TOK-MODE, `rustix` fs). Also the #4941 shared caller-value helpers in
  `toolkit-cli/src/safe_text.rs` at `40c71d7e`: `shown_value`, `redacted_value`, `shown_path`,
  `safe_text`.
- The pinned seams of #5017 S1 at `be0a06df`: `toolkit_aws::stream`, `sdk::roots`, the guarded
  credential sources and `SdkTransport::with_resolver`.
- The **AgentCore service model**, read as JSON data from the local botocore 1.43.108
  (`botocore/data/bedrock-agentcore/2024-02-28/service-2.json`, sha256
  `6ae4e07026769e280e7e076cf3096f7291424e99da95e046599a621001f9a6c5`). `requirements.txt` pins
  botocore 1.43.63. Section 6.1 lists what was taken from the model, and Q1 covers the version
  gap.
- **Not available.** The `bedrock-agentcore` 1.19.0 SDK source (`bedrock_agentcore.payments.PaymentManager`)
  is neither vendored nor installed. Its `generate_payment_header` is therefore rebuilt from the
  service model, the AgentCore wiring reference and the OpenClaw TypeScript, which calls the raw
  API (section 6.3). Every point where the SDK's behaviour is inferred rather than read is marked
  **[inferred]**, and the operator live run checks it (section 11).

## 0. Summary of the boundary

| Plane | What crosses it | Guarantee |
| --- | --- | --- |
| Model → runtime | `toolkit agents-pay fetch` argv: URL, `--method`, `--purchase-id`, `--status`, `--browser-handle` | No argv value is ever authority. Policy comes only from the 0600 operator file, bound to the OS account |
| Runtime → merchant | probe `GET`/`HEAD`, then a paid replay carrying the proof header | https only; every DNS answer and the socket peer must be public; no redirects; no proxy; identity encoding; body cap; deadlines |
| Runtime → AgentCore | `ProcessPayment` (the only spending call), `GetPaymentSession` | Called only after the policy gate has approved exactly one entry. Fixed endpoint, SigV4, derived idempotency token, response cap, allowlisted error codes |
| AgentCore → runtime | the signed `PaymentDocument` (the proof) | A secret: never printed, logged, put in `Debug` or returned to the model. Checked against the approved terms before it is sent |
| Runtime → model | one JSON object on stdout, exit 0/1/2 | Fixed texts plus redacted, bounded caller values. Paid body only on operator opt-in, scrubbed of every live secret |
| Human → admin | `toolkit agents-pay admin …`, `toolkit agents-build setup-payment-user` | Creates the instrument (wallet user) and the session (budget). A TTY gate on session creation, as in the Python. Never reachable from tests (AC3) |

No private key exists anywhere in this process. AgentCore Payments signs inside the service: the
model marks `PaymentDocument` `"sensitive": true`, and the wallet keys live with the connector
(Coinbase CDP or Privy) behind AgentCore's token vault. The port therefore adds **no
cryptographic signing crate** (section 9).

## 1. Inventory (AC1)

### 1.1 Files

| Path (under `plugins/aws-agents/skills/`) | Kind | Lines | Rust owner |
| --- | --- | --- | --- |
| `agents-pay/scripts/x402_policy.py` | library: the policy gate | 629 | `agents_pay::{policy,amount,urlparse,token,config}` |
| `agents-pay/scripts/x402_fetch.py` | library: hardened fetch, settle, status, browser handle | 740 | `agents_pay::{net,agentcore,proof,flow,vault}` |
| `agents-pay/scripts/x402_fetch_cli.py` | entry point the agent runs | 83 | route `agents-pay fetch` |
| `agents-pay/scripts/agents_pay_admin.py` | human-run admin CLI | 1107 | routes `agents-pay admin *` |
| `agents-pay/scripts/test_x402_policy.py` | unittest, 120 tests in 24 classes | 1430 | Rust tests in S1-S6 |
| `agents-pay/scripts/test_resource_resolution.py` | assert script | 100 | S6 tests |
| `agents-pay/scripts/test_portability.py` | harness-load simulation | 187 | S7 test |
| `agents-build/scripts/setup_payment_user.py` | human-run provisioning | 120 | route `agents-build setup-payment-user` |
| `agents-build/scripts/process_payment_tool.py` | copy-into-your-agent template | 150 | Q2 (customer template, not a toolkit command) |
| `agents-pay/packages/openclaw/runtime/agentcore_bridge.py` and its tests | OpenClaw bridge | — | **#4411**, which reuses S4's AgentCore client |
| `agents-pay/packages/openclaw/test/test_setup_openclaw.py` | tests of `agents_pay_admin` functions | 99 | the inventory gives it to #4411. Its four contracts are ported here in S6 (Q12) |

Mirrors. `agents-pay` has no `skills/` canonical copy: the inventory's `canonical_identical_path`
is the plugin path itself. `packages/openclaw/skills/agents-pay/SKILL.md` is a byte-identical
tracked copy staged by `packages/openclaw/scripts/stage-skill.mjs`, which copies `SKILL.md`,
`requirements.txt`, `references/` and `scripts/`. S7 must update both copies. Staging of the
`scripts/` tree belongs to #4411.

### 1.2 `x402_fetch_cli.py` (the agent's interface)

| Input | Behaviour |
| --- | --- |
| positional `url` (optional) | fetch, and pay if the URL returns 402 |
| `--status` | wins over a URL: `payment_session_status()`, exit 0 if `usable` else 2 |
| `--browser-handle URL` | wins over the positional: `prepare_browser_payment(URL, purchase_id)` |
| `--method {GET,HEAD}` | default `GET`. Another value is an argparse usage error (exit 2) |
| `--purchase-id ID` | idempotency discriminator (section 4.4) |
| no URL and no flag | usage on stderr plus `"\nGive a URL, or --status."`, exit 1 |
| output | exactly one JSON document on stdout (`json.dumps`, default separators, `ensure_ascii`) |
| exit | 0 paid or no payment needed; 2 when `refused` is true (policy, configuration, any error inside the flow); 1 for usage or an unexpected crash; 1 on `KeyboardInterrupt` |

Library functions that the SKILL.md documents as importable (`from x402_fetch import x402_fetch,
payment_session_status`, plus `prepare_browser_payment` and `attach_browser_payment`) cannot be
imported from Rust. Q3 covers what replaces them.

### 1.3 `x402_fetch` outputs (key order as built)

- **Not 402:** `{"status_code", "paid": false, "untrusted": true, "content_type", "body_sha256", "bytes", "body_returned", …}`.
  With `return_body` off this adds `"note": <fixed text>`. With it on: `"body"` (the first 10240
  characters after a lossy UTF-8 decode), `"truncated"`, and `"untrusted": true` again (same key,
  so it keeps its first position).
- **Paid:** `{"status_code", "paid": 200<=s<300, "untrusted": true, "receipt": {"amount_usd", "network", "resource": origin+path}, …safe_content}`.
- **Exhausted:** `{"paid": false, "refused": false, "status_code": 402, "attempts": N, "reason": "Paid and replayed N times … no double charge. Retry shortly, or raise X402_MAX_PAYMENT_ATTEMPTS."}`.
  Exit 0, because `refused` is false.
- **Refused:** `{"paid": false, "refused": true, "reason": <PolicyError/PaymentBlocked text>}`, exit 2.
- **Any other exception:** `{"paid": false, "refused": true, "reason": "<ExceptionClassName> during payment flow."}`, exit 2.
- **Browser handle:** `{"paid": true, "handle": "x402h_<32 urlsafe>", "expires_in_seconds": 90, "receipt": {…}, "next_step": <fixed>}`.
  A non-402 response gives `{"paid": false, "refused": true, "reason": "URL returned N, not 402. No payment needed."}`.
- **Status:** `{"usable", "status", "budget_usd"?, "spent_usd"?, "next_step"?}`. With nothing
  configured: `{"usable": false, "reason": "No payment session is configured.", "next_step": …}`.
  If the SDK is missing, or on an exception: `usable: false` with a fixed or class-name reason.

### 1.4 `agents_pay_admin.py`

| Command | Flags | Effects |
| --- | --- | --- |
| `init-config` | `--max-per-payment-usd` (default `0.10`), `--region`, `--user-id`, `--network` (default `eip155:84532`), exactly one of `--recipient` (repeatable) or `--allow-any-recipient`, `--origin` (repeatable), `--path`, `--force` | refuses to overwrite without `--force` (exit 1). The network must be in `KNOWN_USDC`. Writes the policy, generates `user_id` (`agents-pay-` plus 12 hex), seeds `payment_manager_arn` from deployed state, prints the path, the payer identity, the JSON (indent 2) and fixed warnings |
| `show-config` | `--path` | prints the path, file and directory modes and the owner verdicts, then the resources and policy JSON, then the missing keys. Exit 1 when there is no config |
| `create-instrument` | `--user-id`, `--email` (required), `--network-family` (default `ETHEREUM`), `--manager-arn`, `--connector-id`, `--region`, `--path` | STS check; `CreatePaymentInstrument` (`EMBEDDED_CRYPTO_WALLET`, email linked account) **creates a wallet user at the connector**. Persists the resources, then prints the instrument, the wallet, the delegation URL and the funding steps. **No TTY gate** |
| `new-session` | `--user-id`, `--budget` (required, passed through as a string), `--expiry-minutes` (int, default 60), `--manager-arn`, `--region`, `--path` | **TTY gate first**. Then SDK import, STS check, user, manager, a confirmation that requires typing `approve`, then `CreatePaymentSession` with `maxSpendAmount{value, currency: USD}`, persists the session id. No `--yes` exists, by design |
| `preflight` | `--path` | loads the policy with the runtime checks; reports the presence of the `PAYMENT_*` variables, a discoverable manager ARN, any secret-named variable (`CDP_API_KEY_SECRET`, `WALLET_SECRET`, `APP_SECRET`, `AUTHORIZATION_PRIVATE_KEY`, matched as substrings of the upper-cased name) and SDK availability. Exit 0 only when all pass |
| `setup-openclaw` | `--path`, `--project-dir` | TTY gate; interactive wizard covering identity, region, network, recipients, cap, budget, expiry, origins and `return_body`. Writes the config, creates the instrument, waits for delegation, asks for `approve`, creates the session, prints the OpenClaw plugin JSON |

Exits: 0 on success, 1 on any handled failure. An API or SDK exception escapes as a Python
traceback on stderr with exit 1, and the traceback **includes the service message** (D12).

### 1.5 `agents-build/scripts/setup_payment_user.py`

The flags are `--user-id` (required), `--email` (required), `--budget`, `--expiry-minutes`
(default 60, documented as 15-480), `--network` (default `ETHEREUM`), `--manager-arn` (defaults
to `PAYMENT_MANAGER_ARN`), `--connector-id` (defaults to `PAYMENT_CONNECTOR_ID`) and `--region`
(defaults to `AWS_REGION`, else `us-east-1`).

It reads deployed state from `agentcore/.cli/deployed-state.json` only. It understands the list
layouts only: a dict-shaped 0.26.x layout raises inside the `try` and yields no value. It creates
the instrument, and the session only when `--budget` is given. It then prints the IDs and
`export PAYMENT_…=` lines for the operator to copy. Failure is `sys.exit(msg)`, which means exit
1 with the message on stderr, or a traceback.

### 1.6 Environment variables

| Variable | Read by | Meaning |
| --- | --- | --- |
| `PAYMENT_MANAGER_ARN`, `PAYMENT_INSTRUMENT_ID`, `PAYMENT_SESSION_ID`, `PAYMENT_USER_ID`, `AWS_REGION` | runtime | **fallback only**: the config file wins whenever it has a truthy value (R-RES-RT) |
| `X402_MAX_BODY_BYTES` (default 262144, range 1024..=64 MiB), `X402_TIMEOUT_SECONDS` (20, 1..=120), `X402_MAX_PAYMENT_ATTEMPTS` (5, 1..=10) | runtime | tunables, parsed with `float()`. An out-of-range or unparsable value falls back to the default (R-TUNE) |
| `HOME`, `AGENTS_PAY_CONFIG`, `X402_POLICY_FILE` | **ignored by the runtime** on purpose | the runtime config path comes from the OS account database |
| `AGENTS_PAY_CONFIG`, `HOME` (`Path.home()`) | admin | admin config path (flag > env > `~/.agents-pay/config.json`) |
| `AGENTCORE_PROJECT_DIR` | admin | deployed-state search root |
| `PAYMENT_CONNECTOR_ID`, `AWS_DEFAULT_REGION` | admin | connector and region resolution |
| boto3's variables (`AWS_PROFILE`, keys, `AWS_CA_BUNDLE`, `AWS_ENDPOINT_URL*`, `AWS_USE_FIPS_ENDPOINT`, …) | both, implicitly | the credential chain and endpoint. Section 5 restricts this on the runtime path |

### 1.7 Files

| File | Access | Rules |
| --- | --- | --- |
| `<OS-account home>/.agents-pay/config.json` | runtime read | lstat: not a symlink, a regular file, owned by `getuid()`, `mode & 0o077 == 0`. Parent: owned by uid, `mode & 0o022 == 0`. JSON object. Nested `{resources, policy}` or a flat legacy policy |
| admin `--path`, `AGENTS_PAY_CONFIG` or `~/.agents-pay/config.json` | admin read and write | `load_raw_config` is lenient (missing or invalid → empty skeleton; no permission checks). Writes are atomic at 0600: mkdir 0700 with parents, chmod the directory 0700, `mkstemp` in the same directory, fchmod 0600, write, fsync, `os.replace`, chmod 0600 |
| `agentcore/.cli/deployed-state.json`, `.cli/deployed-state.json`, `../.cli/deployed-state.json` (relative to the working directory), or `<root>/agentcore/.cli/…` and `<root>/.cli/…` | admin read | first existing. Three layouts: 0.20.x `targets.<t>.resources.payments[]`, 0.26.x dict-keyed `payments` and `connectors`, legacy top-level `payments` |

### 1.8 Python tests to carry over

`test_x402_policy.py` classes: PolicyFile, ConfigPrecedence, RecipientAndValue,
SingleTenantUserId, RecipientValidation, AdminRecipientMode, OptionalOrigin, Ssrf, Idempotency,
PolicyDirectory, SignerInput, TransientSettlement, SafeMethod, HarnessCli, BrowserHandle,
SessionStatus, DnsPinning, Authorize. Many of them inspect source with `inspect.getsource`, for
example "`for attempt` follows `authorize_payment`" or "`Accept-Encoding: identity` appears". Each
of those becomes a **behavioural** Rust test on fakes. Examples: count `ProcessPayment` calls and
record their `clientToken`; assert the request headers; assert what the signer received. Section
8 maps the tests.

## 2. The x402 flow (current behaviour, kept)

```
fetch(url, method, purchase_id)
 1  policy = load_config()                       refuse before any network I/O
 2  origin = assert_public_https_url(url)        https, host, no userinfo, every DNS answer public
 3  allowed_origins (optional) must contain origin
 4  probe = GET|HEAD url                         pinned public address, no redirect, identity, cap
 5  probe.status != 402  -> metadata output, no payment
 6  challenge = extract(probe)                   header (base64 JSON, then raw JSON), then body
 7  decision = authorize_payment(url, challenge) re-vets 2-3, selects ONE entry, canonical amount,
                                                 version, resource binding, client_token
 8  manager/instrument/session/user resolved     missing -> refused
 9  for attempt in 1..=N:                        same client_token every attempt
        header = generate_payment_header(vetted single-entry challenge)   <- ProcessPayment
        resp = GET|HEAD url + header ; drop header
        resp.status != 402 -> break
10  still 402 -> "exhausted, no double charge" ; else paid output (paid = 2xx)
```

The Rust flow keeps this order. Section 3 records what changes. The one structural change is
**R-PP-ONCE**: `ProcessPayment` is called once per invocation, and the replays reuse its proof.

## 3. Requirements: challenge, policy and spending (AC1)

### R-CH. Challenge extraction (`extract`)

1. Header first: `payment-required`, else `x-payment-required`. Several field lines with the same
   name are joined with `", "`, which is httpx's `get` behaviour.
2. Base64 strict decode, matching `b64decode(validate=True)`: standard alphabet, correct padding,
   no other bytes. Then UTF-8 decode, then JSON. Accept if the result is an object whose
   `accepts` is Python-truthy.
3. Otherwise the raw header text is parsed as JSON, with the same acceptance rule.
4. Otherwise the body is decoded (lossy UTF-8, Q11) and parsed as JSON. A parse failure gives
   `"402 response contains no parseable x402 challenge."`. If the result is an object with
   truthy `accepts` → it. If its `challenge` is an object with truthy `accepts` → that. Otherwise
   `"402 challenge is missing the required 'accepts' array."`.
5. Python truthiness on JSON: `null`, `false`, `0`, `0.0`, `""`, `[]`, `{}` are false.
6. Bounds: the decoded header JSON is at most 64 KiB. Body JSON is bounded by the body cap (R-NET).
   JSON nesting is limited to `toolkit_core::json::MAX_JSON_DEPTH`.
7. **D1 (stricter): duplicate object keys are refused.** `toolkit_core::json::parse_json` already
   rejects them. Python's `json.loads` takes the last value, and that difference would let a
   publisher show one value to a log reader and another to the gate. `NaN`, `Infinity` and
   `-Infinity` literals, which Python accepts, are also refused.

### R-POL. Entry selection (`select_accept_entry`): order and texts unchanged

The policy shape is validated before the loop, each with the Python text:

- `allow_any_recipient` must be a boolean when present.
- `allow_any_recipient` and `allowed_recipients` are mutually exclusive. This is decided by
  **presence**, so `false` together with a list is still an error.
- `allowed_assets` must be an object. `allowed_networks` must not be empty.
- `allowed_schemes` defaults to `["exact"]` when the key is absent; an explicit `[]` gives
  `"allows no schemes"`.
- Unless any recipient is allowed, there must be at least one recipient.
- List keys must be lists (`_policy_list`); their items are rendered with Python `str`.

Each `accepts` entry is then taken in order and skipped (never refused on its own) if any of the
following holds:

- it is not an object;
- `scheme`, `network`, `asset` or `payTo` is missing, null or `""`;
- the scheme is not in `allowed_schemes`, or the network is not in `allowed_networks`;
- the lower-cased asset is not in `allowed_assets[network]` (also lower-cased);
- the lower-cased `payTo` is not in `allowed_recipients` and any-recipient is off;
- the amount is invalid, conflicting, or above the per-payment cap (R-AMT).

The first entry that survives is returned. If none survives, the uniform text applies (`"No payment option in this challenge satisfies
the configured policy (scheme, network, asset, recipient, and amount ceiling are all enforced).
Refusing to pay."`). Uniformity is a security property: the message does not name which field
failed, so a publisher cannot probe the policy. Test T-POL-UNI asserts that the text is
byte-identical across every failure class and that no challenge value appears in it.

Scalar rendering (`str(entry[...])`) follows Python: a string as is; an integer in decimal;
`true`/`false` as `True`/`False`; a float with Python `repr`; `null` as `None`. **D2:** an
array or object field makes the entry ineligible. Python would compare its `repr`, which can only
match an allowlist entry that is itself a Python repr. Neither gives a payment.

**D3:** an `allowed_assets[network]` member that is not a string refuses with `"Payment policy
'allowed_assets' must map networks to lists of strings."`. Python raised `AttributeError`, which
came out as the generic refusal.

**Network family (D4, Q8).** The Rust port pays only on `eip155:*` networks. A selected entry on
another family is refused after selection with `"Payment network family is not supported by
this runtime."`. The reason is that the proof checks of R-PP-VERIFY are specified for EVM
EIP-3009 authorizations only. `KNOWN_USDC` only writes `eip155` policies, so only a hand-edited
Solana policy is affected.

### R-AMT. Amounts and the per-payment cap

- **Challenge amount grammar** (`base_units_to_usd`): a JSON integer, or a string of **ASCII**
  digits only. Python's `str.isdigit` also admits Unicode digits such as `"٣"`; Rust refuses them
  (D5). The value must be greater than zero. These are refused: `1E+7`, `Infinity`, `NaN`,
  `1_000_000`, `500000.0`, surrounding spaces, `+500000`, `0x7A120`, `""`, `-500000`, a JSON
  float, and `true` (Python `True` is an int whose `str` is `"True"`, refused there too). Leading
  zeros are accepted, and the text is kept verbatim, as in Python. A JSON integer outside `i64`
  becomes `Json::Float` in `toolkit-core`, so it is refused. Python would accept it, find it
  above any sane cap, and skip the entry; the outcome is the same.
- **Aliases** (`validated_amount_units`): if both `amount` and `maxAmountRequired` are non-null
  and their Python `str` forms differ → `"Payment challenge contains conflicting amount fields."`.
  Otherwise `amount`, else `maxAmountRequired`, else `"Payment challenge has no amount."`. The
  canonical text is `str(resolved)`.
- **Cap grammar** (`max_per_payment_usd`, with the legacy name `max_amount_usd`): Python accepts
  anything `Decimal(str(x))` accepts. Rust accepts a JSON number, using its literal digits, or a
  string in the finite Python-`Decimal` grammar: surrounding whitespace, a sign,
  digits/`.`/exponent, and `_` between digits. The cap must be `> 0`.
  - **D6: `Infinity`, `NaN`, `sNaN` and an exponent beyond ±1000 are refused** with `"Payment
    policy '<key>' is not a decimal number."`. In Python, `Infinity` passed `cap <= 0` and made
    the per-payment ceiling unbounded, and `NaN` raised an uncaught `InvalidOperation`.
  - A JSON float literal is compared through its literal digits. Python converts the float to a
    shortest `repr` first. The two differ only for literals with more than 17 significant
    digits; the difference is recorded and is not a risk.
- **Comparison** with no floating point: `amount_units / 10^6 > cap` is evaluated as
  `amount_units * 10^s > cap_digits * 10^(6 - e)` over decimal digit strings, comparing length
  first and then lexicographically. No big-integer crate is needed. `USDC_DECIMALS = 6` is used
  for every asset, as in Python (Q9).
- `amount_usd` in the receipt: `str(Decimal(units) / Decimal(10) ** 6)` under Python's default
  context (28 significant digits, ideal exponent 0). Examples: `"0.1"` for 100000, `"1"` for
  1000000, `"10"` for 10000000, `"0.000001"` for 1 and `"0.5"` for 500000. Leading zeros in the
  units do not show. The result is at least 1E-6, so the small-exponent form (adjusted exponent
  below -6) never occurs. The exponent form `…E+n` occurs only when the quotient needs more than
  28 significant digits, and the context then rounds it. T-AMT-USD pins this table and that
  boundary (Q10).

### R-VER. Version and vetted entry

`x402_version = int(challenge.x402Version or challenge.version or 1)`, using Python `int()`
semantics on ASCII: a JSON int; a float truncated toward zero; a string with whitespace, a sign,
digits and `_`; booleans 0/1, where `false` is falsy and so falls through. Anything else, or a
value outside `i64`, gives the generic `ValueError` refusal (R-ERR, D7 for the range).

The vetted entry is a copy of the selected entry, in the original key order, with `amount` and
`maxAmountRequired` removed. The canonical amount is then set under `maxAmountRequired` when the
version is 1 and under `amount` otherwise. In a Python dict, setting a key that was popped puts
it at the end, and Rust must reproduce that.

### R-RESBIND. Resource binding (`_validated_resource`)

If `challenge.resource` is an object whose `url` is a string, then
`scheme://netloc+path` (rstrip `/`) of the requested URL must equal the same form of the resource
URL. `netloc` is raw and case-sensitive; Python's `urlparse` path excludes `;params` (R-URLPARSE).
A mismatch gives `"Challenge resource.url does not match the requested URL."`. The sanitized
`{"url": <resource url>}` is the only resource the signer and the header ever see. Any other
shape counts as no resource.

### R-URLPARSE. Python `urlparse` compatibility (origin, path and token material)

Token material, receipts, handle binding and resource binding all use Python's `urlparse`, not
WHATWG URL parsing. The two differ on params, IPv6 brackets and case. A small module
`agents_pay/urlparse.rs` therefore reproduces the subset that matters:

- the scheme is lower-cased;
- `netloc` runs from `//` to the first of `/`, `?` or `#`;
- `hostname` is `netloc` with userinfo and port removed, brackets stripped and the result
  lower-cased;
- `port` is the decimal after the last `:` outside the brackets. Python raises `ValueError` when
  it is out of range or not numeric, and Rust gives the generic refusal;
- `path` runs to `?` or `#`, and then, as in Python's `_splitparams`, the part from the first `;`
  in the last path segment onwards is removed.

`_canonical_origin` is `scheme://host`, with `:port` added only when the port is truthy and not
443. Python quirks are kept on purpose and covered by tests:

- An IPv6 host loses its brackets in the origin (`https://2606:4700::1`), so an `allowed_origins`
  entry written with brackets never matches. That fails closed.
- Port 0 is dropped from the origin.

These functions are **only** used for comparison and hashing. The request itself goes out to the
validated URL as given.

### R-ORIGIN. Destination checks before any network I/O

- `assert_public_https_url` requires: scheme `https`; a host; no userinfo. Then every resolved
  address must be public under `toolkit_aws::endpoint::is_non_public`. That table is wider than
  Python's `ipaddress` predicates plus CGNAT: it adds the documentation and benchmarking ranges,
  6to4, IPv6 outside `2000::/3`, mapped addresses and NAT64 (D8, stricter, fail-closed).
- Every failure keeps the Python fixed text: `"Only https:// payment URLs are allowed."`, `"Payment
  URL has no host."`, `"Payment URL must not contain embedded credentials."`, `"Payment URL
  hostname does not resolve."`, `"Payment URL resolves to a non-public address."`. The
  `public_addresses` message of `toolkit-aws` names the host and the address, so it is **never**
  shown; it is mapped to these texts.
- `allowed_origins`, when present and non-empty: the lower-cased origin must equal some entry,
  lower-cased with trailing `/` stripped. Otherwise `"Origin {origin} is not in the configured
  allowed_origins."`, with `{origin}` passed through `shown_value` (caller value, #4941).
- **D9:** in the fetch pre-check a non-list `allowed_origins` refuses with `"Payment policy
  'allowed_origins' must be a list."`. Python iterated the characters of a string and so refused
  with the "not in" text.

### R-SPEND. Spend limits: where each is enforced, and that all of them precede signing

| Limit | Enforced by | When |
| --- | --- | --- |
| per-payment ceiling `max_per_payment_usd` | this runtime (R-AMT) | before `ProcessPayment` |
| network, asset, scheme, recipient, origin | this runtime (R-POL, R-ORIGIN) | before `ProcessPayment` |
| cumulative session budget `maxSpendAmount` | AgentCore, server-side, for the session | inside `ProcessPayment`. The runtime cannot raise it: no runtime code path calls `CreatePaymentSession` |
| session expiry (15..=480 min) | AgentCore | inside `ProcessPayment` |
| who may sign | IAM: ProcessPaymentRole versus ManagementRole | AWS |

**Ordering invariant (R-SPEND-ORDER).** The only code that can construct a `ProcessPayment`
request is `agentcore::process_payment(&Approved, …)`. `Approved` is a type with a private
constructor, created only by `policy::authorize` after every check in this section has passed.
It carries the single vetted entry, the canonical amount, the version, the sanitized resource and
the token.

A compile-fail test (doc-test with `compile_fail`) shows that `Approved` cannot be built outside
`policy`. T-ORDER runs the flow over a recording fake and asserts, for every refusal class in
sections 3 and 4, that **zero** `ProcessPayment` calls were recorded.

No local cumulative ledger is added. The service already enforces the budget, and a local
counter would be a second source of truth that drifts (Q13).

### R-TOK. Idempotency key (`derive_client_token`): byte-identical to Python

```
material = "\x1f".join([session, canonical_origin(url), urlparse(url).path or "/",
                        str(network), str(asset).lower(), str(payTo).lower(),
                        canonical_amount_text, purchase_id or ""])
client_token = sha256_hex(material.encode("utf-8"))      # toolkit_core::digest::sha256_hex
```

- `session` is the config-first resolved `payment_session_id`, or `""` when there is none. The
  flow refuses later anyway.
- The publisher nonce, `extra`, `maxTimeoutSeconds` and the query string are **excluded**, as in
  Python. A rotating nonce therefore cannot turn a retry into a second payment.
- **Byte-identity matters across the upgrade.** A purchase started by the Python CLI and retried
  by the Rust binary derives the same token, so `ProcessPayment` replays the same authorization.
  These golden vectors were computed with coreutils `sha256sum` from the material definition
  above; no Python was run. Asset `0x036cbd53842c5426634e7929541ec2318f3dcf7e`, payTo
  `0x1111111111111111111111111111111111111111`, network `eip155:84532`, amount `100000`:
  - session `ps-1`, URL `https://merchant.test/v1/x402-test` (also `https://MERCHANT.test:443/v1/x402-test?x=1`
    and `…/x402-test;v=2`), no purchase id → `c4293dfd364640b953d5ab169ea31f99eed39e706ace87581a511c65b06c2a87`
  - the same with `--purchase-id order-7` → `78b5376d8941566a4f28e9e74c1b738a42c651464fd56a4d853c42ddb9ef98cf`
  - URL `https://merchant.test:8443` (empty path → `/`) → `4cea55bdb8d2335d8bf4a1a5d40a0f1b92e61d8670a47113ad92ebac812110a9`
- The model's `ClientToken` pattern `[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}`, with length 33..=256,
  accepts the 64 lower-hex characters.
- **Kept trade-off.** Two deliberate buys of the same origin, path, terms and session, with no
  `--purchase-id`, collapse to one authorization. That includes two URLs that differ only in the
  query. The second buy is a replay, not a second charge. The SKILL text already documents
  `--purchase-id` for this case.
- Cross-runtime note for #4411. OpenClaw's `deriveClientToken` adds `scheme` and leaves out the
  purchase id, so the same purchase made through OpenClaw and through the CLI gets two tokens.
  This port does not change it; it is recorded for #4411 (Q14).

### R-PP. `ProcessPayment`: the only spending step

- **R-PP-ONCE (D10).** For one invocation, `ProcessPayment` is sent once, with the derived token.
  The transport retries inside that one logical call (R-AWS-RETRY) reuse the same token and body,
  which AWS defines as idempotent (`"idempotent": true` in the model). The merchant replays reuse
  the **same proof header**.

  Python called `generate_payment_header` again on every replay attempt, with the same token. If
  the SDK behaves idempotently, which Python's own comment relies on, that returns the same
  authorization. The Rust form makes it structural instead of depending on server idempotency,
  and it saves N-1 AWS calls.

- **Request** (model, section 6.1): `POST /payments/processPayment`, rest-json. Headers
  `X-Amzn-Bedrock-AgentCore-Payments-User-Id: <user_id>` and
  `X-Amzn-Bedrock-AgentCore-Payments-Agent-Name: openclaw-aws-agents-pay`. Python's runtime
  `AGENT_NAME` really is `openclaw-aws-agents-pay`, and the test
  `test_runtime_calls_are_attributed_to_the_skill` pins it. Body:

  ```json
  {"paymentManagerArn": "...", "paymentSessionId": "...", "paymentInstrumentId": "...",
   "paymentType": "CRYPTO_X402",
   "paymentInput": {"cryptoX402": {"version": "<1|2 as decimal string>", "payload": <PP-PAYLOAD>}},
   "clientToken": "<64 hex>"}
  ```

  **PP-PAYLOAD [inferred, Q4]:** the vetted entry with the canonical amount always under
  `amount` (CAIP-2 input is the same for v1 and v2, per the wiring reference), all other members
  in order, and `maxAmountRequired` removed. Two members are checked before signing (D11,
  stricter); either failure is a selection skip, so the entry is ineligible rather than signed:
  - `maxTimeoutSeconds`, if present, must be a JSON integer in 1..=3600;
  - `extra`, if present, must be an object of at most 4 KiB serialized.

  `permit2AllowanceLimit` is never sent.

- **Response** (cap 64 KiB, R-AWS-BODY): `status` must be `PROOF_GENERATED`. The echoed
  `paymentManagerArn`, `paymentSessionId` and `paymentInstrumentId` must equal the request.
  `paymentOutput.cryptoX402.payload` must be an object, or a JSON string holding an object (as the
  TypeScript accepts), of at most 16 KiB. Anything else → `"Payment header generation returned an
  unexpected shape."` (the Python text).
- **R-PP-VERIFY (D12, new defence in depth).** Before the proof is sent anywhere,
  `payload.authorization` must exist. Its `to` must equal the vetted `payTo` (case-insensitive),
  its `value` must equal the canonical amount, and `validBefore` must lie in the future on the
  injected clock. Otherwise → `"Signed payment does not match the approved terms."`, exit 2, and
  the proof is dropped unsent.

  An unsent EIP-3009 authorization moves no funds. Python trusted the SDK output. Rust still
  trusts AgentCore to sign, but it never hands a merchant terms the gate did not approve.

- **validAfter.** If `authorization.validAfter` is in the future, wait until one second after it,
  but only if that is at most 15 s away; otherwise refuse `"Signed payment is not valid within the
  bounded retry window."`. This is the OpenClaw TypeScript behaviour; Python's SDK behaviour is
  unknown (Q5).

### R-HDR. The proof header [inferred, Q4]

| Version | Header | Value |
| --- | --- | --- |
| 2 (anything not 1) | `PAYMENT-SIGNATURE` | base64 (standard, padded) of compact JSON `{"x402Version":2,"accepted":<vetted entry>,"payload":<signed payload>,"resource":<sanitized resource>}`. `resource` is omitted when absent |
| 1 | `X-PAYMENT` | base64 of compact JSON `{"x402Version":1,"scheme":"exact","network":<entry network>,"payload":<signed payload>}` |

This follows the AgentCore wiring reference, which names the Coinbase facilitator
`x402V2PaymentPayload` schema, and the OpenClaw TypeScript envelope. The signed payload is copied
as returned and is never rebuilt from publisher data. The header value is a
`toolkit_aws::redact::Secret`; R-SEC covers its lifetime.

### R-RETRY. Retries that can never double-pay

| Retry | Mechanism | Why it cannot double-pay |
| --- | --- | --- |
| AWS transport retry of `ProcessPayment` (throttling, 5xx, connect, timeout; the shared `RetryPolicy` default of 3 retries) | same body, same `clientToken` | AWS idempotency: the same token with the same parameters returns the first result |
| Merchant replay while it still returns 402 (attempts 1..=`X402_MAX_PAYMENT_ATTEMPTS`) | the **same header bytes**, no new `ProcessPayment` (R-PP-ONCE) | one authorization, one nonce: it settles once at most |
| A 402 on a replay | its challenge is **never parsed or authorized** (Python ignored it as well; now asserted) | a merchant cannot raise the amount or change the payee mid-flow |
| A new invocation after a crash or lost response | deterministic token (R-TOK) | the same token leads to the same authorization |
| Merchant transport failure on a replay (connect, timeout, cap) | **not retried**. It ends the invocation with the generic refusal text (Python: the exception escaped the loop, giving the same result) | the caller's next invocation replays by token |

No sleep happens between replays in Python. **D13 (Q6):** Rust waits a fixed 1 s between merchant
replays, within the operation deadline (R-NET). An immediate replay of lagging settlement is
almost certain to fail again. The exhausted text keeps "no double charge". T-RETRY asserts:

- `ProcessPayment` is recorded once per invocation over N replays;
- every recorded `clientToken` equals the golden vector;
- the replay headers are byte-equal;
- a hostile 402 challenge on the replay (higher amount, other `payTo`) changes nothing;
- `X402_MAX_PAYMENT_ATTEMPTS` set to `0`, `-1`, `abc`, `99999` or `nan` gives 5.

## 4. Requirements: network, TLS, timeouts and caps

### R-NET. Merchant plane (probe and paid replay)

- **Transport.** #5017 S1's `StreamingTransport::open` over `SdkTransport::with_resolver`, built
  from the R-ROOTS roots (R-TLS-PAY below) and a `GuardedResolver`. Every DNS answer is vetted at
  connect time, and the socket peer is checked to be public (`peer_check`). The pinned
  connector's re-vet replaces Python's `_PinnedResolverTransport`, with no global patching.
- **Method** `GET` or `HEAD` only. Anything else gives the Python `PaymentBlocked` text,
  case-insensitively (`get` → `GET`). No request body ever.
- **Request headers**, exactly:
  - `Accept: application/json, text/plain`;
  - `Accept-Encoding: identity`;
  - on a replay, the one proof header;
  - **D14:** `User-Agent: aws-agents-pay/<crate version>` instead of `python-httpx/0.28.1`.

  No cookies: there is no jar, and every request is fresh. That is the "fresh client" rule of
  the wiring reference.
- **No redirects.** A 3xx is the response, reported as `status_code` with `paid: false`.
  `Location` is never read or printed. On a replay, a 3xx ends the loop just as a 2xx does
  (parity), so `paid` is false.
- **No proxy.** `HTTPS_PROXY` and the others have no effect. Python's explicit transport ignored
  them too.
- **Content-Encoding.** Rust never decompresses (D15, Q7). With a non-identity encoding, a probe
  body does not parse as a challenge, so the result is a refusal and no payment. A paid body is
  hashed and counted as received, and the output gains no new key. Python's httpx would have
  decompressed despite the `identity` request.
- **Body cap** `MAX_BODY_BYTES` (R-TUNE): `StreamLimits::new(cap, 64 KiB)`.
  - A `Content-Length` of ASCII digits greater than the cap is refused at status time with no
    pull: `"Response declares {n} bytes, over the {cap} limit."`, where `{n}` goes through
    `shown_value`.
  - Streaming past the cap: `StreamError::TooLarge` → `"Response exceeded {cap} bytes; refusing
    to buffer more."`. Nothing past the cap is buffered.
- **Deadlines** (TA-STREAM-DL, #5080):
  - connect `min(5 s, T)`;
  - idle read `T` (Python's `httpx.Timeout(T)`);
  - **D16:** an absolute per-request deadline of `3 × T` (default 60 s). Python had no absolute
    bound, so a server trickling one byte every `T - ε` held the process indefinitely within the
    cap.
  - Operation deadline for one `fetch`: `(2 + attempts) × 3 × T + 60 s`, which covers the probe,
    the replays and the AWS call. Hitting it gives the generic refusal text for a timeout
    (R-ERR).
- **Content type** for output: the first `;`-segment, stripped and lower-cased, bounded to 256
  characters (D17; Python was unbounded), or `unknown`.

### R-TLS-PAY. Trust roots on the payment runtime (Q15)

Python preferred certifi (the Mozilla bundle) and otherwise the system store, never with
verification disabled. Botocore uses certifi and honours `AWS_CA_BUNDLE`.

The runtime treats caller-controlled environment as untrusted for security configuration, which
is why it ignores `HOME` and config-path overrides. Rust extends that rule to trust roots. The
**runtime** (`agents-pay fetch`, both planes) builds `ValidatedRoots` from `RootEnv::default()`,
the platform store only, ignoring `SSL_CERT_FILE`, `SSL_CERT_DIR` and `AWS_CA_BUNDLE`.

Otherwise an agent that can set one variable could install its own CA and observe the proof on
either plane. The proof is the very thing the model must never see.

**Admin** commands use the normal R-ROOTS behaviour (`RootEnv::from_process()`), because the
human's environment is trusted there. Roots that cannot be validated fail closed with the S1
`RootsFailure` text. Verification is always on, and no flag disables it.

### R-AWS. AgentCore and STS plane

- **Endpoint.** `https://bedrock-agentcore.{region}.amazonaws.com` (endpoint rule set, aws
  partition, no FIPS or dual-stack), built by `toolkit_aws::endpoint::Endpoint::for_service`, and
  `https://sts.{region}.amazonaws.com` for the admin STS check.
  - **D18: the runtime ignores `AWS_ENDPOINT_URL`, `AWS_ENDPOINT_URL_BEDROCK_AGENTCORE` and
    `AWS_USE_FIPS_ENDPOINT`.** Botocore honoured them, so an agent could have sent the SigV4-signed
    `ProcessPayment` to its own host and replayed it within the 5-minute signature window, which
    yields a proof.
  - Admin commands keep the standard endpoint too (Q16).
- **Region.** Runtime: config `resources.region` > `AWS_REGION` > the region field of the
  manager ARN (D19, Q17) > the profile region (the toolkit-aws region chain). Admin: the Python
  chain (R-RES-ADM). `is_valid_region` is enforced.
- **Credentials.** The #5017 S1 guarded chain (`default_chain_with_http`, R-SRC/R-PROV, D2 null
  stdin for `credential_process`). These are the same sources as boto3. Under the IAM split the
  agent runs as `ProcessPaymentRole`; this port does not change IAM.
- **R-AWS-BODY.** Every AWS response is read through `StreamingTransport` with a 64 KiB ceiling
  for success and error bodies alike, using the launch R-API-BODY sequence.
- **R-AWS-RETRY.** The shared `RetryPolicy` default: 30 s connect and 30 s read per attempt, and
  a 60 s absolute deadline per logical call. Botocore defaults were 60/60 s with legacy 5
  attempts, so this is recorded as D20.
- **R-AWS-CODE.** Error codes are normalised as in launch R-API-CODE: the header
  `x-amzn-ErrorType`, else the body keys `__type`, `code`, `Code`; the text after the last `#`
  and before the first `:`; compared byte-exactly against an allowlist. The allowlist is the
  model's errors for these four operations:
  - `AccessDeniedException`, `ConflictException`, `InternalServerException`,
    `ResourceNotFoundException`, `ServiceQuotaExceededException`,
    `SubscriptionRequiredException`, `ThrottlingException`, `ValidationException`;
  - plus the launch protocol list.

  Nothing received is printed beyond the matching constant (`UnrecognizedError` when there is no
  match). The service message, `fieldList` and request id are never printed, because a
  `ValidationException` can echo the email, the session id or the payload.

## 5. Requirements: keys, credentials and redaction

### R-SEC. Where secrets come from and where they never go

| Secret | Source | Never |
| --- | --- | --- |
| AWS credentials | S1 guarded chain (env, profile, SSO, login, `credential_process`, web identity, container, IMDS) | in argv; printed; passed to a child except the SDK's own `credential_process` (null stdin, inherited env, as boto3) |
| wallet and provider keys (`CDP_API_KEY_SECRET`, `WALLET_SECRET`, Privy app secret, authorization private key) | **not in this process at all**: AgentCore's token vault and connector | read by any Rust code. `preflight` still fails when one is present in the environment (parity) |
| the proof (signed `PaymentDocument` and the header value built from it) | `ProcessPayment` response | stdout, stderr, logs, `Debug`/`Display`, files, the model; the handle vault holds it only in process memory |
| `payment_session_id` (a spending identifier, not a signing secret) | config (0600) or env | in runtime output. Admin output prints it, as Python does, for the human to copy (Q18) |
| AgentCore delegation `redirectUrl` | `CreatePaymentInstrument` | runtime output; printed by admin for the end user, as data |

- **Typed secrets.** The proof bytes, the header value and the AWS keys are wrapped in
  `toolkit_aws::redact::Secret`, whose `Debug` is redacted. `ProcessPayment` request and response
  structs implement `Debug` by hand and print `<redacted>` for the payload. T-SEC-DBG covers it.
- **Lifetime.** The header is built once (R-PP-ONCE), borrowed for each replay and dropped when
  the loop ends, on every path including errors (RAII; no `del` discipline is needed). No
  `zeroize`, following the launch decision; Python's `dict.clear()` gave no stronger guarantee.
- **No children with secrets.** The runtime spawns no process. Admin spawns none either: it only
  *prints* `agentcore` commands for the human. No secret is ever placed in a child's argv or
  environment.
- **No secret in argv.** No route accepts a key, token or secret. The route's argv goes through
  `toolkit_aws::credentials::reject_credential_arguments` (the asm-exec pattern), so a pasted
  `AKIA…` or `--secret…` value is refused before any work is done.

### R-RED. Redaction of every output and error

1. **Fixed texts first.** Every runtime reason is a Python constant, with one of three kinds of
   inserted value:
   - a caller value through `shown_value`: an origin, a resource key name, an env var name;
   - a path through `shown_path`;
   - a server value bounded through `shown_value`: a declared length, a status string.

   The generic error texts use a fixed class-name table (R-ERR), never `Display` of an error.
2. **R-SCRUB (D21, launch's known-secret scrub).** Immediately before stdout or stderr is
   written, the serialized text is searched for every live secret of the invocation that is at
   least 8 bytes long: the proof header value, the signed payload's `signature`, the session id
   and any AWS secret, using `safe_text`'s fragment matcher with `MAX_MATCH_WORK` fail-closed.
   - A hit inside the opt-in `body` replaces each occurrence with `[REDACTED]` and adds
     `"redacted": true`.
   - A hit anywhere else replaces the whole document with the generic refusal.

   This closes a hole Python had: a merchant that **echoes the `PAYMENT-SIGNATURE` header in its
   body** would have handed the proof to the model under `return_body: true`.
3. Status strings and amounts from AgentCore are bounded through `shown_value`.
4. The opt-in body is untrusted data. It is lossy-decoded and cut to 10240 scalar values, and the
   JSON serializer escapes it. It is not pattern-redacted beyond R-SCRUB, because it is the
   publisher's content, which the operator chose to receive.

### R-ERR. Generic error texts (class-name table)

Python printed `f"{type(e).__name__} during payment flow."`. Rust maps each internal failure to a
fixed name that matches what Python would have raised:

| Failure | Name |
| --- | --- |
| connect or TLS | `ConnectError` |
| connect timeout | `ConnectTimeout` |
| idle or absolute deadline | `ReadTimeout` |
| malformed HTTP | `RemoteProtocolError` |
| any AgentCore error response | `ClientError` |
| no credentials | `NoCredentialsError` |
| `int()` failure | `ValueError` |
| a JSON failure where Python would raise | `JSONDecodeError` |
| anything else | `RuntimeError` |

The same table applies to `"{name} while reading session status."`. The table is exhaustive over
the Rust error enum, so adding a variant forces a choice; T-ERR enforces it.

## 6. Requirements: resources, configuration and portability

### 6.1 What was taken from the AgentCore model

| Operation | HTTP | Required input | Notes |
| --- | --- | --- | --- |
| `ProcessPayment` | `POST /payments/processPayment` → 200 | `paymentManagerArn`, `paymentSessionId`, `paymentInstrumentId`, `paymentType`, `paymentInput` | `clientToken` is an idempotency token; `userId` and `agentName` are headers; `paymentInput`/`paymentOutput` are unions (`cryptoX402`, `mpp`); `PaymentDocument` is a sensitive document; `status` enum is `PROOF_GENERATED` only |
| `GetPaymentSession` | `POST /payments/getPaymentSession` → 200 | `paymentManagerArn`, `paymentSessionId` | response `{paymentSession}` |
| `CreatePaymentSession` | `POST /payments/createPaymentSession` → 201 | `paymentManagerArn`, `expiryTimeInMinutes` (15..=480) | `limits.maxSpendAmount{value, currency}`; botocore fills `clientToken` with a UUID4 when it is absent |
| `CreatePaymentInstrument` | `POST /payments/createPaymentInstrument` → 201 | `paymentManagerArn`, `paymentConnectorId`, `paymentInstrumentType` (`EMBEDDED_CRYPTO_WALLET`), `paymentInstrumentDetails` (union `embeddedCryptoWallet{network: ETHEREUM\|SOLANA, linkedAccounts (max 1, sensitive)}`) | response `{paymentInstrument{…, paymentInstrumentDetails.embeddedCryptoWallet{walletAddress, redirectUrl}}}` |

**`PaymentSession` has no `status` and no `spentAmount` member.** Its members are
`paymentSessionId`, `paymentManagerArn`, `limits`, `userId`, `expiryTimeInMinutes`, `createdAt`,
`availableLimits{availableSpendAmount, updatedAt}` and `updatedAt`. Python's status reads
`status`/`sessionStatus` and `spentAmount`. Unless the SDK synthesises them **[unknown]**, the
Python `--status` always reported `usable: false, status: "unknown"`. Q19 records the
recommendation to compute usability from the model members the way OpenClaw does.

The JSON request and response shapes are pinned in
`crates/toolkit-cli/src/agents_pay/model/agentcore-payments.json`. That file is an excerpt of the
four operations and their shapes, with its source version and sha256 recorded in a header
comment. It is embedded with `include_str!`, and every response member read goes through
`toolkit_aws::recorded`, following the #4751 contract. Responses are read leniently: unknown
members are ignored and union members other than `cryptoX402` refuse. Missing **required**
members refuse (the launch D10 rule).

Admin creates send their own `clientToken` (a UUID4 from `getrandom`, one per command, reused
across retries), matching botocore's automatic fill.

### R-RES-RT. Runtime resource resolution (config first)

The order is `resources[key]` when truthy, else the env var from `RESOURCE_ENV`, else none. A
string is used as is. A number becomes its Python `str`, `true` becomes `"True"`. **D22:** an
array or object refuses with `"Payment config resource '<key>' must be a string."`.

`load_config` normalises `max_amount_usd` to `max_per_payment_usd`. A flat legacy file (no
`policy` object) is the policy. A `resources` entry that is not an object counts as empty. Session
status without a config file falls back to the environment only, as in Python.

`_require_resource` text: `"Missing payment resource '<key>'. Add it to the config
(agents_pay_admin.py init-config) or set <ENV>. Provision resources with the trusted setup path
first."`. S7 changes the command name in this text to `toolkit agents-pay admin init-config`
(D23, a documentation change applied together with the skill text).

### R-CFG. The runtime config file

- **Path.** The home directory of the OS account of `getuid()`, from `getpwuid_r` through `nix`
  (section 9, Q20), joined with `.agents-pay/config.json`. `HOME`, `AGENTS_PAY_CONFIG` and
  `X402_POLICY_FILE` are ignored. Non-Unix targets, an unknown uid or a relative home fail closed
  with the Python texts.
- **Open (D24, race-free).** Python ran `lstat` and then `read_text`, which follows symlinks, so
  a swap between the two was possible. Rust opens the parent with
  `rustix::fs::open(O_DIRECTORY|O_NOFOLLOW)` and runs `fstat` on it (owner, `mode & 0o022`). It
  then calls `openat(dir, "config.json", O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC)`. `ELOOP`
  gives the symlink text. `fstat` on the descriptor checks a regular file, the owner and
  `mode & 0o077`. The file is read **through that descriptor** with a 1 MiB cap (D25; Python was
  unbounded): `"Payment config {path} is too large."`.
- **Texts.** All Python texts are kept, with the path through `shown_path`. D1 applies here as
  well: a config file with a duplicate key gives the "is not valid JSON" text, which Python
  produced only for real syntax errors. The `chmod 600` and
  `chmod 700` hints print the path through `shown_value`, because they are commands for a human
  to copy.
- Ancestors above the parent are not checked, as in Python. The module docstring already scopes
  the threat: a process running as the owner can rewrite the file anyway.

### R-TUNE. Tunables

`_bounded_env(name, default, min, max)` uses Python `float()` grammar: whitespace, sign, digits,
`.`, exponent, `inf`/`infinity`/`nan` in any case, and `_` between digits. A value that fails to
parse or falls outside `[min, max]` gives the default. The integer tunables are then truncated
with `int()`.

**D26:** `nan` gives the default. In Python, `nan` passed both range comparisons, and
`int(float('nan'))` then raised at import time, so `X402_MAX_PAYMENT_ATTEMPTS=nan` or
`X402_MAX_BODY_BYTES=nan` crashed the CLI with a traceback and exit 1. `X402_TIMEOUT_SECONDS=nan`
gave httpx a NaN timeout.

### R-RES-ADM. Admin resolution (unchanged chains)

| Value | Order |
| --- | --- |
| config path | `--path` > `AGENTS_PAY_CONFIG` > `$HOME/.agents-pay/config.json` (Python `Path.home()` order: `HOME`, then the OS account) |
| manager ARN | flag > `PAYMENT_MANAGER_ARN` > config `resources.payment_manager_arn` > deployed state |
| region | flag > `AWS_REGION` > `AWS_DEFAULT_REGION` > config `resources.region` > none (SDK chain) |
| user id | flag > config `resources.user_id` > `PAYMENT_USER_ID` |
| connector | flag > `PAYMENT_CONNECTOR_ID` > deployed state |

Deployed-state discovery covers the three layouts, the first existing candidate, the
convenience-only rule (any error gives nulls) and `role_arn` from `processPaymentRoleArn`. The
admin load is lenient (missing or invalid → skeleton; a flat file is lifted into `policy`).

`setup-payment-user` keeps its narrower Python discovery (the single candidate; list layouts
only). **D27:** the dict-shaped layout is accepted there too, by sharing the admin discovery
function. Python crashed inside its `try` and fell back to the flags.

### R-ADM-WRITE. Admin writes

These follow launch R-TOK-MODE:

1. Parents are created with `DirBuilder` (intermediate parents get the default mode, as in
   Python's `mkdir(parents=True)`).
2. The final directory is opened `O_NOFOLLOW` and `fchmod 0700`.
3. A temporary `.config-<16 hex>.tmp` is created with `O_CREAT|O_EXCL` at 0600 in that directory.
4. The content is written, then `sync_all`.
5. `renameat` over the target, then `fchmod 0600` on the result.
6. On failure the temporary file is removed.

Output is `json.dumps({"resources", "policy"}, indent=2) + "\n"`, reusing
`aurora::pyfmt::dumps` as the launch design does.

### R-PORT. Portability

The Python `test_portability.py` simulates harness loading. Its checks are:

- frontmatter parses, with `description` present and at least 20 characters, `name` equal to the
  directory, and no `stages` or `owner_*` keys;
- no `` !`cmd` `` and no ```` ```! ```` blocks;
- no `../` links;
- every relative link resolves in an isolated copy;
- the scripts run from the copy.

S7 ports the document checks as a Rust test over both `SKILL.md` copies and `references/`,
using `toolkit_core::frontmatter` and a temporary-directory copy. It replaces "the scripts run
from the copy" with this: every command the skill tells the agent to run is a `toolkit
agents-pay …` or `toolkit agents-build …` invocation that the registry knows, and no
`scripts/*.py` path remains.

Locating the binary from a flattened skill copy is the programme's binary-staging decision
(#4487 is cited by #4413's design). This slice assumes `toolkit` is on `PATH` (Q21).

## 7. User and admin creation operations, and AC3 (no real payments or user creation in tests)

Operations that create things: `CreatePaymentInstrument` (a wallet and a linked-email user at
Coinbase or Privy, which is the "user creation" of AC3), `CreatePaymentSession` (budget),
`ProcessPayment` (spend). `GetPaymentSession` and STS `GetCallerIdentity` only read.

Interactive gates, kept and tested through a `Terminal` seam (`is_tty`, `read_line`, `write`):

- `new-session` and `setup-openclaw` check the TTY **first**, before any dependency, argument or
  credential check. That matches the Python order and its "not order-dependent" comment. A
  headless caller always receives the same refusal text.
- The literal `approve` is required.
- **No `--yes` flag exists.** A registry test asserts that neither route declares a flag
  matching `yes`, `force`, `non-interactive` or `assume`.
- EOF at a prompt: Python raised `EOFError` (traceback, exit 1). **D28:** Rust prints `"Aborted.
  No session created."` and exits 1.
- `create-instrument` has no TTY gate in Python, and IAM (ManagementRole) is the control. **Q22:**
  add one, with the recommended answer **yes**, because this command creates a user.

Validation that moves earlier (D29): `--budget` must be a positive decimal, and
`--expiry-minutes` and the wizard expiry must be in the model's 15..=480. Both are checked before
the confirmation prompt, with fixed messages. Python sent them unchecked and the service failed
with a traceback.

The wizard's prompt text `(1440 = 24 hours)` names a value the service rejects. It becomes
`(15-480; 480 = 8 hours)` (D30), and `format_duration` is kept.

**How tests guarantee no spend and no user creation (AC3):**

- **G1, compile-time.** All flow and admin logic takes `&dyn PaymentsApi`, `&dyn MerchantHttp`,
  `&dyn HostResolver`, `&dyn Terminal` and `&dyn Clock`. The only constructor of the live
  implementations, `live::connect(&LiveCapability)`, is `#[cfg(not(test))]`. So inside the crate's
  unit-test build the live types cannot be reached at all; trying is a compile error. The default
  build (no `aws-sdk` feature) does not compile `SdkTransport`, so the default `cargo test` has
  **no** HTTPS client in the binary.
- **G2, fixture namespace.** All fixtures use RFC 6761 hosts (`merchant.test`, `*.invalid`),
  account `111111111111` and IDs shaped like `payment-session-TESTxxxxxxxxxxx`. `FakePaymentsApi`
  panics on any other manager ARN account. Every fake records each call, and the tests assert the
  exact count, including **zero** for every refusal path (T-ORDER).
- **G3, runtime refusal of test identities on the live path.** `live::connect` refuses to start
  when the manager ARN account is `111111111111` or `123456789012`, or when the URL host ends in
  `.test`, `.example`, `.invalid` or `.localhost`. Refusal text: `"Payment resources are test
  fixtures; refusing to contact live services."`, exit 2. A fixture leaking into a real invocation
  therefore cannot reach AWS.
- **G4, source gate** (`tests/agents_pay_gate.rs`, the `r_log_gate` pattern). It parses the
  slice's test files with `syn` and fails if any test refers to `live::`, `SdkTransport`,
  `SystemResolver` or `default_chain`. It also fails if a test spawns the `toolkit` binary with
  `agents-pay fetch` and a URL that is not refused before DNS (`http://…`, userinfo, or with no
  config file), or with any `agents-pay admin create-*`/`new-session`.
  Process-level CLI tests run the default-feature binary only and set no AWS variables.
- **G5.** `make` SDK targets (#5017) compile the `aws-sdk` feature. G1 still holds there: the live
  constructor is gated by `cfg(test)`, not by the feature. G4 runs in both.

## 8. Rust layout, slices and tests (tests first)

### 8.1 Modules (`crates/toolkit-cli/src/agents_pay/`)

| Module | Contents |
| --- | --- |
| `mod.rs` | `SLICE` (`agents-pay`), routes, output and exit rendering. A second `SLICE` constant for the `agents-build setup-payment-user` route lives in `agents_pay/build.rs` with slice name `agents-build`. Both sit in `registry.rs` `SLICES` at their alphabetical place with `NEW_RANK`; doc `docs/migration/agents-pay.md` (written in S7) |
| `urlparse.rs` | R-URLPARSE (pure) |
| `amount.rs` | R-AMT grammars and the digit-string comparison, `Decimal` rendering for `amount_usd` (pure) |
| `pyval.rs` | Python `str`/truthiness/`int()`/`float()` over `toolkit_core::json::Json` (pure) |
| `challenge.rs` | R-CH extraction from a status, headers and body (pure) |
| `policy.rs` | R-POL, R-VER, R-RESBIND, R-ORIGIN (with a `HostResolver`), `Approved` (private constructor), R-TOK |
| `config.rs` | R-CFG, R-RES-RT, R-TUNE, the runtime path through `nix` |
| `net.rs` | `MerchantHttp` trait and the live impl over `StreamingTransport` (R-NET, R-TLS-PAY) |
| `agentcore.rs` | `PaymentsApi` trait; request builders and response readers for the four operations plus STS; R-AWS-*; `FakePaymentsApi` (test) |
| `proof.rs` | R-PP-VERIFY, R-HDR, `validAfter` |
| `flow.rs` | `fetch`, `status`, `prepare_browser_payment` orchestration; R-RETRY; R-SCRUB; R-ERR |
| `vault.rs` | in-process handle vault: 90 s TTL on the injected monotonic clock, single use, origin and path bound; handle `x402h_` plus 32 base64url characters from 24 `getrandom` bytes |
| `admin/` | `init_config.rs`, `show_config.rs`, `create_instrument.rs`, `new_session.rs`, `preflight.rs`, `setup_openclaw.rs`, `deployed.rs`, `write.rs`, `terminal.rs` |
| `live.rs` | `#[cfg(not(test))]` live constructors, G3 |
| `model/agentcore-payments.json` | the model excerpt (6.1) |

Routes, keeping the Python flag spellings:

- `toolkit agents-pay fetch [URL] [--status] [--browser-handle URL] [--method GET|HEAD] [--purchase-id ID]`;
- `toolkit agents-pay admin {init-config|show-config|create-instrument|new-session|preflight|setup-openclaw} …`;
- `toolkit agents-build setup-payment-user …`.

Router facts, as recorded for launch:

- unknown flags and extra positionals are usage errors (exit 2; Python argparse also exits 2);
- `--method` is a strict value flag;
- no URL and no flag keeps the Python exit 1 and its text, so it is checked in the handler;
- `-` is refused for every value (#4780).

### 8.2 Slices

Each slice writes its failing tests first, then the code. Each needs a daemon `scoped_test` and
an independent cross-family review at the exact SHA.

| Slice | Content | Tests (ported Python classes → new) | Prerequisites |
| --- | --- | --- | --- |
| **S1** pure policy | `urlparse`, `amount`, `pyval`, `challenge`, `policy` (no network: `FixedResolver`) | RecipientAndValue, RecipientValidation, OptionalOrigin (pure part), Authorize, Idempotency (+ golden vectors R-TOK), SignerInput (pure part), canonical amounts; T-POL-UNI, T-URLP (params, IPv6, port, case), T-AMT (grammar tables, Infinity/NaN cap D6, digit comparison at the boundary `cap` = amount), T-AMT-USD, T-VER, T-CH (header base64/raw/body, duplicate keys D1, truthiness), `Approved` compile_fail | #4379 only; can start now |
| **S2** config | `config.rs`, `nix` user lookup | PolicyFile, ConfigPrecedence, PolicyDirectory, SingleTenantUserId (resolution part), bounded env; T-CFG-RACE (symlink swapped in after the parent check, through a `rustix` test seam), T-CFG-SIZE, T-TUNE (`nan`, `inf`, `1e1`, `5.9`) | S1 |
| **S3** merchant net | `net.rs` | Ssrf, DnsPinning (resolver returns public then private; peer check), SafeMethod; T-NET: exact request headers, no cookie carry, 3xx reported, Content-Length precheck with zero pulls, cap, deadline trickle (TA-STREAM-DL fake clock), encoding D15, no proxy | S1, #5017 S1 on rolling, **#5080** TA-STREAM-DL |
| **S4** AgentCore client | `agentcore.rs`, model excerpt | SessionStatus (fake, D19 logic), runtime/admin agent names; T-AWS: golden request bytes (path, both headers, body order, token) for the four ops; response validation (status, echo, required members, payload shape and size); error allowlist (each seeded secret in `x-amzn-ErrorType`/`__type`/`message` → absent, launch T-CODE pattern); fixed endpoint despite `AWS_ENDPOINT_URL*` (D18); `Debug` redaction | S1, #5017 S1 |
| **S5** flow + `fetch` route | `proof`, `flow`, `vault`, `live`, route | TransientSettlement, BrowserHandle, HarnessCli; T-ORDER (zero `ProcessPayment` on every refusal class), T-RETRY, T-PROOF (R-PP-VERIFY mismatch on `to`, `value`, expired `validBefore`; `validAfter` wait ≤ 15 s), T-HDR (v1/v2 bytes), T-SCRUB (merchant echoes the header in its body → `[REDACTED]`; an echo in `content_type` → generic), T-ERR exhaustive, CLI process tests (exit 2 on refusal, single JSON, usage exit 1); G4 gate | S2-S4, #4941 at `40c71d7e` on rolling |
| **S6** admin | `admin/`, `agents-build setup-payment-user` | AdminRecipientMode, test_resource_resolution (all asserts), test_setup_openclaw (`usd_to_atomic`, `build_openclaw_config`, `discover_deployed` with `--project-dir`, `format_duration`); T-TTY (order: TTY first even without credentials; `approve` exact; EOF D28; no `--yes`), T-WRITE (atomic, modes, temporary file removed on failure), T-VAL (D29), T-PREFLIGHT (secret-name scan), D27 | S2, S4 |
| **S7** cutover docs | `SKILL.md` (both copies), `references/*.md`, `agents-build/references/payments.md` steps that run scripts, `docs/migration/agents-pay.md`, the portability test, inventory rows → `ported` | R-PORT test; `every_slice_documents_its_commands` | S5, S6; the Q2 and Q3 decisions |

The Python files are retired only after S7 has been verified, and only by the programme's
retirement step (#4415 pattern), never inside these slices.

## 9. Dependencies

Workspace dependencies come first. Every addition is pinned with `=`.

| Crate | Status | Why |
| --- | --- | --- |
| `toolkit-aws` (workspace) | existing | `endpoint` (`is_non_public`, `Endpoint::for_service`, `HostResolver`), `stream`, `sdk` (`SdkTransport::with_resolver`, `ValidatedRoots`, guarded credentials, `SigV4Signer`), `redact::Secret`, `RetryPolicy`, `recorded`, `testing` fakes |
| `toolkit-core` (workspace) | existing | `json::parse_json` (duplicate-key refusal, depth), `digest::sha256_hex`, `frontmatter` |
| base64 | **no crate** | promote `toolkit-cli/src/bedrock_resolve/image.rs` `base64_encode` to `toolkit-core` and add a strict standard decoder (shared with launch L0's base64url if it lands first) |
| `getrandom =0.3.4` | already compiled (through `ahash`); a new direct dependency, the same as launch | handle randomness, admin `clientToken` UUID4s, temporary names, `generate_user_id` (6 bytes → 12 hex) |
| `rustix =1.1.5` (`fs`, `process`), `cfg(unix)` | pinned by #5017 S1, and by launch for `fs` | `O_NOFOLLOW` open and `openat`, `fstat`, `fchmod`, `renameat`, `getuid` (R-CFG, R-ADM-WRITE) |
| **`nix =0.31.1`**, `default-features = false`, `features = ["user"]`, `cfg(unix)` | **new** (in the local registry; its dependencies `libc`, `bitflags`, `cfg-if` are already in `Cargo.lock`, to be confirmed with `cargo tree` in S2) | safe `User::from_uid` (`getpwuid_r`) for the OS-account home. `std` and `rustix` have no passwd lookup, and the workspace forbids `unsafe`. Parsing `/etc/passwd` by hand would miss NSS (LDAP, sssd) users, and spawning `getent` would be `PATH`-hijackable. Used in one function. Alternative in Q20 |
| crypto or signing crate | **none** | signing happens in AgentCore (`PaymentDocument` is server-produced). SHA-256 comes from `toolkit-core`, SigV4 from the existing `aws-sigv4`. No secp256k1, k256 or EIP-712 crate. The R-PP-VERIFY checks are string comparisons and do **not** verify the signature; verification is the merchant's job, and doing it locally would add a crypto dependency for no extra safety over the to/value check |
| decimal or big-integer crate | **none** | digit-string comparison (R-AMT) and a small `Decimal` renderer for `amount_usd` |
| URL crate | **none** | WHATWG parsing would differ from Python `urlparse` on exactly the fields hashed into the token. Validation reuses `toolkit_aws::endpoint` |
| HTTP server, `zeroize`, `tempfile`, `dirs`/`home` | not added | as decided for launch; `home`/`dirs` would read `HOME`, which R-CFG must ignore |

## 10. Divergences (D), for the slice page

These are stricter or safety-only unless noted:

- D1 duplicate JSON keys and non-finite literals refused.
- D2 array or object challenge fields make the entry ineligible.
- D3 a non-string asset gives a fixed policy error.
- D4 `eip155` networks only.
- D5 ASCII digits only.
- D6 an `Infinity` or `NaN` cap is refused.
- D7 an `int()` value outside `i64`.
- D8 the wider non-public table.
- D9 the `allowed_origins` type error text.
- D10 one `ProcessPayment` per invocation.
- D11 `maxTimeoutSeconds` and `extra` bounds.
- D12 proof terms are checked before sending.
- D13 a 1 s pause between replays.
- D14 `User-Agent`.
- D15 no decompression.
- D16 absolute deadlines.
- D17 `content_type` bound.
- D18 endpoint override variables ignored on the runtime.
- D19 the manager ARN supplies the region before the profile does.
- D20 AWS retry and timeout defaults.
- D21 known-secret scrub.
- D22 non-scalar resource values.
- D23 command names in texts (documentation).
- D24 race-free config open.
- D25 1 MiB config cap.
- D26 `nan` tunables.
- D27 `setup-payment-user` accepts the 0.26 layout.
- D28 EOF at a prompt.
- D29 early validation of budget and expiry.
- D30 the wizard expiry text.
- D31 the admin `ClientError` traceback is replaced by `"<Operation> failed: <allowlisted code>"` on
  stderr with exit 1. The Python traceback printed the service message.
- D32 the default build without `aws-sdk`: fetch refuses (exit 2) with `"This toolkit build has
  no network support; install a build with the aws-sdk feature."`, and status reports
  `usable: false` with the same reason. This replaces the "bedrock-agentcore … not installed"
  texts.

## 11. What is offline-testable and what needs the operator's live run

**Offline (every slice, default `cargo test` and the daemon `scoped_test`):**

- all of sections 3-7 over fakes: policy, amounts, tokens and golden vectors, challenge parsing;
- request bytes for the four AgentCore operations, response validation, error allowlists;
- the merchant HTTP behaviour over `FakeStreamingTransport` and a fake resolver;
- deadlines on a fake clock;
- the handle vault;
- admin file writes in temporary directories, TTY gating over a fake terminal;
- deployed-state layouts, the CLI exit codes for pre-network refusals;
- the G4 source gate.

No test needs credentials or network access.

**Operator live run (gate: it creates a real wallet user, creates a session, and spends; on
Base Sepolia the USDC is testnet, but the resources are real and mainnet `eip155:8453` would be
real money, so that network is out of scope unless the operator decides otherwise).**

Use an `aws-sdk` build, a disposable account or manager, and budget `0.10`:

1. `admin init-config --recipient <sandbox payTo> --origin <sandbox origin>`, `show-config`,
   `preflight`. No spend.
2. `admin create-instrument --email <operator>`, which **creates a user**. Then the human
   delegation and faucet funding.
3. `admin new-session --budget 0.10 --expiry-minutes 30` at a TTY.
4. `fetch --status`. Confirms the Q19 status computation against a real `GetPaymentSession`.
5. `fetch <v2 sandbox URL>` → `paid: true`. Record the `PAYMENT-SIGNATURE` acceptance (Q4 shape)
   and **exactly one** on-chain transfer.
6. Re-run (5) at once: the same token, no second transfer, and `availableSpendAmount` unchanged
   (R-TOK).
7. `fetch <same URL> --purchase-id 2` → one more transfer.
8. A URL priced above the cap → refused; CloudTrail shows **no** `ProcessPayment`.
9. A v1 merchant, if one is available (`X-PAYMENT`, Q4).
10. `--browser-handle` → a handle; confirm no proof appears in stdout (Q3).
11. A session drained or expired → `usable: false`, and `fetch` refused by the service with
    `ClientError` text.
12. Compare with the Python CLI on the same session, captured by the operator. Python stays
    read-only evidence for agents.

## 12. Open questions, each with the recommended answer

- **Q1. Model version.** The model was read at botocore 1.43.108; the skill pins 1.43.63. Should
  the excerpt be pinned to 1.43.63? **Recommend:** use the 1.43.108 shapes, recorded with their
  sha256. The operations are additive, and the live run (Q4) is the real check. S4 re-reads the
  1.43.63 data if the manager makes it available, and records any difference.
- **Q2. `process_payment_tool.py`.** This is a template for the user's own Python agent, not a
  toolkit command. It is also weaker than the gate: a random UUID token per fetch, the raw 402
  forwarded to the signer, no cap, and the body returned. **Recommend:** a product decision for
  PM or operator. Proposed: replace the `.py` with a reference snippet in
  `agents-build/references/payments.md` that calls `toolkit agents-pay fetch` from the user's
  agent through a subprocess, keeping the gate in the path. Keep the framework-native options
  (Strands, LangGraph) as they are. Until decided, S7 leaves the file and records it as a
  remote-payload compatibility item (AGENTS.md).
- **Q3. Python import API and the browser handle.** The handle vault is in-process. The Python
  CLI's `--browser-handle` already returns a handle that dies with the process, so it can never
  be redeemed. A later fetch of the same URL replays the same authorization by token, and if the
  60 s proof has expired that resource cannot be bought without `--purchase-id`.
  **Recommend:** keep CLI parity in S5 and document the limitation. Add a follow-up slice for a
  long-lived `toolkit agents-pay serve` (JSON lines over stdio, owned by trusted glue) that
  offers `fetch`, `status`, `prepare_browser_payment` and `attach_browser_payment`. It replaces
  the importable functions; to be filed by the manager after review.
- **Q4. Payload and header shapes [inferred].** **Recommend:** use PP-PAYLOAD and R-HDR as
  written, from the wiring reference and the TypeScript. The live run steps 5 and 9 confirm them.
  A mismatch is a P1 for S5 before cutover.
- **Q5. `validAfter`.** **Recommend:** adopt the OpenClaw wait of at most 15 s.
- **Q6. Pause between replays.** **Recommend:** 1 s (D13).
- **Q7. Content-Encoding.** **Recommend:** never decompress (D15).
- **Q8. Non-EVM networks.** **Recommend:** refuse (D4) until a Solana proof shape is specified
  and has had a live run.
- **Q9. Six decimals for every asset.** **Recommend:** keep parity. `init-config` admits only
  `KNOWN_USDC`. Record in the security reference that a hand-allowlisted asset with other
  decimals breaks the cap.
- **Q10. `amount_usd` rendering.** **Recommend:** S1 implements the two rules that apply:
  - exact division with the ideal exponent 0, reduced as Python does;
  - Python's `Decimal.__str__` for that quotient, with 28-digit rounding past that length.

  These are taken from the documented decimal arithmetic specification, not from running Python.
  T-AMT-USD carries the hand-derived table in R-AMT. Amounts beyond 28 significant digits can
  only reach a receipt under an absurd cap, so they are tested for no panic and a stable
  rendering, not for byte parity.
- **Q11. Non-UTF-8 charsets in a challenge body.** **Recommend:** always lossy UTF-8. JSON
  challenges are UTF-8 in practice, and Python decoded with the declared charset.
- **Q12. `test_setup_openclaw.py`.** It is inventoried to #4411 but tests `agents_pay_admin`.
  **Recommend:** S6 ports its four contracts, and #4411 retires the Python file by citing them.
- **Q13. Local cumulative ledger.** **Recommend:** no. The service enforces the budget.
- **Q14. TypeScript and Python token divergence.** **Recommend:** out of scope here. #4411
  should decide whether OpenClaw adopts R-TOK (and with it shared purchase idempotency across
  runtimes).
- **Q15. Runtime trust roots ignore `SSL_CERT_*` and `AWS_CA_BUNDLE`.** **Recommend:** yes
  (R-TLS-PAY). Cost: a TLS-inspecting corporate proxy breaks the payment runtime; such users need
  an operator-installed platform root. Alternative: bundle `webpki-roots` (Mozilla, the same
  source as certifi, already in the local registry) for exact certifi parity on hosts with no
  system store. Not recommended for now, because it adds a crate.
- **Q16. Endpoint overrides on admin.** **Recommend:** ignore them there too, for one code path.
  Admin users with VPC endpoints can be served by a later reviewed flag.
- **Q17. Region from the manager ARN (D19).** **Recommend:** yes. It removes the
  "manager-not-found in the wrong region" failure that the Python comments describe.
- **Q18. Session id in admin output.** **Recommend:** keep printing it (parity; the human needs
  it; it is not a signing secret).
- **Q19. Status computation.** **Recommend:** read `paymentSession` per the model:
  - `expired` = `createdAt` + `expiryTimeInMinutes` ≤ now (injected clock);
  - `usable` = not expired and `availableSpendAmount.value` (when present) > 0;
  - `status` is one of `ACTIVE`, `EXPIRED`, `DRAINED`;
  - `budget_usd` = `limits.maxSpendAmount.value`;
  - `spent_usd` = `budget_usd` − `availableSpendAmount.value`, computed with the exact decimal
    arithmetic of R-AMT, when both are present, and omitted otherwise. That keeps the Python key
    set; no new key is added.

  An unknown or missing `createdAt` is never usable (fail-closed). Confirmed by live run step 4.
- **Q20. OS-account home.** **Recommend:** `nix` `user` feature. Alternative: a fail-closed
  `/etc/passwd` parser for the uid (no new crate, but NSS users are refused).
- **Q21. Binary location for skills.** **Recommend:** `toolkit` on `PATH`, per the programme's
  staging decision. S7 waits for that decision before changing the SKILL text.
- **Q22. TTY gate on `create-instrument` (and `setup-payment-user`).** **Recommend:** yes, so
  that a headless agent cannot create a wallet user. D33 if accepted.
- **Q23. `--status` exit code when the build lacks `aws-sdk`.** **Recommend:** 2, which is
  parity with "SDK not installed".

## 13. Review checklist for the AC1 pre-review

A reviewer should be able to confirm each of the following from this document alone:

1. **Order.** Every spending call is behind `Approved`, and every refusal records zero
   `ProcessPayment` calls (R-SPEND-ORDER, T-ORDER).
2. **Tokens.** The token is byte-identical to Python (R-TOK vectors), and no retry path can
   produce a second authorization (R-RETRY table).
3. **The proof.** It is never printed or logged, never put in `Debug` or a file, and never left
   unscrubbed in a body (R-SEC, R-RED, R-SCRUB); it is checked against the approved terms before
   it is sent (R-PP-VERIFY).
4. **Network.** The merchant plane allows only https, public addresses at both the resolver and
   the peer, no redirects, no proxy, caps and deadlines (R-NET). The AWS plane uses a fixed
   endpoint, ignores env overrides on the runtime, and allowlists error codes (R-AWS).
5. **Configuration.** The config is bound to the OS account and opened race-free; the
   environment is a fallback only (R-CFG, R-RES-RT).
6. **AC3.** Tests cannot reach live services or create users (G1-G5).
7. **Dependencies.** Each new dependency is justified, and no crypto crate is added (section 9).
