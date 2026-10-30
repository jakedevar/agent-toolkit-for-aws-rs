# x402 payments: Rust port design and boundary pre-review (#4410, phase 1)

Status: design, revision 2 (r1 repair of the #5091 pre-review; section 14 maps every finding to
the changed sections and test IDs). No implementation code. Programme #4369, slice
`payments-policy`, wave 3, dependency #4379 (aws-core). AC1 of #4410 requires the payments,
credential and network semantics to be reviewed before implementation. This document is the
thing to review. Sibling: #4411 (`payments-bridge`, the OpenClaw AgentCore bridge, which depends
on this slice).

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
| Runtime → merchant | probe `GET`/`HEAD`, then a paid replay carrying the proof header | https only; every DNS answer and the socket peer must be public; no redirects; no proxy; identity encoding; body cap; deadlines (with the documented DNS residual, R-DNS); environment-independent payment roots (R-TLS-PAY) |
| Runtime → AgentCore | `ProcessPayment` (the only spending call), `GetPaymentSession` | Called only after the policy gate has approved exactly one entry. Fixed endpoint, SigV4, derived idempotency token, response cap, allowlisted error codes, the same payment roots as the merchant plane |
| AgentCore → runtime | the signed `PaymentDocument` (the proof) | A secret: never printed, logged, put in `Debug` or returned to the model. Checked against the approved terms before it is sent |
| Runtime → model | one JSON object on stdout, exit 0/1/2 | Fixed texts plus redacted, bounded caller values. Paid body only on operator opt-in, scrubbed of every live secret |
| Human → admin | `toolkit agents-pay admin …`, `toolkit agents-build setup-payment-user` | Creates the instrument (wallet user) and the session (budget). A TTY gate and typed `approve` before every creation (session as in the Python; instrument new, D33). Never reachable from tests: no verification artifact contains a live constructor (AC3, section 7) |

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
2. Base64 strict decode: standard alphabet, correct padding, no other bytes; D35 tightens
   Python's `b64decode(validate=True)` for padding after a complete quartet. Then UTF-8 decode,
   then JSON. Accept if the result is an object whose
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
  - **D6: `Infinity`, `NaN`, `sNaN`, a written exponent with `|x| > 1000`, and a mantissa of
    more than 1000 digits are refused** with `"Payment policy '<key>' is not a decimal
    number."`. In Python, `Infinity` passed `cap <= 0` and made the per-payment ceiling
    unbounded, and `NaN` raised an uncaught `InvalidOperation`.
  - A JSON float literal is compared through its literal digits. Python converts the float to a
    shortest `repr` first. The two differ only for literals with more than 17 significant
    digits; the difference is recorded and is not a risk.
