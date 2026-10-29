# agents-pay (x402 payments)

Port of `plugins/aws-agents/skills/agents-pay` (#4410). The design, with every
requirement and test ID used here, is
[x402-payments-design.md](x402-payments-design.md). S7 completes this page and
the skill cutover. This section records slice S5 (#5352).

## `toolkit agents-pay fetch`

```
toolkit agents-pay fetch [URL] [--status] [--browser-handle URL]
                         [--method GET|HEAD] [--purchase-id ID]
```

It replaces `x402_fetch_cli.py`. It prints one JSON document on stdout,
formatted like Python `json.dumps` (`, ` and `: ` separators, ASCII only).

| Input | Result | Exit |
| --- | --- | --- |
| `URL` | fetch it; pay only when it returns 402 and the policy approves | 0 paid or no payment needed, 2 when `refused` |
| `--status` (wins over a URL) | `{"usable", "status", "budget_usd"?, "spent_usd"?, "next_step"?}` from `GetPaymentSession` (Q19) | 0 usable, else 2 |
| `--browser-handle URL` (wins over the positional) | pay, keep the proof in process, return `x402h_…` and a receipt | 0, or 2 when refused |
| no URL and no flag | the usage line and `Give a URL, or --status.` on stderr | 1 |
| `--method` other than `GET`/`HEAD`, an unknown flag, a second positional, `-` | usage error | 2 |

**Payment support needs a release build (D32).** Payment code that
constructs live pieces (the OS-account config, credentials, system DNS,
payment TLS roots, transports) is in `agents_pay/live.rs`, compiled only with
the Cargo feature `agents-pay-live`. No verification build (`make test`,
`make test-sdk`, `make clippy-sdk`, the daemon `scoped_test`) enables it, and
every test target refuses to compile with it (G1a). Without it every route
prints `This toolkit build has no payment support; install a release build of
toolkit.` and exits 2, after argv parsing and before any file, variable,
credential or network access. A source gate holds that order
(`t_ac3_noread_*` in `tests/agents_pay_gate.rs`, #5376): the no-feature arm
of `Deps::for_process` must be exactly the D32 error, and the route functions
(`fetch_command`, `parse`, `emit`) may evaluate only an allowlist of paths,
methods and macros (argv, the credential-flag check, the D32 call, the JSON
document), so an added read fails the gate. `make clippy-live`
lints the live module (library and binaries only). G4 also scans the inline
`#[cfg(test)]` modules of the payment sources (#5377); the D32 unit test in
`deps.rs` is the one allowed naming of `Deps::for_process`.

## Flow (S5)

- Order: config, R-ORIGIN (with deadline admission around each lookup), the
  method gate, the probe, the challenge, the policy gate, then exactly one
  `ProcessPayment` with the derived token (R-PP-ONCE), then R-PP-VERIFY. Up to
  `X402_MAX_PAYMENT_ATTEMPTS` replays reuse the same header bytes, with a 1 s
  pause between them (D13). A 402 on a replay is never parsed.
- R-PP-VERIFY: `authorization.to` (case-insensitive), `value` (exact
  canonical amount) and a future `validBefore`; otherwise `Signed payment does
  not match the approved terms.` and nothing is sent. A future `validAfter` is
  waited for when it is at most 15 s away and the authorization is still
  valid then; otherwise `Signed payment is not valid within the bounded retry
  window.`
- R-HDR: `PAYMENT-SIGNATURE` (v2, with the vetted entry as `accepted` and the
  sanitized resource) or `X-PAYMENT` (v1), base64 of compact JSON.
- R-SCRUB: the live secrets are the header value, the signature, the
  idempotency token and the session id (without its public
  `payment-session-` head); secrets under 8 bytes are not searched. Any piece
  of a secret of 8 bytes or more, at any offset, ignoring ASCII case, as
  given or in its escaped and JSON spellings, is a hit
  (`safe_text::KnownSecrets`). In an opt-in body each hit span becomes
  `[REDACTED]`, before the body is cut to 10240 characters, adding
  `"redacted": true`. Anywhere else in the rendered document (a cut or
  lower-cased content type, a partial echo) a hit replaces the whole document
  with `RuntimeError during payment flow.` Past `MAX_MATCH_WORK` (2^26 steps:
  text bytes plus learned windows) the matcher fails closed: the body becomes
  `[REDACTED]` and a document is refused.
- R-ERR: one exhaustive class-name table (`flow::FlowError`); the merchant
  plane maps malformed HTTP to `RemoteProtocolError` (S3 reported
  `RuntimeError`).
- Trust roots: the merchant and `AgentCore` planes use `PaymentRoots`, and
  credential-provider HTTP uses the S1 provider roots (`agents_pay::wiring`,
  T-PROOTS-USE).
- G3 (live builds): a fixture manager account or a `.test`, `.example`,
  `.invalid` or `.localhost` host is refused with `Payment resources are test
  fixtures; refusing to contact live services.` A fixture host is never
  looked up.

## S5 decisions

- `--browser-handle` keeps the Python lifetime: the vault dies with the
  process, so a CLI handle cannot be redeemed (boundary B1, #5103).
  `vault::Vault::attach` is the in-process redemption.
- `--status` with an unresolvable region, a non-scalar resource or a missing
  user id reports `RuntimeError while reading session status.`
- The secret scrub (REPAIR 1, #5374, #5375) extends `safe_text`'s fragment
  rule (prefixes from 8 bytes) to every offset: each 8-byte window of each
  spelling of a secret is learned, and the text is scanned with one lookup
  per byte, so any shown piece of 8 bytes or more is found and redacted whole.
  Case folding is ASCII only; the secrets are ASCII (base64, hex, service
  ids). A secret split into pieces under 8 bytes is not found. It runs on the
  whole decoded body before the cut, and the final check runs on the rendered
  JSON. AWS credentials never reach the flow; they stay inside the signer.
- R-SEC: the route refuses credential flags by name
  (`reject_credential_arguments`); the router already refuses every unknown
  flag.
