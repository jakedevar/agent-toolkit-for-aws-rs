# Launch with AWS migration

The Rust `toolkit launch` routes implement the Launch with AWS workflow described in
[the accepted design](launch-with-aws-design.md). L5 (#5345) is implemented with
its acceptance tests; it awaits independent exact-source review and the L7 security review. The
legacy scripts and skill callers remain in place until cutover (L8). Ordinary tests use
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
Output follows the Python contract (section 1.3 of the design): success is
`json.dumps(value, indent=2)` with ASCII escapes, Python float `repr` (`12.0`) and the
service model's member order; failure is `{"error": "<message>"}` on stderr with exit 1.
Every launch route, including `auth-start`, `auth-wait` and the snapshot routes, uses
the same two writers.

The download URL is returned on stdout by `get-launch-download-url` and by
`get-launch ... download_url` (PM ruling, #5358). Access, refresh and client secrets,
the PKCE verifier, the authorization code, every `clientToken`, the presigned upload
URL, its query string and each query value (raw and percent-decoded) never appear in
stdout, stderr, the pending record, child argv, child environment or `Debug`. Error
codes are allowlisted constants; a rendered result or error that still contains a live
secret is replaced by the generic failure message.

Order of work in every API command: argument checks (missing argument, repository URL,
name 1..128, id 1..2048), then region and `LAUNCH_WITH_AWS_BASE_URL` validation
(including its DNS check), then the single access-token fetch (and refresh), then the
archive for `create-launch`, then the requests. A refused endpoint therefore never
causes a token refresh.

Retries follow the shared policy (3 retries on 429, 500, 502, 503, 504, connect and
timeout) for every API operation; `CreateUploadUrl`, `CreateLaunch` and
`StartLaunchExecution` retry only with their UUID v4 `clientToken`, which is drawn once
per operation and reused across retries. The presigned upload `PUT` is sent once.

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

L5 implementation decisions (#5345), each a narrowing of the Python behaviour:

- `list-launches`: `errors[region]` holds the Python `str(err)` text (no 401 hint;
  hints are added only to the top-level error, as `main` did). An invalid response is
  not a per-region failure: it ends the command, as botocore's `ResponseParserError`
  escaped the Python loop.
- An invalid `--region` is refused before the token fetch for every command, including
  `list-launches` (the Python reported it from `load_config` inside the loop, after
  signing in).
- Responses are parsed by the strict `toolkit_core` reader: duplicate object keys,
  a byte-order mark and integers outside the 64-bit range in integer members are
  invalid responses (Python `json.loads` kept the last duplicate). String timestamps
  must be RFC 3339 with `Z` or an offset (dateutil accepted more forms).
- A union with more than one member set, or none, is an invalid response (botocore
  parity; `__type` and `null` members are ignored).
- A presigned upload URL that fails validation ends with the generic failure message,
  which never names the URL.
- Every launch route prints the Python output form; before L5 the sign-in routes printed
  compact JSON and errors without the Python separators.

## Remaining work

L7 security review and L8 caller cutover (SKILL.md, mirrors, retirement of the six
scripts) remain separate slices. The operator's live account run (section 8 of the
design) is operator-owned. Non-Unix builds are not exercised by these offline tests.
