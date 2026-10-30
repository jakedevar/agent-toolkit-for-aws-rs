# agents-pay operator guide

Operator-facing setup and role guidance. **Read the IAM section before deploying.**

(Kept in `references/` rather than as a skill-level README: in this repo READMEs live
at plugin level, and every skill's operator material goes in `references/`.)

Every payment control is enforced in code, from an operator-owned config file,
before any signing. Instructions to a model are not access controls.

---

## Before you deploy: IAM role separation

**This is the most important section. Read it first.**

AgentCore Payments defines a four-role model. Follow the official guide:
**<https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/payments-iam-roles.html>**

| Role | Purpose | Who uses it |
|---|---|---|
| **ControlPlaneRole** | Payment managers, connectors, credential providers | Administrator |
| **ManagementRole** | Payment instruments and sessions. Carries an explicit `Deny` on `ProcessPayment` | **A human**, running this skill's `toolkit agents-pay admin` commands |
| **ProcessPaymentRole** | Executes payments against an existing session. **No session writes** | **The agent runtime** |
| **ResourceRetrievalRole** | Service role AgentCore assumes to retrieve credentials | `bedrock-agentcore.amazonaws.com` |

### The rule

> **The agent must have neither the ManagementRole nor the ability to run
> `toolkit agents-pay admin`.**

A human uses the **ManagementRole** to create the payment instrument and to
authorize each spending session. The agent runs with the **ProcessPaymentRole**,
which can spend an already-approved session but cannot create one.

If the agent gets both, it can mint itself a fresh budget whenever it exhausts one,
and the per-session limit stops bounding anything. The AWS guide is explicit:

> "Do not include PaymentSession *write* permissions (for example,
> `CreatePaymentSession`) and `ProcessPayment` in the same role, or the caller can
> bypass payment limits by creating new sessions with elevated budgets."

and on why the split exists at all:

> "Separating payment management from payment execution prevents a single
> compromised identity from both creating sessions with unlimited budgets and
> executing payments against those sessions."

### How to enforce it

1. **IAM is the real boundary.** The runtime role must exclude
   `CreatePaymentSession` and every `Create*` setup action. Verify:

   ```bash
   aws iam get-role-policy --role-name <ProcessPaymentRole> --policy-name <Policy> \
     | grep -c CreatePaymentSession        # must be 0
   ```

   A runtime `AccessDeniedException` on `CreatePaymentSession` is **the control
   working**. Do not "fix" it by granting the permission.

2. **Keep the admin CLI out of the agent's reach.** Put it on a workstation or a
   separate host, not in the runtime image.

3. **The TTY gate is defence in depth, not the boundary.** `new-session` refuses
   to run without an interactive terminal and has no `--yes` flag, so a headless
   agent cannot drive it. But an agent running as your user in an interactive
   terminal could. IAM is what actually stops that.

### Which role runs which command

| Step | Command | Role |
|---|---|---|
| 1 | `agentcore add payment-manager` / `payment-connector` / `deploy` | ControlPlaneRole |
| 2 | `toolkit agents-pay admin init-config` | none — writes a local file |
| 3 | `toolkit agents-pay admin create-instrument` | **ManagementRole** |
| 4 | `toolkit agents-pay admin new-session` | **ManagementRole** |
| 5 | agent runs `toolkit agents-pay fetch` | **ProcessPaymentRole** |

---

## Quick start

```bash
# 0. Prerequisites: `toolkit` on PATH (a release build)
toolkit help
npm install -g @aws/agentcore

# 1. Provision (ControlPlaneRole; only step that touches provider secrets)
agentcore add payment-manager                     # NO FLAGS — interactive wizard
agentcore add payment-connector                   # NO FLAGS — interactive wizard
agentcore deploy

# 2. Write the config with at least one approved payee
toolkit agents-pay admin init-config \
  --max-per-payment-usd 0.05 \
  --recipient 0xMerchantWalletAddress

# 3. Create the wallet (ManagementRole) — payer identity comes from the config
toolkit agents-pay admin create-instrument --email you@example.com

# 3a/3b. THE END USER now delegates in a browser and funds the wallet with USDC.
#        Both belong to the wallet, and both must happen BEFORE step 4 —
#        sessions are time-bounded, so minting one first just burns the clock.
#        Step 3 prints the delegation URL and the wallet address.

# 4. Authorize a budget (ManagementRole; interactive — you type "approve")
toolkit agents-pay admin new-session --budget 1.00

# 5. Verify, then let your agent run `toolkit agents-pay fetch`
toolkit agents-pay admin preflight
```

### The interactive approval, concretely

Step 4 is an ordinary command that prompts. It is not a web console or a callback —
you run it in your shell and type one word:

