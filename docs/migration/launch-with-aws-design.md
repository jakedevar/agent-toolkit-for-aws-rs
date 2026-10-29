# Launch with AWS: Rust port design and boundary pre-review (#4382, phase 1)

Status: design, revision 3 (pre-review #5038 round 1 at `87a5543` and round 2 at `4d6f61c`
answered; tables "Pre-review r1 resolutions" and "Pre-review r2 resolutions" below). No code
changes. Programme #4369, slice key `launch`, wave 2,
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
  `RevokeToken` (relevant to `sign-out`, section 2.4);
- revision 2 only: the #5017 (#4413 S1) commit `be0a06df33a2985e96af3444f9a10e56e1efc668`
  (not yet on rolling at `87a5543`), for `toolkit_aws::stream` (`StreamingTransport`,
  `ResponseStream`, `StreamLimits`, `ChunkLimiter`, `StreamError`), `SdkTransport`'s
  `StreamingTransport` implementation and `toolkit_aws::testing::FakeStreamingTransport`, and for
  its pin `rustix = "=1.1.5"` (a `cfg(unix)` dependency of `toolkit-aws` behind the `sdk`
  feature). The `rustix` 1.1.5 sources in the local registry were read for `openat`, `statat`,
  `fstat`, `mkdirat`, `unlinkat`, `Dir` and `renameat_with` (`cfg(any(apple, linux_kernel))`);
- revision 2 only: the PM rulings on #4382 (sequence 2, and the sequence-3 amendment putting a
  strict ZIP64 subset in scope for writer and reader) and the ten #5038 findings #5039-#5047, #5051.
- revision 3 only: the #5038 round-2 verdict and its six findings #5061-#5066; the PM rulings on
  revision 2 (Q21 yes, D18 yes, Unix-only accepted with `rustix =1.1.5`, L3 after S1, live-run ZIP
  construction record); and, again from the local Git object `be0a06df` (read, not built),
  `toolkit-aws` `stream.rs` (`StreamLimits::new` refuses a zero bound; limits are taken by
  `open` before the status exists; `ChunkLimiter` keeps the raw frame it admitted),
  `sdk.rs` (`SdkStream` captures `request.timeouts.read()` once at `open` and uses it for every
  `body.next()`; `connector_for` caches one connector per distinct `Timeouts`; `GuardedResolver`
  resolves with a blocking `to_socket_addrs` inside `DnsFuture::ready`), `transport.rs`
  (`Timeouts::new` refuses zero and values over one hour), `error.rs` (`TimeoutPhase` is
  `Connect | Read`) and `testing.rs` (`FakeStreamingTransport`, `FakeChunk`). The only
  `ResponseStream` implementations at that commit are `SdkStream` and the fake, both in
  `toolkit-aws`.

## Pre-review r1 resolutions

Pre-review #5038 (Codex, CHANGES_REQUESTED at `87a5543`) filed ten findings. Each is closed by a
requirement in this revision and by offline tests named here; section 7.2 places the tests in
slices, section 9 records the resulting divergences and residuals.

| Finding | Requirement (section) | Offline tests |
| --- | --- | --- |
| #5039 P1 post-open checks cannot stop a FIFO hang or symlink disclosure | R-AR-OPEN (5): descriptor-relative walk from a held root fd, `openat(O_NOFOLLOW \| O_NONBLOCK \| O_NOCTTY)` on every component, `fstat` type and `(dev, ino)` identity on the opened handle before any read, read bounded by the walked size; Unix only, other targets refused (D16) | T-AR-OPEN-1..9 (injected swap to FIFO, same-size outside symlink, symlinked ancestor, directory swapped after listing, file grown or shrunk, FIFO and socket present at walk time, ordinary tree, root symlink parity, non-Unix platform refusal; every swap case asserts zero upload requests) |
| #5040 P1 overlay can install `.git` control files | R-AR-VCS (5): VCS denylist on normalised components (NFC, case fold, HFS ignorables, trailing dot/space, `:` stream suffix, `GIT~1`) refuses the snapshot before extraction; R-AR-APPLY (5): new `launch apply-snapshot` replaces `rsync -a`, re-walks the tree with the same denylist, never writes through a link, refuses before the first write | T-AR-VCS-1..8, T-APPLY-1..10 (r3: completed by R-AR-META for the actual Git directory, #5061, T-APPLY-11..17) |
| #5041 P2 per-read timeouts do not bound the callback lifetime | R-CB-TIME (3): monotonic flow deadline F, per-connection head and write deadlines, every blocking call capped by the remaining time, exchange grace to a fixed cutoff T = F + 60 s, commit gate and watchdog, terminal status kept for `auth-wait` (r3: "no session write after T" withdrawn; admission vs publication, never-waiting watchdog and lock-based `auth-wait` per #5062; every blocking network call bounded by TA-STREAM-DL per #5065) | T-CB-TIME-1..8 (r3: 7 replaced, 9..14 added) |
| #5042 P2 accept cap lets unauthenticated traffic cancel sign-in | R-CB-LOAD (3): no traffic counter of any kind ends the flow; bounded slots (16), per-slot buffers and deadlines; only a state-matching callback, the deadline or helper failure are terminal; residual R-CB-DOS (delay, never abort) (r3: drain-all accept replaced by a fixed per-turn quantum, #5063) | T-CB-LOAD-1..5 (r3: 4 replaced, 6..10 added) |
| #5043 P2 ZIP framing and exact consumption unspecified | R-ZIP-FRAME (5): checked u64 arithmetic, unique EOCD, exact central count and span, byte-contiguous layout, flag/version/method allowlists, extra-field framing, local/central and data-descriptor consistency, exact stored/deflate consumption with a terminal decoder state, directory syntax and mode reconciliation | T-ZIP-FRAME-1..34 (invariant table in R-ZIP-FRAME) |
| #5044 P2 caps need a bounded transport seam | R-API-BODY (6): every network body read goes through #5017 S1 `StreamingTransport` (`be0a06df`) and a launch `BoundedBody` collector with a per-operation cap, Content-Length pre-check, cumulative count, lifetime deadline and cancellation by drop; `Transport::send` is never used by the slice (r3: positive transport ceilings, status-time narrowing and absolute deadlines through TA-STREAM-DL, #5064/#5065) | T-BODY-1..10 (r3: 9 replaced, 11..16 added; T-SDL-1..8) |
| #5045 P2 error code can echo an opaque bearer token | R-API-CODE (6): codes allowlisted per service (model exceptions plus a fixed AWS protocol list); the printed code is our constant, unknown → `UnrecognizedError`; request id and service message never passed; a final known-secret scrub replaces any message containing a live secret | T-CODE-1..8 |
| #5046 P2 ZIP64 versus 100 000-entry upload parity | R-ZIP-FRAME ZIP64 rules per the PM amendment (seq 3): strict ZIP64 in writer and both reader profiles; the writer emits the ZIP64 EOCD record and locator only at 65 535 entries or more (a classic count field equal to `0xFFFF` is always a sentinel); the 100 000 limit is unchanged | T-ZIP-64-1..10 |
| #5047 P1 staging path replaceable through a writable parent | R-AR-STAGE (5): parent opened once and held, parent ownership/permission policy (refuse group/other-writable without sticky), all creation `mkdirat`/`openat(O_EXCL \| O_NOFOLLOW)` relative to held fds, identity checks, publication by `renameat_with(RENAME_NOREPLACE)` of the held staging tree with before/after identity checks, fd-relative cleanup | T-STAGE-1..9 |
| #5051 P2 DeleteLaunch 204 | R-API-OPS (6): per-operation status and body table from the embedded model; `DeleteLaunch` 2xx with an empty body is success and prints the existing JSON; presigned PUT success never parses a body | T-OPS-1..7 |

## Pre-review r2 resolutions

Pre-review #5038 round 2 (Codex, CHANGES_REQUESTED at `4d6f61c`) accepted the round-1 directions
and filed six findings. Revision 3 changes only what they require. Q21, D18, the Unix-only scope
with `rustix =1.1.5`, the strict ZIP64 amendment, the live-run ZIP construction record and every
round-1 resolution above stand. The round-1 rows above are amended only where a row below says so.

| Finding | Requirement (section) | Offline tests |
| --- | --- | --- |
| #5061 P1 apply denylist misses the repository's actual separate Git directory | R-AR-META (5, new): before any write, `apply-snapshot` discovers the target's real Git directory and common directory with one hardened `git rev-parse`, pins their `(dev, ino)` identities and refuses a target root inside them. It also refuses any planned path whose existing directory has a protected identity or looks like a Git directory to Git itself (`commondir`, or `HEAD` with `objects`/`refs`), and any snapshot directory with that shape. It fails closed when discovery fails, when the target is not a Git work tree, or when `GIT_DIR`-style variables are set. The literal/alias denylist of R-AR-VCS is kept | T-APPLY-11..17 (separate-git-dir under a non-denied in-tree name with `config` `core.fsmonitor` and hook payloads: refused before any change, metadata bytes unchanged, no marker on the following `git status`; common-dir indirection; a normal separate-git-dir overlay succeeds; a target root inside metadata; nested in-tree admin directories and Git-shaped snapshot directories; every fail-closed discovery case; a swap after planning) |
| #5062 P2 commit gate authorises before the cutoff but can publish after it and stall the watchdog | R-CB-TIME (3), commit protocol rewritten: the session bytes are written and synced to a temporary file *before* admission. Admission (CAS `Exchanging → Committing` while `now < T`) is distinguished from publication (one `rename`). The watchdog at `W = T + 2 s` never waits: it either aborts an un-admitted flow or exits at once. A late outcome is made accountable, not denied: `auth-wait` treats the helper's lock as the only proof that publication is over. It never reports a final timeout while the lock is held, and reports a distinct `helper_blocked` outcome instead. The residual filesystem assumption R-CB-FS is recorded | T-CB-TIME-7 (replaced), T-CB-TIME-9..14 (pause after a successful CAS and before `rename` with the clock past `T`, `W` and `cutoff + 5 s`; blocked publication; watchdog record-write failure and block; the CAS race at `T`; the `auth-wait` decision table; no helper child inherits the lock) |
| #5063 P2 drain-all accept loop can starve slots and deadline checks | R-CB-LOAD (3), scheduler rewritten: a turn has at most 8 `accept` calls (accepted or closed, stopping at `WouldBlock`) and then exactly one non-blocking operation per occupied slot in rotating order. The monotonic clock is read before every `accept` and every slot operation, so `F` and slot deadlines are observed at most one non-blocking call late. Each turn lasts at least 2 ms (10 ms when idle). That gives at most 500 turns, 4 000 accepts and 8 000 slot operations per second. No counter ends the flow | T-CB-LOAD-4 (replaced), T-CB-LOAD-6..10 (always-ready scripted listener with a fake clock that advances on every operation: bounded accepts per turn, an already admitted valid callback completes, `F` and head expiry observed within one operation, rate ceilings, rotation fairness) |
| #5064 P2 zero limits are unsupported and status caps are chosen before the status exists | R-API-BODY (6), call sequence rewritten against the pinned S1 API plus prerequisite TA-STREAM-DL: every `open` uses a positive *transport ceiling* (`StreamLimits::new` never sees 0). The status is inspected before any pull. Zero-read outcomes (PUT, `DeleteLaunch` 2xx, every 3xx, download errors) drop the stream with zero pulls and never look at `Content-Length`. Error statuses narrow the limit to 64 KiB through TA-STREAM-DL's `narrow_body_limit` before the first pull. Retained bytes are accounted on both sides of the seam | T-BODY-11..15 (production limit construction for every row; PUT 200/403 and `DeleteLaunch` 204/200-with-body with zero pulls; `DeleteLaunch` 404 collected under 64 KiB; one operation receiving 500 then 200 with the right limit each time; an oversized first raw frame refused before any copy, with retained bytes asserted); T-SDL-4..6 (`toolkit-aws`) |
| #5065 P2 an in-flight S1 read cannot be shortened to the absolute deadline | Named prerequisite **TA-STREAM-DL** in `toolkit-aws` (section 7.2, before L3): `StreamLimits::with_deadline(Instant)`. `SdkTransport::open` runs connect, TLS, request write and head wait under `tokio::time::timeout_at(deadline)`. `SdkStream::next_chunk` waits `min(idle, deadline − now)` and cancels the in-flight `body.next()` by dropping it, returning `StreamError::DeadlineExceeded`. No thread is created. The launch runner gives every attempt `min(attempt start + 120 s, operation deadline)` and stops backoff that would cross it. The one non-cancellable step, the blocking system DNS lookup inside `GuardedResolver`, is recorded as residual R-DNS with its bound rather than claimed | T-SDL-1..3, T-SDL-7..8 (loopback TLS harness: a body read stalled just before the cutoff, a server that never sends the head, a stalled TLS handshake, each returning at the deadline within a slack bound, the connection dropped); T-BODY-9 (replaced) and T-BODY-16 (retries and backoff near the operation bound) |
| #5066 P2 extraction entry limit omits implicit parent directories | R-AR-PLAN (5, new): after validation and before the first `mkdirat`, a materialisation plan is derived from the central directory. It holds one node per unique path (files, explicit directories and every implicit ancestor, merged), with nodes referencing name bytes in the archive buffer. Derivation stops at the first node over the budget of **200 000 nodes** (files are still at most 100 000 by the entry limit). Staging creates only plan nodes, and the cleanup ledger is pre-sized to the plan. `apply-snapshot`'s walk uses the same budget | T-PLAN-1..8 (byte-built valid archives: the #5066 shape refused under an injected budget and under the production budget with zero filesystem calls and planner memory bounded; boundary success; shared-prefix deduplication; explicit/implicit merge; ledger bound on a late failure; apply walk limits) |

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

