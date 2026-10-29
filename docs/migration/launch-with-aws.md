# Launch with AWS migration

The Rust `toolkit launch` routes implement the Launch with AWS workflow described in
[the accepted design](launch-with-aws-design.md). L5 is **in progress**; these
routes have not yet passed the full acceptance matrix or independent review. The
legacy scripts and skill callers remain in place until cutover. Ordinary tests use
injected transports and fixtures, without credentials, AWS or a live network.

## Commands

The registered routes are `toolkit launch resolve-region`, `toolkit launch auth-start`,
`toolkit launch auth-wait`, `toolkit launch auth-callback`,
`toolkit launch session-status`, `toolkit launch sign-out`, `toolkit launch create-launch`,
`toolkit launch get-launch`, `toolkit launch list-launches`,
`toolkit launch delete-launch`, `toolkit launch start-launch-execution`,
`toolkit launch get-launch-status`, `toolkit launch get-launch-download-url`,
`toolkit launch download-snapshot`, and `toolkit launch apply-snapshot`.

| Route | Purpose |
| --- | --- |
| `resolve-region` | Resolve the region and endpoint. |
| `auth-start`, `auth-wait`, `auth-callback` | Start and complete browser sign-in; callback is an internal helper. |
| `session-status`, `sign-out` | Inspect or remove the local session. |
| `create-launch DIR [NAME]` | Build an archive of a local directory, upload it, and create a launch. |
| `get-launch ID [INCLUDE]` | Retrieve a launch, optionally requesting comma-separated sections. |
| `list-launches` | List launches in both regions, or one with `--region`. |
| `delete-launch ID`, `start-launch-execution ID` | Delete a launch or start its execution. |
| `get-launch-status ID` | Report execution progress. |
| `get-launch-download-url ID` | Print the download URL when available. |
| `download-snapshot ID`, `apply-snapshot DIR` | Download and safely extract, then apply a snapshot. |

Region-aware API commands accept `--region` and `--aws-mcp-url`. Network operations
require an `aws-sdk` build; the default build reports that network support is absent.
The download URL is intentionally returned on stdout for the two documented query
paths. Access and refresh tokens, the upload URL and its signature are not intended
to appear in output, diagnostics, child arguments or child environments. A presigned
upload PUT is sent once; only operations with a stable `clientToken` are retried.

## Recorded divergences

These are the design's D1–D22; the design has the full requirements and rationale.

- D1–D4: router flag placement and strict value handling differ from the old dispatcher;
  usage errors exit 2 and bare `toolkit launch` displays toolkit usage.
- D5–D7: callback state travels on stdin; callback validation and response headers are
  stricter; `auth-wait` does not signal the helper and reports specific outcomes.
- D8: the OIDC endpoint is fixed and issuer URLs with userinfo are rejected.
- D9: launch identifiers and names are validated locally, and authentication precedes
  archive construction.
- D10: null required response members are invalid.
- D11: archives skip non-regular files, use deterministic ordering and bounded sizes,
  refuse non-UTF-8 names, and harden Git execution.
- D12: HTTP proxy and `AWS_CA_BUNDLE` behavior is not inherited; SDK trust roots use
  the shared Rust policy.
- D13: connectivity errors use a fixed regional message.
- D14: `download-snapshot` replaces remote GitHub ZIP helpers; source repositories
  are cloned locally before `create-launch`.
- D15: unknown service error codes become `UnrecognizedError`; request IDs and
  service messages are not printed.
- D16: archive creation, snapshot download and snapshot application require the
  supported Unix filesystem boundary.
- D17: `apply-snapshot` replaces `rsync` with guarded writes and refuses unsafe entries.
- D18: snapshot output parents are checked for ownership and unsafe permissions.
- D19: callback connections, deadlines and work per turn are bounded.
- D20: snapshot application is restricted to a verified Git work tree and excludes
  Git metadata and unsafe Git environment overrides.
- D21: network operations use whole-operation deadlines and bounded body collection.
- D22: snapshot extraction and application enforce a 200,000-node budget.

D23 and D24 further describe the single-use authorization-code retry restriction
and the conservative collector memory bound in the design.

## Remaining work

Complete the full T-TOK, T-OPS, T-CODE, T-API, T-SHAPE, T-CMD and T-CLI matrices,
including seeded secret and error paths, non-Unix behavior, and service-model edge
cases. Verify the final committed source against the current rolling base, run the
required formatting, default and SDK clippy and SDK test recipes, and obtain an
independent exact-source review. L7 security review and L8 caller cutover remain
separate. Live account testing and any spending are operator-owned.
