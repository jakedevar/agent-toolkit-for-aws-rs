# OpenClaw Setup

Plugin references are plain files; the host does not substitute their variables.
Get the absolute installed plugin root from the loaded parent skill (its
`${CLAUDE_PLUGIN_ROOT}` token is substituted there). In your own terminal, set
`export CLAUDE_PLUGIN_ROOT='/absolute/installed/plugin'` to that actual path.
The guarded commands below refuse an unset or empty root; never use PATH.

OpenClaw uses the `@aws/aws-agents-pay` package from ClawHub. Its canonical
source lives at
`plugins/aws-agents/skills/agents-pay/packages/openclaw/` in this repository.

For background on AgentCore Payments, see the
[getting started guide](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/payments-getting-started.html).

## Step 0: Choose your user identity

Pick a stable `userId` before provisioning any payment resources. This identity
must be used consistently across `create-instrument`, `new-session`, and your
OpenClaw plugin config — a mismatch means the session cannot spend the
instrument.

```bash
export PAYMENT_USER_ID="my-openclaw-agent"
```

Pass `--user-id $PAYMENT_USER_ID` to `init-config`, `create-instrument` and `new-session`, or run `init-config`
first (which generates and stores one in `~/.agents-pay/config.json` for you).

## Install

```bash
openclaw plugins install clawhub:@aws/aws-agents-pay
```

The plugin can load before payment resources are configured so the bundled
setup skill remains discoverable. Both payment tools still fail closed until a
complete trusted configuration exists.

## Provision the AgentCore project

Create an AgentCore project before adding and deploying the payment resources:

```bash
npm install -g @aws/agentcore
PROJECT_NAME=openclaw-payments
agentcore create --project-name "$PROJECT_NAME" --no-agent
cd "$PROJECT_NAME"
agentcore add payment-manager
agentcore add payment-connector
agentcore deploy
export AGENTCORE_PROJECT_DIR="$PWD"
```

Run both `agentcore add` commands without flags so credentials remain in the
interactive terminal. Run the setup wizard in that same human-controlled
terminal so `AGENTCORE_PROJECT_DIR` remains set.

On Windows outside Git Bash or MSYS, use `bin\toolkit.exe` in place of
`./bin/toolkit` and the corresponding Windows path for the installed extension.

## Quick Setup (Recommended)

After deploying the AgentCore project, the human runs `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay admin setup-openclaw` in their own terminal using the bundled dispatcher. The interactive wizard creates the payment instrument and session, then generates the OpenClaw configuration:

```bash
cd ~/.openclaw/extensions/aws-agents-pay
./bin/toolkit agents-pay admin setup-openclaw \
  --project-dir "$AGENTCORE_PROJECT_DIR"
```

The wizard walks through: user identity → network → recipients → spend limits →
instrument creation → delegation/funding → session approval → generates your
OpenClaw config JSON ready to paste.

## Manual Setup

Add to your OpenClaw config (`~/.openclaw/openclaw.json` or via
`openclaw config`).

If payment infrastructure or a session does not exist, stop the LLM workflow.
Open a separate terminal and run the human setup from the bundled skill:

```bash
cd ~/.openclaw/extensions/aws-agents-pay
./bin/toolkit agents-pay admin init-config \
  --max-per-payment-usd 0.05 \
  --network eip155:84532 \
  --recipient 0xMerchantWalletAddress
./bin/toolkit agents-pay admin create-instrument --email you@example.com
./bin/toolkit agents-pay admin new-session \
  --budget 1.00 \
  --expiry-minutes 60
./bin/toolkit agents-pay admin preflight
```

The two `agentcore add` commands must run without flags so credentials remain in
the interactive terminal. Do not paste credentials, command output, deployed
state, or generated identifiers into an LLM conversation.

Complete delegation and testnet funding before creating the session. Then edit
the OpenClaw config locally in the same terminal or a local editor. Explicitly
allow the installed plugin:

> **Important:** The `userId` below must match the identity used in
> `create-instrument` and `new-session`. A mismatch means the session cannot
> spend the instrument.

