# Launch with AWS: Rust port design and boundary pre-review (#4382, phase 1)

Status: design, revision 1. No code changes. Programme #4369, slice key `launch`, wave 2,
dependency #4379 (aws-core boundaries, integrated). AC1 of #4382 requires that the OAuth callback
and archive boundaries are reviewed **before** implementation; sections 3-6 are the material for
that pre-review and every requirement there has an ID (`R-...`) and a test (`T-...`) so a reviewer
can check the plan, then the code, against the same list.

Evidence base, all read and never executed (no Python, network or AWS call was run):

- this repository at `fa3fa13` (the allocated base, equal to `origin/rolling` at writing):
  `skills/core-skills/launch-with-aws/` (`SKILL.md`, `references/launchwithaws-2026-06-15.json`,
  six scripts). The `plugins/aws-core/skills/launch-with-aws/` mirror is byte-identical
  (`diff -q` on every file), so one port covers both (`docs/migration/inventory-audit.md` rows:
  canonical `PORT-RUST`, mirror `PORT-RUST-VIA-CANONICAL`);
- the toolkit conventions on rolling: `crates/toolkit-cli/src/registry.rs` (`SLICES`,
  `LEGACY_ROUTING_ORDER`, `the_public_route_order_is_pinned`), `crates/toolkit-cli/src/lib.rs`
  (`CommandSpec`, `route`, `Output`), `crates/toolkit-cli/tests/recorded_contract.rs`
  (`INVENTORY`, #4751), `crates/toolkit-aws` (`docs/migration/aws-core-contracts.md`:
  `endpoint`, `transport`, `client`, `retry`, `redact`, `error`, `sdk`);
- the #4780 bare `-` policy, `docs/migration/cli-bare-dash.md` at `f7028a6` (not yet on rolling at
  `fa3fa13`; it lands with the integration that carries it);
