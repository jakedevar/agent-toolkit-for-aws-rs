# Shared AWS/API client boundaries (#4379)

Slice `aws-core`, wave 1 of programme #4369. Stacked on #4374 commit
`c208205a28d731e19edfbe3ab548228589e5ed7e` (frozen foundation `e5670754`, verified,
under review). The security pre-review that fixed these decisions before any
code is `docs/migration/aws-core-security-review.md`. Verification evidence is
in the Issue #4379 handoff, not here, so this page never changes after the
verified commit.

## What this slice adds

A new crate `crates/toolkit-aws` depending only on `toolkit-core` and `std`
(`Cargo.lock` gains exactly one workspace-member entry; no third-party crate).
`toolkit-core` is unchanged.

| Module | Contract | Python evidence |
| --- | --- | --- |
| `config` | `resolve_region` (flag > `AWS_REGION` > `AWS_DEFAULT_REGION` > `us-east-1`, empty = unset, profile region ignored), `resolve_profile` (`AWS_PROFILE`, credentials only), `resolve_endpoint` (flag > `AWS_ENDPOINT_URL_<SERVICE>` > `AWS_ENDPOINT_URL`), `ClientConfig::resolve` | `di_region.py`, `di_session.py` |
| `endpoint` | `validate_endpoint`: https only, no userinfo/query/fragment, restricted host characters, host allowlist (`.api.aws`, `.amazonaws.com`), DNS answer must be public (injected `HostResolver`); `Endpoint::for_service` synthesizes `https://<service>.<region>.amazonaws.com` after label/region validation | `launch_config._validate_base_url` |
| `credentials` | `CredentialsProvider` seam; `ProviderChain` with botocore rules (first success wins, `NoCredentials` falls through, any other error stops); `Credentials`/`Secret` with fixed redacted `Debug`; `reject_credential_arguments`; `STANDARD_CHAIN_STAGES` pins the stage names an SDK adapter must report | `di_session.build_client`, absence of credential arguments everywhere |
| `retry` | `RetryPolicy` (default 3 retries, `Adaptive` label, 1 s base, 20 s cap, hard cap 10 retries), `run_with_retry`, injected `Sleeper`/`Jitter` | `launch_api_client.py` (`max_attempts: 3, mode: adaptive`) |
| `transport` | `ApiRequest` → `HttpRequest`, `Transport` and `Signer` seams, `Timeouts` (default 60 s; the launch slice sets 120 s), header-injection and path validation, percent-encoded queries, redacting `Debug` | `launch_api_client.py` timeouts |
| `client` | `ApiClient::execute`/`execute_json`: per-attempt fresh signature, status ≥ 400 → `ServiceError` (code from `x-amzn-ErrorType` or JSON `__type`/`code`/`Code`/`errorCode`, message from JSON, request id header), retries | `launch_api_client._client_error_to_api_error` |
| `paginate` | `collect_pages` with page cap (1000), item cap (100 000), repeated-token detection, empty token ends the walk; `next_token_from` | `nextToken`/`NextToken` loops in `fetch_bedrock_agent.py`, `evaluate_traces.py`, `di_*_tools.py` |
| `error` / `redact` | `AwsError` (stable `aws_*` codes, `From` into `ToolkitError`: credential-flag misuse = usage/exit 2, everything else runtime/exit 1), `render_failure` (`Failed to <action>` block), `redact_text`/`redact_url`/`sanitize_path`/`snippet` | `di_error_translation.py`, `ApiError._sanitize_path` |
| `testing` | `FixedResolver`, `RecordingSleeper`, `FakeProvider`, `MockTransport`, `FakeSigner` | — |

No `toolkit` subcommand is added: this is a library for the domain slices.

## AC mapping

- **AC1** standard credential chain, region/endpoint, retries/timeouts/pagination,
  redacted errors: `credentials`, `config`+`endpoint`, `retry`+`transport`,
  `paginate`, `error`+`redact`. The *chain semantics* and the seam are
  implemented and tested; the *production binding to the AWS SDK's default
  provider chain* is the residual below.
- **AC2** mocks prove behavior without live spending: every test uses the
  `testing` doubles; no test opens a socket, resolves DNS, sleeps, reads a
  credential variable or file, or reads the real environment (one test asks
  `ProcessEnv` for a variable name that cannot exist).
- **AC3** pre-review before implementation:
  `aws-core-security-review.md`, committed before the first line of code.

## Intentional differences from the Python

| Change | Reason |
| --- | --- |
| Region must look like an AWS region name | A typo fails early instead of building a bogus host |
| Endpoint overrides reject userinfo, query and fragment; path must not contain `..`/whitespace/backslash; port must be 1–65535 | Credentials in URLs and URL-confusion tricks; Python accepted them |
| Carrier-grade NAT (`100.64/10`), multicast, unspecified and IPv4-mapped IPv6 ranges are treated as non-public in the DNS check | Wider than Python's `is_private/loopback/link_local/reserved` |
| Failure-block troubleshooting step 2 reads "Verify the identifiers and parameters passed in the request" | The Python text named CloudWatch-specific identifiers; domain slices pass tailored `Guidance` |
| Service errors keep a redacted, truncated (1024 char) message; raw bodies are never kept | The Python `ApiError` kept the raw body on the exception |
| `max_attempts` is expressed as `max_retries` (3 retries = 4 attempts) | botocore's `Config(retries=...)` counts retries, so the Python made four attempts |
| `Adaptive` is carried as a label; the runner applies the standard schedule | The client-side rate limiter belongs to the SDK binding |

## Residual: SDK binding (needs a follow-up)

The AWS SDK for Rust must arrive as optional cargo features of a feature-gated
adapter (`toolkit-aws` feature `sdk`, implementing `CredentialsProvider`,
`Transport` and `Signer`). The workspace verifies with `--locked`; the SDK tree
needs a regenerated `Cargo.lock`, which this session could not produce (no
cargo, no sanctioned lock-generation recipe). Adding unlocked optional
dependencies would break every `--locked` job. The seams above are the exact
adapter points, so the follow-up is additive. DNS rebinding between the
endpoint check and the SDK connection also remains (same residual as Python).
