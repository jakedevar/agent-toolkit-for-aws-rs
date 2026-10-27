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
| `endpoint` | `validate_endpoint`: https only, no userinfo/query/fragment, restricted host characters, host allowlist (`.api.aws`, `.amazonaws.com`), every DNS answer must be public per the explicit range policy below (injected `HostResolver`); `Endpoint::for_service` synthesizes `https://<service>.<region>.amazonaws.com` after label/region validation | `launch_config._validate_base_url` |
| `credentials` | `CredentialsProvider` seam; `ProviderChain` with botocore rules (first success wins, `NoCredentials` falls through, any other error stops); `Credentials`/`Secret` with fixed redacted `Debug`; `reject_credential_arguments`; `STANDARD_CHAIN_STAGES` pins the stage names an SDK adapter must report | `di_session.build_client`, absence of credential arguments everywhere |
| `retry` | `RetryPolicy` (default 3 retries, `Adaptive` label (inert: no rate limiter), 1 s base, 20 s cap, hard cap 10 retries), `run_with_retry`, injected `Sleeper`/`Jitter` | `launch_api_client.py` (`max_attempts: 3, mode: adaptive`) |
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
  implemented and tested. The *production binding to the AWS SDK's default
  provider chain* is the `sdk` feature (#4463), described in "SDK adapter"
  below.
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
| The DNS check uses an explicit non-public range policy (see "Non-public address policy" below): the special-purpose IPv4 blocks of RFC 6890, and for IPv6 everything outside `2000::/3` plus the special-purpose blocks inside it; IPv4-mapped and NAT64 addresses are judged by their embedded IPv4 address, not rejected wholesale | Wider than Python's `is_private/loopback/link_local/reserved` |
| `AWS_PROFILE` is limited to 128 bytes drawn from ASCII letters, digits and `. _ @ + / -`; an empty value is "unset"; any other value is `aws_invalid_config` and is never echoed | **Intentional compatibility decision (#4473).** `di_session.py` passed `AWS_PROFILE` straight to `boto3.Session`. This boundary never reads the shared config or credentials files, so it cannot know which names the SDK accepts. It accepts the conventional profile-name alphabet (default, SSO and role-profile naming schemes, `name/with/slashes`, `user@host` styles) and refuses the rest so a hostile `AWS_PROFILE` cannot smuggle control characters, whitespace or secret-looking text into logs and later file lookups. The deferred SDK adapter (#4463) may relax this to what the SDK supports; widening is backward compatible, narrowing is not. |
| Failure-block troubleshooting step 2 reads "Verify the identifiers and parameters passed in the request" | The Python text named CloudWatch-specific identifiers; domain slices pass tailored `Guidance` |
| The credential troubleshooting step reads "Verify AWS credentials with: aws configure list" (Python: "Verify AWS credentials: aws configure list") | Under full-key label matching (#4516) `credentials:` is a labelled key, so the Python wording would render as `credentials: [REDACTED] configure list` |
| Service errors keep a redacted, truncated (1024 char) message; raw bodies are never kept | The Python `ApiError` kept the raw body on the exception |
| `max_attempts` is expressed as `max_retries` (3 retries = 4 attempts) | botocore's `Config(retries=...)` counts retries, so the Python made four attempts |
| `Adaptive` is carried as a label only; the runner applies the standard schedule for both modes and no client-side rate limiter exists (the SDK adapter adds none) | Kept so the Python `mode: adaptive` setting maps to a named value; adding a limiter would change behaviour and was ruled out on #4463 |

## Non-public address policy (#4470)

`validate_endpoint` with `EndpointPolicy::aws_public()` asks the injected
`HostResolver` for every address of the host and rejects the host if **any**
answer is non-public. `endpoint::is_non_public` is an explicit range policy,
not a list of std predicates:

| Family | Non-public | Public |
| --- | --- | --- |
| IPv4 | `0.0.0.0/8`, `10/8`, `100.64/10` (CGNAT), `127/8`, `169.254/16`, `172.16/12`, `192.0.0/24`, `192.0.2/24`, `192.88.99/24`, `192.168/16`, `198.18/15` (benchmarking), `198.51.100/24`, `203.0.113/24`, `224/4` (multicast) and `240/4` (reserved, includes `255.255.255.255`) | everything else |
| IPv6 | everything outside `2000::/3` (so `::`, `::1`, `::2`-style reserved, IPv4-compatible `::/96`, `100::/64`, `5f00::/16`, `fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`, `64:ff9b:1::/48`), and inside it `2001::/23` (IETF assignments, Teredo, benchmarking), `2001:db8::/32`, `2002::/16` (6to4) and `3fff::/20` (documentation) | the rest of `2000::/3` |
| IPv4-mapped IPv6 (`::ffff:a.b.c.d`) | judged by `a.b.c.d` with the IPv4 rules | same |
| NAT64 `64:ff9b::/96` | judged by the embedded IPv4 address | same (a translated public address stays public) |

The tables are the public constants `NON_PUBLIC_V4` and
`NON_PUBLIC_V6_IN_GLOBAL`; the rejection message names the address and the
block. Tests pin both edges of every IPv4 block and their public neighbours,
representative and edge IPv6 addresses, mapped and NAT64 forms, mixed answers
(one non-public answer in any position rejects) and real public AWS answers.
The policy deliberately rejects *more* than Python's `ipaddress` predicates
(for example `198.18.0.1`, `224.0.0.1`, `2001:db8::1`); it never accepts less.
The DNS-rebinding residual between the check and the SDK connection is
unchanged.

## Error redaction contract (#4468, #4469, #4494, #4515, #4516, #4536)

One scrubber, `redact::redact_text`, backs every error path: `AwsError`
`Display` and `Debug`, the `From<AwsError> for ToolkitError` conversion,
`ServiceError` (fields at construction, one-line `Display`, `Debug`),
`render_failure` and every request/response/header/env/provider/settings
`Debug`. It is conservative by construction: wherever it cannot be sure where
a secret ends, it hides more.

- **Union of spans.** One pass runs every recognizer on the *same* input (URL
  userinfo, labelled values, scheme credentials, access-key IDs, STS session
  tokens) and replaces the union of their spans, so one recognizer can never
  remove the context another needs. Each recognizer is tried at *every*
  position of the original input, including positions inside another
  recognizer's span (#4515): in `token=Bearer X` the label hides `Bearer` and
  the scheme recognizer still hides `X`. Passes repeat until the text stops
  changing (a still-changing text after eight passes becomes `[REDACTED]`), so
  `redact_text` is idempotent by construction.
- **URL userinfo.** For every `scheme://` (also `:\/\/` and further escaped
  forms), the authority runs to the first `/`, `?`, `#` or whitespace and
  everything before its *last* `@` is userinfo, whatever it contains (quotes,
  apostrophes, backslashes, more `@`). An `@` in the path, query or fragment
  is kept. Without a `/` the authority can reach a later `@` in unrelated text;
  that over-redaction is accepted.
- **Labelled values.** A key containing a secret label (`password`, `token`,
  `secret`, `credential`, `signature`, `api_key`, `x-amz-*`, ...; matched
  anywhere in the key, case-insensitively, so `nextToken`, `my_token`,
  `token_value`, `my_Password_Old` and `password_backup` all count: the key is
  read through to its end, letters, digits, `_`, `-` and `.`, before the
  separator is looked for (#4516)) followed by `=`,
  `:` or `=>` (any whitespace, newlines and `\n`/`\t`/`\u…` escapes around it).
  A quoted value follows JSON escape rules at escape depths up to 32
  backslashes; a value opened by a deeper escape is hidden to the end of the
  input (#4536). It is trusted
  only when its closing quote is followed, after optional whitespace, by `,`,
  `}`, `]`, `;`, `)` or the end (possibly after the quotes that close
  enclosing strings); otherwise, or when unterminated, the value is hidden to
  the end of the input. An unquoted value ends at whitespace, `&`, `,`, `;`,
  `)` or `}`; if a quote cuts it short, the same clean-close rule applies.
  Header-line keys (`authorization`, `cookie`, ...) hide to the end of the
  line.
- **Authentication schemes.** `Bearer`, `Basic`, `NTLM`, `DPoP`, `GNAP`,
  `Negotiate`, `token` (token credentials, up to the next delimiter) and
  `AWS4-HMAC-SHA256`, `AWS4-ECDSA-P256-SHA256`, `PrivateToken`, `SCRAM-SHA-*`,
  `vapid`, `HOBA`, `Digest`, `OAuth` (parameter lists, to the end of the line),
  case-insensitive. Any run of whitespace (Unicode, newlines, escaped forms)
  separates scheme and credential. For the English words `token`, `digest`,
  `negotiate` and `oauth`, a following short all-lowercase word is prose
  (`token included`, `digest mismatch`) and is kept, unless the scheme word
  itself lies inside a labelled value (`password=Token abcd` hides `abcd`).
- **Whole-output pass.** Every hand-written `Display`/`Debug` scrubs each field
  and then scrubs the whole printed text again (`write_scrubbed_debug`/
  `write_scrubbed_display`); `render_failure` scrubs the assembled block. Rust's
  escaping in `Debug` therefore cannot turn a scrubbed field into a leak.
  `ServiceError` scrubs the path *before* `<id>` rewriting and the method
  *before* upper-casing, since both transformations can remove the context a
  recognizer needs. `snippet` scrubs, flattens and truncates, then scrubs its
  own output.
- **Bounded work (#4536).** Redaction is linear in the length of its input.
  Each pass builds lookup tables in one right-to-left sweep (backslash runs,
  whitespace runs including escapes, line ends, unquoted value ends, base64
  runs), so every scan in a recognizer is a table lookup. Each complete key is
  classified once by the labels it contains and its separator and value are
  parsed once, however often a label repeats inside it (`token` x 20,000 + `=x`
  no longer walks the key suffix once per occurrence). Two quoted values with
  the same quote and escape depth cannot overlap, so with the depth cap each
  of the at most 66 quote/depth classes crosses the input at most once. The
  clean-close check after an unquoted value is cached by the value's end.
  At most nine passes run. Text longer than 256 KiB
  (`redact::MAX_REDACT_INPUT_BYTES`) is replaced by `[REDACTED]` without
  scanning. It is never cut first, since a cut could split a key ID or token
  so that its remainder is no longer recognized. The cap also bounds the
  lookup tables to a fixed amount of memory. Tests count elementary scan steps
  deterministically: 27 adversarial input families must grow about linearly
  in work when the input grows fourfold, and must stay under 1,000 steps per
  byte. They do not rely on timing.

`Debug` is enforced at formatting time, independent of how a value was built:
`AwsError`, `ServiceError`, `Guidance`, `ClientSettings`, `SigningScope`,
`Headers`, `ApiRequest`/`HttpRequest`/`HttpResponse`, `MapEnv`,
`ProviderChain` and `FakeProvider` all scrub; `Headers` also hides the value of
any secret-bearing header name (`hides_header_value`), request/response bodies
print lengths only and `Page` prints counts only. `Endpoint` `Debug` shows
the scheme, host and port only and renders the base path as
`<path: N bytes>`, never its text (#4469; validation proves where requests go,
not that the path is free of secrets). `ClientConfig` `Debug` nests that
`Endpoint` form, scrubs its service and profile strings and is scrubbed again
as a whole. The operational URL (`Endpoint::url`/`Display`) is unchanged.
`ResolvedRegion` prints only its validated region name and source.

Accepted over-redaction: ordinary words after a scheme name (`a basic check
failed` becomes `a basic [REDACTED] failed`), the first word after any key
that contains a label (`ExpiredTokenException: The …` becomes
`ExpiredTokenException: [REDACTED] …`, `tokens: 5` becomes
`tokens: [REDACTED]`), and a value with glued text
after its closing quote hides the rest of the input. A diagnostic text longer
than 256 KiB is hidden entirely, and so is a quoted value escaped more than 32
levels deep from its opening quote on. The adversarial fixture table
`redact::ADVERSARIAL` runs every case through all of these paths and asserts
that no synthetic secret survives and that a second pass changes nothing.

Known limit, unchanged: a bare 40-character secret with no label cannot be told
from an identifier and is not recognized, which is why the crate never formats a
secret value in the first place (`Secret`/`Credentials` print a fixed marker).

## SDK adapter (feature `sdk`, #4463)

This replaces the former "Residual: SDK binding" section. The AWS SDK for Rust
now binds the three seams behind the **non-default** cargo feature `sdk`. The
default build (every recipe: `make fmt`, `make clippy`, `make test`) compiles
no SDK crate: `cargo tree -p toolkit-aws` still shows only `toolkit-core`.

Lock generation follows the manager ruling on #4463: the worker ran
`cargo tree` / `cargo fetch` in its own turn (metadata and downloads only), and
the regenerated `Cargo.lock` is committed. It adds 157 packages for the
optional tree. No existing locked version changed. The workspace stays valid
under `--locked`.

### What binds what

| Seam | Adapter (`toolkit_aws::sdk`, feature `sdk`) | Behavior |
| --- | --- | --- |
| `CredentialsProvider` | `SdkCredentialsProvider::default_chain(&runtime, &config)` | The SDK default chain: environment, then shared config/credentials profile (static keys, SSO, assume-role, `credential_process`), then web identity, then ECS/EKS container role, then EC2 instance metadata. `AWS_PROFILE` (validated by `ClientConfig`) selects the profile **for credentials only**. The chain gets the boundary's resolved region for its STS/SSO calls, so a profile region never selects the target region. Reports the name `sdk-default-chain`. `from_sdk` wraps any other SDK provider. |
| `Signer` | `SigV4Signer::new(provider)` | `aws-sigv4` header signing (`authorization`, `x-amz-date`, `x-amz-security-token` for temporary credentials, signed payload hash). Credentials are loaded from any `CredentialsProvider` on every attempt; credentials expired at signing time are refused. |
| `Transport` | `SdkTransport::new(&runtime)` | smithy hyper client, rustls with the aws-lc provider (one crypto provider, the same one aws-config's own client uses). Connect and read timeouts come from `Timeouts`, and the read timeout also bounds reading the body. The transport never follows redirects (hyper does not; a 3xx is returned as a response), configures no proxy, sends only `https://` URLs without userinfo, and resolves hosts through the **connect-time DNS guard**. |

`SdkRuntime` is a current-thread tokio runtime the synchronous adapters drive
SDK futures on. The adapters must not be called from inside another tokio
runtime.

### SDK-independent half, tested in the default build (`binding`)

Every adapter decision that needs no SDK type lives in `toolkit_aws::binding`,
so the default `make test` covers it:

- `credential_error`: an SDK `CredentialsNotLoaded` becomes `NoCredentials`, so
  a `ProviderChain` moves on. Timeout, invalid configuration, provider and
  unhandled failures become `CredentialProvider` errors and stop the chain
  (botocore rules). The detail is redacted and truncated to 512 characters.
- `adopt_credentials`: an empty key id or secret is refused
  (`PartialCredentials`); expired credentials are refused
  (`CredentialsExpired`).
- `connector_error` / `timeout_phase`: timeouts map to `Timeout` (connect or
  read, from the connector's `HTTP connect` / `HTTP read` wording); I/O and
  unknown failures map to `Connect` (retryable); a request the client refused
  as built maps to non-retryable `InvalidConfig`.
- `guarded_addresses` / `GuardRejection` / `find_guard_rejection`: the
  connect-time DNS guard. It applies `endpoint::public_addresses`, the exact
  policy `validate_endpoint` uses (refactored out of it, unchanged), to the
  addresses the socket will connect to. A rejection is carried through the
  SDK error chain and surfaces as non-retryable `InvalidConfig` (field
  `endpoint`).
- `require_https`, `response_headers` (wire headers that `Headers::insert`
  refuses are dropped rather than failing the response).

### Tests (all offline)

- Default build: `binding` tests cover every mapping above, including redaction
  of secret-bearing SDK messages, guard acceptance and rejection, chain
  search through `source()` and `io::Error` payloads, and the https gate.
- `--features sdk` (`sdk::tests`): fake SDK `ProvideCredentials`
  implementations exercise adoption, expiry, not-loaded fall-through, the
  stopping kinds with redaction, and participation in a `ProviderChain`.
  SigV4 is checked against the AWS signing test-suite vector `get-vanilla`
  (exact `authorization` value), plus session-token, expiry and
  credential-error cases. A fake smithy `HttpConnector` checks request and
  response conversion, the https gate, connect and read timeouts, I/O
  redaction and guard refusal. The guarded SDK resolver is tested with a fixed
  answer, and the production connector is built without connecting. An
  end-to-end `ApiClient` run uses `SigV4Signer` + `SdkTransport` over the fake
  connector: 503 then 200, with two fresh signatures for the right
  region/service. No test calls `default_chain` (which would read the real
  environment and files), opens a socket or resolves DNS.

### Dependencies (feature `sdk` only; pinned with `=`)

| Crate | Version | Why it is needed | Features |
| --- | --- | --- | --- |
| `aws-config` | 1.13.0 | The only supported implementation of the standard credential chain (profile files, SSO, assume-role, web identity, container and IMDS providers) | `default-features = false`; `rustls` (aws-config's own HTTPS client for IMDS/ECS/STS/SSO; pulls rustls with aws-lc, never native-tls), `rt-tokio`, `sso` (SSO profiles, used by the Python via boto3), `credential-process` (`credential_process` profiles, as boto3) |
| `aws-credential-types` | 1.3.1 | `ProvideCredentials`, `SharedCredentialsProvider`, `Credentials`, `CredentialsError`: the types the chain returns and the signer consumes | default (none heavy) |
| `aws-sigv4` | 1.6.3 | SigV4 signing; hand-rolling HMAC canonicalisation would be riskier than the reference implementation | `default-features = false`, `sign-http` only (no `http` 0.2/1.x request integration needed) |
| `aws-smithy-http-client` | 1.5.1 | The SDK's hyper 1.x client with connect/read timeouts and a pluggable DNS resolver (needed for the connect-time guard) | `default-features = false`, `rustls-aws-lc` (same crypto provider as aws-config, so `ring` is never compiled) |
| `aws-smithy-runtime-api` | 1.19.1 | `HttpConnector`, `ResolveDns`, `Identity`, `ConnectorError` and the request/response types the client speaks | `client` |
| `aws-smithy-types` | 1.9.0 | `SdkBody`, `ByteStream` (body collection) and `DisplayErrorContext` (rendering SDK error chains through `Display` for redaction, never `Debug`) | default |
| `aws-smithy-async` | 1.3.1 | `TokioSleep`, the sleep implementation the client's timeouts need | `rt-tokio` |
| `tokio` | 1.54.1 | The runtime the synchronous adapters block on; `time` for the body read timeout, `net` for the client's sockets | `default-features = false`; `rt`, `time`, `net` |

Transitively the tree includes `aws-lc-rs`/`aws-lc-sys` (C/assembly crypto,
built by `cc`; the cost of rustls with the SDK's default provider), hyper 1.x,
h2, rustls 0.23 with native roots, and the generated `aws-sdk-sts`,
`aws-sdk-sso` and `aws-sdk-ssooidc` clients that aws-config uses for
assume-role and SSO. `ring` appears in `Cargo.lock` only as an optional entry
under `rustls-webpki` and is never compiled. No `openssl` or `native-tls`.

### Decisions and residuals

- **Retries stay with the boundary.** `ApiClient` + `RetryPolicy` remain the
  one retry loop. The transport sends exactly one request per call. The SDK's
  client-side rate limiter (adaptive mode) runs only inside the SDK
  orchestrator for generated service clients, which this adapter does not use.
  So `Adaptive` is a label only: nothing in this crate, the SDK adapter
  included, applies a client-side rate limiter, and `run_with_retry` uses the
  standard schedule for both modes. Per the #4463 ruling no rate limiter is
  added; the label is documented as inert instead.
- **`AWS_PROFILE` stays strict** (#4473): `default_chain` passes only the
  validated name. Relaxing it is backward compatible and left for a real need.
- **IMDS, container and STS/SSO traffic** of the credential chain uses
  aws-config's own HTTPS client, not the guarded transport, by design: the
  chain must reach `169.254.169.254` and container endpoints, which the
  public-address policy refuses.
- **SDK tracing**: `aws-sigv4` emits `trace`-level events of its inputs. The
  SDK's own `Debug` for credentials redacts the secret. This crate installs no
  `tracing` subscriber, so nothing is emitted.
- **Verification**: the fmt, clippy and workspace-test recipes build only the
  default feature set, so the default build stays SDK-free. The `sdk` feature
  has its own recipes, `clippy-sdk`
  (`cargo clippy -p toolkit-aws --features sdk --all-targets --locked -- -D warnings`)
  and `test-sdk` (`cargo test -p toolkit-aws --features sdk --locked`),
  declared in `.rsi/jobs.toml` and `.rsi/project.toml` with a 20-minute
  timeout and a 200% CPU cap (#4463 ruling, #4577).
- The DNS-rebinding residual recorded in T3 is **closed for the SDK
  transport**: the guard checks the addresses the socket connects to.
