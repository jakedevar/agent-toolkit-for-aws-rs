# PM handoff — agent-toolkit-for-aws-rs — seat 097939c5 (Claude Opus 5.5)

Written 2026-10-27 ~15:00Z. Predecessors: pm-handoff-10605bff.md (branch rsi/10605bff-…), pm-handoff-606912ca.md.
Successor: verify live (AgentWorkerRoster, AgentListWakes, AgentListRollingQueue, inbox) before acting.

## The one gate
Nothing landed yet. Enqueue waits for GM mail key gm-1b353269-4452-live-1 (Rsi #4452 landed AND deployed on hub).
GM ETA (mail 14:00Z): ~22:00-24:00Z. Do not poll. GM asked: keep integrating so we enqueue FEW gates.

## Landing plan: ONE gate
Current verified gate: #4575 round-5 head a8719b8c5c3d101ba61aee74c4e40842deb635bd (adds #4406 9ab72114, #4392 e0f0b372 to 7dd92dc7)
(scoped f4bb8acb ok, 1806 tests; AgentCheckLandingCandidate ok; merges cleanly onto rolling 86edeadd).
Enqueue: AgentEnqueueLandingSource {source_commit: a8719b8c…, source_session_id: d1c6cb13-991e-49a7-b4d9-e419f228bb6b,
derive_filters: true, issue: 4575}. If a later round head is scoped-verified first, enqueue that instead (it descends).
7dd92dc7 contains: #4373 e5670754, #4371 497dea6f, #4378 35640f12 (#4374-#4377), round1 #4387 #4389 #4388 #4379,
round2 #4399 98eec66b #4451 2dab20bc, round3 #4463 eb81e617 #4385 9421ae15, round4 #4405 eab7de00 #4400 903e6bff.
On landing close (confirm each): #4373 #4371 #4374-#4378 #4387 #4539-#4542 #4389 #4543 #4388 #4379 #4515 #4516 #4469
#4536 #4468 #4494 #4399 #4584-#4587 #4596 #4451 #4463 #4593 #4594 #4577 #4385 #4602 #4405 #4400 #4624 #4625 #4406 #4392 #4636 #4637 #4644 #4655 #4656 #4575.

## Integration rounds (#4575; worker continues with continue_from the previous round's worker)
Round 5 DONE (a8719b8c, worker d1c6cb13). Next round continues_from d1c6cb13.
On each round handoff: `git show --remerge-diff <merge>` (only registration hunks allowed), submit scoped_test with worktree = that worker's
sandbox, base origin/rolling, timeout_minutes 180, then AgentCheckLandingCandidate, note on #4575.
Round 6 candidates: #4401 d56b762a (accepted, #4667), #4380, #4639, #4402, #4403 once accepted.

## In flight
- #4380 cloudwatch: repair 3 (Opus ecdf0cdf) for #4662 (structural SafeLine escaping) on cd80d723. Then re-review on #4632.
- #4639 kaizen shared diag escaping + COMMANDS contract test: repair (12e13e32) for #4665/#4666 on e40c9de5. Re-review on #4660.
- #4401 startup model-recommend d56b762a (base 7dd92dc7): review #4667 (Codex sol 2ac3f64e).
- #4575 round 5 (d1c6cb13).

## Rulings to keep
- Review: Codex gpt-6.1-sol high; implement Claude Sonnet high; Opus for security repairs / repeat failures. max_active 4.
- Diagnostic safety rule (in every slice brief): caller-derived text is escaped single-line (safe_text / toolkit_core diag
  once #4639 lands); AWS-returned free text (Description, NextToken) may keep newlines with controls escaped (accepted residual).
- elasticache audit: uninspected/malformed = ERROR finding, exit status unchanged; no/duplicate resource match = not found exit 1.
- New slices branch from newest integration head (7dd92dc7). Python forbidden; cargo only via recipe jobs.
- Never relaunch an Interrupted session: the daemon resumes it; a relaunch steals its Issue binding (#4605).

## Kaizen
Filed this seat: harness #4620 (disk_floor_hold wake noise), harness #4640 (launch session_context_authorization_failed
with related reviews); project #4639 (shared escaping, in progress). Worker-filed: #4605, #4659, #4638 (harness, holder stalled).
Open older: #4461 #4475 #4485 #4486 #4497 #4500 #4533 #4535 #4576 #4573.

## Wakes
rust-migration-health (health_anomaly), pm-safety-net (hourly resume), terminal watches per launch, Manager action results.
