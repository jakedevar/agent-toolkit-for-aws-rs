# aws-core security pre-review (#4379 AC3)

Written and committed **before** any `toolkit-aws` code, so the credential,
IAM and network-sensitive decisions are fixed ahead of the implementation. It
is the author's threat review; an independent review by the other model
family is still required on the exact verified SHA (see the Issue handoff).
Evidence of the implementation lives in `docs/migration/aws-core-contracts.md`
and the Issue handoff.

## Evidence read (Python, read-only)

| Pattern | Source | Behavior to preserve |
| --- | --- | --- |
| Session / profile / client | `aws-observability/scripts/cloudwatch/di_session.py` | `AWS_PROFILE` selects **credentials only**; credentials otherwise come from boto3's ambient default chain; no credential arguments anywhere |
| Region precedence | `di_region.py` | explicit flag > `AWS_REGION` > `AWS_DEFAULT_REGION` > `us-east-1`; empty value counts as unset; profile region ignored |
| Endpoint safety | `launch-with-aws/scripts/launch_config.py` | https only, hostname must equal or end with `.api.aws` / `.amazonaws.com`, DNS must resolve, no private/loopback/link-local/reserved address ("Refusing to send credentials to a non-public address") |
| Retries / timeouts | `launch_api_client.py` | `retries={"max_attempts": 3, "mode": "adaptive"}`, connect/read timeout 120 s (service slice value), botocore default otherwise |
| Pagination | `fetch_bedrock_agent.py`, `evaluate_traces.py`, `di_*_tools.py`, `launch_with_aws.py` | opaque `nextToken`/`NextToken`, falsy token ends the walk, tokens echoed to the caller when a single page is requested |
| Errors | `di_error_translation.py`, `launch_api_client.py::ApiError` | `Failed to <action>` / `Error: <code> - <message>` / `ATTEMPTED PARAMETERS:` / `POSSIBLE CAUSES:` / `TROUBLESHOOTING:` blocks; `ApiError` text shows method, **sanitized** path (`<id>` for id-like runs, no query), status and code only |
| Allowlisted client operations | `di_logs_client.py`, `di_gateway.py` | callers may invoke only a fixed set of operations; no string-driven dispatch |

## Threats and decisions

| # | Surface | Threat | Decision |
| --- | --- | --- | --- |
| T1 | Credentials | Credentials passed on a command line / stored by the toolkit / printed | The boundary never reads credential environment variables or files. Credentials only enter through a `CredentialsProvider`, whose production binding is the standard AWS provider chain. `reject_credential_arguments` refuses `--access-key-id`/`--secret-access-key`/`--session-token`-style flags (flag name only is echoed, never the value). `Credentials` and `Secret` have a fixed `[REDACTED]` `Debug`, no `Display`, no serialization. |
| T2 | Logs and errors | Secret or token echoed in an error, `Debug`, request dump or test failure | Every error message is redacted when built **and** again when displayed (`redact_text`: access-key IDs, secret/session/refresh/access tokens, `Authorization`/`Bearer`, presigned-URL `X-Amz-*` query values, cookies, userinfo). Request `Debug` prints method, redacted URL, redacted headers and the body *length* only. Raw response bodies are never retained beyond a truncated, redacted message. Over-redaction is preferred to leakage. |
| T3 | Endpoint override | `AWS_ENDPOINT_URL*` or a flag redirects signed requests to an attacker host (credential exfiltration, SSRF to metadata/private ranges) | Every override goes through `validate_endpoint`: https only, no userinfo, no query/fragment, hostname characters restricted, host allowlist (`.api.aws`, `.amazonaws.com`), DNS result checked against non-public ranges (resolver injected, so tests are offline). Loopback / LocalStack endpoints are **not** accepted by the default policy (intentional; the Python behavior). Residual: DNS can change between this check and the SDK connection (rebinding); same residual as the Python. |
| T4 | Synthesized endpoint | Region or service string injected into a host name | Region must match the AWS region shape and the service label `[a-z0-9-]{1,63}` before `https://<service>.<region>.amazonaws.com` is built. |
| T5 | Header / request injection | CR/LF or NUL in a header, path or query reaches the wire | `Headers::insert` and path validation reject control characters; the query is percent-encoded by the boundary. |
| T6 | Retries | Retry storm, runaway cost, retry of auth failures | Bounded retries (default 3, cap 10), exponential backoff with full jitter capped at 20 s, retries only for throttling, 5xx/429, timeouts and connection failures; credential and validation errors are never retried. Sleep and jitter are injected so tests spend no time and no live requests. |
| T7 | Pagination | Non-terminating or unbounded token loop; memory growth | `collect_pages` enforces a page cap (default 1000), an item cap (default 100 000) and rejects a repeated token; exceeding a cap is an error, never silent truncation. |
| T8 | IAM | Broadening permissions / calling arbitrary APIs | This slice defines no IAM policies and calls no service. `ApiClient` takes whatever operation the domain slice builds; domain slices keep Python's operation allowlists (`di_logs_client`) in their own code. Signing uses the provider chain's credentials only; nothing here widens a role or assumes one. |
| T9 | Dependencies | Heavy / untrusted SDK pulled into every build | `toolkit-aws` depends only on `toolkit-core` and `std`. See "SDK binding" below. |
| T10 | Tests | Live AWS spend, credential reads in CI | No test reads credential variables or files, opens a socket or resolves DNS. Transport, signer, resolver, sleeper, jitter and environment are injected fakes with fixed data. |