```text
$ toolkit agents-pay admin new-session --budget 1.00

About to create a payment session:
  manager   : arn:aws:bedrock-agentcore:us-west-2:1234:payment-manager/pm-abc
  payer     : agents-pay-0e2ed33d1100
  budget    : 1.00 USD (hard cap for the whole session)
  expires in: 60 minutes
Type 'approve' to continue: approve        <-- you type this

Payment session created: ps-7f3a9c2e
Recorded in            : ~/.agents-pay/config.json  (nothing to copy by hand)
```

Anything other than `approve` aborts and no session is created. If stdin is not a
terminal — an agent shelling out, CI, or a pipe — the command refuses outright
rather than prompting:

```text
Refusing to create a payment session without an interactive terminal.
Session creation requires a human typing 'approve' at a TTY. There is no
non-interactive mode: that would let an automated caller mint spending budget.
```

Piping `approve` in does not work either.

### OpenClaw

If the agent runs on OpenClaw, install the published plugin instead of wiring the
`toolkit agents-pay fetch` command by hand:

```bash
openclaw plugins install clawhub:@aws/aws-agents-pay
openclaw plugins inspect aws-agents-pay               # confirm what it registers
```

OpenClaw executes payments through the TypeScript plugin's `get_paid_content`
tool. Other hosts use the equivalent `toolkit agents-pay fetch` command. Choose exactly one
runtime path; enabling both would make policy enforcement path-dependent. The
production plugin uses a fixed platform binary and Rust builds the payment
payload. After a proof, its final output check covers text and details; it
returns a fixed refusal if the result cannot be checked.

Two things to confirm before you trust the OpenClaw runtime:

1. **No tool takes a wallet secret or provider key as a parameter.** Credentials
   belong in the environment or the `agentcore` wizard, never in a model-visible
   schema — otherwise they land in transcripts, traces, and logs.
2. **No model-callable tool creates a payment session.** If the model can mint
   budget, the per-session cap bounds nothing.

If either is wrong, disable the plugin before switching to the
`toolkit agents-pay fetch` path.

### If you use the Strands plugin or LangGraph middleware instead

Those integrations settle payments inside the framework, so **none of this skill's
controls apply** — no per-payment ceiling, no origin vetting, no derived idempotency
token, and `auto_session=True` requires `CreatePaymentSession` on the runtime role,
which is the arrangement the IAM guide warns against.

They are a reasonable choice for a prototype or an agent whose whole tool surface you
own. For an agent spending your money against the open web, route payments through
`toolkit agents-pay fetch` instead. Either way, **do not enable both** — the model would choose which one settles
a given `402`, and the gate becomes advisory.

See "The gate only covers what routes through it" in
[`security-model.md`](security-model.md) for the full comparison.

### Registering the tools

There is nothing to import. Expose `toolkit agents-pay fetch` and
`toolkit agents-pay fetch --status` through your host's shell or tool mechanism, or
wrap them in a tool that runs the command as a subprocess and returns its JSON. See
"Known limits" in `SKILL.md` for the import API that no longer exists.

---

## What the agent can and cannot do

| Capability | Command | Causes spending? | Can raise a limit? |
|---|---|---|---|
| Pay and fetch | `toolkit agents-pay fetch URL` | Yes, within both ceilings | **No** |
| Check remaining budget | `toolkit agents-pay fetch --status` | No — read-only | **No** |
| Pay for a browser navigation | `toolkit agents-pay fetch --browser-handle URL` | Yes, within both ceilings | **No** |

Out of reach entirely: session creation, infrastructure provisioning, provider
credentials, and signed payment proofs.

---

## Configuration

One file, `~/.agents-pay/config.json` — mode `0600`, in a `0700` directory, written
atomically. It holds **no secrets**: only resource identifiers and limits.

```json
{
  "resources": {
    "payment_manager_arn": "arn:aws:bedrock-agentcore:...:payment-manager/pm-1",
    "payment_instrument_id": "pi-...",
    "payment_session_id": "ps-...",
    "user_id": "agents-pay-0e2ed33d1100",
    "region": "us-west-2"
  },
  "policy": {
    "max_per_payment_usd": "0.05",
    "allowed_networks": ["eip155:84532"],
    "allowed_assets": { "eip155:84532": ["0x036CbD53842c5426634e7929541eC2318f3dCF7e"] },
    "allowed_schemes": ["exact"]
  }
}
```

`create-instrument` and `new-session` write their results here, so nothing needs
copying by hand. The sanctioned runtime reads this file and exposes no config-write
operation.