- **R-AMT-REP. The one decimal representation (r1, #5097).** Every cap, whatever its spelling,
  is parsed once into `CapDecimal { digits: D, exp: E }`, meaning **value = D × 10^E** exactly:
  1. Strip surrounding whitespace. Take the optional sign; a `-` sign makes the value negative.
     Remove each `_` that sits between two digits (any other `_` is a grammar error).
  2. Split the rest into a mantissa and an optional exponent at the first `e`/`E`. The exponent
     is an optional sign plus ASCII digits, giving the signed integer `x` (`x = 0` when absent).
  3. The mantissa is `I` or `I.F` or `.F` or `I.` (Python `Decimal` accepts all four), where `I`
     and `F` are ASCII digit strings, not both empty.
  4. `D` = `I ‖ F` with leading zeros removed (`"0"` if nothing is left), and **`E = x − len(F)`**.
     The digits after the point therefore lower the exponent; nothing is ever divided.

  Examples: `"0.10"` → (10, −2); `"0.1"` → (1, −1); `"1e-1"` → (1, −1); `"10e-2"` → (10, −2);
  `"0.01e1"` → (1, −1); `"1.5E+2"` → (15, 1); `"1"` → (1, 0). All of `"0.10"`, `"0.1"`, `"1e-1"`,
  `"10e-2"` and `"0.01e1"` denote 0.1, and they must compare identically.

  The cap must be `> 0`: a negative sign or `D = "0"` gives the Python `cap <= 0` text, so
  `"-0"`, `"0e5"` and `"-1e-1"` are refused. `E` is never stored in floating point, and
  `|E| ≤ 2000` follows from the two D6 bounds.
- **R-AMT-CMP. Comparison (r1, corrected).** The challenge amount is `A` base units, an ASCII
  digit string (R-AMT grammar above). Its USD value is `A × 10^−6`. The entry is over the cap
  exactly when

  `A × 10^−6 > D × 10^E`  ⇔  `A > D × 10^(E + 6)`.

  Let `k = E + 6`. Both sides are made into integers with no division:
  - `k ≥ 0`: compare `A` with `D ‖ "0"×k` (the cap in base units);
  - `k < 0`: compare `A ‖ "0"×(−k)` with `D`.

  Each side has its leading zeros stripped, with `"0"` kept for zero. Then the strings are
  compared by length, and lexicographically when the lengths are equal. **Over the cap** means
  the left side is greater; **equal is allowed**, as Python's `>` allows it. The padding never
  exceeds 2006 characters, so no big-integer crate is needed.

  Worked check of the #5097 case: cap `"1e-1"` gives D = 1 and E = −1, so k = 5, and the cap in
  base units is `100000`. A challenge of 1000000 units ($1) is over the cap and is skipped, which
  matches Python's `Decimal` comparison (`x402_policy.py:427,458`). The r0 formula `10^(6 − e)`
  gave 10^7 here, letting $1 through under a $0.10 cap. `USDC_DECIMALS = 6` is used for every
  asset, as in Python (Q9).
- **T-AMT-CAP (S1, r1).** One table-driven test over spellings × boundaries. Every row also runs
  through the full S1 `authorize` on a recording fake, and the over-cap rows assert the uniform
  refusal text and **zero** `ProcessPayment` calls (T-ORDER-PRE).
  - T-AMT-CAP-1, $0.10. Spellings: `"0.10"`, `"0.1"`, `"1e-1"`, `"1E-1"`, `"+1e-1"`, `"10e-2"`,
    `"0.01e1"`, `"100000e-6"`, `".1"`, `" 0.1 "`, `"1_0e-2"`, and the JSON number literals `0.1`,
    `1e-1` and `10E-2`. Amounts: 99999 and 100000 are allowed; 100001 and 1000000 are over.
  - T-AMT-CAP-2, $1. Spellings: `"1"`, `"1."`, `"1.0"`, `"1e0"`, `"1E+00"`, `"0.1e1"`, `"10e-1"`,
    and the JSON numbers `1` and `1.0`. Amounts: 999999 and 1000000 are allowed; 1000001 is over.
  - T-AMT-CAP-3, positive exponents. `"1e2"` and `"0.001e5"` ($100): 100000000 is allowed and
    100000001 is over. `"1.5E+2"`: 150000000 is allowed and 150000001 is over.
  - T-AMT-CAP-4, sub-unit caps. `"1e-6"`: units 1 is allowed and 2 is over. `"5e-7"` (k = −1): 1
    unit is over, because `"10"` > `"5"`. `"15e-7"`: 1 is allowed and 2 is over.
  - T-AMT-CAP-5, refusals with the D6 or `cap <= 0` text: `"1e-1001"`, `"1e1001"`, a mantissa of
    1001 digits, `"-1e-1"`, `"0e5"`, `"-0"`, `"1__0"`, `"_1"`, `"1e"`, `"e1"`, `"."`.
  - T-AMT-CAP-6, property. For each pair of spellings in one row, the decision for every tested
    amount is identical. This pins the equivalence and is not a separate mechanism.
- `amount_usd` in the receipt: `str(Decimal(units) / Decimal(10) ** 6)` under Python's default
  context (28 significant digits, ideal exponent 0). Examples: `"0.1"` for 100000, `"1"` for
  1000000, `"10"` for 10000000, `"0.000001"` for 1 and `"0.5"` for 500000. Leading zeros in the
  units do not show. The result is at least 1E-6, so the small-exponent form (adjusted exponent
  below -6) never occurs. The exponent form `…E+n` occurs only when the quotient needs more than
  28 significant digits, and the context then rounds it. T-AMT-USD pins this table and that
  boundary (Q10).
- The same `CapDecimal` type parses `--budget` (D29) and Q19's `spent_usd` arithmetic, so the
  representation is defined once (section 7, Q19).

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
| session expiry (15..=480 min) | AgentCore | inside `ProcessPayment`. A refusal here is a post-authorization failure (T-ORDER-POST), since the call was made |
| who may sign | IAM: ProcessPaymentRole versus ManagementRole | AWS |

**Ordering invariant (R-SPEND-ORDER, r1 for #5102).** The rules are about types, so the compiler
enforces them:

1. **`Approved` lives in the leaf module `agents_pay/policy/approved.rs`.** All its fields are
   private to that file: the vetted entry (`Json`, already in its final key order), the canonical
   amount text, the version, the sanitized resource, the token, and the resolved
   `PaymentIds { manager_arn, session_id, instrument_id, user_id }`. Its one constructor,
   `Approved::new`, is `pub(in crate::agents_pay::policy)`, and its only caller is
   `policy::authorize`, after every check in this section has passed.
2. **Immutable.** No `&mut self` method, no `DerefMut`, no field setter and no builder. The
   accessors return `&str`, `&Json` or `u32`. `Approved` is **neither `Clone` nor `Copy`**, and
   its `Debug` prints only the token prefix and the amount.
3. **Consumed exactly once.** The spending entry point is
   `agentcore::process_payment(api: &dyn PaymentsApi, approved: Approved) -> Result<Authorized, PayError>`.
   It takes `Approved` **by value** and has no other term or resource parameter: the r0
   ellipsis is removed. The manager, session, instrument and user ids are inside `Approved`,
   and the session id is the same value that went into the token. A call site therefore cannot
   substitute an unvetted entry, amount, token or resource, and one `Approved` yields at most
   one logical `ProcessPayment`.
4. **The request is built only from `Approved`.** `ProcessPaymentRequest` has private fields and
   a single constructor, `ProcessPaymentRequest::from_approved(&Approved)`, which is private to
   `agentcore.rs`. `PaymentsApi::process_payment` takes that request type, so neither a fake nor
   the live client can be handed a hand-built body.
5. **The proof is gated in the same way.** `Authorized` (the proof plus a borrow-free copy of the
   approved `payTo`, amount and version) is consumed by `proof::verify(Authorized, &dyn Clock) -> Result<VerifiedProof, _>`.
   The replay header (R-HDR) can be built only from a `VerifiedProof`, so a proof that fails
   R-PP-VERIFY can never reach the merchant.

Tests for these rules (S1 and S5):

- **T-ORDER-CTOR.** `compile_fail` doc-tests from outside the crate cover: a struct literal
  `Approved { .. }`; a call to `Approved::new`; a field assignment; `approved.clone()`; calling
  `process_payment` twice with one value (use after move); `ProcessPaymentRequest::from_approved`;
  and building a replay header from an `Authorized`.

  Inside the crate, rustc's module privacy already stops the sibling modules (`flow`,
  `agentcore`, `proof`, `vault`, `admin`) from constructing or mutating `Approved`, because the
  fields are private to `approved.rs` and `new` is restricted to `policy`.
  `tests/agents_pay_privacy.rs` compiles the actual `approved.rs` source with offline data and
  formatting stubs, a sibling inside `policy` and a spending sibling outside it. A positive
  control compiles read-only access. Negative cases require the specific rustc privacy errors
  for amount/token/entry mutation, struct construction and the constructor call outside
  `policy`. No `trybuild` crate is added. A `syn` gate in `tests/agents_pay_gate.rs` additionally
  covers the in-crate source shape:
  - `Approved::new` has exactly one call site crate-wide, in `policy::authorize`;
  - no `Approved {` struct literal exists outside `approved.rs`;
  - no `impl` block for `Approved` outside `approved.rs`, and none with a `&mut self` receiver;
  - no `#[derive(Clone)]` or `#[derive(Copy)]` on `Approved`.
- **T-ORDER-PRE (zero authorizations).** Each locally decidable refusal runs the flow over the
  recording fakes and asserts **zero** `ProcessPayment` calls and **zero** merchant requests
  carrying a proof header. The refusals:
  - config and resource errors: R-CFG, R-RES-RT, a missing resource, a non-scalar resource;
  - every R-ORIGIN failure: non-https URL, no host, userinfo, a DNS answer that does not
    resolve or is non-public, `allowed_origins`;
  - a probe that is not 402, malformed probe HTTP, a probe body over the cap;
  - every R-CH failure;
  - every R-POL skip class, ending in the uniform text;
  - every R-AMT refusal, including the T-AMT-CAP over-cap rows;
  - R-VER, a R-RESBIND mismatch, D4 non-EVM, the D11 bounds;
  - the method gate;
  - D32, a build without the live feature;
  - the R-DNS deadline admission (T-DNS-DL-1).
- **T-ORDER-POST (one logical authorization, nothing further).** These failures can only be
  observed after `ProcessPayment` was sent. Each asserts:
  - exactly **one** logical `ProcessPayment`. Its transport attempts are at most
    `1 + RetryPolicy` retries, and every attempt carries byte-identical body and token;
  - **zero** additional authorizations, with no second token derived;
  - the outcome-specific merchant counts below.

  | Post-authorization failure | Merchant proof sends |
  | --- | --- |
  | AgentCore error response: session budget exhausted or expired (enforced in the service), `ConflictException`, throttling after retries, any allowlisted code | 0 |
  | Response shape, `status`, echo mismatch, payload size (R-PP response) | 0 |
  | R-PP-VERIFY mismatch on `to`, `value` or `validBefore`; `validAfter` outside the 15 s window | 0 |
  | Operation deadline passed after authorization and before the replay (R-DNS admission) | 0 |
  | Merchant replay transport failure: connect, timeout, cap, malformed HTTP | ≥ 1, and each send carries the same header bytes |
  | Replays exhausted (still 402) | `X402_MAX_PAYMENT_ATTEMPTS`, same bytes |
  | 3xx or 2xx on a replay | as sent, then the loop ends |

  T-PROOF and T-RETRY (section 3) are written against this table, so their expectations
  cannot contradict T-ORDER.

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
  by the Rust binary derives the same token. The token bytes are kept exactly (r1: no change to
  the material, and no field added or removed). Whether `ProcessPayment` then replays the same
  authorization is bounded by R-TOK-SCOPE below. These golden vectors were computed with coreutils `sha256sum` from the material definition
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

**R-TOK-SCOPE. What the token does and does not guarantee across invocations (r1, #5100).**

- **Guaranteed:** within one invocation, exactly one logical authorization (R-PP-ONCE,
  T-ORDER-POST).
- **Conditional:** across invocations, which covers a restart, a crash, a lost response or a
  retry by the caller. A second invocation reuses the first authorization **only if both** of
  these hold:
  1. it resolves the same session, origin, path, network, asset, `payTo`, canonical amount and
     purchase id, so the token is equal; and it sends the same `ProcessPayment` body (the vetted
     entry, including `extra` and `maxTimeoutSeconds`, and the version);
  2. the service still holds the first record for that token. The service's idempotency
     retention window is **[unknown]**: it is not in the model, and live-run step 6b measures it
     only roughly.
- **Not guaranteed (documented product limitation L-TERMS).** The second invocation probes the
  merchant again, so a merchant may answer with different terms that still pass the policy: a
  different amount under the cap, or another allowed recipient. That derives a **different
  token**, and the service may authorize a second payment. This is the legacy behaviour:
  `test_x402_policy.py:647-660` expects different tokens for a different amount or payee. Each
  such authorization is still bounded by the per-payment cap, the recipient allowlist and the
  session budget. S7 adds L-TERMS to the SKILL security notes. Exactly-once purchasing across
  changed challenges is **out of scope** (boundary B2, section 12a): it would need a durable
  purchase-to-terms binding, which this design does not add.
- **Same token, different body.** `extra`, `maxTimeoutSeconds` and the entry's other members are
  sent in the body but are not part of the token, so a changed challenge can produce the same
  token with a different body. The service's response to that is **[unknown]**: AWS idempotency
  convention is a `ConflictException` or `ValidationException`. An actual service error is a
  post-authorization failure (T-ORDER-POST): the invocation ends with the `ClientError` text and
  sends no proof. A successful response instead goes through the ordinary response and proof
  checks. If it contains a stored proof matching the current approved recipient, amount and
  validity window (and the required response echoes), the client accepts and sends it. The
  stateless client cannot detect that a previous invocation sent different `extra` or timeout
  bytes. T-RESTART-6 exercises both service errors and this successful-response boundary.
  Operator gate 6c must establish the real mismatch contract before any deployment relies on
  mismatch rejection; no such live-service result is established by the offline tests.
- **R-TOK-NOFRESH. No fresh-token recovery.** `derive_client_token` is the only producer of a
  `clientToken` on the runtime path. Its material has no time, random value, attempt counter or
  error-dependent input. After a conflict, an unknown-outcome timeout, an expired idempotency
  record or any other error, the runtime **never** derives or mints another token, and never
  retries `ProcessPayment` with a changed body. A `ConflictException` is not retryable in the
  `RetryPolicy` classification. The source gates check for clock/randomness names in pure
  policy/token construction and randomness names in `flow.rs`, `proof.rs` and the
  `agentcore::process_payment` function bodies, alongside the private Approved-to-request
  mapping. These are syntax checks, not a transitive call-graph proof. They permit the handle
  vault's required 24 cryptographic random bytes: repeated browser preparations for identical
  terms have distinct handles and identical `clientToken` and request-body bytes.
  Admin creates keep their own UUID4 tokens (section 6.1).

**T-RESTART (S5).** Each case is two invocations over one shared stateful fake. The fake service
keeps an idempotency table `token → (body hash, proof)` and an authorization ledger, and the fake
merchant keeps a settlement ledger. Every case asserts the exact per-invocation `ProcessPayment`
count, the tokens recorded, the ledger totals and the outputs.

| ID | Invocation 1 | Invocation 2 | Expected |
| --- | --- | --- | --- |
| T-RESTART-1 | authorized; aborted before the merchant send (fake flow cancellation) | same challenge | equal tokens and equal bodies; the service returns the stored proof; ledger: **1** authorization; the merchant receives the proof once |
| T-RESTART-2 | authorized; the merchant settles, but the response is lost (reset after the request is written) | same challenge (402 again) | equal tokens; the same proof bytes are replayed; ledger: 1 authorization and at most 1 settlement |
| T-RESTART-3 | `ProcessPayment` times out after the service recorded it (lost AWS response) | same challenge | equal tokens; the stored proof is returned; 1 authorization |
| T-RESTART-4 | authorized and paid | the merchant changes the amount (still under the cap) | **different** token; ledger: **2** authorizations. This pins L-TERMS so that it stays visible |
| T-RESTART-5 | authorized and paid | the merchant changes `payTo` (another allowed recipient) | different token; 2 authorizations (L-TERMS) |
| T-RESTART-6 | authorized | the merchant changes only `extra` or `maxTimeoutSeconds` | **equal** token, different body; invocation 2 has exactly 1 call and no other token. Fake `ConflictException` and `ValidationException`: 0 new proof sends, `ClientError` text, exit 2. Fake successful stored proof matching current terms: accepted, same signed payload in a v2 header carrying the current accepted entry, 1 service authorization in total. The fake header ledger does not establish on-chain settlement, and these scenarios do not establish the real service contract |
| T-RESTART-7 | authorized | same challenge after the fake has expired the idempotency record | equal token (no fresh token); the fake issues a new proof; ledger: 2 authorizations. This pins that the boundary is the service's retention window, not the runtime |
| T-RESTART-8 | `ConflictException` on the first call | — | 1 call; no second token within the invocation (R-TOK-NOFRESH) |

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
| A new invocation after a crash or lost response | deterministic token (R-TOK) | **conditional** (R-TOK-SCOPE): the same token leads to the same authorization only when the resolved identity, terms and body are unchanged and the service still holds the record. Changed terms can authorize again (L-TERMS, boundary B2); a conflict never leads to a fresh token (R-TOK-NOFRESH). T-RESTART pins each case |
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
  from the `PaymentRoots` of R-PROOTS (R-TLS-PAY below; wired in S5 `wiring.rs`) and a `GuardedResolver`. Every DNS answer is vetted at
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
  - These bounds cover **everything except system DNS**, as qualified by R-DNS below. They are
    not a strict wall-clock limit on the process.
- **R-DNS. The blocking-resolver residual (r1, #5101; carried from #4382 R-DNS and the #5080
  exclusion).**
  - **Where it blocks.** Name resolution runs synchronously in two places: the R-ORIGIN
    pre-check (`HostResolver` → `SystemResolver`, blocking `ToSocketAddrs`), and #5017 S1's
    `GuardedResolver::resolve_dns` at connect time. At `be0a06df` the latter evaluates
    `guarded_addresses` before it constructs `DnsFuture::ready`. Neither call can be interrupted
    by the `tokio::time::timeout_at` wrappers of TA-STREAM-DL. On a current-thread runtime a
    stalled lookup also stops the timer from firing until it returns.
  - **The contract.** Every per-request and per-operation deadline above excludes time spent
    blocked inside a system lookup. The worst case is the stated deadline plus the sum of the
    stalls. Each stall is bounded only by the system resolver's own policy: `resolv.conf`
    `timeout × attempts` per nameserver, or the platform equivalent. The process remains bounded
    by that, not by `T`.
  - **R-DNS-ADMIT. Deadline admission, which is what the runtime does guarantee.** The flow
    checks the injected clock against the operation deadline **before** starting each lookup
    and again **immediately after** each lookup returns. If the deadline has passed, it starts no
    further work: no connect, no `ProcessPayment`, no proof send. It then ends with the timeout
    text (`ReadTimeout during payment flow.`).

    A lookup that stalls after authorization can therefore delay the end of the invocation, but
    it can never cause a proof to be sent after the deadline, nor a second authorization. In
    T-ORDER-POST terms this is one authorization and zero proof sends.
  - **No async-resolver prerequisite.** The PM ruling for #5101 accepts the residual. A strict
    wall-clock bound would need a cancellable resolver, and that would be a separate reviewed
    prerequisite for both this design and #4382. Resolver threads are never abandoned or
    detached to fake a bound.
  - **Documentation.** D16 is qualified by this residual (section 10), and S7 states it in the
    SKILL troubleshooting text: "a stalled DNS lookup can exceed X402_TIMEOUT_SECONDS".
  - **Tests (S3 and S5).** They use a fake `HostResolver` that advances the fake clock, and they
    are separate from the TA-STREAM-DL streaming tests:
    - T-DNS-DL-1: a lookup that would start past the deadline is not started; zero lookups and
      zero `ProcessPayment` calls are recorded;
    - T-DNS-DL-2: the probe lookup returns after the deadline; no connect is recorded, and the
      output is the timeout text;
    - T-DNS-DL-3: the replay lookup returns after the deadline, after authorization; exactly 1
      `ProcessPayment`, 0 proof sends and the timeout text;
    - T-DNS-DL-4: lookups within the deadline proceed (control);
    - T-DNS-DL-5: the `GuardedResolver` path is checked against the deadline on return in the
      same way, using #5017's resolver seam with an injected resolver.
- **Content type** for output: the first `;`-segment, stripped and lower-cased, bounded to 256
  characters (D17; Python was unbounded), or `unknown`.

### R-TLS-PAY. Trust roots on the payment runtime (Q15)

Python preferred certifi (the Mozilla bundle) and otherwise the system store, never with
verification disabled. Botocore uses certifi and honours `AWS_CA_BUNDLE`.

The runtime treats caller-controlled environment as untrusted for security configuration, which
is why it ignores `HOME` and config-path overrides. Rust extends that rule to the trust roots
that protect the proof. Otherwise an agent that can set one variable could install its own CA
and read the proof in transit. The proof is the very thing the model must never see.

**What r0 got wrong (#5098).** r0 prescribed `RootEnv::default()` to get the platform store only.
That does not work. At `be0a06df`, `validate_roots` with an empty `RootEnv` calls
`NativeLoader::load_native()`, which calls `rustls_native_certs::load_native_certs()`. In the
pinned 0.8.5 source (`src/lib.rs:118-125`) that function begins with `CertPaths::from_env()` and
loads `SSL_CERT_FILE` and `SSL_CERT_DIR` whenever they are set. Its Unix platform branch then
calls `openssl_probe::probe()`, and openssl-probe 0.2.2 (`src/lib.rs:70-71,120-131`) starts from
`ProbeResult::from_env()` as well. Both loaders therefore read the variables again, below any
`RootEnv` view.

**S1's provider policy is unchanged (PM ruling).** #5017 S1's R-ROOTS deliberately honours
`SSL_CERT_FILE` and `SSL_CERT_DIR` for AWS provider HTTP, matching the AWS CLI. This design does
not change `validate_roots`, `RootEnv`, `NativeLoader` or `ValidatedRoots::from_process`, and it
claims nothing about #5017 or #4382 depending on the new seam. The seam below is an **addition
for x402 only**.

#### R-PROOTS. The payment root seam (`toolkit-aws`, prerequisite slice P-ROOTS)

P-ROOTS is a `toolkit-aws` slice, separate from the x402 slices S1-S7. It adds
`sdk/roots_payment.rs`, which reuses the shape of S1's `validate_roots`: a pure function over
injected loaders, a closed failure set with fixed path-free reasons, a non-empty de-duplicated
result, and no I/O outside the loader.

```text
pub struct PaymentRoots(ValidatedRoots);        // private field; no From<ValidatedRoots>
pub enum PaymentRootPlatform { UnixFixedTable, NativeStoreOnly }   // ::current() by cfg
pub trait PaymentRootLoader {
    fn load_paths(&self, file: Option<&Path>, dir: Option<&Path>) -> LoadedCerts; // explicit paths only
    fn native_store(&self) -> LoadedCerts;      // NativeStoreOnly targets
    fn exists(&self, path: &Path) -> bool;
}
pub fn validate_payment_roots(
    presence: &RootEnv,                          // consulted ONLY on NativeStoreOnly (rule 3)
    loader: &dyn PaymentRootLoader,
    platform: PaymentRootPlatform,
) -> Result<PaymentRoots, PaymentRootsFailure>;
impl PaymentRoots { pub fn from_process() -> Result<Self, PaymentRootsFailure>; pub fn roots(&self) -> &ValidatedRoots; }
```

The rules:

1. **Linux and other non-macOS Unix (`UnixFixedTable`): the environment is never read.** The
   candidate locations are a constant table in `roots_payment.rs`, copied per `target_os` from
   openssl-probe 0.2.2's `CERTIFICATE_FILE_NAMES` and `CERTIFICATE_DIRS`. The file constants are
   private in openssl-probe, so the table is restated. A comment cites the source version, and a
   unit test pins the Linux rows. The rule is the probe's own: the first existing file, plus
   every existing directory. Each location is loaded with
   `rustls_native_certs::load_certs_from_paths(Some(file), None)` or `(None, Some(dir))`. That
   public 0.8.5 function takes explicit paths and reads no environment (`src/lib.rs:239-280`).
   `load_native_certs()` and `openssl_probe::probe()` are **never called** on this branch.
2. **Validation reuses S1's rules.** Platform mode keeps the certificates that parse and counts
   the errors, as `validate_roots`'s platform branch does. If nothing usable remains, the result
   is `PaymentRootsFailure::NoRoots`. P-ROOTS makes two additive changes to `sdk/roots.rs` and
   nothing else:
   - a new variant `RootMode::PaymentPlatform`, so diagnostics and tests can tell the source;
   - `ValidatedRoots::new` and `parsable` become `pub(super)`, so the sibling module can build
     the value.

   No existing behaviour, signature or test of S1 changes.
3. **macOS (`NativeStoreOnly`).** The only public loader for the system keychain is
   `load_native_certs()`, and it reads the variables first. The seam therefore makes the
   variables **unable to influence the roots** by refusing to load when either is present:
   - if `presence.file` or `presence.dir` is `Some` (set at all, even empty), it fails with
     `PaymentRootsFailure::OverrideVariablePresent`;
   - otherwise it calls `native_store()` once.

   The view is taken by `RootEnv::from_process()` immediately before the call. This requires a
   stable process environment during loading. The workspace is edition 2021; its unsafe-code
   lint does not prohibit `std::env::set_var`, nor constrain dependency internals. Source review
   of `main.rs` → `toolkit_cli::run` → `agents_pay::fetch_command` → `World::for_process` finds no
   first-party environment mutator, and `World::for_process` builds the payment roots before the
   SDK runtime or provider roots. Two source gates now check this (#5114):
   `p_roots_startup_chain_crates_never_mutate_the_process_environment` scans `toolkit-cli`,
   `toolkit-core` and `toolkit-aws` for `set_var`, `remove_var` and the C equivalents, with a
   seeded mutator that a snapshot-only test cannot see, and
   `p_roots_world_builds_payment_roots_before_runtime_and_provider_roots` pins the order. They are
   syntax checks over first-party code only: dependency internals and an embedding host that
   mutates the environment concurrently remain a documented assumption. T-PROOTS-8 proves
   presence refusal for a supplied snapshot; T-PROOTS-9 checks the payment module.

   **Remaining design decision (#5114):** keep the checked first-party stable-environment
   contract with its dependency and embedding-host assumption (current state, enough for the
   intended CLI startup), or use an environment-independent native-store loader, which removes
   the two-read race entirely but needs a reviewed macOS loader/dependency and platform
   validation. Recommend the latter only if an embedding host is ever supported. Retain
   presence refusal either way. Do not mutate the global environment.
   The seam never falls back to `NativeLoader`. Windows needs no branch: the runtime refuses
   on non-Unix targets already (R-CFG).
4. **`AWS_CA_BUNDLE`** is read by no code in `toolkit-aws` at `be0a06df` (`git grep`). It cannot
   affect the payment roots, and a regression test pins that.
5. **Failure texts** are fixed and path-free, and they differ from S1's message because the remedy
   differs:
   - `"Payment TLS trust roots are unusable (no-roots); install the platform CA certificates."`
   - `"Payment TLS trust roots ignore SSL_CERT_FILE/SSL_CERT_DIR; unset them to make payments on this platform."`

   The x402 runtime exits 2 with either text before any network I/O. That makes it a
   T-ORDER-PRE refusal.
6. **Type separation.** `PaymentRoots` cannot be built from a `ValidatedRoots`; its only producers
   are `validate_payment_roots` and `from_process`. The x402 wiring functions take
   `&PaymentRoots`, so passing provider roots where payment roots are required is a type error.

**Hostile-CA offline regressions (P-ROOTS; no network).** The fixtures are checked in under
`crates/toolkit-aws/tests/fixtures/payment_roots/`: a self-signed "hostile" root and a leaf it
signs, with the leaf's test-only key. That key is a TLS test fixture, not a wallet key, and it
signs nothing outside these tests.

- **T-PROOTS-1/2/3.** In a child process with the real loader, `SSL_CERT_FILE` names the hostile
  root (1), `SSL_CERT_DIR` names a directory holding it (2), or both are set (3). In each case
  `PaymentRoots::from_process()` on Linux does not contain the hostile DER, and it equals the
  result of the same call with both variables unset.
- **T-PROOTS-4.** `AWS_CA_BUNDLE` names the hostile root: the same assertions.
- **T-PROOTS-5.** Empty values, `:`, and non-existent paths in either variable leave the result
  unchanged. S1 refuses `DirListEmpty` in this case; the payment seam does not care.
- **T-PROOTS-6, the loader recorder.** With a recording `PaymentRootLoader` and many `RootEnv`
  views, including hostile, empty and absent ones, the `UnixFixedTable` branch makes the same
  call sequence (only table paths) and returns the same result. `native_store` is never called.
- **T-PROOTS-7, in-memory handshake.** A rustls client with `PaymentRoots` built under
  `SSL_CERT_FILE=<hostile root>` connects to an in-memory rustls server presenting the hostile
  leaf, and the handshake **fails** (unknown issuer). The **control**, S1's
  `ValidatedRoots::from_process()` under the same environment, **succeeds**, which proves the test
  discriminates. A second control: a server leaf chained to a root injected into the fake
  loader's platform store succeeds, so platform trust remains usable.
- **T-PROOTS-8, `NativeStoreOnly` on an injected platform** (runs on every host). With either
  variable present, even empty, the result is `OverrideVariablePresent` with zero loader calls.
  With both absent, `native_store` is called once.
- **T-PROOTS-9, source gate.** `roots_payment.rs` does not call `load_native_certs`,
  `openssl_probe::probe`, `std::env::var*`, `set_var` or `remove_var` on the Unix branch. The
  only environment read is `RootEnv::from_process()` on the macOS branch.
- **T-PROOTS-10.** `compile_fail` doc-tests: `PaymentRoots(validated)`, and
  `From<ValidatedRoots>` used through `into()`.
- **T-PROOTS-11, S1 policy unchanged (control).** The existing S1 R-ROOTS tests still pass
  unmodified, and `validate_roots` still honours `SSL_CERT_FILE`.

#### Which roots each plane of the payment process uses (decision)

| Plane in `agents-pay fetch` | Roots | Why |
| --- | --- | --- |
| Merchant probe and paid replay | `PaymentRoots` | the replay carries the proof header |
| AgentCore data plane: `ProcessPayment`, `GetPaymentSession` (one `bedrock-agentcore` host and one TLS context) | **`PaymentRoots`** | see below |
| Credential provider HTTP (STS, SSO, SSO-OIDC, Sign-in, ECS, IMDS) through #5017's `ProviderHttp::Validated` | S1 provider roots (`ValidatedRoots::from_process()`), unchanged | see below |

**Decision: `ProcessPayment` uses the payment roots.** The threat is a caller, typically the
model, that can set environment variables for the `toolkit` process and so install its own CA.
Against that caller, the AgentCore data plane is as sensitive as the merchant plane, and more so:

- **Its response is the proof.** `paymentOutput.cryptoX402.payload` is the signed
  `PaymentDocument`. A hostile CA on this connection exposes the proof before R-PP-VERIFY or any
  merchant ever sees it. That defeats the Q15 ruling as fully as a hostile CA on the merchant
  plane would.
- **Its request is replayable.** A MITM holding the SigV4-signed `ProcessPayment` request can
  resend it to the real endpoint within the signature window. With the same token it gets the
  same proof back.
- **The cost is small.** The corporate-proxy breakage of Q15 already applies to the merchant
  plane in the same process. One set of roots for every proof-bearing connection gives one rule
  to review and to test.

The reviewer's suggestion to share the validated roots is therefore adopted for every
proof-bearing plane.

**The credential provider plane keeps S1's provider roots.** That is the accepted provider
policy, and it is not overridden here. The justification against the same threat model:

- **The caller gains nothing it lacks.** That plane returns AWS credentials, and the caller can
  already obtain them without TLS interception. It runs as the same OS account with the same
  environment, so it can read the profile, SSO cache and credential files, run the configured
  `credential_process`, or set `AWS_*` key variables itself.
- **The real boundary is IAM.** A caller holding `ProcessPaymentRole` credentials can call
  `ProcessPayment` directly and bypass the gate whatever the roots are. That is the IAM split's
  boundary (R-SPEND table, "who may sign"), and this port does not change it.
- **Diverging would cost consistency.** The shared credential chain would differ from #5017 for
  one route, for no gain in what the proof boundary protects.

T-PROOTS-USE (S5) asserts this split:
- the merchant transport's roots and the AgentCore data-plane transport's roots are the **same
  allocation** as the `PaymentRoots` built by the wiring (`ValidatedRoots::same_allocation`);
- the provider HTTP client receives `ValidatedRoots::from_process()`.

The wiring helpers are compiled in every `aws-sdk` build, so the test can call them. The test
performs no I/O, and the G4 gate forbids tests from calling `open` or `send` on what the helpers
return.

**Admin** commands (`agents-pay admin …`, `agents-build setup-payment-user`) use S1's R-ROOTS
(`ValidatedRoots::from_process()`) on every plane, because the human's environment is trusted
there. Roots that cannot be validated fail closed with the S1 `RootsFailure` text. Verification
is always on in every mode, and no flag disables it.

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
  attempts, so this is recorded as D20. The 60 s bound has the same R-DNS residual: the
  AgentCore and credential-provider hosts are resolved through the same blocking resolver. The
  R-DNS-ADMIT check runs before the `ProcessPayment` call. The logical call's deadline is
  `min(60 s, the remaining operation time)`, and that is passed to `RetryPolicy`, so no transport
  attempt starts after the operation deadline. Time blocked inside a lookup is still excluded, as
  R-DNS states. T-ORDER-POST counts transport attempts separately from logical authorizations.
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
- **D33 (accepted Q22, #5091).** `create-instrument` and `agents-build setup-payment-user` get the
  same gate as `new-session`. Python had none on either; IAM (ManagementRole) was the only
  control. The gate:
  - **TTY first**, before any dependency, argument or credential check. The refusal text is
    `"create-instrument must be run by a human at a terminal; it creates a wallet user."`, with
    `setup-payment-user` naming itself, and exit 1;
  - then the input checks, the STS check and the resolution;
  - then a summary of what will be created: the instrument for `--email` through `shown_value`,
    the network family, the manager, the connector and, for `setup-payment-user --budget`, the
    session budget and expiry;
  - then the literal `approve`. Anything else gives `"Aborted. No instrument created."`, exit 1.
    EOF gives the same text (the D28 rule);
  - one approval covers the instrument and the optional session of `setup-payment-user`.
    `setup-openclaw`'s existing per-step approvals are unchanged.
  - The registry test (no `--yes`, `force`, `non-interactive` or `assume`) is extended to both
    routes.
  - **T-TTY-CI (S6).** For each of `create-instrument` and `setup-payment-user`:
    - T-TTY-CI-1: a headless terminal gives the refusal with **zero** recorded calls, even with
      no credentials and with invalid arguments, which pins the order;
    - T-TTY-CI-2: an answer of `yes`, `Approve` or `approve ` (trailing space) aborts with zero
      `CreatePaymentInstrument` calls;
    - T-TTY-CI-3: EOF aborts with zero calls;
    - T-TTY-CI-4: `approve` gives exactly one `CreatePaymentInstrument`, plus one
      `CreatePaymentSession` only when `setup-payment-user --budget` is given;
    - T-TTY-CI-5: the summary shows the email through `shown_value` and nothing from the
      response before approval.

Validation that moves earlier (D29): `--budget` must be a positive decimal, and
`--expiry-minutes` and the wizard expiry must be in the model's 15..=480. Both are checked before
the confirmation prompt, with fixed messages. Python sent them unchecked and the service failed
with a traceback.

The wizard's prompt text `(1440 = 24 hours)` names a value the service rejects. It becomes
`(15-480; 480 = 8 hours)` (D30), and `format_duration` is kept.

**How tests guarantee no spend and no user creation (AC3, r1 for #5099):**

r0's G1 gated the live constructor with `#[cfg(not(test))]`. That only covers the crate's own
unit-test build:

- **Integration tests** link the library as an ordinary artifact, without `cfg(test)`. They can
  reach live constructors through the public `toolkit_cli::run` (`src/lib.rs:507`), with no
  live name spelled out in the test.
- **`CARGO_BIN_EXE_toolkit`** is an ordinary binary. In an `aws-sdk` run, feature unification
  builds it with the SDK.
- **Inherited real sources** (`~/.aws`, SSO caches, IMDS, `~/.agents-pay/config.json`) are
  outside G2/G3's fixture checks.

r1 replaces the custody model with the following.

- **G1, artifact custody: no verification artifact contains a live constructor.**
  - **The feature.** The live constructors sit behind a dedicated Cargo feature of `toolkit-cli`,
    `agents-pay-live = ["aws-sdk"]`, and **not** behind `aws-sdk` itself.
  - **What is live.** The module `agents_pay/live.rs` is
    `#![cfg(feature = "agents-pay-live")]`. It is the only code that builds the real pieces:
    `OsAccount` (the `nix` lookup), the config opener on the real home, the credential chain
    (`default_chain_with_http`), `SystemResolver`, `PaymentRoots::from_process()` and provider
    roots, the TLS transports, the real `Terminal` and `SystemClock`, the
    deployed-state working-directory reader, and the admin write target.
  - **What remains offline.** Everything else is compiled in every build and testable with
    injected fakes: the adapters `net::StreamingMerchant<T>` and
    `agentcore::Client<T, S>`, which receive an already-built transport and signer, and the root
    and transport wiring helpers used by T-PROOTS-USE.
  - **Verification builds never enable it.** `make test`, `make test-sdk`, `make clippy-sdk` and
    the daemon `scoped_test` build without `agents-pay-live`. In those builds the module does not
    exist, so neither the library linked by integration tests nor `CARGO_BIN_EXE_toolkit` nor any
    doc-test contains a live constructor.
  - **Release builds enable it.** The release and distribution build (the programme's
    binary-staging decision, #4487, Q24) and the operator live run (section 11) enable it
    explicitly.
- **G1a, fail-closed compile guards against enabling the feature in tests.**
  - The library carries
    `#[cfg(all(any(test, doctest), feature = "agents-pay-live"))] compile_error!("agents-pay-live is never built for tests (AC3)")`.
  - Every test crate root under `crates/toolkit-cli/tests/` begins with
    `#[cfg(feature = "agents-pay-live")] compile_error!(…)`.
  - So `cargo test --features agents-pay-live`, with or without `--test <name>`, `--lib` or
    `--doc`, fails to **compile** before any test runs. Cargo builds all selected test targets
    before it runs any of them.
  - T-AC3-GUARD, a `syn` gate in `tests/agents_pay_gate.rs`, asserts that every test crate root
    in `toolkit-cli` carries the guard, and that `toolkit-cli` has no `examples/` or `benches/`
    target that names `agents_pay`.
- **G1b, route dispatch without live dependencies.**
  - Every side-effect route obtains its world only through one injected value,
    `agents_pay::Deps`: `&dyn PaymentsApi`, `&dyn MerchantHttp`, `&dyn HostResolver`,
    `&dyn Terminal`, `&dyn Clock`, `&dyn ConfigStore` (runtime read, admin read and write),
    `&dyn DeployedState` and `&dyn AdminApi`. The routes are `fetch`, `--status`,
    `--browser-handle`, all six `admin` commands (`init-config`, `show-config`,
    `create-instrument`, `new-session`, `preflight`, `setup-openclaw`) and
    `agents-build setup-payment-user`.
  - The entry is `agents_pay::run_with(args, &Deps, out, err)`. The registry handler calls
    `run_with(args, &Deps::for_process()?, …)`.
  - **Without `agents-pay-live`**, `Deps::for_process()` returns `Unavailable` **after argv
    parsing and before** it reads any config file, home directory, environment variable,
    credential source, deployed state or terminal. The route then prints the D32 text: exit 2 for
    `fetch` and `--status`, exit 1 for admin and `setup-payment-user`.
  - Tests that exercise handler behaviour call `run_with` with fixture `Deps`. The fakes
    (`FakePaymentsApi`, `FakeMerchant`, `FixedResolver`, `FakeTerminal`, `FakeClock`,
    `TempConfigStore` rooted in a test temporary directory, `FakeDeployedState`) are public and
    harmless: none of them performs I/O outside its temporary directory.
- **G1c, T-AC3-INH: recorder-backed negative tests under an inherited, hostile, non-fixture
  environment.**
  - **Builds.** It runs in **both** verification builds (default and `aws-sdk`). `make test-sdk`
    gains `cargo test -p toolkit-cli --features aws-sdk --locked --test agents_pay_ac3` for it.
  - **Environment.** For every route listed in G1b, it runs once through `toolkit_cli::run` in
    process and once through `CARGO_BIN_EXE_toolkit` as a child, with a hostile environment:
    - `HOME` is a temporary directory holding a valid `.agents-pay/config.json` (mode 0600) with
      a **non-fixture** account and session, and `~/.aws/{config,credentials}` with
      syntactically valid, non-fixture fake keys and a profile;
    - `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, `AWS_PROFILE`, and the `PAYMENT_*`
      variables are set;
    - `AWS_ENDPOINT_URL`, `AWS_ENDPOINT_URL_BEDROCK_AGENTCORE`,
      `AWS_EC2_METADATA_SERVICE_ENDPOINT` and `AWS_CONTAINER_CREDENTIALS_FULL_URI` all point at
      a **loopback recorder** owned by the test (the #5017 T-ABS canary pattern);
    - `AGENTCORE_PROJECT_DIR` names a deployed-state fixture, and the working directory is a
      temporary directory.
  - **Assertions.**
    - The output is the D32 text with the documented exit code.
    - The recorder accepted **zero** connections, so no `ProcessPayment`,
      `CreatePaymentInstrument`, `CreatePaymentSession`, STS, IMDS or ECS request was made.
    - Every file in the temporary `HOME` and the working directory is byte- and mtime-identical,
      and no file was created.
    - The child had no terminal and read no stdin.

    These assertions are about effects, not spellings, so they hold however a route is
    dispatched.
- **G2, fixture namespace.** All fixtures use RFC 6761 hosts (`merchant.test`, `*.invalid`),
  account `111111111111` and IDs shaped like `payment-session-TESTxxxxxxxxxxx`. `FakePaymentsApi`
  panics on any other manager ARN account. Every fake records each call, and the tests assert the
  exact count: **zero** for every pre-authorization refusal (T-ORDER-PRE), and one logical
  authorization for the post-authorization failures (T-ORDER-POST). `TempConfigStore` refuses
  any path outside its own temporary root.
- **G3, runtime refusal of test identities on the live path** (defence in depth, compiled only
  under `agents-pay-live`). `Deps::for_process()` refuses to start
  when the manager ARN account is `111111111111` or `123456789012`, or when the URL host ends in
  `.test`, `.example`, `.invalid` or `.localhost`. Refusal text: `"Payment resources are test
  fixtures; refusing to contact live services."`, exit 2. A fixture leaking into a real invocation
  therefore cannot reach AWS.
- **G4, source gate** (`tests/agents_pay_gate.rs`, the `r_log_gate` pattern). This is now a
  secondary lint, since G1-G1c carry the guarantee. It parses the `toolkit-cli` test files with
  `syn` and fails if any test:
  - names `live::`, `Deps::for_process`, `PaymentRoots::from_process`, `SdkTransport`,
    `SystemResolver`, `OsAccount` or `default_chain`;
  - calls `open` or `send` on a wiring-helper result;
  - names the `agents-pay-live` feature anywhere except in the G1a guard.

  It also carries T-AC3-GUARD, T-ORDER-CTOR's in-crate checks and T-PROOTS-9's counterpart for
  `agents_pay`.
- **G5, the SDK verification configuration (`aws-sdk` without `agents-pay-live`).** G1 holds
  there by construction: the guard depends on the dedicated feature, not on `cfg(test)` or on
  `aws-sdk`.
  - **T-AC3-FEAT** reads `crates/toolkit-cli/Cargo.toml` and every workspace manifest. It
    asserts that `default` and `aws-sdk` do not include `agents-pay-live`, and that no workspace
    member enables it through a dependency's `features`. Feature unification therefore cannot
    turn it on in a `make` or daemon run.
  - **T-AC3-MAKE** rejects `agents-pay-live` in test/executing verification targets. The only
    allowed feature-enabled recipe is `clippy-live`: `cargo clippy` with `--lib --bins`, no
    test targets and no `--all-targets`. Positive and negative fixtures pin this exception.
  - T-AC3-INH and G4 run in both verification builds.
  - This covers the SDK library tests, `CARGO_BIN_EXE_toolkit` under `aws-sdk`, the admin
    creation routes and `agents-build setup-payment-user` / `setup-openclaw` alike.

## 8. Rust layout, slices and tests (tests first)

### 8.1 Modules (`crates/toolkit-cli/src/agents_pay/`)

| Module | Contents |
| --- | --- |
| `mod.rs` | `SLICE` (`agents-pay`), routes, output and exit rendering. A second `SLICE` constant for the `agents-build setup-payment-user` route lives in `agents_pay/build.rs` with slice name `agents-build`. Both sit in `registry.rs` `SLICES` at their alphabetical place with `NEW_RANK`; doc `docs/migration/agents-pay.md` (written in S7) |
| `urlparse.rs` | R-URLPARSE (pure) |
| `amount.rs` | R-AMT grammars and the digit-string comparison, `Decimal` rendering for `amount_usd` (pure) |
| `pyval.rs` | Python `str`/truthiness/`int()`/`float()` over `toolkit_core::json::Json` (pure) |
| `challenge.rs` | R-CH extraction from a status, headers and body (pure) |
| `policy/mod.rs`, `policy/approved.rs` | R-POL, R-VER, R-RESBIND, R-ORIGIN (with a `HostResolver`), R-TOK; `Approved` in its own leaf module (private fields, `pub(in policy)` constructor, not `Clone`, R-SPEND-ORDER) |
| `config.rs` | R-CFG, R-RES-RT, R-TUNE, the runtime path through `nix` |
| `net.rs` | `MerchantHttp` trait and the adapter `StreamingMerchant<T: StreamingTransport>`, which receives a built transport (R-NET, R-DNS-ADMIT); no root or transport construction |
| `agentcore.rs` | `PaymentsApi` trait; the `Approved`-only request builder; response readers for the four operations plus STS; R-AWS-*; `FakePaymentsApi` (public, harmless) |
| `deps.rs` | `Deps`, `run_with`, `Deps::for_process()` (returns `Unavailable` unless `agents-pay-live`), the public fixtures of G1b |
| `wiring.rs` | `aws-sdk` only: builds the merchant and AgentCore transports from `&PaymentRoots`, and provider HTTP from S1 roots. No I/O and no credentials (T-PROOTS-USE) |
| `proof.rs` | R-PP-VERIFY, R-HDR, `validAfter` |
| `flow.rs` | `fetch`, `status`, `prepare_browser_payment` orchestration; R-RETRY; R-SCRUB; R-ERR |
| `vault.rs` | in-process handle vault: 90 s TTL on the injected monotonic clock, single use, origin and path bound; handle `x402h_` plus 32 base64url characters from 24 `getrandom` bytes |
| `admin/` | `init_config.rs`, `show_config.rs`, `create_instrument.rs`, `new_session.rs`, `preflight.rs`, `setup_openclaw.rs`, `deployed.rs`, `write.rs`, `terminal.rs` |
| `live.rs` | `#![cfg(feature = "agents-pay-live")]`: the only live constructors (G1), G3 |
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
| **P-ROOTS** (`toolkit-aws`, not an x402 slice) | `sdk/roots_payment.rs`: `PaymentRoots`, `validate_payment_roots`, `PaymentRootLoader`, the fixed Unix location table, `RootMode::PaymentPlatform` (R-PROOTS) | T-PROOTS-1..11 (hostile `SSL_CERT_FILE`/`SSL_CERT_DIR`/both/`AWS_CA_BUNDLE`, empty values, the loader recorder, the in-memory handshake with an S1 control, the macOS presence refusal, the source gate, compile_fail, S1 unchanged) | #5017 S1 (`be0a06df`) on rolling. It is not yet an ancestor of the `fa3fa13` base |
| **S1** pure policy | `urlparse`, `amount`, `pyval`, `challenge`, `policy` (no network: `FixedResolver`) | RecipientAndValue, RecipientValidation, OptionalOrigin (pure part), Authorize, Idempotency (+ golden vectors R-TOK), SignerInput (pure part), canonical amounts; T-POL-UNI, T-URLP (params, IPv6, port, case), T-AMT (grammar tables, Infinity/NaN cap D6), **T-AMT-CAP-1..6** (R-AMT-REP/R-AMT-CMP spellings at the cap−1/cap/cap+1 boundaries), T-AMT-USD, T-VER, T-CH (header base64/raw/body, duplicate keys D1, truthiness), **T-ORDER-CTOR** (compile_fail and the in-crate `syn` checks), **T-ORDER-PRE** (policy part, over a recording fake) | #4379 only; can start now |
| **S2** config | `config.rs`, `nix` user lookup | PolicyFile, ConfigPrecedence, PolicyDirectory, SingleTenantUserId (resolution part), bounded env; T-CFG-RACE (symlink swapped in after the parent check, through a `rustix` test seam), T-CFG-SIZE, T-TUNE (`nan`, `inf`, `1e1`, `5.9`) | S1 |
| **S3** merchant net | `net.rs` | Ssrf, DnsPinning (resolver returns public then private; peer check), SafeMethod; T-NET: exact request headers, no cookie carry, 3xx reported, Content-Length precheck with zero pulls, cap, deadline trickle (TA-STREAM-DL fake clock), encoding D15, no proxy; **T-DNS-DL-1, -2, -4, -5** (R-DNS-ADMIT, separate from the streaming tests) | S1, #5017 S1 on rolling, **#5080** TA-STREAM-DL. Not P-ROOTS: the adapter receives a built transport |
| **S4** AgentCore client | `agentcore.rs`, model excerpt | SessionStatus (fake, D19 logic), runtime/admin agent names; T-AWS: golden request bytes (path, both headers, body order, token) for the four ops; response validation (status, echo, required members, payload shape and size); error allowlist (each seeded secret in `x-amzn-ErrorType`/`__type`/`message` → absent, launch T-CODE pattern); fixed endpoint despite `AWS_ENDPOINT_URL*` (D18); `Debug` redaction; `ProcessPaymentRequest` only from `Approved`; `ConflictException` not retryable (R-TOK-NOFRESH) | S1, #5017 S1, **#5080** TA-STREAM-DL. Not P-ROOTS: the adapter receives a built transport |
| **S5** flow + `fetch` route | `proof`, `flow`, `vault`, `deps`, `wiring`, `live`, the `agents-pay-live` feature and the G1a guards, route | TransientSettlement, BrowserHandle, HarnessCli; **T-ORDER-PRE** (flow part), **T-ORDER-POST** (table in R-SPEND-ORDER), T-RETRY, **T-RESTART-1..8**, T-PROOF (R-PP-VERIFY mismatch on `to`, `value`, expired `validBefore`; `validAfter` wait ≤ 15 s; zero proof sends), T-HDR (v1/v2 bytes), T-SCRUB (merchant echoes the header in its body → `[REDACTED]`; an echo in `content_type` → generic), T-ERR exhaustive, **T-DNS-DL-3**, **T-PROOTS-USE**, CLI process tests (exit 2 on refusal, single JSON, usage exit 1), **T-AC3-INH** (fetch routes), **T-AC3-GUARD**, **T-AC3-FEAT**, **T-AC3-MAKE**; G4 gate | S2-S4, #4941 at `40c71d7e` on rolling, **P-ROOTS on rolling** (the `wiring.rs` and `live.rs` constructors take `&PaymentRoots`) |
| **S6** admin | `admin/`, `agents-build setup-payment-user` | AdminRecipientMode, test_resource_resolution (all asserts), test_setup_openclaw (`usd_to_atomic`, `build_openclaw_config`, `discover_deployed` with `--project-dir`, `format_duration`); T-TTY (order: TTY first even without credentials; `approve` exact; EOF D28; no `--yes`), **T-TTY-CI-1..5** (D33 for `create-instrument` and `setup-payment-user`), T-WRITE (atomic, modes, temporary file removed on failure), T-VAL (D29, `--budget` through `CapDecimal`), T-PREFLIGHT (secret-name scan), D27, **T-AC3-INH** (all admin and `agents-build` routes) | S2, S4, S5 (it reuses `Deps` and the G1a guards). Not P-ROOTS: admin uses S1 roots |
| **S7** cutover docs | `SKILL.md` (both copies), `references/*.md`, `agents-build/references/payments.md` steps that run scripts, `docs/migration/agents-pay.md`, the portability test, inventory rows → `ported` | R-PORT test; `every_slice_documents_its_commands`; the SKILL text carries L-TERMS, the R-DNS residual, the payment-roots boundary (B3, B4) and the B1 import-API boundary | S5, S6; the Q2 and Q3 decisions |

**Which slices wait on P-ROOTS: S5 only**, through `wiring.rs` and `live.rs`; S6 and S7 come
after S5. S1, S2, S3, S4 and P-ROOTS can proceed in parallel once their own prerequisites
land. P-ROOTS is owned by `toolkit-aws` and gets its own scoped test and its own review. It
neither changes nor is required by #5017's or #4382's provider-root policy.

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
| **`nix =0.29.0`**, `default-features = false`, `features = ["user"]`, `cfg(unix)` | **new**; `nix 0.29.0` and its `cfg_aliases 0.2.2` build dependency are new lockfile packages; `libc`, `bitflags`, and `cfg-if` were already present (confirmed with `cargo tree` in S2) | safe `User::from_uid` (`getpwuid_r`) for the OS-account home. `std` and `rustix` have no passwd lookup, and the workspace forbids `unsafe`. Parsing `/etc/passwd` by hand would miss NSS (LDAP, sssd) users, and spawning `getent` would be `PATH`-hijackable. Used in one function. Alternative in Q20 |
| crypto or signing crate | **none** | signing happens in AgentCore (`PaymentDocument` is server-produced). SHA-256 comes from `toolkit-core`, SigV4 from the existing `aws-sigv4`. No secp256k1, k256 or EIP-712 crate. The R-PP-VERIFY checks are string comparisons and do **not** verify the signature; verification is the merchant's job, and doing it locally would add a crypto dependency for no extra safety over the to/value check |
| `rustls-native-certs =0.8.5`, `rustls` (P-ROOTS) | already pinned in `toolkit-aws` under `sdk` by #5017 S1; no new crate | `load_certs_from_paths` (explicit paths, no environment) and, on macOS, `load_native_certs` behind the presence check. The in-memory handshake of T-PROOTS-7 uses the `rustls` server half already in the tree; it adds no `rcgen` and no `trybuild`. The certificates are checked-in fixtures |
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
- D16 absolute deadlines, excluding time blocked in system DNS (R-DNS residual). Deadline
  admission (R-DNS-ADMIT) prevents any work from starting after the deadline.
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
- D32 (r1, widened for #5099) a build without `agents-pay-live` has no payment support. That
  covers the default build, the `aws-sdk` verification build and every test artifact.
  - **Every** agents-pay route and `agents-build setup-payment-user` refuse after argv parsing
    and before any config, home, environment, credential, deployed-state or terminal access.
    The text is `"This toolkit build has no payment support; install a release build of
    toolkit."`.
  - `fetch` and `--browser-handle` exit 2; `--status` reports `usable: false` with that reason
    and exits 2; admin and `setup-payment-user` print the text on stderr and exit 1.
  - This replaces the "bedrock-agentcore … not installed" texts.
- D33 (accepted Q22) `create-instrument` and `setup-payment-user` require a TTY first and a typed
  `approve` before `CreatePaymentInstrument` (and before `CreatePaymentSession` for
  `--budget`). Python had no gate on either.
- D34 (#5098) the payment runtime's trust roots for the merchant and AgentCore data planes do not
  depend on the environment:
  - Linux and other Unix: a fixed location table; `SSL_CERT_FILE`, `SSL_CERT_DIR` and
    `AWS_CA_BUNDLE` are ignored;
  - macOS: a refusal while `SSL_CERT_FILE` or `SSL_CERT_DIR` is set.
  Python and botocore honoured certifi and `AWS_CA_BUNDLE`. Credential-provider HTTP and the
  admin commands keep S1's R-ROOTS.
- D36 (#5166, #5168) use nix =0.29.0 for the same safe getpwuid_r lookup: nix 0.31.1 pins libc =0.2.180, conflicting with the workspace mio requirement >=0.2.183. This also adds the cfg_aliases 0.2.2 build crate.
- D37 (#5177, #5186) non-ASCII Unicode decimal digits and ASCII U+001C..U+001F padding are unparseable by the Rust tunable parser and give the bounded default. NBSP and other Unicode whitespace are trimmed as in Python.
- D38 (#5181): STS uses XML error envelopes, so its error-code fallback reads a single `Error/Code` in the STS namespace when `x-amzn-ErrorType` is absent. It applies the same fixed allowlist and never emits the message or request ID. Malformed, ambiguous, and unknown codes become `UnrecognizedError`; an existing header retains precedence.
- D39 (#5331): the shared SDK transport classifies a response-body `UnexpectedEof` anywhere in its error chain, or a hyper HTTP parse error other than too-large, as non-retryable `AwsError::Protocol`; previously these were retryable `Connect` errors. This also covers a connection dropped mid-body, not just malformed framing. The tradeoff is fewer automatic recoveries from transient truncation: payment operations avoid replaying an exchange whose outcome may be ambiguous; MCP proxy requests surface the failure rather than replaying an operation with potentially unknown effects; `asm_exec` secret retrieval fails rather than using incomplete data, and the user can retry the command. Other shared SDK consumers likewise receive a terminal error and must decide whether a fresh operation is safe. See `aws-core-contracts.md` for the transport boundary.
- D35 (#5157) reject `=` after a complete Base64 quartet, although Python 3.11
  `b64decode(validate=True)` accepts it. A header with this padding falls through
  to raw-header and body extraction; it may therefore be refused.

## 11. What is offline-testable and what needs the operator's live run

**Offline (every slice, default `cargo test` and the daemon `scoped_test`):**

- all of sections 3-7 over fakes: policy, amounts, tokens and golden vectors, challenge parsing;
- request bytes for the four AgentCore operations, response validation, error allowlists;
- the merchant HTTP behaviour over `FakeStreamingTransport` and a fake resolver;
- deadlines on a fake clock;
- the handle vault;
- admin file writes in temporary directories, TTY gating over a fake terminal;
- deployed-state layouts, the CLI exit codes for pre-network refusals;
- the G4 source gate, T-AC3-GUARD, T-AC3-FEAT and T-AC3-MAKE;
- T-AC3-INH, whose only socket is a loopback recorder owned by the test, which must see zero
  connections;
- P-ROOTS's T-PROOTS-*, using checked-in certificates and an in-memory handshake.

No test needs credentials or external network access.

**Operator live run (gate: it creates a real wallet user, creates a session, and spends; on
Base Sepolia the USDC is testnet, but the resources are real and mainnet `eip155:8453` would be
real money, so that network is out of scope unless the operator decides otherwise).**

Use a build with `--features agents-pay-live`, built by the operator outside every
verification target (G1), a disposable account or manager, and budget `0.10`:

1. `admin init-config --recipient <sandbox payTo> --origin <sandbox origin>`, `show-config`,
   `preflight`. No spend.
2. `admin create-instrument --email <operator>`, which **creates a user**. Then the human
   delegation and faucet funding.
3. `admin new-session --budget 0.10 --expiry-minutes 30` at a TTY.
4. `fetch --status`. Confirms the Q19 status computation against a real `GetPaymentSession`.
5. `fetch <v2 sandbox URL>` → `paid: true`. Record the `PAYMENT-SIGNATURE` acceptance (Q4 shape)
   and **exactly one** on-chain transfer.
6. Re-run (5) at once: the same token, no second transfer, and `availableSpendAmount` unchanged
   (R-TOK-SCOPE, same terms).
   - 6b. Re-run (5) after 10 minutes and after 60 minutes. Record whether the service still
     returns the stored proof for the same token. That gives a rough lower bound on the retention
     window, which is [unknown] in the model.
   - 6c. **Required operator validation gate before relying on body-mismatch rejection.** With
     an authorized sandbox merchant that can vary `extra` and `maxTimeoutSeconds`, record the
     real service's same-token/different-body contract: `ConflictException`,
     `ValidationException`, or a successful response, including echoed IDs and proof validity.
     Do not infer rejection from AWS convention. If success returns a valid matching stored
     proof, record that the stateless client accepts it; no fresh-token recovery is permitted.
     If this gate cannot be run, the mismatch contract remains unknown. This document and the
     offline tests do not authorize running it or establish a live result.
7. `fetch <same URL> --purchase-id 2` → one more transfer.
8. A URL priced above the cap → refused; CloudTrail shows **no** `ProcessPayment`.
9. A v1 merchant, if one is available (`X-PAYMENT`, Q4).
10. `--browser-handle` → a handle; confirm no proof appears in stdout (Q3).
11. A session drained or expired → `usable: false`, and `fetch` refused by the service with
    `ClientError` text.
12. Compare with the Python CLI on the same session, captured by the operator. Python stays
    read-only evidence for agents.
13. With `SSL_CERT_FILE` naming an unrelated CA bundle, run `fetch <v2 sandbox URL>`. On Linux
    it is paid, because the variable is ignored (D34). On macOS it is refused with the
    payment-roots text and no AWS call. On a host behind a TLS-inspecting proxy, it is refused
    with a TLS error (boundary B4).
14. Run `create-instrument` and `setup-payment-user` with stdin redirected (headless): both are
    refused, and CloudTrail shows no `CreatePaymentInstrument` (D33).

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
  the importable functions.

  **Ruled (#5091): accepted.** The follow-up is filed as #5103. Until it lands, the import API is
  not replaced (boundary B1, section 12a).
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

  **Ruled (#5091): accepted**, platform store only for the payment process, as a documented boundary
  (B4). No claim is made that #4382 or #5017 needs the new seam.
  - **r1 (#5098)** specifies how. The payment roots come from the new P-ROOTS seam, because
    `RootEnv::default()` is not environment-independent. They cover the merchant and AgentCore
    data planes.
  - #5017 S1's provider roots, which honour `SSL_CERT_*` as the AWS CLI does, are unchanged and
    remain in use for credential-provider HTTP and admin.
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
- **Q22. TTY gate on `create-instrument` (and `setup-payment-user`).** **Ruled (#5091):
  accepted.** This is D33, specified in section 7 with T-TTY-CI-1..5 and live-run step 14.
- **Q23. `--status` exit code when the build lacks payment support.** **Recommend:** 2, which is
  parity with "SDK not installed". r1: this applies to every build without `agents-pay-live`
  (D32).
- **Q24. The `agents-pay-live` feature and the release build (r1, #5099).**
  - **Assumption:** the programme's release and distribution build (#4487) enables
    `agents-pay-live`, and every verification target builds without it. That includes
    `make test`, `make test-sdk`, `make clippy-sdk` and the daemon `scoped_test`.
  - Clippy coverage of `live.rs` needs a lint-only target,
    `cargo clippy -p toolkit-cli --features agents-pay-live --lib --bins`, with **no**
    `--all-targets` (no test targets) and **no** `--all-features` elsewhere. S5 adds it as
    `make clippy-live`.
  - If any verification job uses `--all-features`, the G1a guards make it fail to compile, which
    is fail-closed and visible. The manager should then move that job to explicit features.
  - **Recommend:** adopt as written.

## 12a. Documented boundaries (not guaranteed by this design)

- **B1. The Python import API is not replaced.** `from x402_fetch import x402_fetch,
  payment_session_status, prepare_browser_payment, attach_browser_payment` has no Rust
  equivalent. The CLI `--browser-handle` keeps its Python lifetime: a handle cannot be redeemed
  after the process exits (Q3). The long-lived trusted stdio API is the follow-up #5103. S7
  states this in the SKILL text.
- **B2. No exactly-once purchase across changed challenges.** Legacy token parity gives one
  authorization per invocation. Across invocations it reuses an authorization only for
  unchanged terms and body within the service's retention window (R-TOK-SCOPE). A merchant that
  changes the amount or the recipient within policy can obtain a second authorization (L-TERMS,
  T-RESTART-4/5/7). A durable purchase-to-terms binding is out of scope; it would need its own
  design and review, and must not persist proof secrets.
- **B3. Deadlines exclude blocking system DNS** (R-DNS). The deadline-admission rule prevents any
  work, and in particular any proof send, from starting after the deadline. No async resolver is
  added.
- **B4. Payment roots are the platform store only** (Q15, D34). TLS-inspecting proxies break the
  payment runtime. On macOS it refuses while `SSL_CERT_FILE` or `SSL_CERT_DIR` is set.
  Credential-provider HTTP and admin keep S1's R-ROOTS, which honours them.
- **B5. IAM is the outer boundary.** A caller that holds `ProcessPaymentRole` credentials can
  call `ProcessPayment` directly, bypassing this runtime's gate. The port does not change IAM
  (R-SPEND, R-TLS-PAY decision).

## 13. Review checklist for the AC1 pre-review

A reviewer should be able to confirm each of the following from this document alone:

1. **Order.** Every spending call consumes one immutable, non-`Clone` `Approved` by value.
   Pre-authorization refusals record zero `ProcessPayment` calls (T-ORDER-PRE).
   Post-authorization failures record one logical authorization, no further one, and zero proof
   sends when the proof is invalid (T-ORDER-POST). See R-SPEND-ORDER and T-ORDER-CTOR.
2. **Tokens.** The token is byte-identical to Python (R-TOK vectors). Within an invocation no
   retry path can produce a second authorization (R-RETRY table). Across invocations the
   guarantee is conditional (R-TOK-SCOPE, T-RESTART, B2), and no fresh token is ever minted
   (R-TOK-NOFRESH).
2a. **Cap.** The cap has one decimal representation and the comparison is correct at every unit
   boundary for equivalent spellings (R-AMT-REP, R-AMT-CMP, T-AMT-CAP).
3. **The proof.** It is never printed or logged, never put in `Debug` or a file, and never left
   unscrubbed in a body (R-SEC, R-RED, R-SCRUB); it is checked against the approved terms before
   it is sent (R-PP-VERIFY).
4. **Network.** The merchant plane allows only https, public addresses at both the resolver and
   the peer, no redirects, no proxy, caps and deadlines, with the DNS residual (R-NET, R-DNS).
   The AWS plane uses a fixed endpoint, ignores env overrides on the runtime, and allowlists
   error codes (R-AWS). Every proof-bearing connection uses environment-independent payment
   roots (R-PROOTS, T-PROOTS).
5. **Configuration.** The config is bound to the OS account and opened race-free; the
   environment is a fallback only (R-CFG, R-RES-RT).
6. **AC3.** No verification artifact (lib unit tests, the library linked by integration tests,
   `CARGO_BIN_EXE_toolkit`, doc-tests) contains a live constructor, under the default or the
   `aws-sdk` build. Every side-effect route dispatches through injected `Deps`. An inherited real
   environment causes no connection and no file change (G1-G5, T-AC3-*).
7. **Dependencies.** Each new dependency is justified, and no crypto crate is added (section 9).

## 14. r1 resolution of the #5091 pre-review

| Finding | Severity | Ruling applied | Changed sections | Test IDs |
| --- | --- | --- | --- | --- |
| #5097 cap exponent sign | P1 | One decimal representation; corrected comparison; equivalent spellings at exact unit boundaries in S1 | R-AMT (D6 bounds, R-AMT-REP, R-AMT-CMP, worked check), the S1 row, checklist 2a | T-AMT-CAP-1..6; T-ORDER-PRE over-cap rows |
| #5098 CA variables reach NativeLoader | P1 | As corrected by the PM: S1 provider roots unchanged. A new x402-only payment-root seam in `toolkit-aws` (P-ROOTS) reuses the `validate_roots` shape. `ProcessPayment` and `GetPaymentSession` use the payment roots; credential-provider HTTP and admin keep S1's roots. Only S5 waits on P-ROOTS | R-TLS-PAY (R-PROOTS, the plane decision), section 0, 8.1 (`wiring.rs`), 8.2 (P-ROOTS row and the "which slices wait" note), section 9, D34, Q15, B4, B5, live-run step 13 | T-PROOTS-1..11, T-PROOTS-USE |
| #5099 G1/G5 do not cover SDK test and CLI artifacts | P1 | Fixture-only dispatch and artifact custody | Section 7 (G1, G1a, G1b, G1c, G3, G4, G5), 8.1 (`deps.rs`, `live.rs`), 8.2 S5/S6, D32, Q24, section 11 | T-AC3-INH, T-AC3-GUARD, T-AC3-FEAT, T-AC3-MAKE, G4 |
| #5100 restart claim omits changed terms and body conflicts | P2 | Legacy token bytes kept; restart limitation documented; no fresh token after a conflict; two-invocation cases; a cross-challenge binding is out of scope | R-TOK (R-TOK-SCOPE, L-TERMS, R-TOK-NOFRESH), R-RETRY row, the S4/S5 rows, B2, live-run 6b/6c | T-RESTART-1..8 |
| #5101 blocking-DNS residual | P2 | The residual is carried into the deadline contract, as #4382 did; no async-resolver prerequisite | R-NET (R-DNS, R-DNS-ADMIT), R-AWS-RETRY, D16, B3, the S3/S5 rows, S7 text | T-DNS-DL-1..5 |
| #5102 T-ORDER phases and `Approved` | P2 | Pre- and post-authorization split; immutable private `Approved` consumed by value; the request only from `Approved` | R-SPEND-ORDER (rules 1-5, both test phases), R-SPEND table, section 7 G2, 8.1 `policy/`, the S1/S5 rows, checklist 1 | T-ORDER-CTOR, T-ORDER-PRE, T-ORDER-POST |
| #5114 macOS environment-race rationale | P2 | Edition claim removed; first-party startup chain checked by source gates (no mutator; payment roots built first); dependency/embedding-host assumption stays documented, with an environment-independent native loader as the optional stronger fix | R-PROOTS macOS branch | `p_roots_startup_chain_crates_never_mutate_the_process_environment`, `p_roots_environment_gate_catches_a_mutator_a_snapshot_does_not`, `p_roots_world_builds_payment_roots_before_runtime_and_provider_roots` |
| #5115 successful body-mismatch response | P2 | Service errors fail with no new proof send; a successful matching stored proof is accepted by the stateless client; actual service contract requires operator gate 6c | R-TOK-SCOPE, T-RESTART-6, live-run 6c | `t_restart_6_body_mismatch_service_contract_is_not_assumed` (Conflict, Validation, StoredProof × extra, timeout) |
| #5116 browser randomness versus client token | P2 | Syntax gate described accurately; vault randomness allowed; identical purchase terms keep token/body bytes while handles differ | R-TOK-NOFRESH | `browser_handle_expires_and_handles_are_unique`, R-TOK-NOFRESH source fixtures |
| #5117 lint-only live-feature exception | P3 | Executing verification targets reject the feature; only the library/binary clippy recipe is allowed | T-AC3-MAKE, Makefile `clippy-live` | `t_ac3_make_no_test_target_passes_the_live_feature`, `t_ac3_make_catches_a_test_target_with_the_feature` |
| Accepted Q22 (#5091 ruling) | — | A TTY gate and `approve` on `create-instrument` and `setup-payment-user` | Section 0, section 7 (D33), the S6 row, D33, Q22, live-run step 14 | T-TTY-CI-1..5 |
| Documented boundaries (PM) | — | The import API is not replaced by the CLI handle (#5103); cross-challenge exactly-once is not guaranteed | Section 12a (B1, B2), Q3 | T-RESTART-4/5/7 pin B2 |

Carried unchanged into the implementation reviews:
- Q4 covers PP-PAYLOAD and both R-HDR versions. A successful live run for one version does not
  prove the other; an unavailable v1 merchant leaves v1 unverified, never silently accepted.
- The model provenance gap stays under #5089.