## SDK binding (decision and residual)

The AWS SDK for Rust crates must be optional features. The workspace verifies
with `cargo ... --locked`, and the SDK dependency tree needs a regenerated
`Cargo.lock` (hundreds of locked crates). This session may not run cargo and has
no sanctioned way to produce a lock file, so the SDK binding is **not** added
here; adding unlocked optional dependencies would break `--locked` for the whole
workspace. The boundary is shaped so the binding is purely additive later:

- `CredentialsProvider` is the SDK `ProvideCredentials` adapter point;
- `Transport` + `Signer` are the SDK HTTP/SigV4 adapter points;
- `RetryPolicy`, `Timeouts`, `ResolvedRegion` and `Endpoint` map one-to-one onto
  SDK config (`RetryConfig`, `TimeoutConfig`, `Region`, endpoint URL).

A follow-up Issue is filed for the feature-gated adapter and lock generation.

## Addendum after independent review #4467 (bounded repair of #4379)

The review of source `f30a9138` found that rows T2 and T3 above were stated more
strongly than the first implementation delivered. The pre-review text is left as
written; the corrected, authoritative contracts are in
`aws-core-contracts.md` ("Error redaction contract", "Non-public address
policy"):

- T2: userinfo was only stripped by `redact_url`, labelled JSON values with
  escaped quotes or newline separators leaked a tail, and `Debug` of `AwsError`
  and of unrecognized header names bypassed the scrubber. Now one scrubber
  (`redact_text`, including embedded-URL userinfo and JSON string rules) backs
  every error and `Debug` path, with hand-written `Debug` that scrubs at
  formatting time (#4468, #4469).
- T3: "DNS result checked against non-public ranges" relied on std predicates
  that miss multicast, benchmarking, documentation and reserved space. Now an
  explicit RFC 6890-based range policy rejects the whole host if any answer is
  non-public (#4470).
- New residual recorded: the future SDK adapter (#4463) must disable or
  validate redirects and must connect to the already-validated address (or
  re-run the same policy on the connected peer); the current client has no
  production transport.

## Addendum after re-review #4467 of `b897cd82`

The re-review found that the first repair still lost to edge cases: userinfo
with an apostrophe, a glued closing quote followed by whitespace, several
spaces after `Bearer`/`Basic` (#4494), and raw `Debug` fields outside the
error types. The scrubber was rewritten to be conservative by construction
rather than patched per case (manager ruling); the contract is in
`aws-core-contracts.md` ("Error redaction contract"). In short: every
recognizer runs on the same input and the union of their spans is hidden;
URL userinfo runs to the authority's last `@` whatever it contains; a
labelled value that is not cleanly terminated is hidden to the end of the
input; any whitespace run separates an authentication scheme from its
credential; and every `Display`/`Debug`/rendered block is scrubbed again on
its fully printed text, which makes the output idempotent. Over-redaction of
some ordinary words (`a basic check`, `negotiate TLS`) is the accepted cost.

## Addendum after re-review #4467 of `6ba2ac90`

The third review found three remaining leaks, each repaired by a manager
ruling:

1. **Overlapping recognizers (#4515).** The label scan jumped to the end of
   each match, so in `token=Bearer X` the label hid only `Bearer` and the
   scheme recognizer never saw it; `X` survived. Every recognizer now runs at
   every position of the original input, inside other spans too, and the
   union is replaced. A scheme word that lies inside a labelled value also
   loses the prose exemption (`password=Token abcd` hides `abcd`).
2. **Label followed by more key text (#4516).** `token_value` and
   `password_backup` were not recognized because the separator was expected
   right after the label. The key is now read through to its end (letters,
   digits, `_`, `-`, `.`) before the separator is looked for. The cost is
   more over-redaction of the word after such keys
   (`ExpiredTokenException: [REDACTED] …`). The credential troubleshooting
   step was reworded (`Verify AWS credentials with: aws configure list`) so it
   is not itself hidden.
3. **Validated endpoint path in `Debug` (#4469).** `Endpoint` and
   `ClientConfig` used derived `Debug`, which printed a path that passed
   validation but carried a secret. Product choice (manager): `Debug` shows
   scheme, host and port, and the path only as `<path: N bytes>`. The
   operational URL is unchanged.

The shared `redact::ADVERSARIAL` table gained the overlap and full-key cases
(start/middle/end of a key, mixed case, escaped JSON, dotted and hyphenated
keys, header-line keys). They run through every error, header, request,
response and signing-scope wrapper, with exact output and a second-pass
identity check. Endpoint and resolved-configuration paths have their own table
tests in both `Debug` forms.

## Addendum after re-review #4467 of `7aeeb319`

The fourth review accepted the three leak repairs (#4515, #4516, #4469) and
found one availability regression (#4536, P2). Making a label match anywhere
in a key had made diagnostic redaction quadratic: every occurrence of a label
re-read the rest of its key. `"token".repeat(k) + "=x"` cost about `5k²/2`
byte checks per pass. Untrusted service error text reaches the scrubber
before `snippet` truncates it (`client::service_error` -> `ServiceError::new`),
so a large error body could use excessive CPU.

Repair (`crates/toolkit-aws/src/redact.rs`). The accepted redaction rules and
their outputs are unchanged, except for two hard limits that hide more:

- Each scrub pass builds per-input lookup tables in linear time: backslash
  runs, whitespace runs including escapes, line ends, unquoted value ends and
  base64 runs. Every scan a recognizer used to repeat from many start
  positions is now a lookup. These scans covered the remainder of a key, a
  nested value, a header line, a parameter credential, a token credential
  across escaped newlines, a base64 run, a backslash run inside a value and an
  unclosed `\u{` escape. An earlier static audit found each of them quadratic
  on a crafted input.
- Each complete key is classified once by the labels it contains, and its
  value is parsed at most twice: once as a header line, once as an ordinary
  value. Recognizers still run at every position of the original input, so
  the overlap fix (#4515) is unchanged.
- Quoted-value scans are linear because two values with the same quote and
  escape depth cannot overlap. To bound the number of depth classes, a value
  opened by more than 32 escaping backslashes is hidden to the end of the
  input. This is conservative: it hides more and never leaks.
- Text longer than 256 KiB is replaced by `[REDACTED]` without being scanned.
  It is never truncated before redaction.

Evidence: a deterministic step counter, compiled only for tests. It covers 27
adversarial families, including the reported input with and without a
separator and with short and long values. Each family must stay under 1,000
steps per byte, and its work may grow at most fivefold when its input grows
fourfold. The reported 100,002-byte input must stay under 400 steps per byte
and produce its exact output. Separate tests cover the size cap and the depth
cap. All existing exact-output, leak and idempotence tables are unchanged.

## Addendum: SDK binding (#4463)

The "SDK binding" decision above is now implemented as the non-default cargo
feature `sdk` (`toolkit_aws::sdk`). The authoritative description, the
dependency justifications and the test list are in `aws-core-contracts.md`
("SDK adapter (feature `sdk`, #4463)"). Security-relevant points, per threat:

- **T1 credentials**: credentials still enter only through the standard chain
  (`aws-config` default chain), selected at most by the validated
  `AWS_PROFILE`. The adapters hold credentials only as `Credentials`/`Secret`.
  `SdkCredentialsProvider`, `SigV4Signer`, `SdkTransport` and `SdkRuntime`
  print fixed, value-free `Debug`; no SDK value is formatted with `Debug`.
  `credential_process` and SSO profiles are honoured, as boto3 does.
  Running the configured process is standard chain behavior, not a new input.
- **T2 logs and errors**: every SDK error (credential, signing, connector,
  body read) is rendered through its `Display` chain (`DisplayErrorContext`)
  into the redacting `AwsError` constructors and truncated. Tests feed
  secret-bearing SDK messages and assert that nothing survives in `Display` or
  `Debug`. `aws-sigv4` traces its inputs at `trace` level; the SDK's own
  `Debug` for credentials redacts the secret, and this crate installs no
  `tracing` subscriber.
- **T3 endpoint / rebinding**: the residual from the #4467 addendum is
  addressed for the SDK transport. Redirects are never followed (hyper; a 3xx
  is returned to the client as a response). No proxy is configured. Only
  `https://` URLs without userinfo are sent. Hosts resolve through a
  connect-time guard that applies the same public-address policy
  (`endpoint::public_addresses`, factored out of `validate_endpoint`
  unchanged) to the very addresses the socket uses. A refusal is
  non-retryable `InvalidConfig`. The credential chain's own IMDS/container/STS
  traffic uses aws-config's client by design (it must reach link-local
  metadata endpoints).
- **T6 retries**: unchanged. The transport sends one request per call;
  `ApiClient` keeps the only retry loop.
- **T9 dependencies**: the default build still depends only on `toolkit-core`
  and `std`. With `--features sdk` the tree adds 157 locked packages: rustls
  with the aws-lc provider, no OpenSSL or native-tls, and `ring` never
  compiled. Direct dependencies are pinned with `=` and justified one by one.
- **T10 tests**: no test builds the real default chain (it would read the real
  environment and files), opens a socket or resolves DNS. Fakes implement the
  SDK `ProvideCredentials` and `HttpConnector` traits and the existing
  `HostResolver`.

Feature verification: the `clippy-sdk` and `test-sdk` recipes compile, lint
and test `--features sdk` (`.rsi/jobs.toml`, `.rsi/project.toml`); the job
ids and results at the verified SHA are in the #4463 handoff.
