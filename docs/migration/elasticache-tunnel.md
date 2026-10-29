# ElastiCache tunnel, connection and discovery tools (slice `cache-ops`, #4393)

`toolkit elasticache find-tunnel-host`, `toolkit elasticache start-tunnel` and
`toolkit elasticache test-connection` replace three scripts of the
`amazon-elasticache` skill. They extend the `elasticache` command family of
`cache-analysis` (#4392, [elasticache.md](elasticache.md)) as a second
registration (`elasticache::tunnel::SLICE`, slice name `elasticache-tunnel`),
the way `cloudwatch` registers two slices. Code is in
`crates/toolkit-cli/src/elasticache/tunnel/`.

| Command | Replaces |
| --- | --- |
| `toolkit elasticache find-tunnel-host --vpc-id VPC [--region R] [--profile P] [--recorded FILE]` | `find_tunnel_host.py` |
| `toolkit elasticache start-tunnel --instance-id ID --cache-host HOST [--cache-port N] [--local-port N] [--region R] [--profile P] [--recorded FILE]` | `start_tunnel.py` |
| `toolkit elasticache test-connection HOST [--port N] [--timeout S] [--no-tls] [--tunnel-mode] [--server-name NAME] [--username U] [--password P] [--iam-auth --iam-user U --cache-name NAME] [--serverless] [--region R] [--recorded FILE]` | `test_connection.py` |

## Offline: the network and credential boundary (AC1, AC3)

This change opens **no socket, starts no subprocess, makes no AWS call and
never connects to Redis or Valkey**. Every one of those is a seam
(`tunnel/seams.rs`) with two implementations:

| Seam | What a live build would do | Without `--recorded` | With `--recorded FILE` |
| --- | --- | --- | --- |
| `TunnelAws` | `ec2:DescribeInstances`, `ec2:DescribeInstanceStatus`, `ssm:DescribeInstanceInformation` | `UnavailableAws` | `RecordedAws` (shared `toolkit_aws::recorded::StepQueue`) |
| `TunnelHost` | bind `127.0.0.1:port`, `PATH` lookup, connect probe, spawn `aws ssm start-session` | `UnavailableHost` | `RecordedHost` / `RecordedProcess` |
| `CacheEnvironment` | DNS lookup, Valkey client, IAM token signer | `UnavailableCache` | `RecordedCache` / `RecordedConnection` |

Without a recording the first seam call fails with the typed error
`elasticache_seam_unavailable` (exit 1, message on stderr): "<seam> is not
available in this build: it opens no sockets, starts no processes and makes no
AWS or Redis/Valkey connections; pass --recorded FILE to replay a recorded
run". The error names the seam and never a request value. Argument validation
and the `[FAIL]` port-range pre-flight run before any seam is used.

Credentials: `--password` and the IAM token are `toolkit_aws::Secret`s from
the argument to the client seam. They are never printed, never part of a
diagnostic (the connect plan's `Debug` hides them), never in a recording and
never compared by the replay. The tunnel command echo holds the instance id,
host, ports, region and profile name only.

Caller text in diagnostics (instance names and types, SSM process output,
error messages, the echoed command, host names) goes through `clean`
(`tunnel/clean.rs`): `toolkit_aws::redact_text` on the whole text, then a cut
to 200 characters (`...`), then escape of control, bidirectional and invisible
characters (`safe_path`). The order matters: a credential that crosses the
bound is still recognized, and an escape never grows past the cut.

No cache client or server string is ever printed, recorded or put in an error
(PM ruling for #5004, #5005 and #5007, which replaced the by-value scrubber of
#4986; scrubbing every encoding of a secret out of free text is an arms race).
A client-init, auth, `PING` or connection failure is a `CacheErrorKind`, a
closed set (`authentication_failed`, `connection_refused`,
`tls_handshake_failed`, `timeout`, `dns_failed`, `other`), and prints fixed
first-party text only (`authentication failed`, `connection refused`,
`TLS handshake failed`, `timed out`, `host name lookup failed`, `client error
(details withheld)`). The TLS hint follows the kind and `--no-tls`, never any
text. The only server text printed is `engine version` and `mode`, through
`SecretGate` in `tunnel/clean.rs`: the version must match
`^[0-9A-Za-z][0-9A-Za-z.+-]{0,31}$`, the mode must be `standalone`, `cluster`
or `sentinel`, and neither may contain the supplied password or the active
password or IAM token as a substring; otherwise `unknown` is printed. The
containment check is one exact-value pass with capped input: a value over
1 KiB is refused unread (`unknown`), and a secret longer than the value cannot
occur in it. The encoding enumeration of the former scrubber is gone.

No command writes a file: the scripts only print, so there is no output to
write privately or atomically (the `write_private` helper of `dynamodb_deploy`
is not needed here).

### Recording format (`--recorded FILE`)

One JSON object; each command accepts only the members it reads
(`find-tunnel-host`: `calls`; `start-tunnel`: `calls`, `host`, `tunnel`;
`test-connection`: `cache`). The module comment of `tunnel/recorded.rs` has
the full example.

- `calls[]`: `{service, operation, request?, response | error}` for `ec2`
  `DescribeInstances`, `ec2` `DescribeInstanceStatus`, `ssm`
  `DescribeInstanceInformation`; replayed in order per operation. The recorded
  `request` of **every** step, `error` steps included, is compared with the
  request made (key order ignored) before the outcome is replayed. An `error`
  has `kind` `client` (default), `connect`, `timeout` or `no_credentials`,
  `code`, `message`.
- `host`: `programs` (on `PATH`), `busy_ports`, `port_checks` (the answer of
  each connection probe, in order).
- `tunnel`: `settle_exit_code`, `exit_output`, `events` (`{line}`,
  `{idle: true}`, `{interrupt: true}`), `exit_code`, `stops_gracefully`,
  optional `command` that must equal the command built.
- `cache`: `address` (DNS answer; absent is a failed lookup), `iam_token`,
  `connect.request` (the plan without credentials; compared), `connect.init_error`
  (a kind name, see above; free text is refused),
  `ping_ms` or `ping_error {kind}` (a kind name; a `message` or an unknown kind
  rejects the recording without echoing it)
  (exactly one when the run builds a client and so reaches PING; both, or
  neither, is a recording error and is never read as success; `ping_ms` is
  finite and not negative),
  `server {version, mode}` (absent: INFO refused by an ACL).

Every optional member is absent or of its type: a present member of another
type, `null` included, rejects the recording (the shared rule of #4727,
#4712/#4713/#4726); parse errors name the member and the types, never a value.
A call, probe, process or cache with nothing recorded, a request that differs
and a recorded step, probe or event that the run never used all fail
(`elasticache_recording_mismatch`, `elasticache_recording_unused`). Each cache
step is consumed by its own call: `address` by the DNS lookup (skipped for
`127.0.0.1`, `localhost` and `::1`), `iam_token` by the signer (only with
`--iam-auth`), `connect.request` and `connect.init_error` by the connect,
`ping_ms`/`ping_error` by `PING` and `server` by `INFO`. A run that stops early
(an init error, a failed `PING`) leaves the later steps unused and fails.

## Host selection (AC2)

`find-tunnel-host` lists the running instances of the VPC (filters `vpc-id`,
`instance-state-name=running`, every `NextToken` page), asks SSM which of them
are managed (`InstanceIds` filter, every page) and orders them SSM-managed
first, then by instance type (stable). The first SSM-managed instance is the
recommendation, labelled by its `Name` tag or else its type. No instances:
guidance and exit 1. An SSM failure lists everything with "SSM status
unknown" (access-denied codes `AccessDeniedException`, `AccessDenied`,
`UnauthorizedAccess` get their own message). An EC2 failure prints
`ERROR: ...` on stdout, exit 1.

## Process lifecycle and timeouts (AC2)

`start-tunnel` runs the pre-flight checks in the script's order (port range,
local port free, `aws`, `session-manager-plugin`, instance running, SSM online;
SSM offline is a warning and the run goes on), then spawns the session and
supervises it with the script's times, passed to the seam and never slept
here: 2 s settle (an exit in that time prints the output and exits 1), 15 s
first probe of the local port, 30 s output-idle interval with a 3 s probe and
a warning when it fails, Ctrl+C = SIGTERM, 5 s grace, then SIGKILL, and
"Tunnel closed." (exit 0); a non-zero exit after the output ends is exit 1. An
AWS-class failure in the loop prints `Error: ...`, kills the process and exits
1; a recording failure kills the process and is a typed error. The tests script
the process through `TunnelHost` / `TunnelProcess` mocks and assert the calls
and the durations; none starts a process.

## Divergences from the scripts

1. **No live clients; `--recorded FILE` and typed refusals** (above). The live
   `boto3`, `socket`, `subprocess`, `valkey` and `botocore` signer paths are
   the follow-up below. `--profile` is validated and accepted; offline it only
   appears in the built tunnel command (a recording selects the account).
2. **Argument validation.** The scripts passed everything to `boto3` and the
   shell. Here `--vpc-id` must be `vpc-` + 8-17 hex, `--instance-id` `i-` or
   `mi-` + 8-17 hex, hosts and `--server-name` letters, digits and `. - _ :`
   (253 characters), `--region` a lower-case region code, names and the profile
   printable, at most 128 characters, no leading `-`. A violation is a usage
   error (exit 2) that states the rule and the length, never the value.
3. **Router policy (#4780).** Every command is `strict_values`: an empty value
   or one starting with `-` is refused, `--flag=-` included, and a bare `-` is
   never a stdin/stdout marker. No audited Python line reads `-` for any of
   these arguments, so none is listed in `STDIO_DASH`. Consequence: a password
   that starts with `-` is refused (argparse refused it as well).
   `--password` stays an argv value for parity; prefer a recording or an IAM
   token; it is never echoed.
4. **`test-connection` ports and timeout.** `--port` must be 1-65535 and
   `--timeout` positive and finite (usage error, exit 2); the script handed
   any number to the client. `start-tunnel` keeps the script's exit-1 `[FAIL]`
   for ports.
5. **Argparse text.** Usage errors are `error: ...` plus usage with exit 2 as
   in the other slices.
6. **Next-step hints** name the Rust commands (`toolkit elasticache
   start-tunnel`, `test-connection`, `find-tunnel-host`) instead of
   `python scripts/...`. The original hint told readers to pass `--tls` and
   `--sni`, options `test_connection.py` never had; it now says
   `--tunnel-mode --server-name`.
7. **Echoed command** is shell-quoted per argument (the script joined with
   spaces, so its JSON parameter was not copy-pasteable) and cleaned.
8. **Output is read to the end.** The script left the loop as soon as the
   process had exited and dropped unread lines; the port reads until the output
   closes, so no line is lost.
9. **Client name.** The connecting line says `via <client>`; the recorded
   client is `recorded replay` (the script printed `valkey-py`).
10. **Private address test** follows Python `ipaddress.is_private` for the
    ranges that matter (RFC 1918, loopback, link-local, unspecified,
    broadcast, documentation, `198.18/15`, `240/4`, unique-local and
    link-local IPv6, IPv4-mapped); it is not a full copy of the table.
11. **Malformed AWS responses** (a wrong-typed or `null` member, a missing
    `InstanceId`) are an `ERROR:` (find) or a warning (start pre-flight) with
    the member and type, where the script raised `KeyError`/`TypeError`. A
    `null` list is never an empty discovery.
12. **Pagination** is capped (1000 pages, 100000 items) and refuses a repeated
    token (`toolkit_aws::paginate`). `DescribeInstanceInformation` is not
    chunked to 100 ids as the live API requires; the script did not chunk
    either. A live client must (follow-up).
    When a later `DescribeInstanceInformation` page fails with a service error the
    instance ids of the earlier pages are kept, as in the script (`ssm_managed`
    is filled outside its `try`); the warning or access-denied text is printed
    and the hosts known to be managed are still ordered first and recommended.
    A replay mismatch is never a service error and stays fatal (#4989).
13. **Typed cache diagnostics (#5004, #5005, #5007).** Python printed the
    `redis-py` exception text (`Client init FAILED: {e}`, `PING FAILED:
    Authentication error -- {e}`, and so on) and the raw `redis_version` and
    `redis_mode`. Rust prints a fixed text per error kind, with the same
    headings, hints and exit status, and a server version or mode only through
    the allowlist and secret gate (`unknown` otherwise). A recording names the
    kind (`init_error: "timeout"`, `ping_error: {"kind": "tls_handshake_failed"}`)
    and holds no client text. The live client of #4982 maps its errors to the
    kinds; text it cannot map is `other`.
14. **Python tests.** The skill has no Python tests for these scripts
    (`test_connection.py` is a script, not a test). The Rust tests are named
    after the script functions they port: `find_tunnel_hosts_*`,
    `check_instance_running_*`, `check_instance_ssm_status_*`,
    `validate_port_*`, `check_port_available_*`, `check_aws_cli_and_check_ssm_plugin_*`,
    `wait_for_port_*`, `start_tunnel_*`, `check_dns_*`, `test_connection_*`,
    `get_iam_auth_token_*`.

## Follow-ups

- Live implementations of the three seams behind optional features (AWS
  describe calls through the toolkit-aws seams, a bounded local socket/process
  layer with `kill_on_drop` and the same timeouts, a Valkey client with TLS and
  the IAM token signer): filed as #4982.
