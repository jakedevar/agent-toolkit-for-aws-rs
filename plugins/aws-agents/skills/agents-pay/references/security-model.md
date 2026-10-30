# agents-pay security model

Plugin references are plain files; the host does not substitute their variables.
Get the absolute installed plugin root from the loaded parent skill (its
`${CLAUDE_PLUGIN_ROOT}` token is substituted there). In your own terminal, set
`export CLAUDE_PLUGIN_ROOT='/absolute/installed/plugin'` to that actual path.
The guarded commands below refuse an unset or empty root; never use PATH.

The threat model, and how each control is enforced. Read this before changing
anything in the toolkit's `agents-pay` code — several behaviors that look over-cautious are load-bearing.

## Scope: this is the run-time skill

Worth stating before anything else, because there is an adjacent skill that looks like
it does the same job.

| | `agents-build` → `references/payments.md` | `agents-pay` (this skill) |
|---|---|---|
| Question | "How do I give the agent I am **building** the ability to pay?" | "This agent needs to pay for this **now**" |
| When | Build time, in a product being shipped | Run time, in the session at hand |
| Wallet | One per **end user** of that product | One for this installation |
| Who approves spend | The product's own flow | The operator, at a terminal |
| Threat model here | The product's customers | **Hostile model input routed through the registered payment tools** |

That last row is why this skill exists separately. When a product mints a session per
customer through its own approval flow, the agent is a component inside a system its
author controls. When an agent spends its **operator's** money mid-task, the agent is
the thing that might be compromised, so every limit has to hold against the agent.
`auto_session=True`, while valid in build-time guidance, lets a runtime replace an
exhausted budget and is unsuitable for this skill.

Neither is wrong. They answer different questions, and `agents-build` is left untouched
by this branch.

## The gate only covers what routes through it

A boundary condition worth stating before the trust table, because it is the easiest
way to end up with none of these controls while believing you have them.

Strands and LangGraph ship AgentCore Payments integrations (`AgentCorePaymentsPlugin`,
`AgentCorePaymentsMiddleware`) that intercept `402` from **any** tool call and settle
it. They are genuinely more convenient than running `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch`. They also sit
entirely outside this skill: payment happens inside the framework's own wrapper, so
the toolkit policy is never consulted.

What that costs, concretely:

| Control | Via `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch` | Via native plugin / middleware |
|---|---|---|
| Per-payment ceiling | enforced | **absent** |
| Origin allowlist and SSRF vetting | enforced | **absent** |
| Derived idempotency token | enforced | **absent** — random per call |
| Only the vetted `accepts` entry reaches the signer | enforced | **absent** |
| Proof kept out of model context | enforced | depends on the integration |
| Session creation kept off the runtime role | enforced | **`auto_session=True` requires it** |

An agent that can mint a session can replace a spent budget with a larger one, so
the per-session cap stops bounding anything.

**Do not run both paths in one process.** If the native integration is active *and*
`"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch` is available, the model chooses which one settles a given `402`, so the
gate becomes advisory. Pick one. For an agent spending an operator's money against the
open web, pick `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch`; if the native path is used anyway, at minimum pass an
explicit `payment_session_id` so budget still comes from a human.

## Trust boundaries

| Component | Trusted? | Holds credentials? | Who runs it |
|---|---|---|---|
| Operator at a terminal | Yes — the root of authority | Yes (via the CLI wizard) | Human |
| `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin` | Yes | Only transiently, from the human | Human |
| `~/.agents-pay/config.json` | Yes — the authorization record | No | Written by human, read by runtime |
| The policy code in `toolkit` | Yes — the decision point | No | In-process, runtime |
| The fetch and settle code in `toolkit` | Yes — transport | No (proof is transient) | In-process, runtime |
| The model / agent loop | **No** | No | — |
| Publisher HTTP response | **No** — hostile input | No | — |

The model is inside the threat model, not outside it. The controls hold when hostile
model input reaches the registered payment tools. Unrestricted code execution under
the same OS identity and AWS credentials is a host compromise and needs a separate
process, container, OS account, or IAM boundary.

## Why executable controls matter

Two design rules apply throughout this skill:

