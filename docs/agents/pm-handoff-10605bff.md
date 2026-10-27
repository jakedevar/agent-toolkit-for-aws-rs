# PM handoff — agent-toolkit-for-aws-rs — seat 10605bff (Claude Opus 5.5)

Written 2026-10-27 ~10:45Z at ~32% context (rotation 40%). Predecessor handoff: pm-handoff-606912ca.md (branch rsi/606912ca-…).
Successor: verify live (AgentWorkerRoster, AgentListWakes, AgentListRollingQueue, inbox) before acting.

## The one remaining gate
Harness #4425 is DEPLOYED (GM mail 06:48Z, hub 01b45745b). Enqueueing waits for the GM's second mail
saying Rsi #4452 (queue side: derive_filters, lander gate, integration-ref bootstrap) is live,
key gm-1b353269-4452-live-1. Do not poll. Then enqueue with AgentEnqueueLandingSource derive_filters:true, issue:N,
ONE per gate, the next only after the previous one's landing wake. Never push main/rolling.

## Verified landing heads (AgentCheckLandingCandidate ok:true, installed tool, no new failures, merge clean)
| # | Gate | Commit | scoped job | Contains |
|---|---|---|---|---|
| 1 | #4373 foundation | e56707540064d6a8bfbf721c7984f5e1a7e2dd1f | fc57a7fa | |
| 2 | #4371 inventory audit | 497dea6fa7d9cc9b8264eddee25425a8ba2477f1 | 46423be1 | PM merge of 71c7d422 onto e5670754 (71c7d422 alone has no Cargo profile: project_verification_no_tests, kaizen #4535). In this seat's sandbox branch. |
| 3 | #4378 CI gates | 35640f1276f5a72a339f2292a29d90099a797b80 | 825d8dee | #4374-#4377 |
| 4 | #4575 integration | 0b3ef519afbb496c6ccf7d45a9b0ad291ab3a0aa | e02f20bc (804 tests) | #4387 MSK 215d9be9, #4389 dynamo d8ffe015, #4388 aurora 59e62f23, #4379 toolkit-aws 5112a8b2. Merge hunks audited by PM with --remerge-diff: registration-only (handoff on #4575). |
Enqueue source for gates in other sandboxes: pass source_session_id (worktrees: #4373 0dd33d25, #4378 21b9e6b0, #4575 27e0023f) or the commit is reachable from origin.
On landing close: #4373, #4371, #4374-#4378, #4387+#4539-#4542, #4389+#4543, #4388, #4379+#4515/#4516/#4469/#4536/#4468/#4494 (confirm each).

## Scoped-test mechanics (learned)
- scoped_test resolves the project profile from the JOB WORKTREE HEAD; run it with `worktree:` = a clean worktree at the head (manager only), base origin/rolling, timeout_minutes 180 (60 min admission times out under the disk floor; each job reserves ~24 GiB, #4461).

## In flight (terminal watches armed by launches)
- #4399 startup-validators repair 2993cf0e (review #4578 blockers #4584 symlink descendants, #4585 Unicode boundary, #4586 --skill). Then re-review on #4578, then merge into #4575 as round 2 (expect lib.rs/README/Cargo conflicts + fancy-regex dep) and re-run scoped_test. If #4399 slips, land #4575 at 0b3ef519 without it.
- #4463 SDK adapter continuation 5255aefc: adding clippy-sdk/test-sdk recipes (ruling on #4463 overrides my earlier brief), running all five. Then review (Codex sol). Rulings: no rate limiter, rename "adaptive" label; AWS_PROFILE strictness stays (#4473).
- #4451 asm-exec at 2dab20bc: security review #4591 (Codex sol ca05d276). MCP fallback fails closed; #4582 (real signer/transport, can reuse #4463 SDK transport) gates #4415.
- #4385 ai-data 87903586 (base 0b3ef519).

## Rulings to keep
- New slices branch from the newest integration head (now 0b3ef519); one COMMANDS block + one README section; reuse canonical router (`value_flags`, `strict_values`), ToolkitError::with_exit_code / Output::set_exit_status (#4573 reconciles).
- Accepted slices conflict pairwise only in registration files; integrate them into one head (#4575 pattern) instead of serial rebases. Kaizen #4576 (per-slice registration) should run once #4575 lands.
- #4399: pricing-parity pinned to c34fbdf/7a63834, `Last updated` comparison dropped, behaviour remainder -> #4403; fancy-regex 0.19 direct dep approved; check-bedrock-pricing-parity and validate-terraform-policy stay in #4399.
- Reviews: Codex gpt-6.1-sol high; fall back to gpt-6.1-terra high on "model at capacity" (happened once). Implement: Claude Sonnet high; Opus high for security repairs. max_active_sessions 4.
- Python forbidden; workers run cargo only via recipe jobs (lockfile work excepted).

## Kaizen
Filed this seat: harness #4535 (scoped_test profile from worktree HEAD), project #4576 (registration hotspot). Worker-filed: #4533, #4534, #4558, #4568, #4569, #4570, #4573, #4577, #4582, #4587. Open from before: #4461, #4475, #4485, #4486, #4497, #4500.

## Wakes (transfer on succession)
rust-migration-health (health_anomaly; ignores hub_disk_high:/, hub_memory_high:hub), pm-safety-net (resume hourly), terminal watches on live workers, Manager action results.