- Each accepted connection gets an absolute head deadline and an absolute write deadline
  (R-CB-TIME); the server reads at
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
- Connections are served by the bounded slot loop of R-CB-LOAD. Revision 1's 256-connection cap
  that ended the flow (`too_many_requests`) is **removed** (#5042): no count of connections or
  requests ends the flow.

### R-CB-LOAD. Bounded work without a traffic-driven terminal state (#5042)

- The child's single thread runs one loop over a non-blocking `TcpListener` and at most
  **16 connection slots** (`TcpStream::set_nonblocking(true)`). Each slot owns an 8 KiB head buffer,
  a head deadline and, once a response is queued, a write deadline (R-CB-TIME). Memory is bounded
  by 16 × (8 KiB + one fixed page of at most 1 KiB) whatever the traffic.
- **Turn structure (revision 3, #5063).** Revision 2's "accept every pending connection" is
  withdrawn: an accept queue that a flooder keeps refilling never reaches `WouldBlock`, so that
  drain had no finite work quantum. Each turn is now a fixed, finite sequence:
  1. **Clock check.** Read the monotonic clock. At or past `F` the loop ends (`timed_out`).
  2. **Admission quantum.** At most `A = 8` calls to `accept` per turn, stopping early at
     `WouldBlock`. Each accepted connection is set non-blocking and put in a free slot with its
     head deadline. If all 16 slots are busy it is closed at once (no bytes read, no response) and
     **not counted**. Accepted-and-closed connections use the same quantum, so a flood costs at
     most 8 `accept` + 8 `close` calls per turn. The clock is read before every `accept`. Reaching
     `F` ends the turn and the loop.
  3. **Slot service, fair.** Each occupied slot gets **exactly one** non-blocking operation per
     turn: one `read` of at most the free space of its 8 KiB head buffer, or one `write` of the
     rest of its page. Slots are visited in round-robin order starting one slot later each turn,
     so no slot waits more than one turn behind another. Before each slot operation the clock is
     read. A slot past its own deadline is dropped (closed without a response) instead of being
     served, and reaching `F` ends the loop. A slot that completes a state-matching callback ends
     the loop through R-CB-ONCE.
  4. **Pacing.** The turn then sleeps through the injected `Sleeper` until `turn start + 2 ms`
     when any accept or slot operation transferred data or changed a slot. Otherwise it sleeps
     until `turn start + 10 ms`. A sleep never extends past `F` or the earliest slot deadline.
- **What this bounds.** At most 500 turns per second. Per turn at most 8 `accept`, 8 `close`,
  16 slot operations and 25 clock reads. So at most 4 000 accepts and 8 000 slot operations per
  second, whatever the traffic, and about 100 turns per second when idle. Every syscall in a turn
  is non-blocking, so a deadline (`F`, a head deadline or a write deadline) is observed at most
  **one non-blocking call** after it passes, never after a drain. A valid callback already in a
  slot gets one read in every turn (each turn at most 2 ms of pacing plus its bounded work) while
  the listener is flooded, and a
  browser head is normally read in one call. A flood can still keep all 16 slots occupied, and
  then a new browser connection waits for a free slot. That delay (not abort) is residual R-CB-DOS.
- No rate, count or ratio of any traffic is ever a terminal condition. The quantum and pacing
  limit work per unit of time. They never count towards ending the flow, and they keep no
  per-peer state.
- A slot is never kept beyond its head deadline (2 s from accept) plus its write deadline (2 s), so
  a slot frees within 4 s; a peer must keep reconnecting to hold slots.
- Terminal transitions, the complete list: a state-matching callback (`code` → exchange; `error` →
  `authorization_denied`; invalid code → `invalid_code`), the flow deadline (`timed_out`), and a
  local failure of the helper itself (bind, record write, exchange: `failed` with a fixed reason).
  Malformed requests, wrong or missing state, wrong path or Host, slot exhaustion and resets are
  answered or dropped and change **nothing** in the flow state. The status enum has no
  `too_many_requests` member.
- Residual R-CB-DOS (section 9): a local process that can connect to `127.0.0.1:<port>` can keep
  the 16 slots busy and so delay the browser's callback, possibly until the deadline, which ends the
  flow as `timed_out`. It cannot end the flow early, learn the code or state, or make the child use
  more memory or do more work per second than the bounds above. The same local process could
  equally kill the user's
  browser connection; no loopback design prevents that.

### R-CB-STATE. State and PKCE validation

- State: 32 bytes from the OS CSPRNG (`getrandom`), base64url without padding (43 characters).
  Longer than the Python's 16 bytes; the IdP treats it as opaque.
- The state is compared in **constant time** (length check, then XOR-accumulate over bytes).
- A request whose `state` is absent or wrong is answered `400` (fixed page) and **does not end the
  flow**, whether it carries `code` or `error` (Python: `error` ended the flow without any state
  check, and a wrong state with a code ended it; both let any web page that finds the port abort a
  sign-in). Guessing a 256-bit state is infeasible, and R-CB-LOAD bounds the work.
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

### R-CB-TIME. Timeouts and lifetime (#5041)

All deadlines in the child are **absolute monotonic instants** (`std::time::Instant`, read through
the injected `Clock`), never per-call idle timeouts alone. Wall-clock equivalents are written to
the pending record only for `auth-wait`.

- **Flow deadline** `F = start + 600 s` (`CALLBACK_TIMEOUT_SECS`). The slot loop (R-CB-LOAD) stops
  accepting and drops every slot at `F`. A callback is eligible only if its complete head was read
  before `F`.
- **Per-connection deadlines**: head deadline `min(accept + 2 s, F)`; write deadline
  `min(response queued + 2 s, F + 2 s)`. Sockets are non-blocking, so no read or write can block;
  a peer trickling one byte at a time is dropped at its head deadline however many bytes it sends
  (the 8 KiB cap is a second, independent bound).
- **Exchange cutoff** `T = F + 60 s`, fixed at start. Engineering decision: a callback accepted
  before `F` gets a bounded grace period for the token exchange instead of being discarded, because
  the IdP has already issued a single-use code and discarding it near the deadline forces a second
  sign-in. The exchange goes through R-API-BODY's runner. Every attempt carries the absolute
  deadline `min(attempt start + 30 s, T)` in its `StreamLimits` (TA-STREAM-DL, revision 3) with
  fixed `Timeouts` of 30 s/30 s. The runner gives up when the next backoff would end after `T`, and
  no attempt starts after `T`. The blocking DNS lookup that TA-STREAM-DL cannot cancel (R-DNS) is
  covered here by the watchdog, which does not wait for the main thread.
- **Commit protocol (revision 3, #5062).** Revision 2 claimed that a CAS before `T` meant the
  session was published before `T`, and its watchdog waited for `Committing`. Both claims are
  withdrawn. An atomic state change is not an atomic clock-check-plus-rename: a thread can pass the
  CAS and then be descheduled or blocked inside the file system. Revision 3 separates three events
  and states what each one guarantees.
  1. **Prepare** (state `Exchanging`, no admission yet): after a successful exchange, the complete
     `session.json` bytes are written to the temporary file of R-TOK-MODE and `sync_all`ed. Slow
     I/O happens here, before admission, where the watchdog can still abort the flow.
  2. **Admission**: one compare-and-swap of an `AtomicU8` from `Exchanging` to `Committing`,
     attempted only while `now < T`. If `now ≥ T`, or the CAS fails because the watchdog set
     `Aborted`, the temporary file is removed and nothing is published.
  3. **Publication**: exactly one `rename(temp, session.json)` (atomic replace). Then the state
     moves `Committing → Committed`, the record is rewritten `succeeded` and the helper exits 0.
     Admission guarantees that publication *started* before `T`. It does not guarantee that the
     `rename` *returns* before any instant. Revision 3 does not claim that.
- **Watchdog, never waiting.** A watchdog thread started with the helper sleeps until
  `W = T + 2 s`, then:
  - state `Listening` or `Exchanging`: CAS to `Aborted` (from then on admission is impossible),
    one attempt to write status `timed_out` to the pending record, then `Exit(1)`;
  - state `Committing` (admitted, `rename` in flight): `Exit(1)` at once with **no** record write.
    The rename is atomic, so the session file is either the old one or the new one, never partial;
  - state `Committed`: `Exit(0)` at once. The record write of step 3 is itself temp-plus-rename,
    so the record is either `exchanging` or `succeeded`.
  The watchdog never joins, waits for or signals the main thread.
- **Lifetime claim, as far as it can be made.** After `W` the helper starts no new operation:
  `Exit` is called at `W`, or right after the single `timed_out` record attempt in the first case.
  Process exit finishes when the kernel returns from any system call that a thread is blocked in.
  On a local file system a `rename` or a small write returns in milliseconds. On a network or FUSE
  file system it may block without limit, and no user-space design can end such a thread. That is
  residual **R-CB-FS** (section 9). The design does not claim a hard bound in that case; it makes
  the outcome observable (next bullets). A DNS lookup or TLS handshake in the exchange cannot
  extend the helper's life past `W`, because `Exit` does not wait for the main thread.
- **Publication is over only when the lock is free.** The helper holds its exclusive lock
  (R-CB-WAIT) from start to process exit and never closes the lock file early. It spawns no child
  process and opens every file `O_CLOEXEC`, so nothing inherits the lock. The lock is released
  only when the process is gone, and that happens after any in-flight `rename` has returned.
  Therefore **lock free ⇒ this helper can never publish again**, and the session file at that
  moment is final for this flow.
- **Terminal status kept**: after `succeeded`, `failed` (any reason) or `timed_out` the child
  rewrites the pending record with the status and reason, releases its lock and exits; it does
  **not** delete the record. `auth-wait` deletes it after reporting; `auth-start` deletes
  records whose recorded cutoff is more than one hour old and whose lock is free, and stale
  `session.json.<hex>.tmp` files of its own pattern older than one hour.
- The pending record carries `deadline` (wall-clock of `F`) and `cutoff` (wall-clock of `T`).
  **`auth-wait` decision table** (polled every second; "valid session" = R-TOK-STORE parse plus a
  token valid for more than the 300 s buffer; the first matching row wins):

  | Record status | Helper lock | Valid session | Time | `auth-wait` result |
  | --- | --- | --- | --- | --- |
  | any | any | yes | any | `{authenticated:true, baseUrl}` (exit 0) |
  | `failed` | free | no | any | the fixed message for the reason (exit 1) |
  | `timed_out` | free | no | any | "Sign-in did not finish in time. Run auth-start again." (exit 1) |
  | `listening`, `exchanging`, `succeeded` (session since removed or expired) | free | no | any | "Sign-in did not finish in time. Run auth-start again." when the cutoff has passed, else "Sign-in ended without completing authentication. Run auth-start again." (exit 1) |
  | any | held | no | before `cutoff + 5 s` | keep polling |
  | any | held | no | at or after `cutoff + 5 s` | **`helper_blocked`**: "Sign-in has not finished: the sign-in helper is still blocked. Run session-status in a minute; if it does not report a session, run auth-start again." (exit 1). This is not a final timeout |

  A **final** timeout or "ended" result is reported only when the lock is free, so it is never
  followed by a session that this helper publishes later. The one late-publication case (admitted
  before `T`, `rename` blocked past `W`) is reported as `helper_blocked`. The user's later
  `session-status` or `auth-start` then sees the session, if it arrives, as a normal valid session
  that the user did sign in for. A failed or blocked `timed_out` record write does not change any
  row, because the lock is the deciding fact. A wall-clock jump can move *when* `auth-wait` gives
  up waiting, but it cannot turn `helper_blocked` into a final result.
- The child never needs a signal from the parent.

### R-CB-WAIT. Parent/child handshake without signalling a pid

- `auth-start` creates `~/.launch-with-aws/pending/` (0700) and the child creates
  `pending/<pid>.json` (0600: `{"pid", "port", "deadline", "cutoff", "status"}`, status one of
  `listening`, `exchanging`, `succeeded`, `failed`, `timed_out`, plus a fixed `reason` code; never a secret) and
  holds an exclusive lock on `pending/<pid>.lock` for its whole life (`std::fs::File::lock`, stable
  since Rust 1.89; the toolchain is 1.94.1).
- `auth-wait <pid>` (positional kept for compatibility) reads `pending/<pid>.json`. Missing →
  `{"error": "No sign-in is in progress for pid <pid>. Run auth-start again."}`. Then it polls every
  second and answers by R-CB-TIME's decision table (revision 3): a valid session →
  `{authenticated:true, baseUrl}`. A final failure or timeout is reported only once the helper's
  lock is free. A lock still held at `cutoff + 5 s` → the non-final `helper_blocked` message.
- `auth-wait` **never sends a signal**. The Python's `SIGTERM` to a bare pid could hit an unrelated
  process after pid reuse; in Rust it would also need `unsafe` (`libc::kill`), which the workspace
  forbids. The child's own deadline replaces it.
- Stale records (lock free, cutoff more than one hour past) are deleted by the next `auth-start`
  (R-CB-TIME); a record with a terminal status is otherwise kept for `auth-wait`.
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
- **Response side**: OIDC and Launch API errors surface only an **allowlisted** error code
  (R-API-CODE, #5045): the printed code is always one of our own constants, never bytes received;
  the `error_description`/`message` and the request id are dropped (they can echo submitted
  values). Launch API errors keep the Python `ApiError` text shape:
  `"{METHOD} {sanitized path} failed ({status})[: {code}]"`, produced by
  `toolkit_aws::error::ServiceError`, whose path is scrubbed before `<id>` rewriting; the service
  message is never printed (parity). A final known-secret scrub (R-API-CODE) runs on every
  rendered error and `list-launches` `errors` value.
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
  same bounded request runner as the API (R-API-BODY, `StreamingTransport`, 64 KiB cap) with
  `NoSigner` (both operations are `authtype: none`). Authorize URL
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
files) and, Q1 being accepted, extracting the migrated snapshot and applying it (new commands,
input is untrusted).
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
- Every directory listing, metadata read and open below the root goes through R-AR-OPEN.
  Revision 1's "re-check `File::metadata` after a pathname `open`" is withdrawn (#5039): a blocking
  pathname open of a FIFO never returns, and a same-sized symlink or a symlinked ancestor passes a
  leaf-only post-open check.

### R-AR-OPEN. Race-resistant rooted traversal and open (#5039)

Threat: another local user or process can write inside the source tree while `create-launch`
reads it. The toolkit process itself is not compromised. Requirement: no read blocks on a special
file, no byte from outside the tree selected at walk time is read, and any change seen during the
walk fails the command before any upload request.

- **Platform scope (bounded compatibility decision, D16).** The guarantee needs descriptor-relative
  `openat` with `O_NOFOLLOW`, `O_NONBLOCK` and `fstat`, which safe `std` does not offer (and the
  workspace forbids `unsafe`). It comes from `rustix` `=1.1.5` (`fs` feature; the version #5017 S1
  already pins), compiled on `cfg(unix)` only. On any non-Unix target, `create-launch`,
  `download-snapshot` and `apply-snapshot` fail closed before any network request with "This
  command is not supported on this platform." (exit 1). No supported caller runs the toolkit on
  Windows today: the workspace has no `cfg(windows)` code path.
- **Root.** The caller's path is opened once with `openat(CWD, path, O_RDONLY | O_DIRECTORY |
  O_CLOEXEC)`, following a symlink at the root (parity with `os.path.isdir`) and in the
  caller-named ancestors. Its `fstat` identity `(st_dev, st_ino)` is recorded. Every later
  operation is relative to this held descriptor or to descriptors opened from it; the path string
  is never resolved again. Git (R-AR-GIT) still receives the path, but its output only selects
  names; Git never supplies bytes for the archive.
- **Directories.** A directory is listed with `rustix::fs::Dir::read_from(&dirfd)`. Each name is
  classified with `statat(dirfd, name, AT_SYMLINK_NOFOLLOW)` (skip tables first, then type). A
  subdirectory is entered with `openat(dirfd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW |
  O_CLOEXEC)`, then `fstat` on the new descriptor must report a directory with the same
  `(st_dev, st_ino)` as the `statat` result. A symlink swapped in at any ancestor fails at its own
  `openat` (`ELOOP` or `ENOTDIR`) because no component is ever resolved by path. Open directory
  descriptors are a stack along the current path (depth ≤ 4096 / 2, bounded by the name-length rule
  of R-AR-READ).
- **Files.** A regular file selected at walk time is opened with `openat(dirfd, name, O_RDONLY |
  O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC)`. `O_NONBLOCK` makes an open of a FIFO return at
  once instead of waiting for a writer; `O_NOFOLLOW` makes a symlink leaf fail with `ELOOP`;
  `O_NOCTTY` keeps a terminal device from becoming the controlling terminal. Before any read,
  `fstat` on the handle must show `S_IFREG` and the walked `(st_dev, st_ino)`. A mismatch of type or
  identity is not skipped silently: the command fails with "A file changed while the archive was
  being built: <shown_path>. Re-run create-launch." and makes no request.
- **Reads.** The descriptor becomes a `std::fs::File` (`From<OwnedFd>`). It is read up to
  `min(walked size, 100 MiB) + 1` bytes; more bytes than the walked size, or fewer, is the same
  "changed" error. The 100 MiB entry limit therefore cannot be bypassed by growing a file.
- **Residual R-2** (section 9): a writer who swaps a walked regular file for a device node it
  controls can cause one non-blocking, no-controlling-terminal `open` of that device before `fstat`
  refuses it. Placing a device node in the tree needs `CAP_MKNOD` or a hard link to an existing
  node on the same file system, so it is not available to an ordinary local user.
- Hard links inside the tree are read like any regular file (parity); they are files the tree's
  owner already linked.
- **Race seam for tests.** The walker takes a `WalkHook` (no-op in production) called between
  classification and open of each entry, and between listing and entering each directory. Tests use
  it to swap entries on the real file system at exactly that point.

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
  `0xCBF43926`). Layout: local header, data, next local header, with no data descriptors (flag
  bit 3 never set), no gaps, then the central directory, then the EOCD, exactly the shape
  R-ZIP-FRAME requires. ZIP64 (PM amendment, #5046): the writer emits a ZIP64 EOCD record and
  locator **only** when the entry count is 65 535 or more (so a classic count of `0xFFFF` is never a
  real count, which removes the sentinel ambiguity); the classic EOCD then carries `0xFFFF` in
  both count fields and the real 32-bit central-directory size and offset (they always fit: the
  archive is capped at 500 MiB). No per-entry ZIP64 extra field is ever written, because every
  per-entry size and offset is below 500 MiB. The 100 000-entry upload limit is unchanged.
- Limits are enforced **while building**, not after, with the Python's messages: entry count
  100 000; per-entry uncompressed 100 MiB (checked from the walked `statat` size before
  reading and on the bytes read); per-entry ratio 100:1 (after compressing that entry, compressed size > 0); total
  uncompressed 2 GiB; compressed archive 500 MiB (running). Memory is therefore bounded by the
  500 MiB archive plus one entry, as in the Python's success case, and a pathological tree fails
  early instead of after the whole archive is in memory.
- Then the Python checks on the finished bytes run through the R-AR-READ reader (defence in depth;
  it also proves writer and reader agree): safe names, no link entries, single top-level directory.
- `sanitize_root_name` is ported with Python's Unicode `\w` (alphanumeric per Unicode or `_`;
  parity fixtures for ASCII, Latin-1, CJK, combining marks and digits from other scripts).

### R-AR-READ. The ZIP reader: profiles and entry policy (shared)

One reader, `launch/archive/zip_read.rs`, over an in-memory `&[u8]` of at most 500 MiB (the upload
cap, and the download cap of R-AR-EXT). It runs R-ZIP-FRAME first and the entry policy below
second. It has two profiles. Both accept the strict ZIP64 subset (PM amendment, #5046).

- **Upload** (self-check of the writer's own bytes before the upload, as the Python's
  `validate_archive_bytes`): framing, names, types, declared limits, single top-level directory.
  The writer computed every CRC and size itself, so stored/deflate data is not decoded again in
  production. The round-trip tests decode it with the Snapshot path.
- **Snapshot** (hostile input from the AWS-hosted agent): framing, names, types, declared limits,
  R-AR-VCS, then during extraction the exact decode of R-ZIP-FRAME step 8. No single-root rule (the
  snapshot is a repository root).

Every check fails closed with a fixed `ArchiveError`. Framing failures read "The archive is not a
valid ZIP file (<reason>)." with `<reason>` one of the fixed codes of R-ZIP-FRAME. Entry failures
name the entry through `shown_value`.

- Names: UTF-8 (flag bit 11, or pure ASCII); non-UTF-8 CP437 names → refused. Refused: empty, NUL
  or other control characters, `\` anywhere, a leading `/`, a drive prefix (`[A-Za-z]:`), `.` or
  `..` segments, empty segments (`a//b`), a segment longer than 255 bytes, a name longer than
  4096 bytes. **Directory syntax** (#5043): a directory entry's name ends in exactly one `/`.
  Splitting on `/` then leaves one final empty string, which is the directory marker and not an
  empty segment. Every other segment must be non-empty, so `a//` and `/` are refused. (Windows name
  rules are not needed: the archive commands are Unix-only, D16.)
- Duplicates: two entries whose names are equal after Unicode NFC normalisation and simple case
  folding (`unicode-normalization` is already a `toolkit-cli` dependency) → refused, on every
  platform (case-insensitive and normalising file systems would otherwise merge them). Directory
  markers are removed before comparing, so `a` and `a/` collide. A file entry whose folded name is
  a proper prefix-path of another entry (`a` and `a/b`) → refused (file/directory conflict).
- Type and mode reconciliation (#5043): version-made-by host 3 (Unix) with non-zero type bits in
  `external_attr >> 16`: `S_IFREG` or `S_IFDIR` only. Symlink (`S_IFLNK`), character or block
  device, FIFO and socket → refused. `S_IFDIR` requires the trailing `/`, and a trailing `/`
  requires `S_IFDIR`. Host 0 (MS-DOS) or zero type bits: the name decides. If the DOS directory
  attribute (`external_attr & 0x10`) is set, the name must end in `/`. A directory entry has
  uncompressed size 0 and CRC 0. ZIP has no hardlink entry type; link-like extra fields are
  ignored, except the Info-ZIP Unicode Path field 0x7075, which R-ZIP-FRAME refuses. The extractor
  only ever creates regular files and directories.
- Declared limits, applied to the resolved central values (ZIP64 included) **before any entry
  data is read**: per entry uncompressed ≤ 100 MiB; ratio: compressed size 0 requires
  uncompressed 0, otherwise uncompressed ≤ 100 × compressed (Python's `> 100` refusal); running
  total ≤ 2 GiB; entry count ≤ 100 000 (from the EOCD, R-ZIP-FRAME step 3). Because step 8 also
  requires exact consumption of the compressed span, padding cannot inflate the compressed size
  that the ratio uses.

### R-ZIP-FRAME. Container framing for hostile input (#5043, #5046)

`N` is the data length. All offsets and lengths are `u64`, and every sum uses `checked_add`
(overflow → `bounds`). Every read is a bounds-checked slice of `data[a..b]` with `a ≤ b ≤ N`
established first. No allocation is sized from a declared value before that value has passed its
limit. Little-endian throughout. The result is a fixed list of entries whose byte ranges cover the
data exactly.

1. **EOCD.** `N ≥ 22`, else `eocd_missing`. Window: the last `min(N, 22 + 65 535)` bytes.
   Candidates: every position `p` in the window with signature `0x06054b50` and
   `p + 22 + comment_len == N`. Zero candidates → `eocd_missing`; more than one → `eocd_ambiguous`
   (a forged EOCD inside a comment). The comment's bytes are not interpreted.
2. **Classic fields and ZIP64.** Disk number and central-directory disk must be 0, and entries on
   this disk must equal total entries, else `multi_disk`. ZIP64 is **required** exactly when a
   classic field equals its sentinel (count `0xFFFF`, central size or offset `0xFFFFFFFF`), and is
   **not consulted** otherwise. When required:
   - the ZIP64 locator (`0x07064b50`, 20 bytes) must start exactly at `p − 20`; its disk-of-record
     field must be 0 and its total-disks field 1 (else `multi_disk`);
   - the ZIP64 EOCD record must start at the locator's offset `q`, carry signature `0x06064b50`
     and size-of-record exactly 44 (no extensible data), with version needed ≤ 45, disk fields 0,
     and entries-on-disk equal to total entries. It must end exactly at the locator
     (`q + 56 == p − 20`);
   - per field, **sentinel iff out of range** (PM rule): if the ZIP64 value does not fit the
     classic width (count ≥ `0xFFFF`, size or offset ≥ `0xFFFFFFFF`), the classic field must be
     the sentinel. Otherwise the classic field must equal the ZIP64 value. Any other combination,
     a missing or misplaced locator, or a wrong record → `zip64_inconsistent`.
   The effective values (`count`, `cd_size`, `cd_offset`) come from ZIP64 when it is used,
   otherwise from the classic EOCD. `cd_end` is the start of the ZIP64 record when ZIP64 is used,
   otherwise `p`.
3. **Limits before parsing** (no central record is read yet): `1 ≤ count ≤ 100 000` (else
   `empty` or the Python entry-count message); `cd_size ≤ 64 MiB`; `cd_size ≥ 46 × count`;
   `cd_offset + cd_size == cd_end` **exactly** (no gap and no overlap between the central directory
   and the EOCD structures; else `layout`). These apply to the ZIP64 values when ZIP64 is used.
4. **Central directory: exact count and span.** The cursor starts at `cd_offset`. For `i` in
   `0..count`: `cursor + 46 ≤ cd_end` and signature `0x02014b50` (else `cd_record`); the record
   length `46 + n + m + k` (name, extra and comment lengths) must end at or before `cd_end` (else
   `cd_record`). After exactly `count` records the cursor must equal `cd_end` (else `cd_count`):
   neither fewer records than declared nor trailing bytes. Per record:
   - version made by: host byte 0 (MS-DOS) or 3 (Unix), else `version`; version needed ≤ 45, else
     `version` (widened only from live-run evidence, Q19-style).
   - general-purpose flags: **allowlist** bit 1 and bit 2 (deflate options, method 8 only), bit 3
     (data descriptor, step 6) and bit 11 (UTF-8). Any other bit set → `flags`. That covers
     encryption (bit 0), enhanced deflate (bit 4), patched data (bit 5), strong encryption (bit 6),
     the masked central directory (bit 13) and the reserved bits.
   - method 0 (stored) or 8 (deflate), else `method`. Stored requires compressed size equal to
     uncompressed size (`stored_size`).
   - disk number start must be 0 (a `0xFFFF` disk sentinel is refused: single disk only,
     `multi_disk`).
   - the modification time and date, internal attributes and file comment are not interpreted (the
     comment only counts toward the record length).
5. **Extra fields** (central and local, #5043). The `m`-byte region must parse exactly into
   `(id: u16, len: u16, data[len])` items. A header must fit (a 1-3 byte remainder → `extra_field`),
   each `data` must end inside the region, and the items must consume it exactly. Each id at most
   once per region. Id `0x7075` (Info-ZIP Unicode Path: a second, competing name) → `extra_field`.
   Other ids are skipped uninterpreted, except ZIP64 `0x0001`:
   - **central ZIP64 extra**, present **iff** at least one of uncompressed size, compressed size or
     local-header offset is `0xFFFFFFFF`. It holds exactly one 8-byte value per sentinel field, in
     the order uncompressed, compressed, offset (length `8 × sentinels`), and nothing else (no disk
     field). A ZIP64 extra without a sentinel, a sentinel without the extra, or a wrong length →
     `zip64_extra`. The resolved values then go through every limit;
   - **local ZIP64 extra**, present **iff** both local size fields are `0xFFFFFFFF`, length exactly
     16 (uncompressed, compressed); it never carries an offset.
6. **Local header and data descriptor.** Resolved offset `o`: `o + 30 ≤ cd_offset` and signature
   `0x04034b50`, else `local_header`. Name length and bytes, method and flags must equal the central
   record (else `local_mismatch`); local version needed ≤ 45; the local extra region is parsed by
   step 5 (its length may differ from the central one). Data starts at
   `d = o + 30 + n_local + m_local` and spans `[d, d + compressed)`.
   - Bit 3 clear: local CRC, compressed and uncompressed sizes (ZIP64-resolved) equal the central
     values, else `local_mismatch`.
   - Bit 3 set (data descriptor, accepted and fully validated): local CRC and both sizes must be 0,
     else `descriptor`. The descriptor sits at `d + compressed`, must start with the signature
     `0x08074b50` (the unsigned form is refused, removing the ambiguity), then CRC-32, then the two
     sizes: 8 bytes each **iff** the central record carries ZIP64 sizes, else 4 bytes each.
     Descriptor values must equal the central values (`descriptor`). The descriptor belongs to the
     entry's byte range.
   - The entry's range is `[o, end)`, `end = d + compressed (+ 16 or 24 descriptor bytes)`.
7. **Exact layout, no overlap** (#5043). The entries sorted by `o` must tile the data before the
   central directory exactly: the first `o == 0`, each `end` equals the next entry's `o`, the last
   `end == cd_offset`. A gap (prepended stub, hidden bytes, `layout`) or an overlap (two records
   sharing bytes, the "quoted" overlap bomb, `overlap`) is refused. With steps 3 and 4, every byte
   of the input belongs to exactly one structure.
8. **Data: exact consumption** (Snapshot extraction; tests also run it on writer output).
   - Stored: the span is the content; length equals the uncompressed size (step 4).
   - Deflate: `miniz_oxide::inflate::core::decompress` on a `DecompressorOxide`, raw deflate (no
     zlib header flag), **without** `TINFL_FLAG_HAS_MORE_INPUT`, with input exactly the span and a
     64 KiB wrapping output buffer drained to the sink. It must end in `TINFLStatus::Done` with
     total consumed input equal to the compressed size. `Done` with input left (a stream that ends
     early and is followed by padding) → `trailing_data`. Input exhausted without `Done`
     (`NeedsMoreInput`, `FailedCannotMakeProgress`) → `truncated_stream`; any other failure →
     `deflate`.
   - Output is counted as it is produced. The sink accepts at most the declared uncompressed size,
     and the first byte beyond it stops decoding (`size_mismatch`); fewer bytes at `Done` is also
     `size_mismatch`. CRC-32 over the output must equal the central CRC (`crc_mismatch`).
   - Directory entries: uncompressed size 0, CRC 0; stored with compressed size 0, or deflate whose
     exact decode yields 0 bytes.

Invariant → test (all fixtures are byte-built in the test; the "no record read" checks use a reader
instrumented with a read counter):

| Invariant | Test |
| --- | --- |
| `N < 22`; no EOCD in the window; EOCD farther than 65 557 bytes from the end | T-ZIP-FRAME-1, 2 |
| two EOCD candidates ending at `N` (forged one in the comment) | T-ZIP-FRAME-3 |
| comment length one short / one long | T-ZIP-FRAME-4 |
| non-zero disk fields; entries-on-disk ≠ total | T-ZIP-FRAME-5 |
| count 0 | T-ZIP-FRAME-6 |
| count 100 001 refused with zero central records read | T-ZIP-FRAME-7 |
| `cd_size` > 64 MiB; `cd_size < 46 × count` (refused before parse) | T-ZIP-FRAME-8 |
| `cd_offset + cd_size` overflows or passes `N` (`0xFFFFFFFF` values) | T-ZIP-FRAME-9 |
| gap bytes between the central directory and the EOCD | T-ZIP-FRAME-10 |
| declared count larger than the records present | T-ZIP-FRAME-11 |
| declared count smaller (a trailing extra record) | T-ZIP-FRAME-12 |
| name, extra or comment length overrunning `cd_end` (three cases) | T-ZIP-FRAME-13 |
| wrong signature in the middle of the central directory | T-ZIP-FRAME-14 |
| host byte other than 0/3; version needed > 45 | T-ZIP-FRAME-15 |
| each refused flag bit (0, 4, 5, 6, 12, 13, reserved); bits 1/2 on a stored entry | T-ZIP-FRAME-16 |
| method other than 0/8 | T-ZIP-FRAME-17 |
| extra region: 1-3 byte remainder, item overrun, duplicate id, `0x7075` present | T-ZIP-FRAME-18 |
| central ZIP64 extra without sentinel, sentinel without extra, wrong length, disk sentinel | T-ZIP-FRAME-19 |
| local offset past `cd_offset`; `o + 30` overflow | T-ZIP-FRAME-20 |
| wrong local signature | T-ZIP-FRAME-21 |
| local name, method or flags differ from central | T-ZIP-FRAME-22 |
| local CRC, compressed or uncompressed size differ (bit 3 clear) | T-ZIP-FRAME-23 |
| local extra framing invalid; local ZIP64 extra with an offset or wrong length | T-ZIP-FRAME-24 |
| descriptor: non-zero local fields, missing signature, values ≠ central, 8-byte sizes on a non-ZIP64 entry; a valid descriptor accepted with its bytes in the range | T-ZIP-FRAME-25 |
| first entry not at offset 0 (prepended stub) | T-ZIP-FRAME-26 |
| gap between an entry's end and the next local header | T-ZIP-FRAME-27 |
| two central records sharing one local entry; partially overlapping ranges | T-ZIP-FRAME-28 |
| last entry ending before `cd_offset` | T-ZIP-FRAME-29 |
| stored compressed ≠ uncompressed | T-ZIP-FRAME-30 |
| deflate ends early, padding fills the declared compressed size (ratio would pass) | T-ZIP-FRAME-31 |
| deflate truncated | T-ZIP-FRAME-32 |
| output one byte longer (stopped at declared + 1) / shorter than declared; CRC mismatch | T-ZIP-FRAME-33 |
| directory syntax and modes: `a/` stored and deflate-empty accepted, `a//`, `/`, size > 0, `S_IFDIR` without `/`, `S_IFREG` with `/`, DOS 0x10 without `/`, `a` + `a/b`, `a` + `a/`; valid stored and deflate archives still extract | T-ZIP-FRAME-34 |

ZIP64 (PM amendment seq 3, #5046):

| Case | Test |
| --- | --- |
| writer at 65 534 entries: no ZIP64 records; reader accepts | T-ZIP-64-1 |
| writer at 65 535: ZIP64 record and locator, classic count `0xFFFF`, real size/offset; reader accepts | T-ZIP-64-2 |
| writer at 65 536 and at 100 000: round trip through both profiles | T-ZIP-64-3 |
| 100 001 entries: the Python entry-count message while building, no archive emitted | T-ZIP-64-4 |
| classic count `0xFFFF` without a locator | T-ZIP-64-5 |
| locator not at `p − 20`; record not ending at the locator; record size ≠ 44; version needed > 45 | T-ZIP-64-6 |
| sentinel where the ZIP64 value fits; real classic value differing from the ZIP64 value | T-ZIP-64-7 |
| locator disk ≠ 0 or total disks ≠ 1; ZIP64 entries-on-disk ≠ total | T-ZIP-64-8 |
| ZIP64 count 100 001 (zero records read); ZIP64 `cd_size` > 64 MiB; `u64::MAX` offset (checked arithmetic) | T-ZIP-64-9 |
| a ZIP64 record and locator present with no classic sentinel (`layout`); a 70 000-entry ZIP64 snapshot fixture extracts | T-ZIP-64-10 |

### R-AR-VCS. Version-control paths in snapshots (#5040)

Product decision recorded here (from the #5038 review): a downloaded snapshot may **never** create
or overwrite repository administrative metadata through the automatic path. A migration that
really needs a metadata change goes through a separate, explicitly reviewed path outside this
command. General source changes stay reviewable: they are never silently dropped. A snapshot
carrying VCS metadata is refused as a whole, not filtered, so nothing disappears unseen.

- **Denylist** (component names, at any depth, file or directory): `.git`, `.hg`, `.svn`, `.bzr`,
  `_darcs`, `CVS`, `.jj`, `.pijul`, `.sl`, `.fslckout`, `_FOSSIL_`.
- **Normalisation before comparing** (the aliases Git itself guards against in
  `is_hfs_dotgit`/`is_ntfs_dotgit`): NFC; simple case folding; removal of the HFS+ ignorable code
  points U+200C-U+200F, U+202A-U+202E, U+206A-U+206F, U+FEFF; truncation at the first `:` (NTFS
  stream syntax `.git::$INDEX_ALLOCATION`); removal of trailing `.` and space. The folded component
  also matches when it is an 8.3 short name `git~N`, `hg~N`, `svn~N` or `bzr~N` (`N` digits).
- **Where it applies**: in R-AR-READ's Snapshot profile, before anything is created on disk, with
  "The snapshot contains version-control metadata and was not extracted: <shown_value(name)>.",
  and again independently in R-AR-APPLY's walk of the extracted tree. A validated extraction is not
  approval to overwrite repository metadata.
- **Not denied** (ordinary tracked content; Git runs nothing from them without configuration in
  the repository's metadata, which cannot be written (R-AR-META); `.gitmodules`
  `update = !command` is refused by Git itself): `.gitignore`, `.gitattributes`, `.gitmodules`,
  `.github/`, and names that only contain `git` (`git/`, `my.git/`).
- **The denylist is necessary, not sufficient (revision 3, #5061).** A repository's administrative
  directory need not be named `.git`. With `git init --separate-git-dir` (or a linked worktree, or
  `core.worktree`), `.git` is a one-line file `gitdir: <path>`, and the real directory, possibly
  inside the work tree under any name, holds `config`, `hooks/` and the common directory. Names
  cannot identify it. R-AR-META protects the actual directories by identity at the place where the
  repository is written.
- Tests T-AR-VCS-1..8: (1) `.git/config` with `core.fsmonitor = ./helper` plus `helper` and
  `.git/hooks/post-checkout` → refused, nothing created at the destination; (2) nested `sub/.git`
  as a file and as a directory; (3) case aliases `.GIT/config`, `.Git/hooks/x`; (4) HFS ignorable
  `.g\u{200c}it/config`; (5) NTFS aliases `.git./x`, `.git /x`, `git~1/config`, `.git::$DATA/x`;
  (6) `.hg/hgrc`, `.svn/x`, `.bzr/x`, `_darcs/x`, `CVS/Entries`, `.jj/x`; (7) the "not denied"
  names extract normally; (8) a table test of the normaliser: every denylist entry × every alias
  transformation.

### R-AR-EXT. Safe extraction (new `launch download-snapshot`, Q1)

- `toolkit launch download-snapshot <launch-id> [--output DIR] --region R` (Unix only, Linux and
  macOS for the exclusive rename of R-AR-STAGE; elsewhere the platform message of R-AR-OPEN):
  1. `--output` checks first, before any request: R-AR-STAGE opens and checks the parent and
     refuses an existing destination name. Then `GetLaunch include=download_url`; no URL → the
     Python's "Download URL not available yet..." message.
  2. Download over the #5017 S1 `StreamingTransport` with the R-API-BODY collector: https only, no
     userinfo, host allowlist `.amazonaws.com` and `.api.aws` (Q19), the public-address guard, no
     redirects (a 3xx is an error naming the status only), a 300 s whole-download deadline, cap
     500 MiB (declared `Content-Length` checked first, running count always). The URL is never
     printed in an error.
  3. The archive stays in memory under that cap (bounded like the upload archive). **No temporary
     archive file exists**, so there is no `/tmp` name to race.
  4. Full validation with the Snapshot profile: R-ZIP-FRAME steps 1-7, R-AR-READ entry policy,
     R-AR-VCS and the materialisation plan with its node budget (R-AR-PLAN, revision 3). All of
     it completes **before** anything is created on disk.
  5. Extraction into held-descriptor staging, then exclusive publication, per R-AR-STAGE, creating
     exactly the nodes of the plan; data decoded per R-ZIP-FRAME step 8. Files 0644, or 0755 when the entry's Unix mode has the
     owner-exec bit; directories 0755 inside a 0700 output root; no setuid, setgid, sticky or
     world-writable bit, no ownership change, no timestamps restored.
  6. Default destination: `launch-snapshot-<id-safe>-<random>` under `std::env::temp_dir()`, a
     sticky, root-owned directory that passes R-AR-STAGE's policy. With `--output DIR`, the final
     component of `DIR` must be a normal name (not empty, `.` or `..`) that does not exist; its
     parent must exist. `--output -` is refused by the #4780 router policy; `./-` is a literal path.
  7. Any failure cleans up through R-AR-STAGE's descriptor-relative cleanup; nothing appears at the
     destination.
  8. Output `{"outputDir": "<path>", "files": N, "directories": M, "bytes": B, "region": R}`.
- `SKILL.md` step 8A becomes this one command, and step C becomes `toolkit launch apply-snapshot
  <outputDir>` (R-AR-APPLY) instead of `rsync -a <outputDir>/ .`. The user-consent step before
  applying and the `git status`/`git diff --stat` review after it are kept unchanged. The prose
  rewrite ships in the cutover slice.

### R-AR-PLAN. Materialisation plan and node budget (revision 3, #5066)

Threat: the record, byte and ratio limits count ZIP records. They do not count what extraction
creates. One file record can name about 2 000 path components (4 096 bytes, one-byte segments),
and every missing ancestor becomes a `mkdirat`. 16 000 tiny records with disjoint deep chains fit
every archive limit and still ask for over 31 million directories, plus a cleanup ledger entry
for each. Requirement: the number of filesystem nodes extraction may create, including implicit
parents, is fixed and checked **before the first creation**. Deriving that number must itself run
in bounded memory.

- **Budget.** `MAX_SNAPSHOT_NODES = 200 000` materialised nodes below the output root: every file,
  every explicit directory entry and every implicit ancestor directory, each unique path counted
  once. Files remain at most 100 000 through the entry limit. So up to 100 000 directories can
  accompany a full-size tree. That is compatible with every snapshot of a tree the upload side
  accepts: the writer emits only file records (at most 100 000), and a real source tree has fewer
  directories than files. Recorded as D22 and Q23.
- **Derivation, in bounded memory.** After R-ZIP-FRAME steps 1-7, R-AR-READ and R-AR-VCS have
  accepted the central directory, and before R-AR-STAGE creates staging, the planner walks the
  central records in order. For each name it walks its `/`-separated segments from the root.
  A segment is looked up under its parent node in a hash index keyed by `(parent node id, raw
  segment bytes)`, and a missing node is appended. Nodes are fixed-size, `{parent: u32,
  name_offset: u32, name_len: u16, kind}`, and the name bytes are *referenced* in the in-memory
  archive buffer, never copied. The index reserves at most `MAX_SNAPSHOT_NODES` slots. The planner
  stops with a refusal at the first node that would be number 200 001, so planning memory is at
  most about 200 000 × 32 bytes (node plus index slot, about 6.4 MiB) whatever the archive says.
  Raw-byte keying never undercounts the nodes a case-sensitive file system creates. Case and NFC
  collisions between *entries* stay R-AR-READ's duplicate rule, unchanged.
- **Merging.** An explicit directory entry (`a/b/`) and the implicit ancestor produced by a deeper
  name (`a/b/c`) are the same node, counted once, whichever comes first in central order. A file
  node that another name needs as a directory is already refused by R-AR-READ's file/directory
  conflict rule. The planner asserts it again as an internal invariant.
- **Refusal**, before anything exists on disk (no staging directory, no `mkdirat`): "The snapshot
  would create more than 200000 files and directories and was not extracted."
- **Use by R-AR-STAGE.** Creation iterates the plan (directories in node order, which is
  parents-first by construction, then files). Every `mkdirat` and file `openat(O_CREAT)` names a
  plan node. A creation request for a path outside the plan is an internal error and fails the
  extraction. The cleanup ledger is a `Vec` allocated once with capacity = plan length and holds
  `(node id, dev, ino)`, about 20 bytes each. It cannot grow past the plan. The output's `files`
  and `directories` counts come from the plan.
- **Use by R-AR-APPLY.** The apply walk of the extracted tree applies the same constants: at most
  100 000 files and at most 200 000 nodes in total, counted while walking and refused at the first
  excess, before any write. A snapshot that extracts can therefore always be planned by apply.
- Tests T-PLAN-1..8, archives built byte by byte in the test (valid stored framing, so every
  earlier rule passes; 1-5 run on the planner, a pure function over the archive bytes with no
  file-system access, and 6-8 run end to end with the recording `fsops`): (1) 3 file records each
  under its own 40-component chain, injected budget 100 → refused; the planner's instrumented
  peak node count is ≤ 101 and its index never reserved more than the budget. (2) The production
  constant: 101 records, each a unique top component followed by 1 990 one-character segments and
  a leaf (each name < 4 096 bytes, archive under about 1 MiB, 101 bytes of file data): over
  200 000 nodes → refused; peak node count 200 001; asserts `MAX_SNAPSHOT_NODES == 200_000`. (3) Exactly the injected budget → extracts; budget + 1 →
  refused. (4) 1 000 files under `src/a/b/` → 1 003 nodes. (5) Explicit `a/` and `a/b/` records
  before, and in another fixture after, `a/b/c` → 3 nodes; output `directories` 2, `files` 1.
  (6) A CRC failure in the last file → the ledger length equals the nodes created so far, its
  capacity equals the plan length and never grows; cleanup removes exactly the ledger. (7) The
  fixtures of (1) and (2) through the full `download-snapshot` path → refused after download; the
  recording `fsops` shows zero `mkdirat` and `openat(O_CREAT)` calls, and no staging name and
  nothing at the destination exist. (8) Apply walk: 100 001 files refused; 200 001 nodes with
  injected constants refused; both before any write.

### R-AR-STAGE. Held-descriptor staging and publication (#5047)

Threat: another local user with write access to the destination's parent renames or replaces the
staging directory or the destination name to redirect writes or publication. Requirement:
nothing is written outside the staging tree that was created, nothing but that tree is
published, and cleanup removes only what this process created.

- **Parent, opened once.** The parent path is opened with `openat(CWD, parent, O_RDONLY |
  O_DIRECTORY | O_CLOEXEC)`. That one open follows symlinks in the caller-named path (the approved
  explicit-parent behaviour). Every later operation is relative to this held descriptor `P`.
  Renaming or replacing an ancestor of `P` after that point changes nothing about where writes go.
- **Parent policy** on `fstat(P)`, so that no other user can rename our entries in `P`: the owner
  must be the effective uid (`rustix::process::geteuid`) or root; if the mode has a group- or
  other-write bit, the sticky bit must be set. Anything else is refused before any request: "The
  parent of the output directory can be modified by other users: <shown_path>. Choose a private
  directory." (compatibility decision, D18: the Python used fixed `/tmp` names and had no
  `--output`). `/tmp` (sticky, root-owned) and the user's own directories pass.
- **Staging.** `S = .launch-snapshot-<16 hex>.partial`: `mkdirat(P, S, 0o700)`, then
  `openat(P, S, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`. `fstat` must show a directory owned by the
  euid with no group or other bits, and its identity `(dev, ino)` is recorded. Inside it,
  `mkdirat(S, "tree", 0o700)` is opened and checked the same way. All entries go under `tree`.
- **Creation inside the tree**, in R-AR-PLAN order (parents before children, only plan nodes,
  revision 3): each path is
  walked from the held `tree` descriptor one component at a time with `openat(O_DIRECTORY |
  O_NOFOLLOW)`. A missing directory is created with `mkdirat(parent, seg, 0o755)`, then opened
  `O_NOFOLLOW` and checked (`fstat`: directory, euid owner, and the identity of the inode just
  created). Files: `openat(parent, seg, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
  mode)`, then `fchmod` to exactly 0644/0755 (the umask can only narrow the create mode). `EEXIST`
  is a duplicate and fails. Since `S` is 0700 and owned by us, no other user can create, rename or
  replace anything inside it. Since `P` passed the policy, no other user can rename `S`. Even so, no
  write depends on either: every write uses held descriptors.
- **Publication, exclusive and verified.** (1) `statat(P, S, AT_SYMLINK_NOFOLLOW)` must still show
  the identity of `S`; (2) `renameat_with(tree_parent = S, "tree", P, final_name,
  RenameFlags::NOREPLACE)` (Linux `renameat2`, macOS `renameatx_np(RENAME_EXCL)`). `EEXIST` or
  `ENOTEMPTY` → "Output directory already exists: <shown_path>."; `EINVAL` (a file system without
  exclusive rename) → "The output file system does not support an exclusive rename; choose another
  --output."; (3) `statat(P, final_name, AT_SYMLINK_NOFOLLOW)` must show the `tree` identity, else
  the command fails and reports that the result could not be verified; (4) `unlinkat(P, S,
  REMOVEDIR)` after an identity check. A symlink can never be published as `outputDir`: the only
  rename source is the `tree` entry inside our private `S`. This also removes revision 1's
  residual R-1 (rename over a racing empty directory).
- **Cleanup**, descriptor-relative only. The extractor records each entry it created in the
  pre-sized ledger of R-AR-PLAN (plan node id, identity), so the ledger is bounded by the node
  budget. On failure it removes them deepest first with `unlinkat` relative
  to re-opened `O_NOFOLLOW` parents inside `S`, then `tree` and `S` after identity checks. An entry
  whose identity no longer matches is left alone and reported. `remove_dir_all` on a path is never
  used.
- **Race seam for tests.** An `ExtractHook` (no-op in production) runs before each creation and
  before publication.
- Tests T-STAGE-1..9: (1) private parent: success, `outputDir` 0700, entry modes exact, no
  `.partial` left; (2) parent group- or other-writable without the sticky bit → refused before any
  request (the transport fake records none); (3) sticky other-writable parent owned by the euid →
  accepted; (4) policy function over injected `stat` values: another non-root owner refused, root
  owner with sticky accepted; (5) hook: before the first creation, `S` is renamed away and replaced
  by a symlink to an outside directory → nothing appears in the outside directory, nothing is
  published, cleanup touches only identity-matched entries; (6) the same swap before the next file
  in the middle of extraction; (7) hook: an ancestor of `P` is renamed and replaced by a symlink
  after `P` was opened → every write lands in the original `P` inode, none at the symlink target;
  (8) a directory created at the destination name just before publication → `NOREPLACE` refusal,
  the intruding directory untouched, staging cleaned; (9) a caller-named parent path containing a
  symlink is followed once and succeeds; `--output` with a final `.`, `..` or empty component is
  refused.

### R-AR-META. The target repository's actual Git metadata (revision 3, #5061)

Threat: the target's administrative directory may sit inside the work tree under a name that no
denylist knows. Examples: `git init --separate-git-dir=<T>/repo-admin <T>`, a linked worktree whose
per-worktree directory or common directory was placed in the tree, or a nested repository whose
`.git` file points at an in-tree directory. A snapshot carrying `repo-admin/config` with
`core.fsmonitor = ./helper` would then run its helper at the prescribed `git status`. Requirement:
`apply-snapshot` writes nothing into the directories Git actually uses as metadata for the target,
for any valid layout, and it refuses before the first write when it cannot establish where they
are. This is the PM's Q21 rule ("a snapshot never writes repository metadata") applied to the real
Git admin store, as the #5038 round-2 interpretation requires.

- **Environment precondition.** If the caller's environment sets any of `GIT_DIR`, `GIT_WORK_TREE`,
  `GIT_COMMON_DIR`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
  `GIT_CONFIG_PARAMETERS` or `GIT_CONFIG_COUNT` (non-empty), `apply-snapshot` refuses: "apply-snapshot
  does not run with <NAME> set; unset it and re-run." These variables change which metadata the
  user's later `git` commands use, so a discovery without them could protect the wrong directory,
  and a discovery with them would follow caller-controlled redirection.
- **Discovery: one hardened Git call** (Git is the authority on its own layout; hand-parsing
  `gitdir:` files, `commondir` and `core.worktree` would re-implement and drift from it). Same
  hardening as R-AR-GIT: `git` from `PATH`, `current_dir` = the caller's `--into` path, `stdin`
  null, stdout capped at 64 KiB, stderr discarded, 30 s limit, `-c core.fsmonitor=false`, the
  inherited `GIT_*` variables of R-AR-GIT removed. Command: `git rev-parse --path-format=absolute
  --is-inside-work-tree --is-inside-git-dir --is-bare-repository --show-toplevel --git-dir
  --git-common-dir`. `rev-parse` runs no hook and no fsmonitor and makes no network access.
  Accepted output: exactly six LF-terminated lines `true`, `false`, `false` and three absolute
  paths (a path containing a newline yields a different line count and is refused). Anything else
  fails closed **before the snapshot is read**: Git missing, a non-zero exit (not a repository, a
  `safe.directory` refusal), a timeout, the wrong line count, a relative path, or `--path-format`
  echoed back (Git older than 2.31). The message is "apply-snapshot could not locate the target
  repository's Git metadata (<fixed reason code>); nothing was written.". A target that is not a
  Git work tree is refused the same way (D20). `SKILL.md` step C always applies inside the
  migration branch of a Git repository.
- **Pinning by identity.** The reported Git directory and common directory are opened once
  (`openat(CWD, path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)`, following symlinks as Git does) and
  `fstat`ed. Their `(dev, ino)` pairs form the protected set `M`. A linked worktree's per-worktree
  directory and the shared common directory are both in `M`. Submodule metadata under
  `<common>/modules/` is inside the common directory. The top-level directory's identity `K` is
  recorded too.
- **Target root not inside metadata.** From the held `--into` descriptor, walk upward with
  `openat(fd, "..", O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` until the identity equals `K`. Every
  directory visited, `--into` included, must be outside `M` and not Git-shaped (next bullet).
  Reaching the file-system root (`..` is itself) without meeting `K`, or more than 2 048 steps,
  is refused. So `--into .git`, `--into <T>/repo-admin/hooks` or a directory outside the reported
  work tree is refused before any write, without trusting a path a second time.
- **Git-shaped directories** (a superset of Git's own `is_git_directory` test, so anything Git
  could treat as a repository directory is caught, including ones not linked from this target):
  a directory is Git-shaped if it contains an entry named `commondir`, or an entry named `HEAD`
  together with an entry named `objects` or `refs` (any types, `statat(AT_SYMLINK_NOFOLLOW)` on
  the held descriptor).
- **Where it is enforced.**
  1. *Target side, R-AR-APPLY phase 1:* every existing target directory the plan enters is opened
     `O_NOFOLLOW` and `fstat`ed already. Its identity must not be in `M`, and it must not be
     Git-shaped. Otherwise the apply is refused before the first write: "The snapshot would
     write into Git metadata at <shown_path> and was not applied." This catches an in-tree
     separate Git directory and an in-tree common or worktree directory under any name, as well as
     nested repositories' in-tree admin directories and bare repositories in the tree. The
     identity of every entered directory is recorded.
  2. *Target side, phase 2:* each directory is re-entered `O_NOFOLLOW` and must show the identity
     recorded in phase 1. A mismatch stops the apply before that write (R-AR-APPLY's failure rule).
  3. *Snapshot side, phase 1:* every directory of the snapshot tree is checked with the
     Git-shaped rule. A hit refuses the apply: "The snapshot contains a directory shaped like Git
     metadata: <shown_value(path)>.". An existing `.git` file (here or in a nested repository) may
     name a directory that does not exist yet, and the snapshot must not be able to supply it.
- **What stays as before.** R-AR-VCS's literal/alias denylist runs unchanged on both sides, the
  extraction side is unchanged, and the existing `.git` entry itself is never written. A Git
  directory outside the work tree is never reachable, because apply only writes beneath `--into`
  through no-follow traversal.
- **Over-refusal, accepted.** A source directory that happens to contain `HEAD` next to `refs` or
  `objects`, or a file named `commondir`, is refused with the message above. The user can apply
  such content by hand. Recorded in D20.

### R-AR-APPLY. Applying a snapshot to the repository (new `launch apply-snapshot`, #5040)

`toolkit launch apply-snapshot <snapshot-dir> [--into DIR]` (default `--into .`; Unix only)
replaces `rsync -a <outputDir>/ .`:

- **Phase 0, metadata discovery (revision 3, #5061).** `DIR` is opened as a root (R-AR-OPEN:
  followed once at the caller-named path, held afterwards). R-AR-META's environment check,
  discovery, pinning and root-containment walk then run before the snapshot is read. Any failure
  refuses with nothing written.
- **Phase 1, plan; nothing is written.** `<snapshot-dir>` is opened as a root the same way. The
  snapshot is walked with R-AR-OPEN's
  descriptor-relative, no-follow machinery. Only directories and regular files are allowed.
  Refused: a symlink, a special file, a non-UTF-8 name, an R-AR-READ name-rule violation, an
  R-AR-VCS hit, a Git-shaped snapshot directory (R-AR-META), more than 100 000 files or 200 000
  nodes (R-AR-PLAN, counted while walking), or more than 2 GiB in total. On the target side each
  planned path is checked with `statat(AT_SYMLINK_NOFOLLOW)` along its components. An existing
  symlink at any component, a non-directory where a directory is needed, a directory where a file
  goes, or a special file → refused (D17: `rsync -a` would have replaced user symlinks). Every
  existing target directory the plan enters must pass R-AR-META (identity not in `M`, not
  Git-shaped), and its identity is recorded for phase 2. Any refusal happens before the first
  write.
- **Phase 2, write.** Missing directories: `mkdirat(parent, seg, 0o755)`, then opened `O_NOFOLLOW`
  and checked. Files: a temporary `.<name>.launch-<8 hex>.tmp` created with `openat(O_CREAT |
  O_EXCL | O_NOFOLLOW)` in the held target parent, `fchmod` 0644/0755 (owner-exec bit of the
  source),
  filled from the held source descriptor under R-AR-OPEN's read rules, `sync_all`, then
  `renameat(parent, tmp, parent, name)`. That atomically replaces an existing regular file. If a
  symlink appears there after phase 1, the rename replaces the link itself and never writes through
  it. Target directories are entered only with `openat(O_DIRECTORY | O_NOFOLLOW)`, so a component
  swapped for a symlink after planning fails the write instead of escaping. An entered existing
  directory whose identity differs from the one recorded in phase 1 stops the apply before that
  write (R-AR-META).
- Never deletes target files (parity: `rsync -a` without `--delete`). Existing directories keep
  their modes and timestamps are not preserved (D17).
- A failure in phase 2 stops at once, removes the current temporary file and reports "Applying the
  snapshot stopped after N files: <reason>. Review the working tree with git status." The overlay is
  on the migration branch that `SKILL.md` step C creates first, so it is reviewable and revertable
  with Git, as a partial `rsync` was.
- Output `{"applied": true, "files": N, "directories": M, "into": "<path>"}`.
- Tests T-APPLY-1..10 (temporary repositories built with the `git` binary; no real user
  repository): (1) normal snapshot: files added and replaced byte-exact, modes 0644/0755, nothing
  deleted, exact output; (2) `.git/config` with `core.fsmonitor` plus a marker-writing helper
  planted in the extracted tree → refused before any write; the repository's `.git/config` and
  `.git/hooks` bytes unchanged; a following `git status` (test `git`, the repository's own config)
  never creates the marker; (3) `.GIT/hooks/pre-commit` planted → refused; (4) a symlink planted in
  the snapshot tree → refused, no write; (5) a FIFO planted → refused within a 2 s bound; (6) target
  `docs -> <outside dir>` where the snapshot has `docs/x` → refused, outside untouched; (7) target
  symlink at a file position → refused; (8) file/directory type conflicts → refused before writes;
  (9) hook after planning swaps a target directory for a symlink → that write fails `ELOOP`, outside
  untouched, temporary file removed; (10) an injected write failure halfway → stops, reports the
  count, temporary file removed.
- Tests T-APPLY-11..17 (revision 3, R-AR-META; temporary repositories built with the `git`
  binary. Each layout is first asserted with `git rev-parse --git-dir --git-common-dir`. "Unchanged"
  means the SHA-256 of every file under the target, metadata included, is identical before and
  after. The marker check runs the test `git status` with the repository's own configuration):
  (11) `git init --separate-git-dir=<T>/repo-admin <T>` with one commit; the snapshot carries
  `repo-admin/config` (the real config plus `core.fsmonitor = ./helper`), a marker-writing
  `helper` and `repo-admin/hooks/pre-commit` → refused before any change, target unchanged, no
  marker after `git status`. Run twice with a test-only switch disabling first the identity rule,
  then the Git-shaped rule, so that each rule is shown to refuse on its own. (12) Common-dir
  indirection: a linked worktree `<T>` made with `git worktree add`, whose per-worktree
  directory and common directory are then moved into `<T>` as `wt-admin/` and `common-admin/`
  and re-linked with `git worktree repair`. A snapshot with `common-admin/config` (fsmonitor) →
  refused; one with `wt-admin/config.worktree` → refused; unchanged; no marker. (13) Normal
  separate-git-dir overlay: Git directory outside `<T>`; an ordinary snapshot applies byte-exact
  and the external Git directory is unchanged. (14) Target root inside metadata: `--into
  <T>/repo-admin`, `--into <T>/repo-admin/hooks` and, in an ordinary layout, `--into <T>/.git` →
  refused before any write; `--into <T>/sub` → applies. (15) Nested and shaped directories: an
  untracked nested repository `vendor/x` whose `.git` file points to in-tree `vendor/x-admin`, and
  a snapshot writing `vendor/x-admin/config` → refused; a bare repository `tools/lib.git/` in the
  target and a snapshot writing `tools/lib.git/hooks/x` → refused; a snapshot directory `newmeta/`
  with `HEAD`, `objects/` and `refs/`, and one with a `commondir` file → refused; a snapshot
  directory holding only a file named `HEAD` → applies. (16) Fail closed, each before the snapshot
  is read, with zero writes and its own fixed reason: Git missing (non-existent program path), a
  target outside any repository (exit 128), a fake `git` printing five lines, a relative path, the
  echoed `--path-format=absolute` of an old Git, or sleeping past an injected short limit; and
  `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `GIT_CONFIG_COUNT` each set in `MapEnv`. (17) Swap after
  planning: an `ApplyHook` between phases renames a planned target directory away and moves the
  in-tree Git directory to its name → the phase-2 identity check stops before that write, and the
  metadata is unchanged.

### R-AR-PERM. Permissions summary

Upload: the archive records 0644/0755 only. Extraction: `outputDir` 0700, inside it directories
0755 and files 0644/0755, all set exactly with `fchmod` on the held descriptor (R-AR-STAGE). Apply:
new directories 0755, files 0644/0755 (R-AR-APPLY). The process umask is not changed (no `umask`
in safe `std`). Create modes can only be narrowed by it, and `fchmod` then sets the exact mode.

## 6. API client failure handling

### R-API-HTTP. Requests

- Endpoint: `https://launch-with-aws.<region>.api.aws`, or `LAUNCH_WITH_AWS_BASE_URL` validated by
  `toolkit_aws::endpoint::validate_endpoint` with `EndpointPolicy::aws_public()` (stricter than the
  Python as recorded in `aws-core-contracts.md`: no userinfo, query or fragment; the wider
  non-public range table). DNS uses `SystemResolver`.
- Requests are built with the `toolkit_aws` request types (`HttpRequest`, `Headers`,
  `percent_encode`) and a launch `BearerSigner` (adds `authorization: Bearer <token>`; the token is
  fetched once per command by `get_access_token`, so a refresh happens at most once). They are
  **sent only through #5017 S1 `StreamingTransport::open`** and collected by R-API-BODY. The
  shared `RetryPolicy` default makes the retry decision and backoff (3 retries,
  throttling/5xx/connect/timeout; `Adaptive` label as documented) inside a small launch runner
  (`launch/http.rs`), because `ApiClient` sends through the collected `Transport::send`. Each
  attempt's `Timeouts` are a fixed 120 s/120 s. The remaining operation deadline is enforced by
  the absolute deadline in its `StreamLimits` (R-API-BODY, TA-STREAM-DL), not by shrinking
  `Timeouts`.
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
- Response status and body rules are **per operation** (R-API-OPS, #5051); every body is
  collected under a per-operation cap before it is parsed (R-API-BODY, #5044).
- Presigned PUT: the `uploadUrl` is validated (https, no userinfo, host ends with
  `.amazonaws.com`, public-address guard), sent as one `PUT` with `Content-Type: application/zip`,
  a 300 s whole-request deadline, **no retry** (parity with `urllib`), no redirect. Its success body
  is never read (R-API-OPS).

### R-API-BODY. Bounded body collection before parsing (#5044)

`Transport::send` returns a fully collected body (`toolkit-aws` `sdk.rs` collects a `ByteStream`
with no byte bound), so a cap checked afterwards cannot bound memory. The slice therefore never
calls `Transport::send` or `ApiClient` sends for network traffic. Every network body is read
through `StreamingTransport::open(request, &StreamLimits)` from #5017 S1
(`be0a06df33a2985e96af3444f9a10e56e1efc668`, `toolkit_aws::stream`). There, `ChunkLimiter`
refuses the first frame that would pass `max_body_bytes` (`StreamError::TooLarge`, nothing past
the cap is handed over) and each chunk is at most `max_chunk_bytes`. On top of that seam, a
launch `BoundedBody` collector:

- **Revision 3 call sequence (#5064).** Revision 2 passed a cap of 0 for bodies that are never
  read and chose success-versus-error caps "at open time". Neither is possible with the pinned S1
  API: `StreamLimits::new` refuses a zero bound (`InvalidConfig`), and the limits are taken by
  `open` before the status exists. The sequence for every network operation is now:
  1. `open(request, &StreamLimits::new(ceiling, 64 KiB)?.with_deadline(attempt_deadline))`, where
     `ceiling` is the operation's positive **transport ceiling** from the table below and
     `with_deadline` comes from TA-STREAM-DL. Every row's `StreamLimits::new` call is a
     compile-time-known positive pair, and T-BODY-11 constructs each one with the production
     function.
  2. Read `status()` and `headers()`. **No `next_chunk` call happens before the status is
     classified.**
  3. Classify by the operation's outcome table: *zero-read* → drop the stream at once, with zero
     pulls; *collect* → set the effective cap, narrowing the stream with
     `narrow_body_limit(cap)` (TA-STREAM-DL) when the cap is below the ceiling, then collect.

  | Operation | Transport ceiling at `open` | Status 2xx | 3xx | ≥ 400 |
  | --- | --- | --- | --- | --- |
  | Launch API JSON operations (`GetLaunch`, `ListLaunches`, `CreateUploadUrl`, `CreateLaunch`, `StartLaunchExecution`) | 64 MiB (Q12) | collect, cap 64 MiB, then JSON | zero-read; error naming the status | narrow to 64 KiB, collect for the code (R-API-CODE) |
  | `DeleteLaunch` | 64 KiB | zero-read; success JSON of R-API-OPS | zero-read; error naming the status | collect, cap 64 KiB, for the code |
  | OIDC `RegisterClient`/`CreateToken` | 64 KiB | collect, cap 64 KiB, then JSON | zero-read; failed | collect, cap 64 KiB, for the code |
  | presigned PUT | `NO_READ = StreamLimits::new(1, 1)` | zero-read; success | zero-read; fixed PUT failure text | zero-read; fixed PUT failure text |
  | snapshot download (R-AR-EXT) | 500 MiB | collect, cap 500 MiB, into the in-memory archive | zero-read; error naming the status | zero-read; error naming the status (the S3 error body is never needed) |

  `NO_READ` makes the "never read" rule hold even if a future code path pulled by mistake: such a
  pull could hand over at most one byte before `TooLarge`.
- **Content-Length.** A zero-read outcome **never inspects** `Content-Length` (parity: urllib's
  PUT never read the body, and botocore did not read a 204). A huge, malformed or duplicated value
  there is ignored, and the connection is closed by dropping the stream, never drained. For a
  collect outcome: a value greater than the *effective* cap → `TooLarge` before the first pull. On
  a ≥ 400 status that means "no code from the body": the error still carries the status and the
  allowlisted header code (T-BODY-5). A value within the cap is used only to pre-allocate
  `min(length, 64 KiB)` and is never trusted. If more bytes arrive than declared, the collector
  stops at the first extra byte with `InvalidResponse`. A malformed or duplicated value on a
  collect outcome is `InvalidResponse`.
- **Retained bytes, both sides of the seam.** The collector's `Vec` holds at most the effective cap
  `C`, counted by the collector itself next to the limiter. Inside the stream, `ChunkLimiter`
  keeps the raw frame it admitted until the frame has been handed over in chunks. With
  TA-STREAM-DL, a frame is admitted only when `delivered + frame ≤ C` after narrowing. The length
  is checked *before* the frame is copied out of the transport's buffer, so an oversized frame is
  refused without a copy. So: retained ≤ `C` (collector) + `R` (the admitted raw frame, `R ≤ C −`
  bytes delivered before it) + one 64 KiB chunk in flight. Analytically that is at most
  `2·C + 64 KiB`. For the pinned SDK stack, `R` is also bounded by hyper's read buffer, and
  TA-STREAM-DL's T-SDL-7 pins `R ≤ 512 KiB` (the test fails if a dependency update raises it). The
  bound used for budgeting is therefore `C + 512 KiB + 64 KiB`: 64 KiB-class bodies stay under
  1 MiB, API success under 64.6 MiB, download under 500.6 MiB. The hyper connection buffer itself
  sits below the seam and is the same for every SDK user.
- **Lifetime (revision 3, #5065).** Revision 2 checked the clock before each `next_chunk` and set
  the read timeout to `min(configured, remaining)`. The pinned `SdkStream` captures its idle
  timeout once at `open` and has no per-call deadline, so a read started just before the cutoff
  could overrun it by a whole read timeout. That design is withdrawn. Every operation has an
  absolute monotonic **operation deadline** `D_op`: API 360 s for all attempts; OIDC
  `RegisterClient` and refresh 30 s; the callback's code exchange R-CB-TIME's `T`; PUT 300 s;
  download 300 s. Each attempt has `attempt_deadline = min(attempt start + per-attempt limit,
  D_op)`, with per-attempt limits of 120 s for the API, 30 s for OIDC, and `D_op` itself for PUT
  and download. That instant goes into `StreamLimits::with_deadline`. TA-STREAM-DL then enforces it
  inside the transport, where the blocking happens: on `open` (connect, TLS, request write and
  response head) and on every `next_chunk`. A read in flight at the deadline is cancelled by
  dropping its future, and the call returns `StreamError::DeadlineExceeded` (or `AwsError` timeout
  phase `Deadline` from `open`). No thread is created to abandon a read. `Timeouts` stay fixed per
  operation class (120 s/120 s, OIDC 30 s/30 s, PUT and download 300 s/300 s), so `SdkTransport`'s
  per-`Timeouts` connector cache keeps one entry per class instead of one per attempt.
- **Retries and backoff under the same bound.** The runner starts an attempt only while
  `now < D_op`. Before sleeping a backoff `b` it checks `now + b < D_op`, otherwise it stops with
  the last attempt's error. The sleep itself goes through the injected `Sleeper`, and the attempt
  after it gets the remaining deadline. A deadline failure is never retried.
- **What is not cut: residual R-DNS.** `GuardedResolver` resolves with the blocking
  `to_socket_addrs` inside `DnsFuture::ready`, which runs on the runtime thread before any timer
  can fire. One in-flight system DNS lookup can therefore overrun `D_op` by the system resolver's
  own limits from its configuration (with glibc's defaults of 5 s per attempt, 2 attempts and at
  most 3 name servers, about 30 s per name tried; search-list expansion can try more names). The
  design states the bound as "`D_op` plus at most one system DNS lookup"; it does
  not claim an absolute cutoff for that phase. The callback helper is covered anyway by its
  watchdog (R-CB-TIME). Making DNS cancellable (an async resolver) is a `toolkit-aws` question for
  #5056, not a launch requirement.
- **Cancellation**: on `TooLarge`, deadline, excess bytes, a zero-read outcome or any error, the
  `ResponseStream` is dropped at once, which closes the connection (`SdkStream` owns the hyper
  body). No retry follows a cap violation (it is not a transient condition).
- **Default build**: without the `aws-sdk` feature the slice's `StreamingTransport` is an
  `Unavailable` implementation that returns the Q20 message from `open`. There is no unbounded
  fallback.
- **Dependency**: production network use of L3-L6 waits until S1 is on rolling (Q20). The offline
  tests need `StreamingTransport` and `FakeStreamingTransport` from the same commit, so L3 starts
  after S1 is integrated (section 7.2) **and** after TA-STREAM-DL (below) has landed on top of it.
- **Named prerequisite TA-STREAM-DL (toolkit-aws, revision 3).** A launch-local wrapper cannot
  provide the deadline. `ResponseStream::next_chunk` blocks inside `SdkStream`'s private
  `block_on` with the idle timeout captured at `open`, and the only way to stop it from outside
  would be a thread that abandons the read, which #5065 rules out. So the addition goes into
  `toolkit-aws`, additive and small. The only implementations of the trait are `SdkStream` and
  the fake, both in that crate.
  1. `StreamLimits::with_deadline(self, deadline: std::time::Instant) -> Self` and
     `deadline(&self) -> Option<Instant>`. `StreamLimits` stays `Copy`, and `new` still refuses
     zero bounds. Without a deadline every behaviour is exactly S1's.
  2. `SdkTransport::open`: a deadline already passed → `AwsError::timeout(TimeoutPhase::Deadline,
     ...)` before the connector is called (no byte sent). Otherwise the existing
     `self.call(connector, request)` future runs under `tokio::time::timeout_at(deadline)`.
     Expiry drops the future, which cancels connect, TLS, request write and the head wait, and
     returns the same `Deadline` timeout. `TimeoutPhase` gains `Deadline` (`Display` "deadline").
  3. `SdkStream::next_chunk`: `wait = min(idle, deadline − now)`. A passed deadline returns
     `StreamError::DeadlineExceeded` without polling. Otherwise `tokio::time::timeout(wait,
     body.next())`; expiry while the deadline was the binding bound → `DeadlineExceeded`, else
     `IdleTimeout` as today. The pending `body.next()` future is dropped, so the read in flight is
     cancelled. After `DeadlineExceeded` the stream is terminal (every later call returns it).
     `StreamError` gains `DeadlineExceeded`.
  4. `ResponseStream::narrow_body_limit(&mut self, max_body_bytes: u64)`: lowers the stream's cap
     to `min(current, max_body_bytes)` and never raises it. If more has already been delivered,
     the stream becomes `TooLarge`-terminal. Both implementations delegate to the new
     `ChunkLimiter::narrow` and `ChunkLimiter::remaining() -> u64`.
  5. `SdkStream`'s pull compares the frame's length with `limiter.remaining()` *before*
     `bytes.to_vec()`. A frame over the remainder is refused as `TooLarge` without a copy.
  6. `FakeStreamingTransport`: records the `StreamLimits` (with deadline) of every `open` and
     every `narrow_body_limit` call. A new `FakeChunk::Stall` returns `DeadlineExceeded` when the
     stream has a deadline and `IdleTimeout` otherwise. The fake is a unit seam; the cut of a real
     blocked read is proven on `SdkTransport` (T-SDL-1..3).
  - Tests T-SDL-1..8 (in `toolkit-aws`, offline; the loopback TLS harness of S1's `tls_tests.rs`
    with the test resolver, real time with short deadlines and a slack bound of 250 ms, no
    external network): (1) the server sends the head and one body byte, then stalls with the
    connection open. Idle timeout 120 s, deadline `now + 300 ms` → `next_chunk` returns
    `DeadlineExceeded` within 300 ms + slack, the server observes the connection closed after the
    stream is dropped, and the call never waits for the idle timeout. (2) The server accepts and
    never sends a head → `open` returns the `Deadline` timeout within the bound. (3) The server
    accepts TCP and stalls the TLS handshake → the same. (4) `narrow_body_limit`: narrowing below
    what was delivered → `TooLarge`; raising is a no-op; narrowing then a frame over the
    remainder → `TooLarge`, and nothing past the remainder is handed over. (5) `StreamLimits`:
    `new(0, _)`/`new(_, 0)` still refused; `with_deadline` round-trips; no deadline gives S1
    behaviour byte for byte (the S1 stream tests re-run unchanged). (6) A deadline already passed
    at `open` → no connection is accepted by the harness. (7) Frame bound: the server writes an
    8 MiB body in one `write`; every raw frame `SdkStream` admits is ≤ 512 KiB (pins `R` for
    R-API-BODY's retention bound). (8) The fake: recorded limits and narrows, and `Stall` with and
    without a deadline.
  - Ownership: filed as #5080 (`toolkit-aws`), related to #5056 (promoting
    the bounded body reader into `toolkit-aws`). L3 depends on it. If the PM prefers, it can be
    landed as the first commit of L3, touching `toolkit-aws` only, with T-SDL-1..8.
- Tests T-BODY-1..10: (1) a launch `GeneratingStream` test double that synthesises chunks lazily,
  without end, and counts pulls; on an API success without Content-Length the collector stops at
  cap + 1 with `TooLarge`, keeps ≤ cap + 64 KiB, pulls at most `cap / 64 KiB + 1` chunks and drops
  the stream (the double records the drop); (2) a false small Content-Length (10) followed by more
  bytes → `InvalidResponse` at byte 11, no further pull; (3) Content-Length above the cap → refused
  with zero pulls; (4) exactly `cap` bytes → success (boundary); (5) a 500 status with an endless
  body → stopped at 64 KiB, the code still taken from the header and allowlisted, retry decided
  from the status only; (6) an endless OIDC `CreateToken` body → 64 KiB stop, flow `failed`
  (`invalid_response`), no session written; (7) a PUT answered 200 with an endless body → success
  with zero pulls, and answered 403 → fixed text with zero pulls; (8) an endless download, with the
  cap injected small in the unit and the production constant asserted separately → stopped at cap
  + 1, nothing on disk; (9) (replaced in revision 3) the runner passes
  `min(attempt start + 120 s, D_op)` to `StreamLimits::with_deadline` on every attempt (asserted
  from the limits the fake records), and a fake stream answering `DeadlineExceeded` ends the
  operation with "Could not reach Launch with AWS in {region}." and no retry. That the production
  transport really cuts a blocked read is proven by T-SDL-1..3 below, not by a fake clock between
  immediately returning chunks; (10) a source gate in the style of `r_log_gate`: no
  file under `src/launch/` names `Transport::send`, `.send(` on a `Transport`, or `ApiClient`, and
  the network constructors take `&dyn StreamingTransport`. None of these tests uses a prebuilt
  oversized `Vec` as evidence; only the collector's retained bytes and pull counts are asserted.
- Tests T-BODY-11..16 (revision 3, #5064 and #5065; over `FakeStreamingTransport` as extended by
  TA-STREAM-DL. The fake applies `StreamLimits` and `narrow_body_limit` through the real
  `ChunkLimiter` and records the limits of every `open`. A fake that accepted a zero cap or knew the
  status before `open` would not be valid evidence, and none does): (11) the production
  `limits_for(operation)` constructs `StreamLimits::new` for every table row without error, with
  the values of the table, `NO_READ == (1, 1)` and no zero anywhere; (12) PUT answered 200 and
  403, `DeleteLaunch` answered 204 and 200 with an arbitrary body and a `Content-Length` of
  `2^63`: `frames_pulled() == 0` each time, the correct output or fixed text, and no
  `Content-Length` error; `DeleteLaunch` answered 404 with an error body → narrowed to 64 KiB,
  the allowlisted code read; (13) one `GetLaunch` operation whose first attempt is 500 with a
  64 KiB + 1 body (narrowed limit → `TooLarge`, no body code, retry decided from the status) and
  whose second attempt is 200 with a 1 MiB body → success. The fake's records show ceiling
  64 MiB for both opens and a narrow call only on the 500; (14) an oversized first raw frame:
  a 500 status whose single raw frame is 1 MiB → refused at admission after narrowing, zero bytes
  handed to the collector. A 200 status whose single raw frame is ceiling + 1 → refused at
  admission. The instrumented peak of collector bytes plus limiter pending stays within
  `C + R + 64 KiB` in each case; (15) the download path: 403 and 307 → zero pulls, error naming the
  status, nothing on disk; (16) retries near the bound: with a fake clock at `D_op − 1 s` and a
  scripted 503, a backoff of 2 s is not slept and the operation ends with the 503's error; with a
  backoff of 0.5 s the next attempt's deadline is exactly `D_op`; no attempt opens at or after
  `D_op`.

### R-API-OPS. Expected status and body per operation (#5051)

From the embedded model (`http.responseCode`, `output`), with botocore's acceptance of any 2xx kept:

| Operation | Modelled success | Success rule | Body on success |
| --- | --- | --- | --- |
| `CreateUploadUrl` | 201, `CreateUploadUrlOutput` | any 2xx | required JSON object, 64 MiB cap, shaped |
| `CreateLaunch` | 201, `CreateLaunchOutput` | any 2xx | required JSON object |
| `GetLaunch` | 200, `GetLaunchOutput` | any 2xx | required JSON object |
| `ListLaunches` | 200, `ListLaunchesOutput` | any 2xx | required JSON object |
| `StartLaunchExecution` | 202, `StartLaunchExecutionOutput` | any 2xx | required JSON object |
| `DeleteLaunch` | 204, no output shape | any 2xx | **not read**, never parsed; stdout `{"deleted": true, "id": <redacted_value(id)>, "region": R}` exactly as the Python |
| presigned S3 `PUT` | (outside the model) | any 2xx | **not read**; success as the Python |

- For the JSON operations an empty, non-UTF-8, non-JSON or non-object body is `InvalidResponse`
  ("The Launch with AWS service returned an invalid response."), with no retry.
- 3xx on any operation is an error with the status (`... failed (307)` / `PUT (presigned S3
  upload) failed (307)`). No redirect is followed (urllib does not redirect a `PUT` either).
- ≥ 400: R-API-ERR with the allowlisted code. Retries follow the shared policy on the status alone.
- Tests T-OPS-1..7: (1) `DeleteLaunch` 204 with an empty body → the exact Python JSON (indent 2),
  exit 0, one request, no retry; (2) `DeleteLaunch` 200 with an arbitrary non-JSON body → the same
  success, zero body pulls; (3) each JSON operation with an empty body, malformed JSON, a JSON array
  → `InvalidResponse`, one request; (4) a non-modelled 2xx (200 for `CreateLaunch`) → success;
  (5) a 3xx for an API operation → error naming the status, no second request; (6) a PUT 200 with
  an empty body and with an XML body → success, zero pulls; a PUT 307 → the fixed PUT failure text;
  (7) 4xx/5xx per operation → `ServiceError` text and hints, retried only for retryable statuses.

### R-API-CODE. Error codes are allowlisted, never echoed (#5045)

A server, or anything in the path, can put arbitrary text in `x-amzn-ErrorType` or the body's
`__type`/`code`/`Code`/`errorCode` (and OIDC `error`), including an echoed bearer token, which
generic pattern redaction cannot recognise. So no received code string is ever printed:

- **Normalisation** of the candidate (header first, then the body keys in that order, from the
  64 KiB error body of R-API-BODY): take the text after the last `#` and before the first `:`.
  Compare it byte-exactly with the allowlist.
- **Allowlists**: Launch API: the six model exceptions (`ConflictException`,
  `InternalServerException`, `ResourceNotFoundException`, `ServiceUnavailableException`,
  `ThrottlingException`, `ValidationException`) plus a fixed list of AWS protocol codes
  (`AccessDeniedException`, `UnauthorizedException`, `UnrecognizedClientException`,
  `ExpiredTokenException`, `ForbiddenException`, `BadRequestException`, `NotFoundException`,
  `TooManyRequestsException`, `LimitExceededException`, `ServiceQuotaExceededException`,
  `RequestTimeoutException`, `InternalFailure`, `ServiceUnavailable`). OIDC: the 14 `sso-oidc`
  model exceptions (`AccessDeniedException`, `AuthorizationPendingException`,
  `ExpiredTokenException`, `InternalServerException`, `InvalidClientException`,
  `InvalidClientMetadataException`, `InvalidGrantException`, `InvalidRedirectUriException`,
  `InvalidRequestException`, `InvalidRequestRegionException`, `InvalidScopeException`,
  `SlowDownException`, `UnauthorizedClientException`, `UnsupportedGrantTypeException`) and the
  RFC 6749 section 5.2 `error` values.
- **What is printed**: the matching `&'static str` constant from the allowlist table, never the
  received bytes. No match, or no code → `UnrecognizedError` (D15: the Python printed any code).
  The constant is what goes into `ServiceError`'s `code`. The request id and the service message
  are never passed to `ServiceError` (empty message, no request id), so neither can reach
  `Display` or `Debug`, and nothing received is truncated by `snippet` and then printed.
- **Retry classification** uses the status and the allowlisted constant only.
- **Known-secret scrub** (second layer, R-TOK-RED): before any stderr write, any `errors[region]`
  value, any pending-record `reason` and any `Debug` rendering of a launch error, the text is
  checked for every live secret value of the command (access token, refresh token, client secret,
  PKCE verifier, code, state; each at least 8 bytes). A hit replaces the whole message with the
  generic "The operation failed. Re-run with logging enabled for details." This guards future
  code, not the allowlist.
- Tests T-CODE-1..8: (1) each seeded secret (`FAKE-ACCESS-…`, `FAKE-REFRESH-…`,
  `FAKE-CLIENT-SECRET-…`, verifier, code, state; unlabeled) returned verbatim in
  `x-amzn-ErrorType` → `UnrecognizedError`; the secret is absent from stdout, stderr, the pending
  record and every `Debug`; (2) the same in each body key `__type`, `code`, `Code`, `errorCode` and
  OIDC `error`; (3) decorated allowlisted codes (`com.amazon.launch#ResourceNotFoundException`,
  `ResourceNotFoundException:http://internal/`) → `ResourceNotFoundException` and the 404 hint;
  (4) `list-launches` with one region failing with a secret code → `errors[region]` holds
  `UnrecognizedError`, no secret; (5) 120 bytes of allowlist-looking prefix followed by a secret
  (the truncation case) → absent; (6) `x-amzn-RequestId` echoing a secret → absent; (7) an injected
  internal error whose text contains the access token → replaced by the generic message;
  (8) `Debug` of `ServiceError` and of every launch error type built from these inputs → no
  secret.

### R-API-ERR. Error mapping (stderr `{"error": ...}`, exit 1)

| Condition | Message |
| --- | --- |
| HTTP ≥ 300 (R-API-OPS) | `ServiceError` text with the allowlisted code or `UnrecognizedError` (R-API-CODE), e.g. `POST /api/launches/<id> failed (404): ResourceNotFoundException`, + the 401 or 404 hint exactly as section 1.3 |
| connect failure / timeout after retries | "Could not reach Launch with AWS in {region}." (Python: botocore text naming the URL, or the generic message) |
| invalid response, body over its cap, more bytes than Content-Length (R-API-BODY, R-API-OPS) | "The Launch with AWS service returned an invalid response." |
| operation deadline passed while reading a body (R-API-BODY) | "Could not reach Launch with AWS in {region}." |
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
| `launch/mod.rs` | `SLICE`, the 15 routes (`launch resolve-region`, `auth-start`, `auth-wait`, `auth-callback` (internal), `session-status`, `sign-out`, `create-launch`, `get-launch`, `list-launches`, `delete-launch`, `start-launch-execution`, `get-launch-status`, `get-launch-download-url`, `download-snapshot` (Q1, accepted), `apply-snapshot` (R-AR-APPLY, Q21)), output and error rendering |
| `launch/config.rs` | constants, regions, region chain, AWS profile region reader, base/issuer URL validation |
| `launch/state.rs` | state directory, private atomic writes, session and region files, pending-flow records and locks |
| `launch/oidc.rs` | PKCE, state, `RegisterClient`, `CreateToken` (code, refresh), `get_access_token`, `session_status`, `sign_out` |
| `launch/callback.rs` | request parser, validation, response pages (pure, no sockets) |
| `launch/flow.rs` | `auth-start` spawn, `auth-callback` child loop, `auth-wait` |
| `launch/http.rs` | request runner over `StreamingTransport`, `BoundedBody` collector, deadlines, retry loop over the shared `RetryPolicy` (R-API-BODY) |
| `launch/api.rs` | `BearerSigner`, the six operations, per-operation status/body table, presigned PUT, error-code allowlists, known-secret scrub, error mapping and hints (R-API-OPS, R-API-CODE) |
| `launch/shape.rs` | embedded-model response shaping |
| `launch/archive/fsops.rs` | the only `rustix` user: held-descriptor root/walk/open (R-AR-OPEN), staging, creation, exclusive publication and cleanup (R-AR-STAGE), apply writes (R-AR-APPLY), parent policy; `WalkHook`/`ExtractHook` seams |
| `launch/archive/select.rs`, `git.rs`, `zip_write.rs`, `zip_read.rs` (R-ZIP-FRAME and the profiles), `vcs.rs` (R-AR-VCS), `plan.rs` (R-AR-PLAN, revision 3), `meta.rs` (R-AR-META, revision 3), `extract.rs`, `apply.rs`, `crc32.rs` | R-AR-* |
| `launch/clock.rs` | `Clock`, `Random`, `Exit` seams (production: monotonic and system time, `getrandom`, `process::exit`) |

Seams for offline tests: `StreamingTransport` (`toolkit_aws::testing::FakeStreamingTransport`
from S1, plus the launch `GeneratingStream` double), `HostResolver` (`FixedResolver`), `Env`
(`MapEnv`), `Clock`, `Random` (seeded fake), `Exit` (watchdog), the callback slot I/O (`SlotIo`,
so trickling and blocked peers are scripted without real timing), state directory, `git` program
path, a `Spawner` for the helper, and the `WalkHook`/`ExtractHook` race seams. Revision 3 adds a
`ListenerIo` (scripted `accept`) and the `Sleeper` for R-CB-LOAD pacing; the `CommitHook`, the
`rename` and the pending-record write seams for R-CB-TIME; an `ApplyHook` between R-AR-APPLY's
phases; the recording `fsops` used by T-PLAN; and TA-STREAM-DL's fake recording of limits.

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
works), `[name]` (Q11), launch ids, `include`, `<pid>`, `--region`, `--aws-mcp-url`, `--output`,
`apply-snapshot`'s `<snapshot-dir>` and `--into` refuse `-` through the router; the `diag_contract`
enumeration covers them automatically.

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
    shortened limit); T-AR-OPEN-1..9 (R-AR-OPEN, `WalkHook` swaps on the real file system, each
    under a 2 s watchdog bound with the transport fake asserting zero requests: (1) selected file
    → FIFO, (2) → same-sized symlink to an outside file, whose bytes then appear in no archive byte
    and no message, (3) ancestor directory → symlink to an outside directory with the same names,
    (4) directory replaced by another directory (identity mismatch), (5) file grown or shrunk
    between walk and read, (6) FIFO and Unix socket present at walk time skipped without any open,
    (7) ordinary tree byte-exact in name order, (8) root path that is a symlink followed (parity),
    (9) the non-Unix dispatch returns the platform message (unit test on the platform switch));
    T-AR-ZIP-1..8 (round trip through the reader, CRC check value, mode mapping,
    pre-1980 mtime clamped, layout with no descriptors and no gaps, each limit and its exact Python
    message, `sanitize_root_name` parity table); T-ZIP-64-1..10 (R-ZIP-FRAME ZIP64 table, upload
    boundaries 65 534/65 535/65 536/100 000/100 001); T-AR-READ-1..20 (entry policy, fixture
    archives built in the test from bytes: traversal `../x`, `a/../../x`, absolute `/etc/x`, `\x`,
    `C:x`, `a\..\x`, NUL, control characters, `.` segments, empty segments, overlong names,
    duplicates by case and by NFC, symlink/FIFO/device modes, declared-size and ratio bombs, upload
    single-root rule, Snapshot profile without it); T-ZIP-FRAME-1..34 (the R-ZIP-FRAME invariant
    table, including exact deflate consumption through `miniz_oxide`); T-AR-VCS-1..8 (R-AR-VCS);
    T-PLAN-1..5 (R-AR-PLAN's planner over byte-built archives, pure: no file system).
- **TA-STREAM-DL. `toolkit-aws` stream deadline and narrowing** (revision 3, #5064/#5065; named
  prerequisite of L3, on top of S1; specification in R-API-BODY)
  - Tests first: T-SDL-1..8. Code: `StreamLimits::with_deadline`, `TimeoutPhase::Deadline`,
    `StreamError::DeadlineExceeded`, `ResponseStream::narrow_body_limit`, `ChunkLimiter::narrow`
    and `remaining`, the `SdkTransport`/`SdkStream` deadline paths, the pre-copy frame check, the
    fake's recording and `FakeChunk::Stall`. Touches `toolkit-aws` only.
- **L3. OIDC and token lifecycle** (starts once #5017 S1 is on rolling and TA-STREAM-DL has landed
  on top of it: S1's `StreamingTransport` and `FakeStreamingTransport`, with TA-STREAM-DL's
  deadline and narrowing, are the test seam)
  - Tests: T-BODY-1..16 (R-API-BODY, `launch/http.rs`; 9 replaced in revision 3); T-CODE-1..8 (R-API-CODE; the OIDC half
    here, the Launch API half re-run in L5); T-OIDC-1..10 over `FakeStreamingTransport` (exact
    `RegisterClient` and `CreateToken` request
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
    is refused at TCP level; a silent client dropped at its head deadline; deadline expiry with a
    test clock; pending record states; lock held while alive and free after exit; stale record
    cleanup); T-CB-TIME-1..14 (R-CB-TIME, injected `Clock`, `SlotIo`, `Exit` and, from revision 3,
    a `CommitHook` between admission and `rename`, a `rename` seam and a record-write seam;
    7 replaced, 9-14 added in revision 3: (1) one byte every
    1.9 s keeps no slot past its 2 s head deadline; (2) a valid head trickled over more than 2 s is
    dropped and the flow stays `listening`; (3) a callback complete just before `F` is eligible and
    one completing at `F` is not (`timed_out`); (4) a peer that never reads the page: the slot is
    dropped at its write deadline and a state-matching callback's exchange still proceeds; (5) the
    exchange through a fake transport whose attempts cross `T`: no attempt starts after `T`,
    `timed_out`, no session file; (6) the exchange response arriving at `T` + ε: the commit gate
    refuses, no session file; (7) (replaced) the main thread blocked forever in the transport in
    state `Exchanging`: at `W` the watchdog CASes to `Aborted`, makes one `timed_out` record write
    and calls `Exit(1)` without waiting. The main thread, released afterwards, finds `Aborted`,
    calls no `rename` and writes no record; (8) `auth-wait` reports a kept terminal record and
    deletes it, reports "did not finish in time" once the lock is free after the cutoff, and
    `auth-start` removes records older than one hour and its own stale `.tmp` files;
    (9) `CommitHook` pauses right after a successful CAS at `T − 1 ms`, before `rename`. The clock
    is advanced to `T`, `W` and `cutoff + 5 s`. At `W` the watchdog calls `Exit(1)` at once with no
    record write. With the lock held and no session, the `auth-wait` evaluator keeps polling before
    `cutoff + 5 s` and returns `helper_blocked` at it, never the final timeout. Then the hook is
    released and the rename publishes; once the lock is free the evaluator returns success. In a
    variant where the released rename fails, lock free and no session → "did not finish in time";
    (10) a `rename` seam that never returns: `Exit(1)` at exactly `W`, no record write after `W`,
    the evaluator answers `helper_blocked` at `cutoff + 5 s` and at `cutoff + 1 h`; (11) record
    seams: the watchdog's `timed_out` write fails → `Exit(1)` still at `W`. The write blocks →
    exactly one write attempt and no other operation; the evaluator reports `helper_blocked`
    while the lock is held and "did not finish in time" once it is free; (12) the CAS at `T − ε`
    is admitted and at exactly `T` refused (no `rename` call, temporary file removed). A watchdog
    winning `Exchanging → Aborted` first makes the main CAS fail with no `rename` and no record
    write. The seam call order shows the temporary write and `sync_all` strictly before the CAS;
    (13) the decision table: every combination of record status (none, `listening`,
    `exchanging`, `succeeded`, `failed`, `timed_out`, unreadable), lock held or free, session valid
    or absent, and time (before or after `cutoff`, before or after `cutoff + 5 s`) gives its row.
    No combination with the lock held yields a final failure or timeout; (14) lock custody: the
    helper's `Spawner` records zero spawns, every file it opens is `O_CLOEXEC`, and in T-FLOW a
    real helper's lock becomes free only after `Child::wait` has reaped the process);
    T-CB-LOAD-1..10 (R-CB-LOAD; 4 replaced, 6-10 added in revision 3, with a scripted `ListenerIo`
    and a fake clock: (1) 300 malformed plus 300 wrong-state requests (with `code` and with
    `error`), then a state-matching callback → success; (2) 16 idle connections hold every slot, a
    17th is closed unread and the callback is served after a slot frees; (3) a continuous flood
    until `F` → `timed_out` and no other status; an exhaustive `match` shows the status enum has no
    traffic-driven member; (4) (replaced) instrumented bounds: never more than 16 slots or 8 KiB
    per buffer, per turn at most 8 `accept` and 16 slot operations, busy turns paced to 2 ms and
    idle turns to 10 ms; (5) closed-unread and malformed connections leave the flow state
    byte-identical in the pending record; (6) `accept` always ready (an endless flood), with a
    valid callback already admitted in slot 5 whose head arrives in 3 reads: at most 8 accepts per
    turn, slot 5 read once every turn, success within 3 turns; (7) the clock advances 1 ms per
    operation and `F` falls inside a turn's admission quantum: the loop stops at most one
    operation after `F`, status `timed_out`; (8) a trickling slot's head deadline passes in the
    middle of a turn while `accept` is always ready: the slot is dropped at most one operation
    late and is never served after its deadline; (9) one simulated second (the clock moves by
    sleeps plus 1 µs per operation) with `accept` always ready and all 16 slots always
    progressing: at most 500 turns, 4 000 accepts and 8 000 slot operations, status still
    `listening`; with no traffic at most 100 turns; (10) fairness: 16 readable slots are each
    served exactly once per turn, the starting slot rotates, and no slot waits more than one turn
    between services); T-FLOW-1..8 at binary level using
    `CARGO_BIN_EXE_toolkit` (`auth-callback` with
    valid stdin prints one port line and serves; empty, oversized, invalid stdin → exit 1 without
    binding; helper argv contains no secret (read from `/proc/<pid>/cmdline` on Linux, skipped
    elsewhere); `auth-wait` for an unknown pid; helper exit without session; timeout message;
    success when a session file appears; the exchange path in the default build ends `failed` with
    reason `transport_unavailable`).
- **L5. API client, shaping and the commands**
  - Tests: T-OPS-1..7 (R-API-OPS, `DeleteLaunch` 204 first); T-CODE-1..8 (Launch API half);
    T-API-1..14 over `FakeStreamingTransport` (paths, percent-encoded ids, repeated `include`,
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
  - Docs: `docs/migration/launch-with-aws.md` (commands, divergences D1-D22, residuals), the
    `docs/migration/README.md` row.
- **L6. Snapshot download, extraction and apply** (Q1 accepted)
  - Tests: T-STAGE-1..9 (R-AR-STAGE); T-APPLY-1..17 (R-AR-APPLY, and R-AR-META from revision 3);
    T-PLAN-6..8 (R-AR-PLAN through staging, `download-snapshot` and the apply walk); T-EXT-1..14 (every
    T-ZIP-FRAME, T-ZIP-64, T-AR-READ and T-AR-VCS refusal run end to end through
    `download-snapshot` **with nothing created** at the destination and no `.partial` left; a
    valid snapshot lands atomically with the exact modes; destination exists → refused before
    download; a failure in the middle of extraction (CRC on the last entry) → descriptor-relative
    cleanup, nothing at the destination; size bomb stopped at declared + 1 byte; download:
    non-https, userinfo, disallowed host, 3xx, oversize `Content-Length`, endless body without
    `Content-Length` (R-API-BODY double), error text without the URL; no temporary archive file is
    ever created).
- **L7. Security review** at the exact SHA of L0-L6 by the opposite model family, checklist =
  sections 3-6 and the T- list (AC1 for the implementation; this document is the pre-review input).
- **L8. Cutover** (with #4414/#4487/#4415 and after the operator's live run, section 9): rewrite
  `SKILL.md` invocation (`toolkit launch <command>`), the required-files list, the dependency line,
  step 8 (8A `download-snapshot`, step C `apply-snapshot` in place of `rsync -a`, keeping the
  explicit user consent before applying and the `git status`/`git diff --stat` review); resync
  the `plugins/aws-core` mirror through `toolkit sync`; retire the six scripts and their mirrors
  in the retirement slice once callers and tests are verified.

### 7.3 Dependencies

Prefer what the workspace compiles already; every addition is pinned with `=`.

| Crate | Status | Why |
| --- | --- | --- |
| `toolkit-aws` (workspace) | existing | endpoint validation, request types, `RetryPolicy`, `redact`, `Secret`, `ServiceError`, `recorded`, testing doubles (`Transport::send`/`ApiClient` are not used by the slice, R-API-BODY) |
| `toolkit-core` (workspace) | existing, extended in L0 | JSON, SHA-256 (raw digest added), base64url |
| `unicode-normalization` | existing direct dependency of `toolkit-cli` | NFC duplicate detection in R-AR-READ |
| `getrandom` `=0.3.4` | **new direct**, already compiled in the default build (transitively, `cargo tree -p toolkit-cli`) | OS CSPRNG for PKCE verifier, state, `clientToken` UUIDs and temporary names; `std` has no stable CSPRNG |
| `miniz_oxide` (exact version fixed at implementation, 0.8.x or 0.9.x already in the local registry) | **new**, pure Rust, one transitive crate (`adler2`) | raw deflate/inflate through `inflate::core` with a bounded wrapping output buffer and an exact-consumption check (R-ZIP-FRAME step 8); hand-writing deflate is out of the question (Q2) |
| `rustix` `=1.1.5`, features `std`, `fs`, `process`, `cfg(unix)` only | **new to the default build**; the same version #5017 S1 already pins as an optional `cfg(unix)` dependency of `toolkit-aws` (`std`, `stdio`). Its dependencies `bitflags`, `errno` and `libc` are already in `Cargo.lock`; `linux-raw-sys` arrives with S1 | safe `openat`/`statat`/`fstat`/`mkdirat`/`unlinkat`/`fchmod`/`Dir`/`renameat_with(NOREPLACE)` and `geteuid` for R-AR-OPEN, R-AR-STAGE and R-AR-APPLY (#5039, #5047); safe `std` has no descriptor-relative file API, and the workspace forbids `unsafe`. Used only in `launch/archive/fsops.rs` |
| `toolkit-aws` `stream` module and `testing::FakeStreamingTransport` | from #5017 S1 (`be0a06df`), ungated (not behind `sdk`) | the bounded body seam of R-API-BODY (#5044) |
| `toolkit-aws` TA-STREAM-DL additions | **new prerequisite** (revision 3), additive to S1, no new crate (`tokio::time::timeout_at` is already in S1's tokio features through `tokio::time::timeout`) | absolute per-attempt deadlines on `open` and `next_chunk`, and status-time narrowing of the body cap (#5064, #5065) |
| `toolkit-cli` feature `aws-sdk` | from #4413 S1 (accepted design `fd7e7ffd`, implementation #5017) | the real HTTPS `SdkTransport` (`Transport` and `StreamingTransport`) with R-ROOTS; the default build uses an `Unavailable` streaming transport like `asm exec` |

Not added: an HTTP server crate (the callback needs one request line; `std::net` is enough and
smaller to review), a URL crate (`toolkit-aws` already parses and validates URLs), `zip`
(alternative to `miniz_oxide` + own container code, Q2), `zeroize` (strings are dropped; the
guarantee it adds over that is small and it would touch every secret type), `webbrowser` (Q4),
`libc`/`nix` as direct dependencies (no signals needed, R-CB-WAIT; file-system calls go through
`rustix`), `cap-std` (it would do the job too, but `rustix` is already pinned by S1 and the slice
needs only nine calls), `tempfile` (private directories are `mkdirat` with `getrandom` names).

Python `json.dumps(indent=2)` emulation: the workspace has several slice-local copies
(`aurora/pyfmt.rs` `dumps`, `ai_data/mod.rs` `dumps`, `bedrock_agent.rs`, `dynamodb/pyfmt.rs`
compact `dumps`). L5 reuses `aurora::pyfmt::dumps` (indent 2, `ensure_ascii`, Python float repr)
rather than adding a fifth copy; promoting one shared writer is filed as a kaizen (see the
handoff).

## 8. What is offline-testable and what needs the operator's live run

Offline (default daemon test job, no network, no AWS, no browser): every T- test above. Loopback
sockets on `127.0.0.1` are used by T-CB-NET, T-CB-LOAD and T-FLOW only (as #4413's T-TLS
harness; T-CB-TIME uses the scripted `SlotIo` and a fake clock, so no test waits real minutes);
the `git` binary is used by T-AR-GIT and T-APPLY on temporary repositories; `mkfifo` and Unix
sockets by the Unix-only T-AR-SEL, T-AR-OPEN and T-APPLY cases; the race tests swap entries on
temporary directories through the `WalkHook`/`ExtractHook` seams. Nothing reads the real `HOME`,
`~/.aws` or environment (injected seams, `MapEnv`), and nothing touches a real user repository.
The pre-review resolution tests (T-AR-OPEN, T-AR-VCS, T-APPLY, T-CB-TIME, T-CB-LOAD,
T-ZIP-FRAME, T-ZIP-64, T-BODY, T-CODE, T-STAGE, T-OPS) are all offline. So are the revision-3
tests: T-PLAN and T-CB-LOAD-6..10 (fake clock, scripted listener), T-CB-TIME-9..14 (fake clock and
seams; 14 uses a real helper process), T-APPLY-11..17 (temporary repositories built with the
`git` binary; `git worktree add`/`repair` need no network), and T-SDL-1..8. T-SDL runs in
`toolkit-aws` on S1's loopback TLS harness with real time, deadlines of a few hundred
milliseconds and a 250 ms slack bound, never an external host.

Live-run additions from revision 2: record the snapshot ZIP's version-made-by host, flags and
whether it uses data descriptors or ZIP64 (R-ZIP-FRAME may only be widened from that evidence,
through a new Issue); run `apply-snapshot` on the sample app's migration branch. Revision 3 adds:
record the snapshot's file and directory counts against R-AR-PLAN's budget, and run
`apply-snapshot` once in an ordinary `.git` layout (R-AR-META's discovery on the operator's Git
version).

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
  (R-CB-WAIT). Revision 3 adds the non-final `helper_blocked` message when the helper's lock is
  still held at `cutoff + 5 s` (R-CB-TIME decision table).
- D8: OIDC endpoint fixed, `AWS_ENDPOINT_URL*` ignored; issuer URL rejects userinfo (R-TOK-OIDC).
- D9: local id/name validation and token check before the upload work (R-API-HTTP).
- D10: `null` required members are invalid responses (R-API-SHAPE).
- D11: archive: non-regular files skipped, deterministic order, pre-1980 clamp, limits enforced
  while building, non-UTF-8 names refused, Git hardening options (R-AR-*).
- D12: no HTTP proxy and no `AWS_CA_BUNDLE`; trust roots per #4413 R-ROOTS (Q7).
- D13: connect failures read "Could not reach Launch with AWS in {region}." (R-API-ERR).
- D14: `download-snapshot` is new; `parse_github_url`/`download_github_zip` are not ported (Q1, Q10).
- D15: an error code that is not on the R-API-CODE allowlist prints `UnrecognizedError` (the Python
  printed any code received); the request id is never printed (it never was in `ApiError` text).
- D16: `create-launch`, `download-snapshot` and `apply-snapshot` are Unix-only (the exclusive
  rename makes `download-snapshot` Linux and macOS only); elsewhere they fail closed with "This
  command is not supported on this platform." (R-AR-OPEN). The Python ran anywhere, without these
  guarantees.
- D17: `rsync -a <outputDir>/ .` is replaced by `apply-snapshot`, which refuses VCS metadata,
  links and special files in the snapshot, refuses existing symlinks and type conflicts in the
  target instead of replacing them, does not preserve timestamps and keeps existing directory modes
  (R-AR-APPLY).
- D18: `download-snapshot --output` refuses a parent directory that other users can modify
  without the sticky bit, or that another non-root user owns (R-AR-STAGE). The Python had no
  `--output`; its fixed `/tmp` names were the weakness this replaces.
- D19: the callback serves at most 16 connections at a time with 2 s head and write deadlines, a
  fixed per-turn work quantum and pacing (at most 8 accepts and 16 slot operations per turn, at
  most 500 turns per second; revision 3), and a token exchange started before the deadline may
  finish up to 60 s after it (R-CB-LOAD, R-CB-TIME). The Python served one connection at a time
  with no per-connection timeout.
- D20 (revision 3): `apply-snapshot` runs only inside a Git work tree whose metadata one hardened
  `git rev-parse` (Git 2.31 or later) can locate. It refuses to run with `GIT_DIR`-style variables
  set, refuses to write into the repository's actual Git and common directories under any name,
  and refuses directories that look like Git metadata on either side, including the rare ordinary
  directory holding `HEAD` next to `objects` or `refs` (R-AR-META). `rsync -a` wrote anywhere,
  including the metadata.
- D21 (revision 3): network operations end at their absolute deadline, plus at most one system
  DNS lookup (R-DNS), instead of after a per-read timeout (R-API-BODY, TA-STREAM-DL). Zero-read
  outcomes ignore `Content-Length` entirely, as the Python did.
- D22 (revision 3): `download-snapshot` refuses a snapshot that would create more than 200 000
  files and directories, implicit parents included, and `apply-snapshot`'s walk uses the same
  budget (R-AR-PLAN). `unzip` had no such bound.

Residuals: W-1 Windows file permissions (R-TOK-MODE; tokens only, since the archive commands are
Unix-only); R-2 one non-blocking `open` of a device node that a writer with `CAP_MKNOD` swaps into
the source tree (R-AR-OPEN); R-CB-DOS a local process can keep the callback's 16 slots busy and
delay sign-in until the deadline, never end it early (R-CB-LOAD); L-1 advisory locks on network
home directories (NFS) may not reflect liveness, in which case `auth-wait` falls back to the
recorded cutoff; S-1 the bearer token and secrets live in process memory as ordinary strings until
dropped. Revision 3: **R-CB-FS** a helper whose `rename` (or `timed_out` record write) blocks in
the kernel, typically on a network or FUSE home directory, outlives `W` until that system call
returns. No new operation starts after `W`, the session file is never partial, and `auth-wait`
reports `helper_blocked` rather than a final result while the lock shows the helper alive
(R-CB-TIME); on NFS, residual L-1 can weaken that lock signal. **R-DNS** one blocking system DNS
lookup can overrun an operation deadline by the resolver's own configured limits
(R-API-BODY), pending an async resolver in `toolkit-aws` (#5056's area). Revision 1's R-1 (rename over an empty racing directory) is closed by
`renameat_with(NOREPLACE)` (R-AR-STAGE).

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
  **PM ruling (seq 2, amended seq 3): `miniz_oxide` (raw deflate) plus our own strict container,
  stored and deflate only, single disk, no encryption; ZIP64 in scope for writer and reader under
  strict rules.** Revision 2 specifies the container fully in R-ZIP-FRAME (#5043) and the ZIP64
  subset there (#5046): ZIP64 EOCD record and locator only, written at 65 535 entries or more,
  per-field sentinels exactly where the ZIP64 value is out of range, per-entry ZIP64 extras only
  for sentinel fields, all limits applied to the ZIP64 values before any record is read. The
  `zip` crate stays out.
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
  **PM ruling: yes.** Revision 2 (#5044) removes the "otherwise collected" fallback: every body
  goes through S1's `StreamingTransport` and R-API-BODY, with no unbounded path.
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
  **PM ruling (seq 2), refined in revision 2:** L0-L2 proceed now. L3-L6 offline work starts once
  #5017 S1 (`be0a06df` or its landed successor) is on rolling, because R-API-BODY and its tests
  are built on S1's `StreamingTransport` and `FakeStreamingTransport`. Production network use
  needs the `aws-sdk` build. Revision 3: L3 additionally waits for the `toolkit-aws` prerequisite
  TA-STREAM-DL (R-API-BODY), because the pinned S1 stream cannot carry an absolute deadline or a
  status-time cap; L0-L2 are unaffected.
- **Q21. `apply-snapshot` instead of `rsync -a` (revision 2, #5040).** Recommended: **yes**. A
  denylist in the extractor alone would leave the apply step trusting whatever is in `outputDir` at
  apply time. The command enforces the same denylist where the repository is written and refuses
  rather than replaces user symlinks. Product choice for the PM: hostile snapshots may never write
  repository metadata through the automatic path (R-AR-VCS); any exceptional metadata migration
  needs its own reviewed path. Alternative: keep `rsync -a` with `--exclude` rules. That is weaker
  (textual, case-sensitive matching, follows the target's links) and is not recommended.
- **Q22. Platform scope of the file-system boundary (revision 2, #5039, #5047).** Recommended:
  Unix-only archive commands through `rustix` (D16), rather than claiming a guarantee that safe
  `std` cannot give. No supported caller runs the toolkit on Windows today; a Windows port would
  need its own handle-relative design and review.
- **Q23. Snapshot node budget (revision 3, #5066).** Recommended: 200 000 materialised nodes
  (files plus every explicit or implicit directory), with files still at most 100 000, enforced
  before the first creation and reused by `apply-snapshot`'s walk (R-AR-PLAN, D22). It admits
  every snapshot of a tree the upload side accepts that has no more directories than files.
  The alternative of counting directories against the 100 000 entry limit would refuse real
  repositories near the upload limit. Engineering decision; the PM may change the constant
  without changing the mechanism.
- **Q24. `apply-snapshot` only inside a Git work tree (revision 3, #5061).** Recommended: **yes**
  (R-AR-META, D20). Metadata protection needs Git to say where the metadata is. `SKILL.md` step C
  always applies on a migration branch, and the review after applying uses `git status`, so a
  target without Git is outside the supported workflow. If the discovery cannot be established,
  the command refuses rather than writing unprotected (the #5038 round-2 engineering
  interpretation). Product choice for the PM only if a non-Git target must be supported; that
  would need its own reviewed path.