- the #4941 caller-value helpers at `40c71d7e7fad7d283afcb0a9338595645af652b8`
  (`crates/toolkit-cli/src/safe_text.rs`: `safe_path`, `shown_value`, `shown_path`,
  `redacted_value`, `CallerText`; `docs/migration/core-contracts.md` "Caller text in diagnostics"),
  accepted by the PM and landing with round 20 (#5019);
- the accepted #4413 design at `fd7e7ffd` (`docs/migration/mcp-proxy-design.md`) for the shared
  HTTPS transport plan (its S1: the `toolkit-cli` `aws-sdk` feature, `SdkTransport`, R-ROOTS,
  `StreamingTransport`) and as the depth reference;
- the botocore `sso-oidc` 2019-06-10 service model installed on the build host (botocore
  1.43.108, `botocore/data/sso-oidc/2019-06-10/service-2.json`, read with `jq` as data): it has
  `RegisterClient`, `CreateToken`, `CreateTokenWithIAM` and `StartDeviceAuthorization`, and **no**
  `RevokeToken` (relevant to `sign-out`, section 2.4).

## 1. Inventory: entry points, arguments, outputs

### 1.1 Files

| File | Lines | Role | Port target |
| --- | --- | --- | --- |
| `scripts/launch_with_aws.py` | 395 | CLI dispatcher, 12 commands, global flags, JSON output, error mapping | `launch/mod.rs`, `launch/commands.rs` |
| `scripts/launch_config.py` | 381 | constants, region chain, base/issuer URL validation, private file writes, models | `launch/config.rs`, `launch/state.rs` |
| `scripts/auth.py` | 493 | SSO OIDC PKCE flow, token refresh, session file, `auth-start`/`auth-wait`, callback handler | `launch/oidc.rs`, `launch/flow.rs`, `launch/callback.rs` |
| `scripts/auth_callback_server.py` | 105 | background child: loopback listener, code exchange, session write | `launch/flow.rs` (internal command) |
| `scripts/launch_api_client.py` | 277 | bearer-auth client for the Launch service over a boto3 custom model; presigned PUT | `launch/api.rs`, `launch/shape.rs` |
| `scripts/archive.py` | 495 | source selection (git ignore rules, secret filters), zip build, zip validation; dead GitHub download | `launch/archive/*` |
| `references/launchwithaws-2026-06-15.json` | 1169 | service model (rest-json, `smithy.api#httpBearerAuth`) | embedded read-only by `launch/shape.rs` |

All six scripts exist twice (canonical and `plugins/aws-core` mirror): 12 inventory rows, one port.

### 1.2 Invocation and global flags

`python3 scripts/launch_with_aws.py <command> [args...] [--region R] [--aws-mcp-url URL]`
(`launch_with_aws.py:340-391`).

- `--region` and `--aws-mcp-url` are stripped from **anywhere** in argv before dispatch
  (`_parse_global_flags`, 322-337); each takes the next word whatever it is. A flag at the end with
  no value: `{"error": "Missing value for --region"}`, exit 1. An empty value is "not given"
  (`_FLAGS.get("region")` falsy).
- No arguments, or `args[0]` in `-h`/`--help`: usage on **stdout**, exit 0. `-h` elsewhere is a
  positional.
- Flags only, no command: `{"error": "Missing command"}`, exit 1.
- Unknown command: plain text `Unknown command: X` + blank line + usage on **stderr**, exit 1.
- Fewer positionals than required: `{"error": "Missing required argument for <cmd>"}`, exit 1.
  Extra positionals reach `func(*cmd_args)`, raise `TypeError` and end in the generic message.
- Every other word, dash-prefixed or not, is a positional.
- Python < 3.10 or missing `boto3`: message on stderr, exit 2 (disappears with the port).

### 1.3 Commands

Success: `print(json.dumps(value, indent=2, default=str))`, exit 0 (`_ok`). Failure:
`print(json.dumps({"error": message}), file=sys.stderr)`, exit 1 (`_fail`). `json.dumps` uses
`ensure_ascii=True`; the success form uses `indent=2` (item separator `,` at line end, `": "`),
the error form the compact default separators (`", "`, `": "`).

| Command | Positionals | Network calls | stdout on success |
| --- | --- | --- | --- |
| `resolve-region` | none | DNS only, when `LAUNCH_WITH_AWS_BASE_URL` is set | `{region, regionName, source, sourceLabel, baseUrl, otherRegions:[{region, regionName}]}` |
| `auth-start` | none | DNS (base URL override), OIDC `CreateToken` (refresh) or `RegisterClient` | `{authenticated:true, reusedCachedSession:true\|false, baseUrl}` or `{authenticated:false, signInUrl, pid, port, baseUrl}` |
| `auth-wait` | `<pid>` | none in the parent (the child does the exchange) | `{authenticated:true, baseUrl}` |
| `session-status` | none | none | `{authenticated:false}` or `{authenticated, tokenExpiresInSecs, sessionExpiresInSecs, canRefresh}` |
| `sign-out` | none | none in practice (section 2.4) | `{signedOut:true, sessionRemoved:true\|false}` |
| `create-launch` | `<dir> [name]` | token (maybe refresh), `CreateUploadUrl`, presigned S3 `PUT`, `CreateLaunch` | the launch object without `contextInputs`, plus `region`, `regionName` |
| `get-launch` | `<id> [include]` | `GetLaunch` (`include` = comma list, sent as repeated query) | same launch form |
| `list-launches` | none | `ListLaunches` per region (both unless `--region`) | `{items:[...each + region], nextTokens?:{region:token}, errors?:{region:text}}` |
| `delete-launch` | `<id>` | `DeleteLaunch` | `{deleted:true, id, region}` |
| `start-launch-execution` | `<id>` | `StartLaunchExecution` | launch form |
| `get-launch-status` | `<id>` | `GetLaunch include=execution,cost_estimate` | `{id, region, regionName, status, isComplete, isFailed, completedTasks?, totalTasks?, currentPhase?, costEstimate?, failureReason?}` |
| `get-launch-download-url` | `<id>` | `GetLaunch include=download_url` | `{downloadUrl}`; absent → `_fail("Download URL not available yet. Ensure the launch execution has completed.")` |

Details that the port must reproduce (each has a parity test in section 8):

- `create-launch` refuses an `http`/`https` source first: "create-launch takes a local directory,
  not a repository URL. Clone {source} locally and pass the clone's directory." The display name is
  `name.strip()` or the source's basename after stripping trailing `/` (`_default_repo_name`), or
  `uploaded-app`. After success `save_region(region)` is best effort (an `OSError` is swallowed).
- `_launch_output`: `result.get("launch", result)`, pop `contextInputs`, then set `region` and
  `regionName` (a new key is appended; an existing `region` key keeps its position).
- `list-launches`: one failing region (`ApiError` or `BotoCoreError`) is recorded under
  `errors[region] = str(err)` unless it is the last remaining region, in which case a
  `BotoCoreError` becomes "Could not reach Launch with AWS in us-east-1, eu-central-1." (the regions
  joined) and an `ApiError` is re-raised to the common handler. `SessionExpiredError` and
  `ConfigError` are not caught by the loop and end the command. `--region` is **not** validated
  before the loop (the `ConfigError` comes from `load_config`).
- `get-launch-status` booleans: `isComplete = status == "completed"`, `isFailed = status ==
  "failed"`; the execution members are copied only when `execution` is truthy, `costEstimate` and
  `failureReason` only when truthy.
- Error mapping (`main`, 370-391): `SessionExpiredError`, `ArchiveError`, `ConfigError` → their
  text; `ApiError` → its text plus, for 401, " Hint: the backend rejected the Bearer token. Ensure
  you signed in successfully." and, for 404 on the five launch-id commands when
  `LAUNCH_WITH_AWS_BASE_URL` is unset, " Hint: no launch with this ID exists in {region}. A launch
  exists only in the region it was created in; retry with --region set to that region (one of:
  us-east-1, eu-central-1)."; anything else → "The operation failed. Re-run with logging enabled
  for details." (there is no flag that enables logging).

### 1.4 Hidden behaviour from boto3 that the output depends on

The Python never prints the raw HTTP body. botocore deserialises it through the service model, and
`json.dumps(default=str)` prints the result:

1. **Unmodelled members are dropped**, recursively; `null` members are dropped.
2. **Member order is the model's member order**, not the response order (botocore iterates the
   shape's members). Map keys keep the response order.
3. **Timestamps** (`SyntheticTimestamp_date_time`, `iso8601`: `Launch.createdAt/updatedAt`,
   `LaunchSummary.createdAt/updatedAt`) become `datetime` and print as `str(datetime)`:
   `2026-06-15 12:34:56+00:00`, with `.ffffff` only when microseconds are non-zero.
4. **Float shapes** (`CostBreakdownItem.monthlyCost`, `LaunchCostEstimate.estimatedMonthlyCost`)
   become Python `float`: a wire `12` prints `12.0`. Integer shapes stay integers.
5. **Unions** (`LaunchSource`): only the member that is set; an unknown member becomes
   `{"SDK_UNKNOWN_MEMBER": {"name": "<member>"}}`.
6. `ResponseMetadata` is added by botocore but never printed (every command selects a member).
7. **Client-side parameter validation**: `name` 1..128 characters, `launchIdentifier`
   1..2048 characters. A violation is a `ParamValidationError` (a `BotoCoreError`) that ends in the
   generic message; for `create-launch` this happens only **after** the archive was built and
   uploaded.
8. **Idempotency tokens**: `CreateUploadUrl`, `CreateLaunch` and `StartLaunchExecution` have
   `clientToken` with `idempotencyToken: true`; botocore fills a UUID v4 when the caller gives none
   (the Python never does) and reuses it on retries.
9. botocore honours `AWS_ENDPOINT_URL` / `AWS_ENDPOINT_URL_SSO_OIDC`, `HTTPS_PROXY` and
   `AWS_CA_BUNDLE` for the `sso-oidc` client; `urllib` (presigned PUT) honours `https_proxy`.
10. Python `logging` has no handler configured, so the last-resort handler prints records of
    level WARNING and above to stderr as the bare message. The only one reachable is
    `Using non-default IdC issuer URL: <url>` (when `LAUNCH_WITH_AWS_IDC_ISSUER_URL` is set and an
    interactive registration happens). INFO/DEBUG records are never shown.

### 1.5 Environment and files

| Name | Read by | Effect |
| --- | --- | --- |
| `LAUNCH_WITH_AWS_REGION` | region chain | second signal after `--region`; validated |
| `LAUNCH_WITH_AWS_BASE_URL` | `Config`, `_region_hint`, `create_client` | API endpoint override (validated: https, host `.api.aws`/`.amazonaws.com`, public DNS) |
| `LAUNCH_WITH_AWS_IDC_ISSUER_URL` | `resolve_issuer_url` | `issuerUrl` in `RegisterClient` (https, host `.amazonaws.com`/`.awsapps.com`) |
| `LAUNCH_WITH_AWS_SCOPES` | `_resolve_scopes` | comma list, default `launch:access` |
| `AWS_REGION`, then boto3's `AWS_DEFAULT_REGION` / profile `region` (`AWS_PROFILE`, `~/.aws/config`, `AWS_CONFIG_FILE`) | `_profile_region` | region signal mapped to a cell (`eu-*` → `eu-central-1`, else `us-east-1`) |
| `~/.launch-with-aws/session.json` | session | client registration + tokens, 0600 in a 0700 directory, atomic replace |
| `~/.launch-with-aws/config.json` | saved region | `{"region": ...}`, same write discipline |

Constants (`launch_config.py:20-78`): regions `us-east-1` (US East (N. Virginia)) and
`eu-central-1` (Europe (Frankfurt)); OIDC region `us-east-1`; default issuer
`https://view.awsapps.com/start`; client name `Launch with AWS Agent Skill`; token expiry buffer
300 s; callback timeout 600 s; default token lifetime 28 800 s; session cap 90 days; wait poll 1 s;
request timeout 120 s; upload timeout 300 s; archive limits 500 MiB compressed, 100 000 entries,
2 GiB uncompressed, 100 MiB per entry, ratio 100:1.

## 2. How the Python works today

### 2.1 Region chain (`resolve_region`)

`--region` (validated) → `LAUNCH_WITH_AWS_REGION` (validated) → saved `config.json` (if still
supported) → `--aws-mcp-url` whose host matches `^aws-mcp\.([a-z0-9-]+)\.api\.aws$` with an `eu-`
region (→ `eu-central-1`; any other region is "no signal") → `AWS_REGION`, else boto3's profile
region → mapped to a cell → `us-east-1`. Each result carries a `source` and a fixed `sourceLabel`.

### 2.2 Sign-in (`auth-start`, `auth-wait`, the child)

1. `auth-start` loads the session. A token valid for more than 300 s more → reuse. Else, if the
   client registration is still valid, a silent `CreateToken(refresh_token)`; any failure is
   swallowed. Else a new `RegisterClient` (public client, `authorization_code` + `refresh_token`,
   redirect `http://127.0.0.1`, issuer URL, scopes; client lifetime capped at 90 days).
2. PKCE: verifier = base64url(32 random bytes) without padding; challenge = base64url(SHA-256)
   without padding, method `S256`. State = 32 hex characters (16 random bytes).
3. It spawns `python auth_callback_server.py --state <STATE> --timeout 600` in a new session and
   writes `{client_id, client_secret, client_expires_at, scopes, code_verifier}` as JSON to the
   child's stdin. **The state travels in argv**, visible in `ps` to other local users.
4. The child binds `HTTPServer(("127.0.0.1", 0))`, prints the port on stdout, closes stdout and
   serves until a callback sets its event or 600 s pass. The handler (`auth.py:210-253`) parses the
   query of **any path**: `error` present → 400 page reflecting `error_description` (HTML-escaped)
   and **the flow ends, without any state check**; state mismatch with a `code` → 400 "State
   mismatch" and the flow ends; mismatch or match without a `code` → 204, flow continues; match with
   a code → 200 "Authenticated - you can close this tab." and the flow ends. Request logging is
   silenced.
5. The child exchanges the code (`CreateToken`, `authorization_code`, same redirect URI), writes
   the session and exits 0; a failure exits 1 (a traceback goes to a closed stderr pipe).
6. `auth-start` prints `signInUrl` (`https://oidc.us-east-1.amazonaws.com/authorize?` +
   `response_type, client_id, redirect_uri, state, code_challenge, code_challenge_method, scope`),
   `pid` and `port`. **No browser is launched**: the skill tells the agent to show the URL.
7. `auth-wait <pid>` polls every second for 600 s: a valid session → success; the pid gone
   (`os.kill(pid, 0)`) without a session → `RuntimeError`; deadline → `SIGTERM` **to whatever
   process now has that pid**, then `TimeoutError`. Both errors end in the generic message.

### 2.3 Token use

`get_access_token` is called by every API call: cached token (valid for > 300 s more), else silent
refresh (failure swallowed), else `SessionExpiredError("Session expired or not authenticated. Run
auth-start to sign in.")`. The token is sent as `Authorization: Bearer` through botocore's token
provider. `session-status` reports seconds left (clamped at 0) without any call.

### 2.4 Sign-out

`sign_out` calls `getattr(sso_oidc, "revoke_token", None)`; the botocore `sso-oidc` model has no
`RevokeToken` operation (evidence above), so the call never happens and sign-out is purely local:
unlink `session.json`, report whether it existed. `config.json` (the region choice) is kept.

### 2.5 Archive (`zip_local_repo`)

1. `root = abspath(expanduser(path))`; not a directory → "Not a directory: {path}".
2. File selection: in a Git worktree, `git ls-files --cached --others --exclude-standard -z` minus
   `git ls-files --cached --ignored --exclude-standard -z` (paths relative to `root`). Outside Git
   with a `.gitignore` anywhere below `root`: a temporary bare repository applies the rules
   (`--git-dir=<tmp> --work-tree=<root> ls-files --others --exclude-standard -z`). No Git metadata
   and no `.gitignore`: every file. Git missing, failing or exceeding 30 s → `ArchiveError` when
   rules apply, otherwise "every file".
3. Walk (`os.walk`, filesystem order, symlinked directories not entered): skip directory names in
   `_SKIP_DIRS` (VCS, build, cache, IDE, and credential directories such as `.aws`, `.ssh`,
   `.kube`); skip every symlinked file; skip files outside the Git set; skip secret-looking names
   (`_SKIP_FILES`, `.env.*`, `.pem/.key/.p12/.pfx/.jks/.keystore`, `id_rsa*`-style prefixes),
   case-insensitive. A FIFO or device file is **opened and read** by `zf.write` (a FIFO blocks
   forever).
4. Each file is deflated (level 6) under `<prefix>/<rel>`, `prefix = sanitize_root_name(name)`
   (last path segment, leading dots stripped, `[^\w.-]` → `-`, `-` trimmed, else `app`; `\w` is
   Unicode-aware). The Unix mode goes into `external_attr`; a file older than 1980 raises
   `ValueError` (generic message). No directory entries.
5. The whole archive is built in memory; then: zero files → "No files found under {path}"; over
   500 MiB → size error; then `validate_archive_bytes` (central directory only): entry count, safe
   names (no NUL, no leading `/` or `\`, no drive letter or UNC, no `..` segment after `\` → `/`),
   no symlink entries, per-entry size, per-entry ratio, running total, single top-level directory.

`parse_github_url`/`download_github_zip` (archive.py 224-236, 403-495) have **no caller**: the
skill tells the agent to clone locally and `create-launch` refuses URLs.

### 2.6 Download and unpack (not in the scripts)

`SKILL.md` step 8 tells the agent to run
`curl -L -o /tmp/migration-snapshot.zip "<download_url>"`, `mkdir -p /tmp/migration-output`,
`unzip -o ... -d /tmp/migration-output`, then `rsync -a /tmp/migration-output/ .` into the user's
repository. **No Python code extracts an archive.** The snapshot is produced by an AWS-hosted agent
that modified the user's (possibly hostile, prompt-injecting) source, so its content is untrusted.
The prose has three weaknesses: fixed names in a shared `/tmp` (a local user can pre-create the
file as a symlink or own the directory), `unzip` restores symlinks that `rsync -a` then copies into
the repository, and nothing bounds size or entry count.

## 3. OAuth callback boundary (AC1)

Process model in Rust: `toolkit launch auth-start` re-executes the same binary
(`std::env::current_exe()`) as the internal command `toolkit launch auth-callback`; no Python, no
second program. All callback logic lives in `launch/callback.rs` (pure request handling, testable
without sockets) and `launch/flow.rs` (process lifecycle).

### R-CB-BIND. Bind address and port

- The listener binds exactly `SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)` (`127.0.0.1`, port chosen
  by the OS). Never `0.0.0.0`, never `::`, never a host name, never `localhost` (which can resolve
  to `::1` or be redefined). The redirect URI is the literal `http://127.0.0.1:<port>` and is
  registered as `http://127.0.0.1` (RFC 8252 section 7.3 loopback, any port), as today.
- After `bind`, the child reads back `local_addr()`, asserts it is `127.0.0.1` and non-zero, and
  writes exactly one line `<port>\n` to stdout, then closes stdout.
- Port choice stays OS-ephemeral (parity). A fixed or user-chosen port is not offered: it adds
  collision and squatting cases and the IdP accepts any loopback port.

### R-CB-REQ. Request handling

- Each accepted connection gets a 5 s read timeout and a 5 s write timeout; the server reads at
  most 8 KiB of request head (up to the first `\r\n\r\n`), never a body. Over the cap, a malformed
  request line, a non-`GET` method, or a non-`HTTP/1.x` version → `400` (or `405` for a method),
  `Connection: close`, and the connection does not affect the flow.
- **Path**: only `/` (with a query) is the callback. Any other path (`/favicon.ico`, `/x`) →
  `404` and ignored. Intentional: the Python accepted any path.
- **Host header**: must be present and equal `127.0.0.1:<port>`; otherwise `400`, ignored. This
  blocks DNS-rebinding pages that reach the port under another host name.
- **Query parsing**: `application/x-www-form-urlencoded` rules (`+` is space, percent-decoding,
  invalid UTF-8 → `400`). Parameters other than `code`, `state`, `error`, `error_description`,
  `error_uri`, `iss` are ignored. A **repeated** `code`, `state` or `error` → `400`, ignored
  (Python took the first).
- Connections are handled one at a time on the child's single thread; an accept cap of 256
  connections per flow ends the flow as failed (`too_many_requests`) so a local flood cannot keep
  the child spinning until the deadline.

### R-CB-STATE. State and PKCE validation

- State: 32 bytes from the OS CSPRNG (`getrandom`), base64url without padding (43 characters).
  Longer than the Python's 16 bytes; the IdP treats it as opaque.
- The state is compared in **constant time** (length check, then XOR-accumulate over bytes).
- A request whose `state` is absent or wrong is answered `400` (fixed page) and **does not end the
  flow**, whether it carries `code` or `error` (Python: `error` ended the flow without any state
  check, and a wrong state with a code ended it; both let any web page that finds the port abort a
  sign-in). Guessing a 256-bit state is infeasible, and the accept cap bounds the work.
- Only a request with the right state ends the flow: with `code` → success path; with `error` →
  failed (`authorization_denied`). The `error` value is kept only if it matches the RFC 6749
  `error` grammar (`%x20-21 / %x23-5B / %x5D-7E`) and is at most 64 characters, otherwise
  `invalid_error_code`; `error_description` and `error_uri` are **never** stored, printed or
  reflected.
- PKCE: verifier = base64url(32 CSPRNG bytes), 43 characters, which is within RFC 7636's 43..128
  `unreserved` alphabet; challenge = base64url(SHA-256(verifier)), method `S256`. The verifier and
  the client secret never leave the child except in the `CreateToken` request body.
- The authorization code is accepted only if it is 1..4096 bytes of visible ASCII; otherwise the
  flow fails (`invalid_code`) without an exchange.

### R-CB-ONCE. Single use

- When a state-matching request arrives, the child writes the response page, closes that
  connection and **drops the listener before** the token exchange, so the port no longer accepts
  connections. A second callback cannot be served, and the code is exchanged at most once.
- The child never retries the exchange with the same code if it fails at the HTTP level with a
  response (the code is single-use at the IdP); a connect failure before any byte was sent may be
  retried by the shared `RetryPolicy`, as botocore did.

### R-CB-TIME. Timeouts and lifetime

- The child computes one deadline at start: `now + 600 s` (`CALLBACK_TIMEOUT_SECS`); the accept
  loop uses non-blocking accept with a 200 ms sleep (or `set_nonblocking` + poll) and stops at the
  deadline. The deadline is written into the pending-flow record (R-CB-WAIT) so `auth-wait` uses the
  same instant.
- After the deadline the child marks the flow `timed_out`, removes its pending record and exits 1.
  It never needs a signal from the parent.

### R-CB-WAIT. Parent/child handshake without signalling a pid

- `auth-start` creates `~/.launch-with-aws/pending/` (0700) and the child creates
  `pending/<pid>.json` (0600: `{"pid", "port", "deadline", "status"}`, status one of `listening`,
  `exchanging`, `succeeded`, `failed`, `timed_out`, plus a fixed `reason` code; never a secret) and
  holds an exclusive lock on `pending/<pid>.lock` for its whole life (`std::fs::File::lock`, stable
  since Rust 1.89; the toolchain is 1.94.1).
- `auth-wait <pid>` (positional kept for compatibility) reads `pending/<pid>.json`. Missing →
  `{"error": "No sign-in is in progress for pid <pid>. Run auth-start again."}`. Then it polls every
  second until the recorded deadline: a valid session → `{authenticated:true, baseUrl}`; status
  `failed`/`timed_out` → a fixed message per reason; the lock can be taken (the child is gone)
  without a valid session → "Sign-in ended without completing authentication. Run auth-start
  again.".
- `auth-wait` **never sends a signal**. The Python's `SIGTERM` to a bare pid could hit an unrelated
  process after pid reuse; in Rust it would also need `unsafe` (`libc::kill`), which the workspace
  forbids. The child's own deadline replaces it.
- Stale records (lock free, status `listening`) are deleted by the next `auth-start`.
- Concurrent flows are allowed (one record per pid), as in the Python.

### R-CB-SPAWN. Child process hygiene

- argv of the child is exactly `[current_exe, "launch", "auth-callback"]`: **no state, verifier,
  secret or URL in argv** (Python put the state there).
- The configuration (`client_id`, `client_secret`, `client_expires_at`, `scopes`, `code_verifier`,
  `state`, `deadline`) goes over the stdin pipe as one JSON object, at most 64 KiB, then stdin is
  closed. The child reads stdin to EOF with that cap; anything else (empty, oversized, invalid,
  unknown or missing members) → exit 1 with no listener.
- No secret is ever put in the child's environment. The child inherits the parent's environment
  otherwise unchanged (the parent never holds a secret there).
- stdout: piped, one port line, read by the parent with a 10 s timeout and a 16-byte cap; anything
  that is not 1..65535 → the parent fails with "The sign-in helper failed to start." and kills its
  own child handle (`Child::kill`, a handle, not a pid). stderr: `Stdio::null()`. stdin: piped, then
  closed.
- Detach: `CommandExt::process_group(0)` on Unix (safe API; the Python used `setsid`) so the
  terminal's Ctrl-C does not reach the helper; on Windows
  `creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)`. The parent does not wait for the
  child.
- `auth-callback` is a registered route (the router has no hidden routes) documented as internal.
  Run by hand, it reads stdin; on a TTY or with invalid input it exits 1 without binding.

### R-CB-PAGE. Response page and what is logged

- The page is one of four fixed bodies (success, authorization failed, invalid request, not found),
  `Content-Type: text/html; charset=utf-8`, `Cache-Control: no-store`,
  `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`,
  `Content-Security-Policy: default-src 'none'`, `Connection: close`, an exact `Content-Length`.
  No request value is reflected (Python reflected `error_description`).
- Nothing is logged. The child writes only the port line and the pending record. The parent's
  output is the JSON of section 1.3. The code, state (except inside `signInUrl`, below), verifier,
  client secret and tokens appear in no output, error, log or file other than `session.json`.
- `signInUrl` carries `client_id`, `state` and `code_challenge` by design (the user must open it);
  it is written to stdout verbatim, never through the redactor (#4981: stdout data is never
  rewritten). The state in it is a CSRF nonce, not a credential; with PKCE an attacker who learns it
  still cannot redeem a code.

### R-CB-BROWSER. Browser launch

No browser is launched (parity). The agent shows `signInUrl` as `SKILL.md` step 1 says. Launching
would spawn `xdg-open`/`open`/`start` with a URL built from IdP-registered values, fails in
headless agent sandboxes and SSH sessions, and adds a process-spawn surface for no security gain.
See Q4.

## 4. Token handling (AC1, AC2)

### R-TOK-STORE. Where tokens live

- Only `~/.launch-with-aws/session.json`, schema **byte-compatible** with the Python (keys in this
  order: `client_id`, `client_secret`, `client_expires_at`, `authorize_endpoint`, `token_endpoint`,
  `scopes`, `access_token`, `refresh_token`, `token_expires_at`; `indent=2`), so an existing Python
  session keeps working after cutover and the other way round. `config.json` holds only the region.
- The state directory is `$HOME/.launch-with-aws` (Unix) / `%USERPROFILE%\.launch-with-aws`
  (Windows), resolved once; an empty or relative `HOME` → `ConfigError`. Tests inject the directory.

### R-TOK-MODE. File modes and atomic writes

- Directory: created with `DirBuilderExt::mode(0o700)`; if it exists it must be a real directory
  (`symlink_metadata`, not a symlink) and is `chmod 0700` on every write (Python did the chmod too).
- File: a temporary `session.json.<16 hex random>.tmp` in the same directory created with
  `create_new(true)` and `OpenOptionsExt::mode(0o600)`, written, `sync_all`, then `rename` over
  the target (atomic on POSIX and on Windows `MoveFileEx` replace). A failure removes the
  temporary file.
- Read: `symlink_metadata` must be a regular file (a symlink or anything else → treated as "no
  session", and `sign-out` removes the link itself, never its target); a file with group or other
  permission bits is tightened to 0600 before reading. Size cap 64 KiB.
- Parsing is strict: wrong types, unknown or missing members → "no session" (Python: some of those
  raised later and ended in the generic message).
- Windows has no mode bits; the files inherit the profile directory's ACL (parity: the Python's
  `chmod` there only toggles read-only). Recorded as residual W-1.

### R-TOK-MEM. In-memory handling

- `client_secret`, `access_token`, `refresh_token`, the PKCE verifier, the state and the code are
  held as `toolkit_aws::redact::Secret` (fixed redacted `Debug`, `expose()` only at the two use
  sites: the HTTP body/header and the session file). No `Display` exists for them.
- No secret-bearing value is formatted with `{:?}` of a containing struct that is not redacting;
  `#[derive(Debug)]` is forbidden on types holding `Secret` unless every field redacts (a unit test
  formats each such type with sample secrets and asserts absence).

### R-TOK-RED. Redaction in every output and error

- **Request side**: the bearer token goes only into the `authorization` header, which
  `toolkit_aws::transport::Headers` hides in `Debug` (`hides_header_value`). OIDC bodies are built
  as JSON and never logged; `HttpRequest` `Debug` prints the body length only.
- **Response side**: OIDC errors surface only the error **code** (`x-amzn-ErrorType` or JSON
  `error`, matched against the model's exception names, else `UnknownError`); the
  `error_description`/`message` is dropped (it can echo submitted values). Launch API errors keep
  the Python `ApiError` text: `"{METHOD} {sanitized path} failed ({status})[: {code}]"`, produced
  by `toolkit_aws::error::ServiceError`, whose path is scrubbed before `<id>` rewriting; the
  service message is never printed (parity).
- **The presigned upload URL** (`uploadUrl`, a `SensitiveUrl`) appears in no output or error: a
  failed PUT reads "PUT (presigned S3 upload) failed ({status})" (parity), a transport failure
  "Could not upload the archive: {connect|timeout}" (fixed text). `HttpRequest` `Debug` uses
  `redact_url`.
- **The download URL** is printed by `get-launch-download-url` and `get-launch ... download_url`
  on stdout by design (`SKILL.md`: "Always present the full download URL"). It is never echoed in
  an error.
- **Caller values** in diagnostics follow #4941: paths through `shown_path`, other caller values
  through `shown_value` (then Python-`repr` quoting where the Python used `!r`), caller values in
  stdout data through `redacted_value` (`delete-launch`'s `id`). Sites: the repository-URL refusal
  (`{source}` may be `https://user:token@github.com/...`), "Not a directory", "No files found",
  every `ConfigError` quoting `LAUNCH_WITH_AWS_*` values, region values, archive entry names, the
  issuer-URL warning line.
- **Panics**: secret-bearing code paths use no `unwrap`/`expect` on values whose `Debug` could
  include a secret; the release profile already strips symbols. A panic message never contains a
  secret because no secret type implements a revealing `Debug`.

### R-TOK-ARGV. Never in argv or the environment of children

- Children: the callback helper (R-CB-SPAWN) and `git` (R-AR-GIT). Neither receives a token,
  secret, code, verifier or state in argv or environment. The helper receives its secrets on stdin
  only; `git` receives none.
- The CLI accepts no token, secret or URL-with-credentials flag. `toolkit_aws::credentials::
  reject_credential_arguments` is applied to the launch routes (the shared rejection of
  `--access-key`-style flags).

### R-TOK-LIFE. Lifetimes

Parity: expiry buffer 300 s, default lifetime 28 800 s when `expiresIn` is absent or `0`, client
lifetime `min(clientSecretExpiresAt, now + 90 days)` (`0` = cap). Hardening: a negative or absent
`expiresIn` → default; an `expiresIn` above 90 days is capped at 90 days. A refresh failure is
swallowed and leads to `SessionExpiredError` text (parity). The clock is injected (`Clock` trait)
for tests.

### R-TOK-OIDC. OIDC endpoint

- Fixed: `https://oidc.us-east-1.amazonaws.com` (`/client/register`, `/token`), sent through the
  same `Transport` as the API with `NoSigner` (both operations are `authtype: none`). Authorize URL
  `https://oidc.us-east-1.amazonaws.com/authorize` (parity: the Python ignored the registration's
  `authorizationEndpoint`).
- `AWS_ENDPOINT_URL`, `AWS_ENDPOINT_URL_SSO_OIDC` and profile `endpoint_url` are **ignored** for
  OIDC (botocore honoured them, which could send the client secret and code to any host).
  Intentional hardening, Q8.
- `LAUNCH_WITH_AWS_IDC_ISSUER_URL` keeps the Python rules (https, host equal to or ending in
  `.amazonaws.com` / `.awsapps.com`) and additionally refuses userinfo, a port outside 1..65535,
  whitespace and control characters; the warning line is kept on stderr as
  `Using non-default IdC issuer URL: <shown_value(url)>`.

## 5. Archive boundaries (AC1, AC2)

Two directions: building the upload (parity port of `zip_local_repo`, inputs are the user's own
files) and, if Q1 is accepted, extracting the migrated snapshot (new command, input is untrusted).
Both share one ZIP reader with a strict policy.

### R-AR-SEL. Source selection (upload)

- `~` and `~/...` expand from `HOME` (Python `expanduser`; `~user` is not expanded and is a
  literal path, recorded divergence). The root must be a directory (`metadata`, following a symlink
  at the root as `os.path.isdir` does).
- The walk never follows a symlink below the root (directories or files), skips every non-regular
  file (FIFO, socket, block and character devices: intentional, the Python read them and could hang
  on a FIFO), applies `_SKIP_DIRS`, `_SKIP_FILES`, `.env.*`, suffix and prefix rules exactly
  (tables copied with a parity test per entry), and visits entries in byte order of their names
  (deterministic; the Python used filesystem order, which the server does not depend on).
- A path that is not valid UTF-8 → `ArchiveError` ("Archive cannot include a file whose name is not
  valid UTF-8: <lossy shown_path>"); the Python failed with the generic message on surrogates.
- Each opened file is re-checked after `open` (`File::metadata` must be a regular file, same size
  as walked) so a file swapped for a FIFO between walk and read is refused, not read.

### R-AR-GIT. Git ignore rules

- Same three Git paths and messages as section 2.5, run as `git` from `PATH` with `current_dir =
  root`, `stdin = null`, stdout/stderr piped and capped (stdout 256 MiB, stderr discarded), a 30 s
  wall-clock limit per call (a watcher thread kills the `Child` handle), and
  `-c core.fsmonitor=false` prepended as defence in depth (a repository's own `.git/config` can name
  an fsmonitor hook command, and the option keeps any index refresh from running it; `ls-files`
  runs no other hook and makes no network access; Python set nothing). Git's own `safe.directory`
  refusal of a repository owned by another user stays an `ArchiveError` (parity).
  `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and `GIT_CONFIG_*`
  inherited from the caller are removed from the child environment so the selection always
  describes `root`.
- Temporary bare repository: created in a fresh private temporary directory (0700, random name),
  removed afterwards.
- Output paths are NUL-separated bytes; non-UTF-8 → the R-AR-SEL error.

### R-AR-ZIP. Building the zip

- Deflate (level 6, `miniz_oxide`, Q2) per entry, stored as `<prefix>/<rel>` with `/` separators,
  UTF-8 names with general-purpose flag bit 11, version-made-by Unix, `external_attr = mode << 16`
  limited to `0o100644`/`0o100755` (regular-file type, permission from the user-exec bit; never
  setuid/setgid/sticky or world-writable), DOS time from `mtime` clamped to 1980-01-01..2107
  (Python raised before 1980), CRC-32 (IEEE, table-driven, tested against the standard check value
  `0xCBF43926`), ZIP64 records when the entry count exceeds 65 535 or an offset exceeds 4 GiB.
- Limits are enforced **while building**, not after, with the Python's messages: entry count
  100 000; per-entry uncompressed 100 MiB (checked from `metadata` before reading and again on the
  bytes read); per-entry ratio 100:1 (after compressing that entry, compressed size > 0); total
  uncompressed 2 GiB; compressed archive 500 MiB (running). Memory is therefore bounded by the
  500 MiB archive plus one entry, as in the Python's success case, and a pathological tree fails
  early instead of after the whole archive is in memory.
- Then the Python checks on the finished bytes run through the R-AR-READ reader (defence in depth;
  it also proves writer and reader agree): safe names, no link entries, single top-level directory.
- `sanitize_root_name` is ported with Python's Unicode `\w` (alphanumeric per Unicode or `_`;
  parity fixtures for ASCII, Latin-1, CJK, combining marks and digits from other scripts).

### R-AR-READ. The ZIP reader policy (shared)

Every check fails closed with a fixed `ArchiveError` that names the entry through `shown_value`:

- Locate the end-of-central-directory record in the last 65 557 bytes; exactly one EOCD signature
  candidate whose comment length reaches the end of the data, else invalid. ZIP64 locator and record
  honoured. Multi-disk archives → refused. Central directory must lie inside the data.
- Entry count ≤ 100 000 (checked from the EOCD **before** reading entries), central-directory size
  ≤ 64 MiB.
- Per entry: compression method 0 (stored) or 8 (deflate) only; encrypted (flag bit 0 or strong
  encryption bit 6) → refused; the local header at the recorded offset must exist, carry the same
  name bytes, method and flags as the central record; the byte ranges `[local header, end of
  data)` of all entries must not overlap and must lie before the central directory (blocks
  overlapping-entry "quoted" zip bombs).
- Names: UTF-8 (flag bit 11, or pure ASCII); non-UTF-8 CP437 names → refused. Refused: empty, NUL
  or other control characters, `\` anywhere, a leading `/`, a drive prefix (`[A-Za-z]:`), `.` or
  `..` segments, empty segments (`a//b`), a segment longer than 255 bytes, a name longer than
  4096 bytes. On Windows targets additionally `:` and reserved device names (`CON`, `NUL`, `AUX`,
  `PRN`, `COM1-9`, `LPT1-9`, any extension) and segments ending in `.` or space.
- Duplicates: two entries whose names are equal after Unicode NFC normalisation and simple case
  folding (`unicode-normalization` is already a `toolkit-cli` dependency) → refused, on every
  platform (case-insensitive and normalising file systems would otherwise merge them).
- Type: when version-made-by is Unix (3), the `external_attr` high 16 bits: symlink (`S_IFLNK`),
  character or block device, FIFO, socket → refused; a regular file or directory (or type 0) is
  accepted. A name ending in `/` is a directory and must have size 0. ZIP has no hardlink entry
  type; an Info-ZIP "UNIX extra field" or any extra field claiming a link target is ignored (it
  never becomes a link because the extractor only ever creates regular files and directories).
- Sizes (declared, from the central directory): per entry ≤ 100 MiB, ratio ≤ 100:1, running total
  ≤ 2 GiB. While inflating (extraction only), the actual output is counted and must equal the
  declared size exactly (stop reading at declared + 1 byte), and the CRC-32 must match.

### R-AR-EXT. Safe extraction (new `launch download-snapshot`, Q1)

- `toolkit launch download-snapshot <launch-id> [--output DIR] --region R`:
  1. `GetLaunch include=download_url`; no URL → the Python's "Download URL not available yet..."
     message.
  2. Download over the shared HTTPS `Transport`: https only, no userinfo, host allowlist
     `.amazonaws.com` and `.api.aws` (Q19: confirm the real host in the operator's live run), the
     public-address guard, no redirects (a 3xx is an error naming the status only), 300 s timeout,
     body cap 500 MiB (declared `Content-Length` checked first, running count always). The URL is
     never printed in an error.
  3. The bytes go to a private temporary file (`create_new`, 0600) inside a fresh private staging
     directory, never a fixed `/tmp` name.
  4. Validate with R-AR-READ (no single-root requirement: the snapshot is a repository root).
  5. Extract into the staging directory `<parent>/.launch-snapshot-<random>.partial/` created with
     `create_dir` and mode 0700 on the same file system as the destination. Directories are created
     one level at a time with `create_dir` (never `create_dir_all` through existing components);
     files with `create_new` (O_EXCL), so a duplicate or pre-existing path fails instead of being
     overwritten. The staging tree contains only what the extractor created, and it never creates a
     symlink, so no component can redirect a later write (no traversal through links). Files 0644,
     or 0755 when the entry's Unix mode has the owner-exec bit; directories 0755; no setuid,
     setgid, sticky or world-writable bit, no ownership change, no timestamps restored.
  6. Atomic placement: `rename(staging, destination)` after a check that the destination does not
     exist. Default destination: a fresh private directory under `std::env::temp_dir()`
     (`launch-snapshot-<id-safe>-<random>`), so the default never races with other users. With
     `--output DIR`, `DIR` must not exist and its parent must exist; `--output -` is refused by the
     #4780 router policy, `./-` is a literal path. Residual R-1: `rename` replaces an **empty**
     directory created at `DIR` between the check and the rename on Linux (there is no
     `RENAME_NOREPLACE` in `std`); an attacker needs write access to the parent, and the content
     placed is the validated snapshot.
  7. Any failure removes the staging directory with `std::fs::remove_dir_all` (symlink-safe since
     Rust 1.58.1) and the temporary archive.
  8. Output `{"outputDir": "<path>", "files": N, "directories": M, "bytes": B, "region": R}`.
- `SKILL.md` step 8A becomes this one command; step C keeps `rsync -a <outputDir>/ .` (now from a
  private, link-free tree). The prose rewrite ships in the cutover slice.

### R-AR-PERM. Permissions summary

Upload: the archive records 0644/0755 only. Extraction: as R-AR-EXT step 5; the staging root and
default destination are 0700. The process umask is not changed (no `umask` in safe `std`; explicit
modes are set with `OpenOptionsExt::mode`/`DirBuilderExt::mode`, which the umask can only narrow).

## 6. API client failure handling

### R-API-HTTP. Requests

- Endpoint: `https://launch-with-aws.<region>.api.aws`, or `LAUNCH_WITH_AWS_BASE_URL` validated by
  `toolkit_aws::endpoint::validate_endpoint` with `EndpointPolicy::aws_public()` (stricter than the
  Python as recorded in `aws-core-contracts.md`: no userinfo, query or fragment; the wider
  non-public range table). DNS uses `SystemResolver`.
- Requests through `toolkit_aws::client::ApiClient` with a `BearerSigner` (adds
  `authorization: Bearer <token>`; token fetched once per command by `get_access_token`, so a
  refresh happens at most once), `Timeouts` 120 s/120 s, the shared `RetryPolicy` default
  (3 retries, throttling/5xx/connect/timeout; `Adaptive` label as documented).
- Paths: `/api/uploads`, `/api/launches`, `/api/launches/{id}`, `/api/launches/{id}/start`; `{id}`
  is `toolkit_aws::transport::percent_encode`d (so `/` becomes `%2F`, as botocore's non-greedy
  label). `include` is sent as repeated `include=<value>` query members, values split on `,` and
  stripped (parity; the server validates the enum).
- Idempotency: one UUID v4 (from `getrandom`) per command for `clientToken` on `CreateUploadUrl`,
  `CreateLaunch`, `StartLaunchExecution`, reused across retries (botocore parity, section 1.4.8).
- Local parameter validation **before any request** with fixed messages: launch id 1..2048
  characters, name 1..128 characters (Python: generic message, and for `create-launch` only after
  the upload). `create-launch` also obtains the token **before** building the archive, so an
  expired session fails before minutes of zipping. Both are recorded divergences (D9).
- Response bodies are capped at 64 MiB (Q12) and must be UTF-8 JSON objects; anything else is
  `InvalidResponse` ("The Launch with AWS service returned an invalid response.").
- Presigned PUT: the `uploadUrl` is validated (https, no userinfo, host ends with
  `.amazonaws.com`, public-address guard), sent as one `PUT` with `Content-Type: application/zip`,
  300 s timeout, **no retry** (parity with `urllib`), no redirect.

### R-API-ERR. Error mapping (stderr `{"error": ...}`, exit 1)

| Condition | Message |
| --- | --- |
| HTTP ≥ 400 | `ServiceError` text (`POST /api/launches/<id> failed (404): ResourceNotFoundException`) + the 401 or 404 hint exactly as section 1.3 |
| connect failure / timeout after retries | "Could not reach Launch with AWS in {region}." (Python: botocore text naming the URL, or the generic message) |
| invalid response | as above |
| session missing/expired | "Session expired or not authenticated. Run auth-start to sign in." |
| endpoint/issuer/region configuration | the Python `ConfigError` text with caller values through `shown_value` |
| archive | the Python `ArchiveError` text (R-AR-*) |
| transport unavailable (default build without `aws-sdk`) | "This toolkit build has no network support; install a build with the aws-sdk feature." (fixed; Q20) |
| any internal invariant | "The operation failed. Re-run with logging enabled for details." (kept for the unexpected) |

`list-launches` keeps the per-region rules of section 1.3; an `errors[region]` value is the same
text as the table (never a URL). `nextTokens` are printed as received (stdout data).

### R-API-SHAPE. Model-driven output

`launch/shape.rs` embeds `references/launchwithaws-2026-06-15.json` with `include_str!` and
reproduces section 1.4 points 1-6: modelled members only, model order, nulls dropped, timestamps
rendered as Python `str(datetime)` (RFC 3339 input with `Z` or an offset; a numeric epoch rendered
in UTC; unparsable → `InvalidResponse`), floats with Python `repr` (reuse the existing
`json.dumps` emulation, section 7.3), unions with `SDK_UNKNOWN_MEMBER`. One divergence from botocore
under the #4727 rule: a `null` or wrongly typed **required** member (for example `items`) is an
invalid response instead of being dropped and read as empty (D10).

## 7. Rust layout, slices and dependencies

### 7.1 Modules

One slice module, `crates/toolkit-cli/src/launch/` (slice name `launch`), registered in
`registry.rs` `SLICES` between `crate::elasticache::SLICE` and `crate::manifests::SLICE`; not added
to `LEGACY_ROUTING_ORDER` (new routes follow the pinned legacy order); description and synopsis
ranks `NEW_RANK`; `doc: "docs/migration/launch-with-aws.md"` (the slice's user-facing page written
in L5).

| Module | Contents |
| --- | --- |
| `launch/mod.rs` | `SLICE`, the 13 routes (`launch resolve-region`, `auth-start`, `auth-wait`, `auth-callback` (internal), `session-status`, `sign-out`, `create-launch`, `get-launch`, `list-launches`, `delete-launch`, `start-launch-execution`, `get-launch-status`, `get-launch-download-url`; plus `download-snapshot` if Q1), output and error rendering |
| `launch/config.rs` | constants, regions, region chain, AWS profile region reader, base/issuer URL validation |
| `launch/state.rs` | state directory, private atomic writes, session and region files, pending-flow records and locks |
| `launch/oidc.rs` | PKCE, state, `RegisterClient`, `CreateToken` (code, refresh), `get_access_token`, `session_status`, `sign_out` |
| `launch/callback.rs` | request parser, validation, response pages (pure, no sockets) |
| `launch/flow.rs` | `auth-start` spawn, `auth-callback` child loop, `auth-wait` |
| `launch/api.rs` | `BearerSigner`, the six operations, presigned PUT, error mapping and hints |
| `launch/shape.rs` | embedded-model response shaping |
| `launch/archive/select.rs`, `git.rs`, `zip_write.rs`, `zip_read.rs`, `extract.rs`, `crc32.rs` | R-AR-* |
| `launch/clock.rs` | `Clock`, `Random` seams (production: system time, `getrandom`) |

Seams for offline tests: `Transport` (`toolkit_aws::testing::MockTransport`), `HostResolver`
(`FixedResolver`), `Env` (`MapEnv`), `Clock`, `Random` (seeded fake), state directory, `git`
program path and a `Spawner` for the helper.

Router facts the port must live with (recorded divergences D1-D4):

- flags are parsed only after `toolkit launch <command>`, anywhere among the positionals (Python:
  also before the command);
- `--region` and `--aws-mcp-url` are `value_flags` with `strict_values: true` (an empty or
  dash-leading value is a usage error, exit 2; Python took the next word);
- unknown flags, extra positionals, a `--` separator and a bare `-` are router usage errors (exit 2,
  `error:` text + usage), and `-h`/`--help` anywhere prints help (Python: unknown dash words were
  positionals, extra positionals hit the generic message, help only first);
- **missing** required positionals are checked by the handler so the Python's
  `{"error": "Missing required argument for <cmd>"}` exit 1 is kept;
- `toolkit launch` alone or an unknown launch command is the router's unknown-command usage error
  (exit 2) instead of the Python's usage on stdout/exit 0 and stderr/exit 1.

#4780 bare `-`: no launch parameter enters `STDIO_DASH` or `LITERAL_DASH`. `<dir>` (a path; `./-`
works), `[name]` (Q11), launch ids, `include`, `<pid>`, `--region`, `--aws-mcp-url` and `--output`
refuse `-` through the router; the `diag_contract` enumeration covers them automatically.

#4751 inventory: add `("launch", Class::Recorded)` to `recorded_contract.rs` `INVENTORY` and import
`toolkit_aws::recorded` for every response-member read (`optional_text`, `optional_object`,
`optional_object_list`, `next_token_from`); the model-driven walk in `shape.rs` carries
`// recorded-shape:` markers with specific reasons (Q3).

### 7.2 Slices (tests first in each)

Each slice is one verified commit set: tests written first, then code, then the daemon
`scoped_test` job, fmt and clippy. Network-free throughout.

- **L0. Shared primitives** (`toolkit-core`)
  - Tests: T-SHA-1 (FIPS 180-4 vectors through a new raw `sha256(&[u8]) -> [u8; 32]`, existing
    `sha256_hex` unchanged), T-B64-1 (RFC 4648 section 10 base64url vectors, no padding), T-PKCE-1
    (RFC 7636 Appendix B: verifier `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk` → challenge
    `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM`).
  - Code: raw digest export, `base64url_nopad`.
- **L1. Configuration and local state**
  - Tests: T-REG-1..8 (every region-chain branch, source labels, `aws-mcp` lookalike hosts
    `aws-mcp.eu-west-1.api.aws.evil.com`, `xaws-mcp...`, profile region from `AWS_CONFIG_FILE`
    fixtures incl. `[profile x]`, `[default]`, malformed files → no signal); T-URL-1..6 (base URL and
    issuer: scheme, host suffix, userinfo, non-public DNS answers via `FixedResolver`, caller value
    redacted in the message with a `https://u:FAKE_SECRET@x.api.aws` input); T-STATE-1..8 (0700 dir,
    0600 file, atomic replace leaves no `.tmp` on failure, symlinked `session.json` treated as
    absent and `sign-out` removes only the link, 0644 file tightened, oversize and malformed
    files → no session, Python-written session fixture read back and rewritten byte-identically).
- **L2. Archive build and reader**
  - Tests first: T-AR-SEL-1..10 (each skip table entry, symlinked file and directory skipped, FIFO
    skipped without blocking (a test FIFO created by `mkfifo` through `std::process::Command`, Unix
    only), non-UTF-8 name error, deterministic order, `~` expansion); T-AR-GIT-1..6 (a temporary
    repository built with the `git` binary: tracked, untracked, ignored-but-tracked, sub-directory
    root, non-repository with `.gitignore`, `core.fsmonitor` in `.git/config` pointing at a script
    that would create a marker file: the marker never appears; git missing via a non-existent
    program path; timeout via a fake `git` script that sleeps, killed at the limit in a test-only
    shortened limit); T-AR-ZIP-1..8 (round trip through the reader, CRC check value, mode mapping,
    pre-1980 mtime clamped, ZIP64 at 65 536 entries, each limit and its exact Python message,
    `sanitize_root_name` parity table); T-AR-READ-1..20 (fixture archives built in the test from
    bytes: traversal `../x`, `a/../../x`, absolute `/etc/x`, `\x`, `C:x`, `a\..\x`, NUL, control
    characters, `.` segments, empty segments, overlong names, duplicates by case and by NFC,
    symlink/FIFO/device modes, encrypted flag, unknown method, local/central name mismatch,
    overlapping entries, declared-size and ratio bombs, entry-count bomb refused from the EOCD
    before any entry is read, truncated and multi-EOCD archives).
- **L3. OIDC and token lifecycle**
  - Tests: T-OIDC-1..10 over `MockTransport` (exact `RegisterClient` and `CreateToken` request
    bodies and paths; no `authorization` header; response missing tokens; client-lifetime cap;
    `expiresIn` absent/0/negative/huge; refresh keeps the old refresh token when none is returned;
    refresh failure → `SessionExpiredError` text; OIDC error bodies whose `error_description`
    carries `FAKE_SECRET` never reach any output; `AWS_ENDPOINT_URL*` in `MapEnv` is ignored);
    T-TOK-1..6 (every output and error string of the slice, collected by running every command
    path against fakes seeded with `FAKE-ACCESS`, `FAKE-REFRESH`, `FAKE-CLIENT-SECRET`,
    `FAKE-VERIFIER`, `FAKE-CODE`: none of them occurs in stdout, stderr, the pending record, the
    helper argv or the helper environment; `Debug` of every secret-holding type).
- **L4. Callback and sign-in lifecycle**
  - Tests: T-CB-1..18 on `callback.rs` without sockets (request-line and head-size limits, method,
    path, Host header, form decoding, repeated parameters, wrong/missing state with `code` and with
    `error` → ignored, right state with `error` → failed with the code only, `error_description`
    never stored or reflected, invalid code characters, constant-time comparison used (a counter in
    the comparison helper), fixed headers and exact `Content-Length`); T-CB-NET-1..8 on a real
    loopback listener (bound address is `127.0.0.1`, port non-zero; a second callback after success
    is refused at TCP level; the accept cap; read timeout on a silent client; deadline expiry with a
    test clock; pending record states; lock held while alive and free after exit; stale record
    cleanup); T-FLOW-1..8 at binary level using `CARGO_BIN_EXE_toolkit` (`auth-callback` with
    valid stdin prints one port line and serves; empty, oversized, invalid stdin → exit 1 without
    binding; helper argv contains no secret (read from `/proc/<pid>/cmdline` on Linux, skipped
    elsewhere); `auth-wait` for an unknown pid; helper exit without session; timeout message;
    success when a session file appears; the exchange path in the default build ends `failed` with
    reason `transport_unavailable`).
- **L5. API client, shaping and the commands**
  - Tests: T-API-1..14 over `MockTransport` (paths, percent-encoded ids, repeated `include`,
    bearer header, `clientToken` present and identical across a 503-then-201 retry, no retry of the
    presigned PUT, PUT URL validation refusals, PUT failure text without the URL, 401 and 404 hints
    with and without `LAUNCH_WITH_AWS_BASE_URL`, local id/name validation before any request,
    token checked before zipping, response cap, invalid JSON); T-SHAPE-1..8 (unmodelled member
    dropped, model order, `null` dropped, timestamp rendering with and without microseconds and
    with an offset, float `12` → `12.0`, union unknown member, required `null` list → invalid);
    T-CMD-1..13 golden stdout/stderr/exit per command and branch from section 1.3, including
    `list-launches` partial failure, all-failed connect, all-failed `ApiError`, `--region`, the
    `create-launch` URL refusal with a userinfo URL redacted, best-effort `save_region` failure;
    T-CLI-1..6 (routes registered and documented, `every_slice_documents_its_commands`, usage
    re-pin, bare `-` refusals, missing-argument JSON, `#4751` inventory entry, #4941
    `diag_contract` cases).
  - Docs: `docs/migration/launch-with-aws.md` (commands, divergences D1-D14, residuals), the
    `docs/migration/README.md` row.
- **L6. Snapshot download and extraction** (only if Q1 is accepted)
  - Tests: T-EXT-1..14 (every R-AR-READ fixture refused **with nothing created** at the destination
    and the staging directory removed; a valid snapshot lands atomically with the exact modes;
    destination exists → refused before download; a parent that is a symlink is followed only at
    the destination path the caller named, never inside the tree; size bomb stopped at declared + 1
    byte; CRC mismatch; download: non-https, userinfo, disallowed host, 3xx, oversize
    `Content-Length`, oversize body without `Content-Length`, error text without the URL).
- **L7. Security review** at the exact SHA of L0-L6 by the opposite model family, checklist =
  sections 3-6 and the T- list (AC1 for the implementation; this document is the pre-review input).
- **L8. Cutover** (with #4414/#4487/#4415 and after the operator's live run, section 9): rewrite
  `SKILL.md` invocation (`toolkit launch <command>`), the required-files list, the dependency line,
  step 8; resync the `plugins/aws-core` mirror through `toolkit sync`; retire the six scripts and
  their mirrors in the retirement slice once callers and tests are verified.

### 7.3 Dependencies

Prefer what the workspace compiles already; every addition is pinned with `=`.

| Crate | Status | Why |
| --- | --- | --- |
| `toolkit-aws` (workspace) | existing | endpoint validation, `Transport`/`ApiClient`/`RetryPolicy`, `redact`, `Secret`, `ServiceError`, `recorded`, testing doubles |
| `toolkit-core` (workspace) | existing, extended in L0 | JSON, SHA-256 (raw digest added), base64url |
| `unicode-normalization` | existing direct dependency of `toolkit-cli` | NFC duplicate detection in R-AR-READ |
| `getrandom` `=0.3.4` | **new direct**, already compiled in the default build (transitively, `cargo tree -p toolkit-cli`) | OS CSPRNG for PKCE verifier, state, `clientToken` UUIDs and temporary names; `std` has no stable CSPRNG |
| `miniz_oxide` (exact version fixed at implementation, 0.8.x or 0.9.x already in the local registry) | **new**, pure Rust, one transitive crate (`adler2`) | raw deflate/inflate with bounded output buffers; hand-writing deflate is out of the question (Q2) |
| `toolkit-cli` feature `aws-sdk` | from #4413 S1 (accepted design `fd7e7ffd`) | the real HTTPS `SdkTransport` with R-ROOTS; the default build uses an `Unavailable` transport like `asm exec` |

Not added: an HTTP server crate (the callback needs one request line; `std::net` is enough and
smaller to review), a URL crate (`toolkit-aws` already parses and validates URLs), `zip`
(alternative to `miniz_oxide` + own container code, Q2), `zeroize` (strings are dropped; the
guarantee it adds over that is small and it would touch every secret type), `webbrowser` (Q4),
`libc`/`nix` (no signals needed, R-CB-WAIT), `tempfile` (private directories are three lines with
`DirBuilderExt::mode` and `getrandom` names).

Python `json.dumps(indent=2)` emulation: the workspace has several slice-local copies
(`aurora/pyfmt.rs` `dumps`, `ai_data/mod.rs` `dumps`, `bedrock_agent.rs`, `dynamodb/pyfmt.rs`
compact `dumps`). L5 reuses `aurora::pyfmt::dumps` (indent 2, `ensure_ascii`, Python float repr)
rather than adding a fifth copy; promoting one shared writer is filed as a kaizen (see the
handoff).

## 8. What is offline-testable and what needs the operator's live run

Offline (default daemon test job, no network, no AWS, no browser): every T- test above. Loopback
sockets on `127.0.0.1` are used by T-CB-NET and T-FLOW only (as #4413's T-TLS harness); the `git`
binary is used by T-AR-GIT on temporary repositories; `mkfifo` by one Unix-only test. Nothing reads
the real `HOME`, `~/.aws` or environment (injected seams, `MapEnv`).

Needs the operator's live run (an operator to-do filed by the PM after L5, a disposable sample app,
never CI):

1. Real IAM Identity Center sign-in in a browser through `auth-start`/`auth-wait`
   (`RegisterClient`, the authorize page accepting `http://127.0.0.1:<port>` and the 43-character
   base64url state, `CreateToken`), in both an interactive desktop and a headless/SSH session.
2. `session-status`, silent refresh after expiry (or by editing `token_expires_at` in the session
   file), `sign-out`, and reuse of a session written by the Python scripts.
3. `resolve-region`, `create-launch` of the sample app in `us-east-1` and `eu-central-1`
   (presigned upload host recorded), `get-launch-status` polling, `get-launch` with every include
   section (compare timestamps and floats with the Python output of the same launch, captured by
   the operator), `start-launch-execution`, `get-launch-download-url`, `download-snapshot` (host
   of the download URL recorded for Q19), `list-launches` in both regions, `delete-launch`, and a
   404 with a wrong `--region`.
4. A 401 path (corrupt `access_token` in the session file).

The live run creates and deletes launches in the operator's account; the skill states execution is
at no cost to the user, but any spend or data upload stays an operator decision.

## 9. Divergences recorded for the slice page (D) and residuals

- D1-D4: router facts in section 7.1 (flag placement, strict values, usage errors exit 2, `toolkit
  launch` alone).
- D5: state moved from argv to stdin; 32-byte state, base64url (R-CB-SPAWN, R-CB-STATE).
- D6: callback accepts only `/`, checks `Host`, ignores wrong-state requests including `error`, never
  reflects `error_description`, extra security headers (R-CB-REQ, R-CB-PAGE).
- D7: `auth-wait` never signals; specific timeout/exit messages instead of the generic one
  (R-CB-WAIT).
- D8: OIDC endpoint fixed, `AWS_ENDPOINT_URL*` ignored; issuer URL rejects userinfo (R-TOK-OIDC).
- D9: local id/name validation and token check before the upload work (R-API-HTTP).
- D10: `null` required members are invalid responses (R-API-SHAPE).
- D11: archive: non-regular files skipped, deterministic order, pre-1980 clamp, limits enforced
  while building, non-UTF-8 names refused, Git hardening options (R-AR-*).
- D12: no HTTP proxy and no `AWS_CA_BUNDLE`; trust roots per #4413 R-ROOTS (Q7).
- D13: connect failures read "Could not reach Launch with AWS in {region}." (R-API-ERR).
- D14: `download-snapshot` is new; `parse_github_url`/`download_github_zip` are not ported (Q1, Q10).

Residuals: W-1 Windows file permissions (R-TOK-MODE); R-1 rename over an empty racing directory
(R-AR-EXT); L-1 advisory locks on network home directories (NFS) may not reflect liveness, in which
case `auth-wait` falls back to the recorded deadline; S-1 the bearer token and secrets live in
process memory as ordinary strings until dropped.

## 10. Open questions, each with the recommended answer

- **Q1. Port extraction as a new command?** Recommended: **yes**, L6 `download-snapshot` with
  R-AR-EXT, and rewrite `SKILL.md` step 8A in L8. AC2 asks for "safe extraction" tests and the only
  extraction today is unsafe shell prose (section 2.6). If declined, AC2's extraction tests apply
  to the R-AR-READ validator only and the prose stays.
- **Q2. ZIP implementation.** Recommended: `miniz_oxide` for deflate plus an in-house container
  writer and strict reader (about 600 lines with the policy), because the reader's job is to refuse
  everything unusual (local/central mismatch, overlaps, CP437, ZIP64 oddities) and a small reader
  written to that policy is easier to review than adapting a general reader; the `zip` crate
  (2.x/6.x in the local registry) is the alternative, never with its `extract` helpers.
- **Q3. #4751 inventory class.** Recommended: `Recorded` (responses are external input and the
  #4727 null/type rules should apply), with markers in `shape.rs`. Alternative: `NotRecorded` with
  the reason "live Launch API responses shaped by the bundled service model".
- **Q4. Launch a browser?** Recommended: **no** (parity, headless agents, R-CB-BROWSER).
- **Q5. Move the state off argv?** Recommended: **yes** (D5).
- **Q6. Ignore wrong-state `error` callbacks instead of ending the flow?** Recommended: **yes** (D6).
- **Q7. HTTP proxy and custom CA bundle.** Recommended: not in this port; follow #4413's Q4
  follow-up for all toolkit HTTPS traffic (D12). Users behind a mandatory proxy cannot use the Rust
  build until then; recorded in the slice page.
- **Q8. Ignore `AWS_ENDPOINT_URL*` for OIDC?** Recommended: **yes** (D8).
- **Q9. User-Agent.** Recommended: send `md/awslabs#launch-with-aws#0.1.0` on OIDC calls (parity
  with `user_agent_extra`) and on launch API calls, inside the #4413 Q3 User-Agent policy.
- **Q10. Port `download_github_zip`?** Recommended: **no**; it has no caller. Retire it with the
  scripts.
- **Q11. A display name of `-` or starting with `-`.** Recommended: accept the router refusal (exit
  2) as D1; the agent chooses the name and the default is the directory basename.
- **Q12. Response body cap.** Recommended: 64 MiB for API responses (task logs are the largest
  member), 500 MiB for the snapshot download; streamed through #4413's `StreamingTransport` when it
  exists, otherwise collected under the cap.
- **Q13. Session-file compatibility.** Recommended: **yes**, byte-compatible schema both ways
  (R-TOK-STORE).
- **Q14. Windows ACLs.** Recommended: rely on the profile directory ACL (W-1), as the Python did.
- **Q15. Extraction destination.** Recommended: new private temporary directory by default,
  `--output DIR` must not exist (R-AR-EXT step 6).
- **Q16. Token revocation on sign-out.** Recommended: no network call (there is no `RevokeToken`
  in the `sso-oidc` model); confirm in live run item 2 that sign-out behaves as before.
- **Q17. Specific messages instead of the generic one** for `auth-wait` timeouts, helper failures
  and connect failures. Recommended: **yes** (D7, D13); the agent can act on them.
- **Q18. Keep `auth-wait <pid>`.** Recommended: **yes**, the positional stays and selects the
  pending record; `SKILL.md` keeps working unchanged until cutover.
- **Q19. Download host allowlist.** Recommended: `.amazonaws.com` and `.api.aws` now; widen only to
  the host observed in the live run (for example a CloudFront domain), never to "any https".
- **Q20. Default build without network.** Recommended: the launch slice compiles in the default
  build with the `Unavailable` transport (as `asm exec`), offline commands (`resolve-region`,
  `session-status`, `sign-out`) work, network commands fail with the fixed message; the shipped
  binary for cutover is built with `aws-sdk` (#4413 S1, #4487).
