# Initial manager notebook

Session: 0dd33d25-9c5a-4f66-8053-9d48b025263e; Codex/gpt-6.1-sol/xhigh.
Project: 1b353269-cf0f-4e73-83a8-1c4ada2126b5; agent-toolkit-for-aws-rs.
Group: 2ed90778-60c9-46b8-91a7-5c832cd29ce8.
Epic: 527005f6-9f08-477e-8628-7411156e4725.
Programme #4369; 47 Issues #4369-#4415; 47 manager work records and 89 require-integrated dependencies.
Baseline: 86edeadd0da48e03a8d3045e2775881bd6450e68. Allocated branch only; origin has main, no observed rolling ref at bootstrap.

Operator: fully agent-managed Python-to-Rust conversion. Strip conflicting repository development instructions, preserve shipped AWS product skills/safety/provenance. Primary implementers OpenRouter deepseek/deepseek-v4.1-flash-1017 and z-ai/glm-5.3-flash. Opus 5.5/Sol may plan; Sonnet 5.5/Terra may review/assist. Read live launch grants, provider health/spend before paid runs. Live policy was version 2, scope 1; never reuse these observations blindly.

Root AGENTS.md and CLAUDE.md define Rust development policy. Upstream contribution rejection, AWS CODEOWNERS, PR checklist and upstream-only tracker routing removed/replaced. Existing product instructions remain source material.

Topologies stored revision 1, both validate with zero diagnostics:
- DeepSeek a50c475b-884f-4d1e-8a9f-1ea13405d9b4; sha256:514b134441cc04155e6a6983d2b631a37adc0fad1acbb230e8bed244b723c7b3.
- GLM 466d3b4a-0d17-473a-8a3b-a9c0a7c7c1cf; sha256:158be231fb63f26b2c9bd964aa01e1ab743ec23cdc58e31768a3396fd4b0e57b.
Plan -> implement/daemon verification -> cross-family review -> bounded fix loop -> queue land/close, with bounded landing repair path. Prompts/definitions/runbook committed under docs/migration/topologies.

First wave: DeepSeek #4373 Rust workspace/verification bootstrap, GLM #4371 inventory/instruction/contract audit. Pin both to this manager policy commit; paths disjoint. Research bootstrap hand-driven because runnable Rust verification and integration branch do not yet exist. Subsequent implementation slices use topologies. Never fake successful migration execution or verification based on stored definitions.

Inventory covers all 391 original tracked Python paths, each mapped to issue and original blob. Classification is provisional; #4371 independently audits callers, embedded code, archive payloads and service-only Python restrictions. Remote-service limitations block full-completion claim until resolved explicitly.

Next: commit/push allocated branch; check provider cap; launch first wave through RSI; preserve action ids and terminal wakes. Confirm project-specific Rust verification/queue integration adapter rather than copying RSI harness-specific tools. Publish governance with first independently reviewed verified source; no manual integration/main push.

Skill: /home/jakedevar/rsi/.claude/skills/rsi-project-manager/SKILL.md. Maintain this notebook with Bash cat/tail only.

Kaizen carried: harness #4360 open (language restriction publication). New schema mismatch observed: ManagerUpdate work accepts weight minimum 0 in catalog but refuses zero; use weight 1. File/dedupe follow-up before final handoff.

Bootstrap revision: use DeepSeek topology for foundation #4373 immediately, pinned to manager policy commit. GLM inventory #4371 remains hand-driven read-only research + documentation because verification adapter is not yet present. This exercises reusable topology on first implementation slice. Operator confirmed optional Sonnet/Terra assistance; primary work stays OpenRouter.
