# Rust migration worker contract

Take one assigned Issue. Read AGENTS.md, Issue acceptance criteria, inventory rows and named callers/tests. Start with AgentGetAuthorityCatalog and use only granted RSI controls.

## Source and scope

Keep allocated sandbox/base ancestry. Read Python; write executable implementation/tests in Rust. Preserve source behavior and shipped security controls. Update canonical content and mirrors through shared synchronization. Do not remove unrelated features or rewrite existing JS/TS/Terraform solely for language uniformity.

Keep transient schema/RPC inspection files outside the source tree, using a task-specific temporary directory. Reserve the stamped Cargo target for build output. Planning and review nodes leave no source changes or untracked files; inspect `git status --short` before handoff. A tool result is not a source artifact.

Use plan node output as an aid; actual source and Issue acceptance criteria are ground truth. Add boundary/negative fixtures and document deliberate changes. If scope is too large or a remote service requires unsupported language/runtime behavior, record a bounded follow-up through manager; never silently omit coverage.

## Verification

Commit explicit paths on allocated RSI branch. No branch changes, rebase/reset, PRs or manual integration/main/release pushes. Keep daemon build target and sccache.

Submit final source verification with AgentSubmitJob kind test, wake none, params.scoped_test.base set to the exact execution base SHA (or origin/rolling when available and appropriate). Arm one AgentScheduleWake mode when with required message, when.jobs_terminal job ids and a bounded timeout. End turn while pending, without a PIPELINE HANDOFF. Inspect actual job result after wake. Fix new failures, commit and verify again. Do not manufacture scoped-test receipts or use a generic profile to waive the Rust verification gate.

If verification infrastructure cannot support this fork, report exact refusal/log evidence as technical_impasse. Never call a failed/unsupported verification green. No edits after final verified commit.

## Handoff and review

Topology sessions finish with daemon-valid PIPELINE HANDOFF: status, doc_path/manifest_path (inline allowed), exact commit, AC-by-AC evidence, test/job results and compatibility decisions. Blocked handoffs include blocker_class, blocker_evidence and blocker_question. End final handoff with Friction: none or filed Issue references.

Review is by another model family against exact verified SHA. Reviewer checks behavior, security, test evidence and scope. Manager/daemon owns merge-queue landing; workers do not publish integration branches. Issue completion requires independent integration evidence.

## Resource and authority boundaries

No real payments, AWS deployment/deletion, live model probes, raw secret retrieval or load generation in default tests. Use mocks. Follow live spend/capacity/authority grants and genuine human gates. Engineering and tooling problems go to project manager. Never use provider-native delegation or shell/RPC bypasses.

