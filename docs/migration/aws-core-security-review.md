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