1. **A control that a model can decline is not a control.** Every limit in this
   skill is evaluated in the `toolkit` binary before signing on the sanctioned runtime path.
2. **Documentation must not claim a guarantee the code does not enforce.** Every
   claim below names the function that implements it, so the two cannot drift
   silently.

## Runtime design

| Capability | Here | Security property |
|---|---|---|
| `get_paid_content` | `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch URL` — agent command | Same capability, now behind the policy gate |
| Session status | `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch --status` — agent command | Read-only, cannot mint budget |
| Browser payment | `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch --browser-handle URL` — agent command | The model receives an opaque handle bound to one origin and path; a handle cannot be redeemed after the process exits (no Rust equivalent of the Python import API, follow-up #5103) |
| Create a payment session | `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin new-session` — human at a TTY | A runtime that can mint sessions has no cumulative bound |
| Provision infrastructure | `agentcore` CLI wizard + `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin init-config` | Provider secrets never enter tool parameters, and setup does not exist at runtime |

The browser flow is worth stating plainly, because it is the one case where a
proof must reach a caller: the model receives a handle, never proof bytes. The
handle is single-use, expires in 90 seconds, and is bound to one origin and path,
so a handle lifted from a transcript cannot be redeemed for another resource or
redeemed twice. The Python redeem call (`attach_browser_payment`) has no Rust
equivalent: the handle `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch --browser-handle` prints cannot
be redeemed after the process exits, so a browser-navigation flow needs a
long-lived trusted host (follow-up #5103). Nothing in the model's tool set may
return the real header.

## Security controls and enforcement

| Control | Enforcement |
|---|---|
| Challenge validation | Strict schema, configured scheme and network, exact asset contract, explicit recipient mode, canonical positive amount under `max_per_payment_usd`, and resource/origin checks are enforced before signing. Conflicting `amount` and `maxAmountRequired` aliases are refused; the signer receives one version-canonical amount field. The normal mode requires `payTo` in `allowed_recipients`; the explicit `allow_any_recipient: true` mode delegates beneficiary choice to the publisher. |
| Secret handling | No script accepts a secret argument. Provider credentials go only to the `agentcore` CLI wizard; signing happens inside AgentCore Payments. `preflight` rejects credential-shaped environment variables. |
| Network protection | The network admission check requires HTTPS and reject loopback, RFC1918, link-local, metadata, multicast, reserved, unspecified, CGNAT, and v4-mapped forms. The connection goes to the vetted address; redirects are refused and bodies are capped. |
| Content isolation | Paid bodies are withheld from model-visible output by default. The runtime returns status, content type, byte count, and SHA-256 hash. An operator may opt in to body return by setting `return_body: true` in the config file (OS-account-bound, 0600); when enabled, content is capped at 10 KiB and marked `untrusted: true`. Authorisation never reads content. The OpenClaw plugin (`get_paid_content`) mirrors this: an optional `returnBody: true` plugin-config field (default unset/false, same behaviour as today) opts a single installation in to the same capped, `untrusted`-marked body return — set per operator, not model-controllable. |
| Idempotency | The client token hashes session, origin, path, network, asset, recipient, and amount. It excludes a publisher nonce so retries reuse the same authorisation. |
| Role separation | Session creation exists only in `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin new-session`, which refuses without a TTY and has no `--yes` flag. The human uses **ManagementRole** and the agent uses **ProcessPaymentRole** with no session writes. |
| Proof isolation | `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch` holds the proof locally for one request. The browser path returns an opaque handle bound to one origin and path, which cannot be redeemed after the process exits, and output carries a redacted receipt only. |
| Runtime surface | No provisioning or session-creation tool exists in the runtime path. |
| Reproducible installation | The payment code is the `toolkit` binary, built from a locked dependency graph. Operators requiring full reproducibility should verify the binary they install through their release process. |
| Local configuration | The config writer creates a `0700` directory and `0600` file atomically. Loading checks ownership, type, symlinks, and writable parents. Runtime path resolution uses the OS account and ignores `HOME`, `AGENTS_PAY_CONFIG`, and `X402_POLICY_FILE`; file resource values win over environment fallbacks. |
| Documentation checks | This table names enforcing code; the toolkit's regression tests assert behaviour; `preflight` checks deployed state. |

### Limits of the controls

- **Cumulative ceiling.** A per-session budget plus human-only session
  creation bounds spend per session and forces a human into the loop between
  sessions. It is not a *service-side* cumulative ceiling across sessions — that
  requires support in AgentCore Payments, outside a skill's reach. An operator who
  approves ten sessions has authorized ten budgets.
- **Dependency pinning.** The payment code is the `toolkit` binary, built from a locked dependency graph, so there is no skill-side resolution to pin. Verify the binary you install through your release process.
- **Changed offers (L-TERMS).** Idempotency holds while the merchant's terms stay the same. A retry that meets a different amount or another allowed recipient derives a different token and may be authorized again, still within the per-payment cap, the recipient allowlist and the session budget. Exactly-once purchasing across changed challenges is not provided.
- **Blocking DNS.** A stalled DNS lookup can exceed X402_TIMEOUT_SECONDS; no connection, authorization or proof send starts after the deadline.
- **Platform certificate store only.** The payment runtime ignores `SSL_CERT_FILE`, `SSL_CERT_DIR` and `AWS_CA_BUNDLE`, so TLS-inspecting proxies break it; on macOS it refuses while `SSL_CERT_FILE` or `SSL_CERT_DIR` is set.
- **DNS rebinding.** IP pinning closes the common TOCTOU window by
  dialing the vetted address. A network-level egress allowlist remains the
  stronger control for a payment-capable agent, and is recommended, not replaced.

  Implementation note worth preserving: the pin **must not** be implemented by
  temporarily replacing `socket.getaddrinfo`. That global is shared, so two
  concurrent fetches can restore or observe each other's state and a request can
  end up resolving *unpinned*, silently reopening the window. The pin therefore
  lives in the connection pool's network backend, which is per-transport.
  `test_pin_is_not_implemented_by_patching_a_global` guards the regression.
- **Same-identity code execution.** A process with the runtime's OS identity and
  AWS credentials can modify owner-writable files or call the payment SDK directly.
  Restrict the model to registered tools, or place the signer and policy behind a
  separate process, container, OS account, or IAM role.

## One config file, with a fixed runtime path

Resource identifiers and the payment policy live in one operator-owned file,
`~/.agents-pay/config.json` (`0600`, in a `0700` directory, written atomically):

```json
{
  "resources": { "payment_session_id": "ps-...", "payment_manager_arn": "arn:...", ... },
  "policy":    { "max_per_payment_usd": "0.05", "allowed_networks": ["eip155:84532"], ... }
}
```

They were separate at first, which forced the operator to hand-copy identifiers
between steps. Merging them removed that, but it also bought a control worth
naming.

**The session ID is a spending credential** — it names the budget being drawn
down. `runtime_config_path()` resolves `.agents-pay/config.json` from the OS
account and ignores `HOME`, `AGENTS_PAY_CONFIG`, and `X402_POLICY_FILE`.
`resolve_resource()` then reads the **config file first and the environment
second**, deliberately reversing the usual precedence.

Containers and Lambda may inject resource identifiers when the fixed policy file
omits them. The environment cannot select a replacement policy file. File modes
protect against other principals; unrestricted code already running as the owner
requires a stronger process, container, OS account, or IAM boundary.
*Tests: `test_runtime_ignores_environment_selected_policy_file`,
`test_runtime_config_path_ignores_home_environment`, and
`test_config_file_beats_environment`.*

## Recipient validation

The normal mode requires the payee (`payTo`) named by the publisher to match an
operator-approved entry in `allowed_recipients`. Missing or empty recipient
policy denies every payment. An operator may instead set
`allow_any_recipient: true`, explicitly delegating beneficiary choice to the
publisher. The modes are mutually exclusive, and non-boolean values fail closed.

Open-recipient mode does not relax scheme, network, exact asset, origin/resource,
per-payment, or cumulative session controls. It does remove the deterministic
beneficiary boundary, so it is a deliberate high-risk operator choice.

`RecipientValidationTests` covers unknown-recipient refusal, missing-allowlist
denial, case-insensitive matching, open-recipient acceptance, mode conflicts,
malformed values, and retention of the other policy checks.

## Origins are optional

HTTPS only, internal-address rejection, manual redirect handling, DNS-rebinding
protection, timeouts, and a strict byte limit are enforced unconditionally. An
approved domain egress policy is an additional protection for payment-capable
agents, but is not required.

So `allowed_origins` is **optional**: unset means any public HTTPS site, and a
deployment with a known merchant set can still pin it. *Test: `OptionalOriginTests`.*

## Two ceilings, not a duplicate

A reasonable objection: the session already has a budget, so why does the policy
also carry `max_per_payment_usd`?

Because they bound different things:

| Bound | Scope | Set by |
|---|---|---|
| Session budget | **Cumulative** — total spend before a human must re-approve | `new-session`, typed approval |
| `max_per_payment_usd` | **Per transaction** | the policy section |

With only the session budget, a hostile merchant returns one challenge for the
entire remaining balance and drains it in a single payment. A positive, trusted
maximum for each payment makes the per-payment bound necessary, not redundant. A
missing per-payment ceiling is a refusal, never an unbounded payment.
*Test: `test_missing_per_payment_cap_refuses_rather_than_paying_unbounded`.*

## Role separation is the real boundary

The controls in this skill are meaningful only if the IAM separation described in
the [official guide](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/payments-iam-roles.html)
holds in the account:

| Role | Holds | Must NOT hold |
|---|---|---|
| **ManagementRole** — the human | Create/Get/Delete instrument and session | `ProcessPayment` (explicit `Deny`) |
| **ProcessPaymentRole** — the agent | `ProcessPayment`, Get instrument/balance/session | Any session **write** |

**The agent must have neither the ManagementRole nor the ability to run
`"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin`.** If it has both, it can mint a fresh budget whenever it
exhausts one and the per-session cap bounds nothing.

The TTY requirement on `new-session` is defence in depth, not the boundary. An
agent running as the operator's own user in an interactive terminal could still
drive it — IAM is what actually stops that, which is why the runtime role must
exclude `CreatePaymentSession`.

## Validate one document, sign another

The gate can validate a challenge perfectly and still be useless if the
*signer* is handed something else.

An x402 challenge may carry several `accepts` entries, and the terms can appear
both in the `payment-required` header and in the body. If trusted code validates
one entry but forwards the publisher's raw response to
`generate_payment_header`, the SDK may settle terms the policy never saw:

- **Ordering.** `accepts[0]` = $50 to an attacker, `accepts[1]` = $0.10 to the
  merchant. The gate approves entry 1 and reports $0.10; the signer, given both,
  settles entry 0.
- **Header/body split.** A compliant header alongside a hostile body. The gate
  reads the header and approves; the signer reads the body and authorizes a
  larger amount on a different chain. The receipt then *lies* to the operator.

Both are silent: the returned receipt reflects the approved entry, not what was
signed. A test that asserts only on the gate's return value can pass while the
exploit still works.

The fix is structural: the policy code rejects conflicting amount aliases and builds
a version-canonical vetted entry. The fetch code reserializes that single entry into
a fresh minimal challenge (`{"x402Version": ..., "accepts": [vetted]}`) and passes
only that, with a synthetic `content-type` header. The toolkit's signer-input tests assert on
the object handed to the signer, not only the gate's return value.

**Rule for anyone changing the payment code:** the signer must receive data that
trusted code constructed, never data a publisher supplied.

## Verification

```bash
"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin show-config      # confirm 0600 + contents
"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin preflight        # wiring + secret exposure
```

The reproducible evidence is: the toolkit's regression tests passing, a `show-config` transcript,
and a refusal captured against a live endpoint whose recipient is deliberately
absent from the allowlist.

## Residual risks the operator owns

- **The policy is only as tight as its allowlists.** A wildcard-ish policy (many
  recipients, high ceiling) is permitted by the code and is the operator's risk.
- **Testnet first.** Defaults target Base Sepolia. Moving to mainnet means real
  money; re-check the ceiling before switching `--network`.
- **Wallet funding is a cap of last resort.** Fund the wallet with only what the
  agent may plausibly spend. It is the final backstop if every other control fails.
- **Host or runtime-role compromise is out of scope.** An attacker who can execute
  arbitrary code as the runtime identity, change the policy as its owner, read
  process memory, or call `ProcessPayment` directly can bypass this local gate.