**Single tenant.** One installation is one payer. `init-config` generates the
`user_id` once and every later step reads it from the config, so you never retype it
— and cannot accidentally mismatch it between `create-instrument` and `new-session`,
which would produce a session unable to spend the instrument. It is random rather
than derived from your login or hostname, since it reaches the payments API and the
wallet's linked accounts. Pass `--user-id` to override if you really need to. For
multi-tenant runtime isolation, use a separate OS account or isolated runtime for
each tenant. `AGENTS_PAY_CONFIG` selects paths for human-run admin commands only.

**The runtime config path is fixed.** It resolves `.agents-pay/config.json` from
the operating-system account and ignores `HOME`, `AGENTS_PAY_CONFIG`, and
`X402_POLICY_FILE`. Resource identifiers in that file take precedence over their
environment fallbacks. Containers and Lambda may still inject identifiers by
environment when the file omits them, but the policy itself must come from the
fixed file.

File modes protect against other OS principals, not arbitrary code already running
as the owner. A harness that grants unrestricted same-user shell access must isolate
the signer and config with a separate OS identity, container, or AWS role boundary.

### Browsing the open web

By default the agent may fetch **any public HTTPS site**. `allowed_origins` is
optional; set it only when you want to pin a known merchant set. Payment normally
requires an approved recipient: the challenge `payTo` must appear in
`allowed_recipients`, or trusted code refuses before signing.

For a new merchant, inspect the unpaid 402 challenge, review `payTo`, amount,
asset, and network, then re-run `init-config --force --recipient <payTo>` with the
intended allowlist. Keep the approval in Python policy, not in a prompt.

For an explicit open-recipient policy, run `init-config` with
`--allow-any-recipient` instead of any `--recipient` flags. This delegates
beneficiary choice to the publisher. Scheme, network, exact asset, origin/resource,
per-payment ceiling, and cumulative session budget remain enforced. Never set
`allow_any_recipient` and `allowed_recipients` together; trusted code rejects that
ambiguous policy.

### The two ceilings

| Bound | Scope | Set by |
|---|---|---|
| Session budget | **Cumulative** — total before a human re-approves | `new-session` |
| `max_per_payment_usd` | **Per transaction** | the policy section |

Both are needed. With only the session budget, one hostile challenge for the entire
remaining balance drains it in a single payment. *(A per-payment bound arguably
belongs in the service-side session budget; until AgentCore Payments offers one,
the skill enforces it locally.)*

---

## Security model

The model is treated as **inside** the threat model — the design must hold when
prompt injection succeeds. In summary:

- **Payment decisions are made in code**, from the config file, before signing:
  scheme, network, exact asset contract, recipient, and per-payment ceiling.
- **Only the vetted challenge entry reaches the signer.** The publisher's raw
  response is not forwarded. The `resource` object is included only after
  URL-binding validation (origin+path must match the requested URL); arbitrary
  publisher-injected fields are stripped.
- **Signed proofs never reach the model.** For browser flows, the model gets an
  opaque single-use handle bound to one origin and path, expiring in 90 seconds.
- **Fetching is hardened**: HTTPS only; loopback, private, link-local, metadata,
  multicast, reserved, CGNAT and IPv4-mapped addresses refused; the vetted IP is
  pinned to close the DNS-rebinding window; redirects are never followed;
  compression is refused and the body is capped.
- **Retries are idempotent.** The token is derived, not random, and excludes the
  publisher's nonce, so a retry after a lost response replays one authorization
  instead of paying twice.
- **Paid content is untrusted input** — by default, only content type, byte
  count, and SHA-256 hash are returned. When the operator sets `return_body: true`
  in the config file, body content is returned bounded (10 KiB cap) and marked
  `"untrusted": true`. Instructions inside it are data, never commands.

Full threat model and enforcement table:
[`security-model.md`](security-model.md).

---

## Files

| Path | Role |
|---|---|
| `SKILL.md` | The skill the agent loads |
| [`security-model.md`](security-model.md) | Threat model and enforcement table |
| [`setup.md`](setup.md) | Full provisioning walkthrough and IAM policies |
| [`troubleshooting.md`](troubleshooting.md) | Refusal and failure diagnosis |

The payment code is the `toolkit` binary: `toolkit agents-pay fetch` for the agent and
`toolkit agents-pay admin` for the human.

---

## Operational notes

- **Start on testnet.** Defaults target Base Sepolia. Moving to mainnet means real
  money — re-check the ceiling and the recipient mode first.
- **Fund the wallet with only what the agent may plausibly spend.** Wallet balance
  is the final backstop if every other control fails.
- **Keep sessions short** (≤60 minutes) and budgets small.
- **Alarm on `CreatePaymentSession` by the runtime principal.** It should be
  impossible, so any occurrence means the IAM split has regressed.
- Confirm CloudTrail is recording `bedrock-agentcore` calls, especially
  `ProcessPayment`.
