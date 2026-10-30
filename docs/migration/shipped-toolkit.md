# Shipped toolkit for skills (#5586)

The GM ruling on #5586 stages the CLI into each plugin whose skills invoke
it. `packaging/plugin-binaries.json` is the build authority. Each new
plugin-root entry enables exactly `ai-models-live` and `docgraph-live` and
uses the existing five-platform matrix. Generated binaries, dispatchers and
manifests remain uncommitted release artifacts.

Plugin-stage now accepts `subprocess` at plugin roots for skill invocations;
it was previously restricted to package destinations. Duplicate names are
still unique per destination. Package destinations still require this role,
and subprocess entries cannot register as hooks or MCP servers. Contract
tests cover both refusals and the existing OpenClaw payment entry.

Inventory: search `plugins/` Markdown for `toolkit` followed by a command,
then inspect each match to distinguish CLI invocations from prose and other
products. The invocation roots are:

| Plugin | Skill commands | Dispatcher |
| --- | --- | --- |
| `aws-core` | `aws-ai-ml` references: `ai-payloads`; future `ai-models` callers in #4720 | `${CLAUDE_PLUGIN_ROOT}/bin/toolkit` |
| `aws-agents` | `agents-build setup-payment-user`, `agents-pay` setup/admin/fetch | `${CLAUDE_PLUGIN_ROOT}/skills/agents-pay/packages/openclaw/bin/toolkit` |
| `aws-startup-advisor` | shared model-lifecycle reference and its vendored copies: `startup model-staleness`; README: `startup skill-refs` | `${CLAUDE_PLUGIN_ROOT}/bin/toolkit` |

`aws-data-analytics` and `aws-agents-for-devsecops` have no toolkit CLI
invocations. References to a CDK toolkit stack, the ADK toolkit, article
text and package names are not invocations. Canonical `aws-ai-ml` content
is mirrored with `toolkit sync plugin-skills`; startup references use
`toolkit sync vendored`.

The host substitutes `CLAUDE_PLUGIN_ROOT` with the installed plugin root;
other hosts use that absolute directory. Dispatchers select the platform
binary; skill text never names a platform-specific directory or relies on
PATH. OpenClaw's independently installed extension uses `./bin/toolkit`.

Compatibility decision: keep the existing OpenClaw package entry's
`["agents-pay-live"]` feature list. The new plugin-root binary has exactly
the two features in the ruling and therefore cannot perform payments.
Payment examples, including `agents-build setup-payment-user`, explicitly
use the payment package dispatcher. Neither payment consent nor credentials
or spend controls change.

`default_build.rs` pins the ordinary toolkit CLI's empty default feature
list and its complete default normal/build feature tree for all targets.
The graph fixture comes from the allocated base a82b0843886b5c9349fd94fe79f97902be514eca;
neither Cargo manifests nor the lockfile change. It rejects any SDK entry
and graph drift even when the test harness itself enables a live feature.
Workspace application examples retain their pre-existing dependencies.

Without `ai-models-live`, commands needing AWS model data exit 1 with
`built without ai-models-live`. Explicit `--recorded FILE` still replays
offline. Local filtering and supplied catalog snapshots remain local.
Without `docgraph-live`, explicit `--live` fails before files, credentials
or network work with `built without docgraph-live`. A review/seed/memory
invocation without `--live`, `--recorded` or an allowed `--dry-run` now
fails before producing a misleading report; live-enabled builds require
explicit `--live`. This intentionally replaces the old DocumentDB
success-with-failed-check report. Recordings and dry runs retain their
contracts and never select the live adapter.

#4720 remains separate: its six legacy scripts and callers are not retired
here. It can continue after this packaging change lands on rolling. No
public release is performed by this slice.
