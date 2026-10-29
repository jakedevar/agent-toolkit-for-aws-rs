# telemetry: consent, persistence and emission (#4398)

Slice `telemetry`, wave 2 of programme #4369. Base: the integration head
`b8204e9db480a007c82ff2dc53131fccd0feba84`.

## Already landed?

`git log -S<symbol>` over `crates/` and `docs/` for `telemetry_consent`, `consentStatus`,
`plugin-telemetry`, `skillInvoked`, `migrationActivity` and `toolkit_telemetry` found nothing
before this change. The only earlier work is the **seam** left by #4377: `toolkit sync vendored`
skips the top-level `telemetry/` vendored directory (`toolkit_core::vendored::SKIP_TOP_LEVEL_DIRS`
= `TELEMETRY_BUNDLE_DIR`) and `toolkit_core::treesync` holds the byte-exact, link-refusing
primitives this slice's bundle gate is built on (`docs/migration/sync-contracts.md`, "Telemetry
seam"). Nothing of the consent record, the notice, the client, the hooks or the attribute
derivation existed in Rust.

## Result

One command, registered once (`CommandSpec { path: ["telemetry"], passthrough: true }`), because
the hook subcommands must never turn a word they do not understand into a router usage failure:

```text
toolkit telemetry consent show|status|accept|opt-out|session-start
toolkit telemetry status | reconcile
toolkit telemetry skill-invoked [--skill NAME]
toolkit telemetry migration [--reconcile] [--session-end] [--via cli]
toolkit telemetry bundles [--write | --check] [--plugin-root DIR]
        any of the above also takes  --telemetry-dir DIR
```

| Python file | Rust | Command |
| --- | --- | --- |
| `consent/record.py`, `notice.py` | `telemetry/consent.rs` | (library of the consent commands) |
| `consent/cli.py` | `consent.rs` | `consent show`, `consent status` |
| `consent/accept.py`, `opt_out.py` | `consent.rs` | `consent accept`, `consent opt-out` |
| `consent/session_start.py` | `consent.rs` | `consent session-start` |
| `metric_emission/client.py` | `telemetry/client.rs` | `status`; the sink and payload used by every emitter |
| `metric_emission/skill_invoked.py` | `telemetry/skill.rs` | `skill-invoked` |
| `metric_emission/migration.py` | `telemetry/migration.rs` | `migration`, `reconcile` |
| `metric_emission/migration_attributes.py` | `telemetry/attributes.rs` | (derivation used by `migration`) |
| `cli.py` | `client.rs`, `migration.rs` | `status`, `reconcile` |
| `sync_bundles.py` | `telemetry/bundles.rs` | `bundles` |
| `test/*.py` (5 files, 2 300 lines) | `telemetry/tests/*.rs`, `tests/telemetry.rs` | 156 tests |

## Consent is fail-closed (AC1)

