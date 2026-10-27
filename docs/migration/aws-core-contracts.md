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
| The DNS check uses an explicit non-public range policy (see "Non-public address policy" below): the special-purpose IPv4 blocks of RFC 6890, and for IPv6 everything outside `2000::/3` plus the special-purpose blocks inside it; IPv4-mapped and NAT64 addresses are judged by their embedded IPv4 address, not rejected wholesale | Wider than Python's `is_private/loopback/link_local/reserved` |
| `AWS_PROFILE` is limited to 128 bytes drawn from ASCII letters, digits and `. _ @ + / -`; an empty value is "unset"; any other value is `aws_invalid_config` and is never echoed | **Intentional compatibility decision (#4473).** `di_session.py` passed `AWS_PROFILE` straight to `boto3.Session`. This boundary never reads the shared config or credentials files, so it cannot know which names the SDK accepts. It accepts the conventional profile-name alphabet (default, SSO and role-profile naming schemes, `name/with/slashes`, `user@host` styles) and refuses the rest so a hostile `AWS_PROFILE` cannot smuggle control characters, whitespace or secret-looking text into logs and later file lookups. The deferred SDK adapter (#4463) may relax this to what the SDK supports; widening is backward compatible, narrowing is not. |
| Failure-block troubleshooting step 2 reads "Verify the identifiers and parameters passed in the request" | The Python text named CloudWatch-specific identifiers; domain slices pass tailored `Guidance` |
| Service errors keep a redacted, truncated (1024 char) message; raw bodies are never kept | The Python `ApiError` kept the raw body on the exception |
| `max_attempts` is expressed as `max_retries` (3 retries = 4 attempts) | botocore's `Config(retries=...)` counts retries, so the Python made four attempts |
| `Adaptive` is carried as a label; the runner applies the standard schedule | The client-side rate limiter belongs to the SDK binding |

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

## Error redaction contract (#4468, #4469, #4494)

One scrubber, `redact::redact_text`, backs every error path: `AwsError`
`Display` and `Debug`, the `From<AwsError> for ToolkitError` conversion,
`ServiceError` (fields at construction, one-line `Display`, `Debug`),
`render_failure` and every request/response/header/env/provider/settings
`Debug`. It is conservative by construction: wherever it cannot be sure where
a secret ends, it hides more.

- **Union of spans.** One pass runs every recognizer on the *same* input (URL
  userinfo, labelled values, scheme credentials, access-key IDs, STS session
  tokens) and replaces the union of their spans, so one recognizer can never
  remove the context another needs. Passes repeat until the text stops
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
  anywhere in the key, so `nextToken` and `my_token` count) followed by `=`,
  `:` or `=>` (any whitespace, newlines and `\n`/`\t`/`\u…` escapes around it).
  A quoted value follows JSON escape rules at any escape depth. It is trusted
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
  (`token included`, `digest mismatch`) and is kept.
- **Whole-output pass.** Every hand-written `Display`/`Debug` scrubs each field
  and then scrubs the whole printed text again (`write_scrubbed_debug`/
  `write_scrubbed_display`); `render_failure` scrubs the assembled block. Rust's
  escaping in `Debug` therefore cannot turn a scrubbed field into a leak.
  `ServiceError` scrubs the path *before* `<id>` rewriting and the method
  *before* upper-casing, since both transformations can remove the context a
  recognizer needs. `snippet` scrubs, flattens and truncates, then scrubs its
  own output.

`Debug` is enforced at formatting time, independent of how a value was built:
`AwsError`, `ServiceError`, `Guidance`, `ClientSettings`, `SigningScope`,
`Headers`, `ApiRequest`/`HttpRequest`/`HttpResponse`, `MapEnv`,
`ProviderChain` and `FakeProvider` all scrub; `Headers` also hides the value of
any secret-bearing header name (`hides_header_value`), request/response bodies
print lengths only and `Page` prints counts only. `ClientConfig`, `Endpoint`
and `ResolvedRegion` print only validated values.

Accepted over-redaction: ordinary words after a scheme name (`a basic check
failed` becomes `a basic [REDACTED] failed`), and a value with glued text
after its closing quote hides the rest of the input. The adversarial fixture table
`redact::ADVERSARIAL` runs every case through all of these paths and asserts
that no synthetic secret survives and that a second pass changes nothing.

Known limit, unchanged: a bare 40-character secret with no label cannot be told
from an identifier and is not recognized, which is why the crate never formats a
secret value in the first place (`Secret`/`Credentials` print a fixed marker).

## Residual: SDK binding (needs a follow-up)

The AWS SDK for Rust must arrive as optional cargo features of a feature-gated
adapter (`toolkit-aws` feature `sdk`, implementing `CredentialsProvider`,
`Transport` and `Signer`). The workspace verifies with `--locked`; the SDK tree
needs a regenerated `Cargo.lock`, which this session could not produce (no
cargo, no sanctioned lock-generation recipe). Adding unlocked optional
dependencies would break every `--locked` job. The seams above are the exact
adapter points, so the follow-up is additive. DNS rebinding between the
endpoint check and the SDK connection also remains (same residual as Python).