```json
{
  "plugins": {
    "allow": ["aws-agents-pay"],
    "entries": {
      "aws-agents-pay": {
        "enabled": true,
        "config": {
          "region": "us-east-1",
          "paymentManagerArn": "arn:aws:bedrock-agentcore:us-east-1:ACCOUNT:payment-manager/NAME",
          "paymentInstrumentId": "payment-instrument-XXXX",
          "payment_session_id": "payment-session-XXXX",
          "userId": "your-user-id",
          "networkPreferences": ["eip155:84532"],
          "allowedOrigins": ["https://merchant.example"],
          "allowedRecipients": ["0xMerchantWalletAddress"],
          "allowedAssetsByNetwork": {
            "eip155:84532": [
              "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
            ]
          },
          "maxPaymentAmountAtomic": "100000"
        }
      }
    }
  }
}
```

The `paymentManagerArn`, `paymentInstrumentId`, `payment_session_id`, and
`userId` come from the human-run administrative setup. Configure origins,
recipients, networks, and exact asset contracts from values approved out of
band. To deliberately allow publisher-selected beneficiaries, replace
`allowedRecipients` with `"allowAnyRecipient": true`; the plugin rejects both
fields together. The example `maxPaymentAmountAtomic` value is `100000` (0.10
USDC with 6 decimals), and no default is supplied.

## How it works

OpenClaw executes payments through this TypeScript plugin using
`get_paid_content`. Policy validation, SSRF controls and merchant replay stay
in TypeScript. Other hosts use `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" agents-pay fetch`; enable only one path.

The fixed binary table selects the package's platform-specific Rust toolkit;
unsupported platforms fail closed. The plugin uses a bounded, no-shell bridge.
Rust builds the ProcessPayment payload from the selected offer. After a proof is
obtained, the complete result's text and details pass through a Rust known-secret
scrub after replay. A matching opt-in body is redacted; a metadata or error hit,
invalid result, or failed check yields a fixed refusal. The scrub covers known
secrets and supported encodings, not arbitrary transformations. The bridge zeros
owned Buffers on success and failure; immutable strings and parsed objects cannot
be reliably zeroized, so their references are dropped and the proof object cleared.

The default response withholds the body; `returnBody: true` opts in to an
untrusted body capped at 10,240 UTF-16 code units, which can contain prompt injection.

The plugin exposes two scoped OpenClaw runtime tools only:

| Tool | What it does | Permission |
|---|---|---|
| `get_payment_session_status` | Check session budget/expiry | Read-only |
| `get_paid_content` | Pay for an approved x402 URL and return metadata; body is opt-in | Spend (within pre-approved session budget) |

### Security boundaries

- **No general shell access.** The agent cannot choose a command, script, path,
  or environment variable. The plugin starts only its fixed platform binary
  with `shell: false`, bounded JSON, a timeout, and output limits.
- **No setup at runtime.** Payment Manager, connector, credential provider,
  instrument, and session creation happen outside the model-visible runtime.
- **No credential exposure.** Wallet provider secrets never appear in the tool
  interface. Rotate any provider credential previously entered through chat.
- **No replacement sessions.** The runtime cannot mint a fresh session. If the
  configured session expires or drains, create a new one through the trusted
  management path and update config.
- **Final output check.** Known proof and session secrets are checked in both
  result channels after replay; failures yield the fixed refusal.
- **Trusted challenge policy.** Runtime code selects an approved offer instead
  of trusting `accepts[0]`, and enforces HTTPS, public destinations, no
  redirects, configured origin and network, exact asset, positive bounded
  amount, approved recipient, and matching resource before `ProcessPayment`.
- **x402 v2 only.** The paid replay uses the required `PAYMENT-SIGNATURE`
  header. v1 challenges fail closed.
- **Stable retries.** The AgentCore client token binds the session, resource,
  network, asset, recipient, and amount while excluding publisher-controlled
  nonces.
- **Publisher content isolation.** By default `get_paid_content` returns status,
  content type, byte count, and SHA-256 hash, with no body. Opting in to
  `returnBody` exposes untrusted content to the model.
- **Budget enforcement.** The AgentCore service enforces `maxSpendAmount` and
  `expiryTimeInMinutes` server-side regardless of what the agent requests.

## Publish updates

See
`plugins/aws-agents/skills/agents-pay/packages/openclaw/PUBLISHING.md` in the
source repository for the build and publication workflow.
