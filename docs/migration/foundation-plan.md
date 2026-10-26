# Foundation bootstrap plan (#4373)

Manager decision, 2026-10-26: hand-drive this bootstrap after two plan-only topology failures. Primary implementation remains DeepSeek; another model family reviews the exact verified source. Later ports use the versioned topologies. This plan gives engineering defaults; Issue acceptance criteria and actual source behavior govern delivery.

Evidence: GLM planning sessions `23da0a64-5b2f-5a48-a1de-c1b2b16a97c4` and `903bd09b-feb5-5a36-b2ec-3f7f9477ce53`. Their outputs are migration research, not test results. The original workspace has no Cargo project or Rust test baseline. The installed harness documents a separate Rust scoped-test adapter; generic recipe evidence does not waive it.

## Bounded implementation

Create one Cargo workspace with `crates/toolkit-core` and `crates/toolkit-cli`, a committed lockfile, and a locally available pinned toolchain. Keep dependencies small; no AWS SDK is needed for bootstrap. Shared code should be only what this CLI uses. Do not port downstream validators, synchronization, domain scripts or product hooks here.

Build a useful read-only `toolkit skills list` command that enumerates skill directories under a supplied repository root, sorts output deterministically and optionally emits JSON. Support documented help/version behavior and useful error diagnostics. Define this new command's schema and exits in the delivered documentation. Do not claim its proposed behavior was an existing Python CLI contract. Decide path and duplicate handling from repository layout; keep enumeration inside its supplied root and test the choice.

Add offline CLI integration tests asserting real behavior: expected entries/order and JSON shape, empty input, missing root, usage error and stdout/stderr/exit separation. Tests must exercise the built binary. Add focused unit tests only for meaningful shared behavior. No Python, network or paid/live cloud activity.

Provide fmt/clippy/tests/build entry points within the Issue's scope, preserving daemon-stamped `CARGO_TARGET_DIR`, `RUSTC_WRAPPER` and sccache. `.rsi/project.toml` may declare diagnostic recipes, but cannot substitute for required Rust candidate evidence. `/target/` ignore and clean-tree instructions already belong to the pinned manager source.

## Verification and impasses

Commit the actual implementation on the allocated branch before final verification. Run builds/tests through `AgentSubmitJob`; use the exact launch base for scoped testing. Submit a batch with `wake:none`, arm one `when.jobs_terminal` wake, and end the turn while it runs. Inspect actual job outcomes on resume.

Probe the supported Rust scoped-test path. Do not copy harness-specific crates, forge receipts or weaken a host/repository gate. If a project-local Rust adapter is genuinely supported, implement and test its complete contract within this scope. Otherwise capture the exact daemon refusal and unmet AC4, while preserving the buildable workspace and meaningful test evidence. A diagnostic build or generic recipe pass never becomes candidate verification by assertion.

A missing integration ref or unsupported adapter is a bounded infrastructure impasse for the manager. It is not authority to push rolling/main, select a generic profile to avoid Rust checks, or change harness policy. Keep the source and full logs available for integration once the adapter is supported.

Final handoff follows [the literal contract](topologies/handoff-contract.md), includes the exact commit, every AC's evidence or unmet condition, daemon job ids/outcomes, compatibility decisions and filed friction. Publish on #4373. Do not end with a complete handoff while a result wake remains owed. No edits after the final verified source.
