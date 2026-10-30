# Shipped toolkit for skills (#5586)

The GM ruling on #5586 stages the CLI into each plugin whose skills invoke
it. `packaging/plugin-binaries.json` is the build authority. The PM repair
ruling assigns aws-agents exactly `agents-pay-live`; aws-core and
aws-startup-advisor retain exactly `ai-models-live` and `docgraph-live`.
Every entry uses the existing five-platform matrix. Generated binaries, dispatchers and
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
| `aws-core` | `aws-ai-ml` references: `ai-payloads`; future `ai-models` callers in #4720 | `${CLAUDE_PLUGIN_ROOT}/libexec/toolkit` |
| `aws-agents` | `agents-build setup-payment-user`, `agents-pay` setup/admin/fetch | `${CLAUDE_PLUGIN_ROOT}/libexec/toolkit` |
| `aws-startup-advisor` | shared model-lifecycle reference and its vendored copies: `startup model-staleness`; README: `startup skill-refs` | `${CLAUDE_PLUGIN_ROOT}/libexec/toolkit` |

`aws-data-analytics` and `aws-agents-for-devsecops` have no toolkit CLI
invocations. References to a CDK toolkit stack, the ADK toolkit, article
text and package names are not invocations. Canonical `aws-ai-ml` content
is mirrored with `toolkit sync plugin-skills`; startup references use
`toolkit sync vendored`.

Claude Code substitutes the exact `${CLAUDE_PLUGIN_ROOT}` token in skill
bodies. Bash tool commands do not inherit that variable. Each relevant skill
shows its resolved absolute root; pass that root to reference readers.
References are plain files and humans in their own terminal must first export
`CLAUDE_PLUGIN_ROOT` to that absolute directory. Their commands use
`${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit`
to refuse unset or empty roots before execution. Skill preflights check the
exact substituted root and executable; missing delivery fails loudly.
OpenClaw separately ships via npm and retains `./bin/toolkit` in its directory.

The #5629 ruling uses private `libexec/` executables, with no top-level plugin
`bin/`. The packaging guard rejects that directory even when empty or a
symlink. Claude Code adds a plugin's top-level `bin/` to Bash PATH, creating
name collisions among the three builds; claude.ai and Cowork reject such a
plugin. An explicit `libexec/toolkit` path avoids that automatic registration.
Do not add plugin `libexec/` to PATH. A bare `toolkit` cannot select the
payment build via automatic plugin registration. See the
[host reference](https://code.claude.com/docs/en/plugins-reference).
claude.ai and Cowork install/native execution are unverified release checks;
removing `bin/` does not establish support. The exact Claude validation,
skill and hook release checks are in [plugin-binaries.md](plugin-binaries.md).

Inventory check: all aws-agents toolkit invocations are `agents-pay` or
`agents-build setup-payment-user`, both payment routes. No aws-agents skill
or reference calls an ai-models or docgraph route, so a feature union is
unnecessary. Both payment entries retain exactly `["agents-pay-live"]`.
The plugin-root dispatcher is part of the existing operator-gated plugin
git release layout; the npm package destination is not added to that tree.
Neither payment consent nor credentials or spend controls change.

G1 and G1a guard verification/test artifacts, not release destinations.
T-PKG-NOLIVE previously also pinned the single package destination; it now
pins both declared payment destinations while retaining its ban on staging
from tests or verification targets. Only `make plugin-binaries` stages the
live entries. `toolkit-plugin-check` checks only the model/graph binary;
the existing `clippy-live` exception lints payment libraries/binaries only.
No new verification target enables `agents-pay-live`, and no test enables it.

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

A release that includes the aws-agents plugin binaries ships a payment-capable
`toolkit` (`agents-pay-live`), a real-money surface. The operator must approve
that payment-capable binary by name; a general release approval does not cover it.

Docgraph skills under `skills/specialized-skills/` are outside every plugin.
They deliberately retain bare `toolkit` and the loud `built without
docgraph-live` refusal on a wrong build. Install their CLI separately from a
reviewed checkout: `cargo build --release --locked -p toolkit-cli --bin toolkit
--features docgraph-live`, then install that executable as
`~/.local/libexec/aws-toolkit-docgraph/toolkit` and prepend that dedicated
directory to PATH for the docgraph terminal. Do not put any plugin binary
directory on PATH. Verify `command -v toolkit` before using these skills.
No payment feature is included in this docgraph install.
