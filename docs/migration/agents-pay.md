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
every test target refuses to compile with it (G1a). Without it every
`agents-pay fetch` form, `--status` included, prints the one document
`{"paid": false, "refused": true, "reason": "This toolkit build has no
payment support; install a release build of toolkit."}` and exits 2.

The refusal is structural (PM rulings on re-reviews #5389 and #5397: #5390,
#5391, #5400). In a build without the feature the registry entry points at
`self::no_payment::fetch_command`, in its own file module
`agents_pay/no_payment.rs`, declared `#[cfg(not(feature = "agents-pay-live"))]
mod no_payment;` with no other attribute. That file holds only two functions:
`fetch_command`, whose body is one call,
`self::refuse_without_payment_support(out)`, and that function, which prints
the string literal `super::D32_REFUSAL` and sets exit 2. Every path in the
file is absolute (`::core::result::Result::Ok(())`,
`::toolkit_core::ToolkitError`, `crate::Output`), and it has no `use`, no
macro invocation and no other item, so no module item, import, raw
identifier or textual macro can shadow a name it resolves. Nothing else
runs: argv is not parsed, `Deps::for_process` does not exist (it is
live-only), and no helper, file, variable, credential or terminal is
reached. The live handler (argv, then the process world) is the
`#[cfg(feature = "agents-pay-live")]` `fetch_command` in `mod.rs`.

Behaviour change: in a build without the feature, D32 now wins over the
handler's own usage errors and over `--status`. `toolkit agents-pay fetch`
with no URL, a bad `--method` or a credential flag the router accepts gets
the D32 document and exit 2, not the usage text (exit 1) or a usage error;
`--status` gets the same refusal, not `{"usable": false, ...}`. This is
"refuse before reading anything", and it departs from the design's G1b text
(D32 "after argv parsing"). The generic router still rejects malformed argv
(an unknown flag, a second positional, a `-` value) before any handler, and
a live build keeps every row of the table above.

A source gate pins that surface (`t_ac3_noread_*` in
`tests/agents_pay_gate.rs`): the whole token stream of `no_payment.rs`
exactly, so any change to that file fails it; the `mod no_payment;`
declaration and the two cfg-split `handler:` entries of the registry exactly;
`D32_REFUSAL` a plain `&str` literal equal to the rendered D32 document; and
no top-level binding in `mod.rs` that could shadow `no_payment`,
`D32_REFUSAL` or a prelude name (`Ok`, `Err`, `Some`, `None`, `Result`,
`Option`; no glob import, no item macro). Every gate in that file compares
identifiers after unraw (`r#Ok` is `Ok`), and under `src/agents_pay/` it
forbids every raw identifier, every `macro_rules!` definition, every `use ..
as` rename (so no aliased std/core macro), `extern crate`, and the
`link_section`, `used`, `no_mangle`, `export_name`, `macro_use` and
`macro_export` attributes, raw spellings included. Across the crate's
`src/` it forbids `extern crate`, and the crate manifest may not rename a
dependency to `core`, `std`, `alloc` or `toolkit_core`, so the absolute
paths resolve to the real crates. It is not an allowlist over general code:
the reviewers' helper (#5390), `matches!` shadow (#5391) and raw `fn r#Ok`
(#5400) mutations are committed negative fixtures. `make clippy-live` lints
the live module (library and binaries only). G4 also scans the inline
`#[cfg(test)]` modules of the payment sources (#5377), with no allowance: no
default-build test can name `Deps::for_process`.

Repair 4 (PM ruling on re-review #5419) adds two checks. First, the registry
pin is bound to the entries that are compiled (#5421). `registry::SLICES`
lists `crate::agents_pay::SLICE` as the one payment table, and `mod.rs` has
one top-level `const SLICE` with docs only. Its `commands` is `&[..]` holding
one `crate::CommandSpec` literal, for `agents-pay fetch`. Both literals name
every field in full, with no shorthand and no `..` struct update. Their only
attributes are the two pinned `cfg`s on the two `handler` fields. A pinned
entry kept in dead code (`#[cfg(any())]`), or a handler or table taken from
another module, fails. Second, nothing may run before `main` (#5423).
`#[global_allocator]`, `global_asm!`, `asm!` (or a `::asm` path),
`naked_asm!` and `naked` are banned anywhere in the crate's `src/`. The gate
also checks two manifest settings: the crate manifest keeps exactly
`[lints] workspace = true`, and the workspace keeps `unsafe_code = "forbid"`
in `[workspace.lints.rust]` as its only `unsafe_code` setting. `forbid`
cannot be lowered by a source attribute. Manifest lines are read with
comments and the whitespace outside strings removed, so
`[dependencies . core]` is `[dependencies.core]`. The reviewer's mutations
are committed negative fixtures.

### What each layer covers

The no-read-before-D32 property rests on two independent layers.

1. **The static source gate** (`t_ac3_noread_*`) guards against honest
   mistakes. It is not an adversary-proof sandbox: whoever can edit the
   source can edit the gate. It guarantees three things:
   - the default-build route is exactly the pinned `no_payment.rs`;
   - that route is the one the registry compiles;
   - no source under `src/agents_pay/` defines a macro, renames an import,
     uses a raw identifier or an `.init_array`-style attribute, and no
     source in the crate holds pre-main or inline-assembly code.
2. **The runtime test**
   (`t_ac3_inh_fetch_routes_as_a_child_under_a_hostile_environment` in
   `tests/agents_pay_ac3.rs`) runs the real `toolkit agents-pay fetch` as a
   child under a hostile environment. It requires the D32 document on
   stdout, exit 2 and an empty stderr. So it catches any code on the default
   path that prints, whatever the gate missed.

A change is a blocker only if it passes both layers, or if a cheap, precise
check can close it.

### Accepted residuals

- **A trait impl on a crate type, written under `agents_pay` (#5422).**
  `toolkit_cli::run` drops the default `CallerText` (`output.caller =
  caller.clone();`) before the handler runs. So a safe `impl Drop for
  crate::safe_text::CallerText` in any payment file runs on every route,
  before D32. It needs no `unsafe`, attribute or macro. The gate does not
  ban it, because banning `Drop` would also forbid legitimate
  zeroize-on-drop code in the live module. The runtime child test caught
  the reviewer's mutation (job `69e09e65`). A Drop impl that reads but
  prints nothing would pass both layers. Review is the guard for it.
- **Code outside `src/agents_pay/` and outside `src/`.**
  - Changes to `lib.rs`, `registry.rs` or `safe_text.rs` can reroute or
    precede any command. The gate pins only that `SLICES` lists
    `crate::agents_pay::SLICE`.
  - A dependency, existing or newly added, can run its own pre-main
    constructor. A token gate over this crate's `src/` cannot see that.
  - A `build.rs` or a `RUSTFLAGS` cap on lints is also outside it.

  These need review of the manifest and the crate root.

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

## Admin commands (S6, #5462)

```
toolkit agents-pay admin init-config (--recipient ADDR ... | --allow-any-recipient)
                         [--max-per-payment-usd USD] [--network CAIP2]
                         [--origin URL ...] [--region R] [--user-id ID]
                         [--path FILE] [--force]
toolkit agents-pay admin show-config [--path FILE]
toolkit agents-pay admin create-instrument --email EMAIL [--user-id ID]
                         [--network-family ETHEREUM|SOLANA] [--manager-arn ARN]
                         [--connector-id ID] [--region R] [--path FILE]
toolkit agents-pay admin new-session --budget USD [--expiry-minutes N]
                         [--user-id ID] [--manager-arn ARN] [--region R] [--path FILE]
toolkit agents-pay admin preflight [--path FILE]
toolkit agents-pay admin setup-openclaw [--path FILE] [--project-dir DIR]
toolkit agents-build setup-payment-user --user-id ID --email EMAIL [--budget USD]
                         [--expiry-minutes N] [--network ETHEREUM|SOLANA]
                         [--manager-arn ARN] [--connector-id ID] [--region R]
```

They replace `agents_pay_admin.py` and `agents-build/scripts/setup_payment_user.py`,
keeping their flags, texts and resolution chains (R-RES-ADM). A human runs
them, never the agent. Code: `crates/toolkit-cli/src/agents_pay/admin/`.

- **D32.** In a build without `agents-pay-live` every one of these routes is
  `self::no_payment::admin_command` (`agents_pay/admin/no_payment.rs`, pinned
  token for token by `tests/agents_pay_gate.rs` like the fetch route): it
  prints `This toolkit build has no payment support; install a release build
  of toolkit.` on stderr and exits 1 before any file, variable, credential,
  deployed state or terminal is read. Both admin tables (`SLICE`,
  `agents-pay-admin`, and `BUILD_SLICE`, `agents-build`) live in
  `admin/mod.rs`, so one file holds every admin registry entry.
  `tests/agents_pay_ac3.rs` runs every route in process and as the built
  binary under the hostile environment (T-AC3-INH).
- **G1b.** Each route takes its world from one `admin::AdminDeps` (env, file
  system, terminal, admin client, randomness). `admin::fixtures` are harmless:
  `TempAdminFs` refuses and records any path outside its temporary root, and
  `FakeAdmin` accepts only the fixture account. The live world
  (`live::AdminWorld`) uses the S1 provider roots for the `AgentCore` admin
  plane, STS and credential HTTP (admin keeps R-ROOTS), and G3 refuses a
  fixture manager before anything is created.
- **The creating gate (D28, D33).** `create-instrument`, `new-session`,
  `setup-openclaw` and `setup-payment-user` check the terminal first, before
  any argument, credential or service check; a headless caller always gets the
  same refusal and exit 1. The live registry handlers go through
  `admin::run_process`, which makes that check before it builds the process
  world (`live::AdminWorld::for_process`: roots, SDK runtime, transports), so
  a headless caller gets the refusal even with broken trust roots (#5475;
  pinned by the `t_tty_5475_*` tests with a counting world factory). Then the
  arguments (D29), the STS check, the
  resolution and a summary (values through `shown_value`). The line typed at
  `Type 'approve' to continue: ` must be exactly `approve` (only the line
  terminator is removed: `approve ` with a trailing space aborts, which Python's
  `strip()` accepted). EOF aborts (D28). One approval covers the instrument and
  the optional session of `setup-payment-user`; the wizard keeps its per-step
  prompts. No route declares a `yes`, `force`, `non-interactive` or `assume`
  flag.
- **D29.** `--budget` must be a positive decimal (`CapDecimal`, no float) and
  `--expiry-minutes` and the wizard expiry must be in 15..=480, checked before
  any call or prompt. The budget sent to `CreatePaymentSession` is the
  fixed-point rendering of the parsed value (`1e1` is sent as `10`; `1.00`
  stays `1.00`). D30: the wizard prompt reads `(15-480; 480 = 8 hours)`.
- **D31.** A failed admin call prints `<Operation> failed: <code>` on stderr
  and exits 1, the code from the R-AWS-CODE allowlist or the R-ERR class name;
  the STS check prints the legacy credentials text with that code in place of
  the exception message. A call that succeeded but could not be recorded in
  the config prints the created id with the write failure.
- **R-ADM-WRITE.** `admin::write::atomic_write_0600`: final directory created
  at 0700 and `fchmod`ed through an `O_NOFOLLOW` descriptor (a symlinked
  directory is refused), a `.config-<16 hex>.tmp` created `O_EXCL` at 0600,
  written and synced, `renameat` over the target, `fchmod 0600`; the temporary
  file is removed on any failure (T-WRITE).
- **D27.** `setup-payment-user` keeps its single candidate
  (`agentcore/.cli/deployed-state.json`) but reads it with the shared parser,
  so the 0.26.x dict layout works.
- `preflight` loads the policy through the runtime's checked opener, reports
  the `PAYMENT_*` variables and the deploy-record hint, and fails on any
  variable whose upper-cased name contains `CDP_API_KEY_SECRET`,
  `WALLET_SECRET`, `APP_SECRET` or `AUTHORIZATION_PRIVATE_KEY` (names only,
  never values; T-PREFLIGHT). The SDK line reads `AgentCore payments client
  built in`.

### S6 decisions

- Admin texts name the Rust commands (`toolkit agents-pay admin init-config`,
  `toolkit agents-pay admin new-session ...`) where Python named the script
  (D23 applied to the admin plane).
- `create-instrument` and `setup-payment-user` check the network family
  (`ETHEREUM` or `SOLANA`) before any call; Python passed it to the service.
  `setup-payment-user` runs the STS check too (D33 order).
- Required arguments (`--email`, `--budget`, `--user-id`) and `type=int`
  values are argparse-style usage errors (exit 2), checked after the terminal
  gate on the creating routes, so a headless caller sees the same refusal
  whatever the arguments.
- A region-less admin client (`create-instrument`, `new-session` and the
  wizard's STS check with nothing configured) uses `AWS_REGION`,
  `AWS_DEFAULT_REGION`, the profile region, then `us-east-1`.
- An unreadable or oversized (over 1 MiB) admin config reads as the empty
  skeleton, like invalid JSON; a `resources` or `policy` member that is not
  an object is replaced by `{}` when the config is next written (Python
  raised there).
- Deployed-state values that are not strings read as absent.