The only thing that permits an event is `~/.aws-startup-advisor/plugin-telemetry.json` with
`consentStatus` exactly `ACCEPTED` and a UUID `installId`. No environment variable has any
effect. These all read as **no consent** and send nothing: no home directory, a missing,
unreadable (permissions, a directory, non-UTF-8), oversized (> 1 MiB), truncated or non-JSON
record, JSON that is not an object, an unknown or differently-cased status, a non-string status,
and `ACCEPTED` with a missing, empty, non-string or non-UUID `installId`. `OPT_OUT` needs no
`installId`, so the edit the notice describes (setting only `consentStatus`) is a valid record,
and a UTF-8 BOM is accepted. A symlinked record (a dotfile manager's link) is followed, as the Python did. The gate is checked again by `client::prepare`, the **only** way to
obtain a request body, for every request (a record that turns into an opt-out between two
requests stops the second), and `migration` reads the record before it reads a snapshot, so a user
who never agreed leaves no snapshot, no lock and no trace.

Writes are fail-loud and atomic: temporary file in the same directory (mode 0600 on Unix, as
Python's `NamedTemporaryFile`), fsync, rename; `accept`/`opt-out` exit 1 with an explanation when
they cannot write; `opt-out` sends nothing, and `accept` sends `consentRecorded` fire-and-forget
after the record is on disk.

## When the Rust binary is missing (F-11)

Telemetry defaults to **no emission** and never blocks the host:

- Today no caller is switched (see "Cutover"), so nothing changes for users.
- After a cutover each hook is `sh -c 'toolkit telemetry … 2>/dev/null || true'`: a missing
  `toolkit` is exit 127 swallowed by `|| true`; a present one never exits non-zero for a hook
  subcommand (they swallow every error, as the Python did).
- With no binary the consent notice is never raised and no record is ever written, which is "no
  consent", which is no emission. The skill protocol (`PROTOCOL.md`: "If … the bundle, or
  permission to run the command is unavailable, skip telemetry and continue") covers the
  skill-driven route.
- The hook subcommands (`skill-invoked`, `migration`, `consent session-start`) always exit 0,
  print nothing except the session-start instruction, and read stdin only with a 2 s deadline
  (`consent session-start` does not read stdin at all).

## Emission and persistence (AC2, AC4)

- **Injected sink.** Every request goes through `EventSink::post(&PostRequest)`. A request is a
  JSON `POST` of `body` (Python `json.dumps` format, `ensure_ascii`) to the endpoint
  (`AWS_STARTUP_ADVISOR_PLUGIN_TELEMETRY_ENDPOINT`, blank = prod) with `Content-Type:
  application/json` and `User-Agent: aws-startup-advisor-plugin`, 3 s timeout (shorter under the
  SessionEnd budget). Tests use a recording sink that answers with scripted statuses; no test
  opens a socket, reads credentials or contacts an endpoint.
- **This build sends nothing.** There is no HTTP client in `toolkit-cli` (`toolkit-aws` has none
  in its default build either), so the shipped sink, `NoNetworkSink`, answers `Unreachable` to every
  request. For `migration` that means the events are *held* exactly as an offline machine's would
  be: the snapshot is not advanced, so a build with a real transport reports them later and nothing
  is silently lost. `tests/telemetry.rs` has a source tripwire (no `std::net`, sockets, process
  spawning or HTTP crate in the module) and drives the real binary end to end.
- **Payload.** `{installId, source, pluginVersion, occurredAt (epoch ms), pluginTelemetryEvent}`;
  `pluginVersion` comes from `version.json` of `--telemetry-dir` then the plugin manifests of a
  recognised layout, never a default; `source` from the first present host marker
  (Cursor-specific, then Codex, Kiro, `CURSOR_AGENT`, Claude Code), else `OTHER`. A skill id outside
  the 12 `PluginSkillId` members is dropped before the wire.
- **Persistence.** Snapshot `.telemetry-snapshot.json` (indent 2), per-run lock directory
  `.telemetry-lock` (stale after 60 s), temp+rename writes, hold/retry semantics (403, 429, 5xx and
  no connection are held; 400, other statuses and timeouts are never repeated), one
  `RUN_COMPLETED` per `runMode`, session ownership for `--session-end`, snapshot rebuild without
  sending when the snapshot is corrupt: all ported with the Python tests.

## Intentional changes from the Python

1. **No interpreter.** The session-start instruction and the opt-out help name
   `toolkit telemetry consent show|accept|opt-out` instead of absolute
   `python3 ".../cli.py"` paths; `status`/`reconcile` replace `cli.py status|reconcile`. The notice
   text itself is unchanged (word-for-word test against an independent copy).
2. **`--telemetry-dir DIR`** replaces "the directory of the script": it names the directory
   holding `version.json` (a bundle or `scripts/telemetry`) and fixes the plugin layout used for
   the version fallback and `reportingMode`. Without it `pluginVersion` is `null` and nothing is
   sent (fail-closed).
3. **No network in this build** (above), so `NoNetworkSink` replaces `urllib`.
4. **Reporting-mode marker.** A hook counts as registered when its command names the Python script
   (and the script exists) **or** `telemetry migration`, so a cutover does not flip hosts to the CLI
   route.
5. **Stricter reads of customer-editable files.** The consent record is bounded (1 MiB); run
   state, gate, snapshot and artifact files are read with the link-refusing, size-bounded reader
   (a symbolic link or oversized file reads as unreadable, where Python followed it); a symbolic
   link `.migration/<id>` is not a run (Python's `is_dir` followed it); runs are visited in name
   order (Python: directory order); a `UUID` with a trailing newline is invalid (Python's `$` let
   it through); the accuracy-band pattern uses ASCII digits.
6. **Diagnostics never echo file values.** Write failures name the record path and a reason class
   (`permission denied`, `not found`, ...), not the OS message; the bundle drift lines no longer
   print the two versions with `%r` (`name: version does not match plugin.json`) and neutralise
   control characters in file names.
7. **Home directory.** `HOME`, then `USERPROFILE`; empty or unset means unknown, which is no
   consent (Python: `Path.home()`, which also falls back to the password database).
8. **Bundle writes** use the shared link-refusing primitives: a destination link is replaced as a
   link and never written through; a link in the destination path is refused; a link inside a
   bundle is reported as an unexpected file.
9. **The hook payload** from stdin is bounded to 1 MiB.
10. A non-string `sessionId` in a snapshot reads as absent; the lock's staleness is measured
    against the injected clock (tests) rather than wall time.
11. **Private snapshot and lock (#4894).** Snapshots are written 0600 (temporary file created
    with that mode, synced, then renamed) and lock directories are created 0700, whatever the umask
    (Python used the umask defaults, so 0664/0775 under umask 002). A snapshot that is group- or
    other-writable, or not owned by the current user, is never read: it is treated like a corrupt
    one, so nothing is sent and it is replaced by a private snapshot of the current state (the
    transitions since are not reported). A group- or other-writable lock directory is not honoured
    as a holder (anyone could keep it fresh) and is replaced by a private one, as a stale lock is.
    While the Python hooks still share these files, a snapshot they write under a group-writable
    umask is rebuilt the next time the Rust command runs.
12. **A bare `-` path flag (#4961).** `--telemetry-dir` and `bundles --plugin-root` take the
    router's bare-`-` policy (#4780), applied by `telemetry::screen` because a passthrough
    command's words never reach the router (the two flags are declared in the slice's
    `value_flags`, which the router ignores for a passthrough command and the
    `passthrough_commands_declare_their_value_flags_and_refuse_dash_unless_a_hook` contract test
    reads). Python's `argparse` took `-` as a plain directory string; here, for the interactive
    subcommands (`status`, `reconcile`, `consent show|status|accept|opt-out`, `bundles`, and
    anything unknown), `--flag -` and `--flag=-` exit 2 with the flag-naming usage error and a bare
    `-` word is refused too, before any path is read or written. **Divergence for the hooks**
    (`skill-invoked`, `migration`, `consent session-start`), which must never fail: the flag is
    treated as absent, so nothing is read from `./-`, nothing is emitted, the exit is 0 and the
    output is byte-identical to the same hook without the flag (fail-closed: `pluginVersion` is
    `null`). `./-` remains an explicit literal path.

Unchanged on purpose: the consent file layout, the wire fields, the notice and acknowledge
wording, the snapshot layout (`runId`, `sessionId`, `started`, `phases`, `gateFailures`,
`completed`, `via`, `updatedAt`, `completedRunModes`), every enum table, Python `round()`/`float()`/
`str()`/`or {}`/equality semantics of the attribute derivation (an artifact of a shape the Python
raised on loses its attributes, never the event).

## Bundle sync (AC5)

`toolkit telemetry bundles` is `sync_bundles.py`: it distributes the ten runtime files plus a
generated `version.json` into `skills/{azure-to-aws,gcp-to-aws,heroku-to-aws,llm-to-bedrock}/
references/vendored/telemetry/`, checks the three DSL `INTERPRETER.md` copies, refuses to run when
a host manifest disagrees with `plugin.json`, and reports (never deletes) unexpected bundle files.
`toolkit sync vendored` still skips `telemetry/`, so exactly one gate owns the bundle. Both
`--check` against the committed tree (`the_committed_bundles_are_in_sync_with_the_canonical_runtime`,
`tests/telemetry.rs`) and the generation/drift/orphan/link cases are tested. The 45 mirror files
and the nine canonical files they copy are unchanged.

## Cutover (not done in this change)

Callers and the Python stay as they are, as in every earlier port (#4399, #4408): the shipped hooks
(`com.anthropic.claude-code/hooks/hooks.json`, `.cursor-plugin/hooks.json`), `PROTOCOL.md`,
`commands/usage-data.md` and the bundles still run the Python, which reads and writes the same
consent record and snapshot, so the two can coexist. There is no distribution channel for the
`toolkit` binary in this repository yet; switching the hooks before there is one would silently turn
telemetry (and the notice) off for every user. The follow-up is: ship the binary, change the hook
commands to `sh -c 'toolkit telemetry … 2>/dev/null || true'`, rewrite `PROTOCOL.md` and
`usage-data.md` to the commands above, regenerate the bundles with `toolkit telemetry bundles
--write` (the bundle then needs only `PROTOCOL.md` and `version.json`), and delete the Python and its
tests (`python-inventory.json`: 17 canonical rows, 45 mirrors).

## Classification for the recorded-input contract

`tests/recorded_contract.rs` has no `INVENTORY` at this base (it scopes slices by signal). The
slice is **NotRecorded**: it reads no recorded or replayed AWS or service response and has no
`--recorded` flag, replay type or `toolkit_aws::recorded` import. Its inputs are the consent
record, the migration run files, the hook payload and the manifests, read through bounded
readers; its single outbound call is the injected sink. When the inventory lands the row is
`telemetry | NotRecorded | outbound telemetry only; inputs are local files, no service response`.
