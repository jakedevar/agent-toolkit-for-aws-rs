# Python to Rust conversion

Operator authorized fully agent-managed conversion on 2026-10-26. Programme: RSI #4369. Project: agent-toolkit-for-aws-rs. Initial manager: session 0dd33d25-9c5a-4f66-8053-9d48b025263e, Codex/gpt-6.1-sol/xhigh. Manager skill: /home/jakedevar/rsi/.claude/skills/rsi-project-manager/SKILL.md.

## Baseline and scope

Baseline commit: 86edeadd0da48e03a8d3045e2775881bd6450e68. Inventory: [python-inventory.json](python-inventory.json), 391 tracked Python paths, 271 distinct blobs. Initial line counts: 127,558 including duplicate paths; 97,599 after exact-blob deduplication, including 28,208 lines in named tests/diagnostics. Counts include comments, fixtures and examples.

Every Python path maps to an assigned slice. Inventory is an auditable baseline, not evidence of a completed port. GLM audit Issue #4371 must check classification, callers, embedded Python, archived payloads and untracked external dependencies. Preserve product documentation and legal attribution while replacing toolkit runtime/development dependencies. Managed-service language restrictions need explicit resolution.

## Delivery process

Two durable topology profiles share one versioned process: plan -> implement and daemon verification -> independent review -> bounded fixes -> queue landing and Issue closure. A refused landing has one bounded repair/review path. Definitions are [DeepSeek](topologies/deepseek.json) and [GLM](topologies/glm.json); see [runbook](topologies/README.md). Prompts apply root Rust policy and source/verification custody.

Preferred primary implementation: DeepSeek deepseek/deepseek-v4.1-flash-1017 and GLM z-ai/glm-5.3-flash, high effort. Default independent review uses the opposite family. Opus 5.5 / GPT-6.1 Sol may plan; Sonnet 5.5 / GPT-6.1 Terra may review/assist. Live grants currently permit GLM but exclude DeepSeek. Retain the stored DeepSeek profile without execution; the GLM revision uses Sol planning and Sonnet independent review. Future managers re-read live grants, provider health and spend caps before launch. Do not auto-select an ungranted model.

Initial bootstrap: DeepSeek #4373 creates workspace and trustworthy verification through the DeepSeek topology; GLM #4371 separately audits inventory/instruction conflicts. They may start from same pinned manager policy commit before it lands, with nonoverlapping paths. The research audit is hand-driven because no runnable Rust verification/integration baseline exists yet; it must not fake verification. Foundation is the first live topology trial; an adapter impasse is recorded and fixed before downstream runs.

Bootstrap update: the first two #4373 topology trials stopped at planning (scratch-only preserved work, then invalid handoff stage). Neither delivered implementation. Revision 3 supplied parser-checked handoff templates. The hand-driven DeepSeek foundation worker was halted for prohibited Python bridge work; replacement GLM worker `b5d95527-4f41-4584-828f-e892e358ab24` committed the Rust workspace as `bb0df26d290b3823d2661e4e856f621ac6d238a6`. It was then halted after unsafe credential discovery while troubleshooting job transport. Source is preserved and unaccepted; manager-owned daemon verification is pending. Allowed Sonnet assistance must independently review the exact GLM source after verification; the earlier DeepSeek review assignment is superseded by the live grant. The [foundation plan](foundation-plan.md) retains verification, review and queue gates. GLM revision 4 tightens generic prompts and records review-step limitations; the DeepSeek profile remains at stored revision 3, unavailable under current launch policy. No revision has a successful end-to-end execution. See #4418 and harness #4419, #4423 and #4424.

Verification update (2026-10-26): native manager jobs built exact `bb0df26` and passed 35 tests (21 core, five CLI unit, nine binary integration). Candidate job `7191726a-b7a6-4ea6-832a-e92a9cec2033` failed with exit 127 / `scoped_test_receipt_missing`: `scripts/scoped-test` is absent. Harness #4425 owns portable trusted Cargo verification; package passes do not satisfy that gate. Diagnostic recipe submissions returned `job_recipe_invalid`, tracked by harness #4427. Independent preliminary Sonnet review #4426 found a symlinked `SKILL.md` containment defect, nested frontmatter-key misparse and CLI panic gaps. Fresh GLM repair worker `5ef8c728-66a0-4314-a17b-906d9868ceec` owns one bounded repair round from the frozen source, including documented `.rsi/jobs.toml` diagnostic recipes. Nothing is accepted or landed. Sonnet's static re-review accepted the `95c0ad2` doc/prompt repairs; manager readback confirmed the entire stored GLM revision-4 definition equals its committed JSON. Runtime topology criteria remain open.

Latest foundation source: manager combined the preserved GLM source and governance documentation on the allocated branch. Commit `1ab92d8` built cleanly and all 42 offline tests passed (26 core, five CLI unit, 11 binary integration). Its fmt and clippy jobs ran after adding the required `cpu_quota_percent = 200` to each recipe; their diagnostics identified two formatting sites and one control-flow lint. GLM worker `c0fdbb81-d558-446f-8f5b-23496e7fcea9` committed that bounded cleanup as `d9c6e3c`. The final combined source still needs exact-commit daemon verification and independent review. Harness #4425 remains the trusted candidate blocker; no acceptance or landing is claimed. Inventory correction continues separately under GLM worker `8268e555-ffae-4416-b6d1-c04a5578162b`, with the stranded DeepSeek draft retained as unaccepted evidence.

Dependencies exist in manager ledger with require_integrated true, as well as linked Issue bodies. Pinned bootstrap stacking is an explicit exception recorded by manager. At most two initial workers; normal concurrency stays inside project cap. No ready label bypasses prerequisites.

## Issue map

The table and issues.json preserve the initial owner map. Bootstrap substitutions above and the daemon ledger determine the current implementer and opposite-family reviewer.

| Issue | Slice | Primary owner | Prerequisites |
| --- | --- | --- | --- |
| #4369 | program | manager | — |
| #4370 | governance | manager | — |
| #4371 | inventory | glm | #4370 |
| #4372 | topology | manager | #4370 |
| #4373 | foundation | deepseek | #4370 |
| #4374 | core | glm | #4373 |
| #4375 | manifests | deepseek | #4374 |
| #4376 | spec | glm | #4374 |
| #4377 | sync | glm | #4374 |
| #4378 | ci | deepseek | #4375, #4376, #4377 |
| #4379 | aws-core | deepseek | #4374 |
| #4380 | cw-domain | glm | #4374 |
| #4381 | cw-api | deepseek | #4379, #4380 |
| #4382 | launch | deepseek | #4379 |
| #4383 | bedrock-fetch | glm | #4379 |
| #4384 | ai-models | deepseek | #4379 |
| #4385 | ai-data | glm | #4374 |
| #4386 | ai-payloads | deepseek | #4379, #4384, #4385 |
| #4387 | msk | glm | #4374 |
| #4388 | aurora | deepseek | #4374, #4379 |
| #4389 | dynamo-cost | glm | #4374, #4379 |
| #4390 | dynamo-bench | deepseek | #4379, #4389 |
| #4391 | dynamo-deploy | glm | #4379, #4389 |
| #4392 | cache-analysis | deepseek | #4374, #4379 |
| #4393 | cache-ops | glm | #4379, #4392 |
| #4394 | relational | deepseek | #4374, #4379 |
| #4395 | docgraph | glm | #4379 |
| #4396 | marketplace | deepseek | #4379 |
| #4397 | serverless | glm | #4379 |
| #4398 | telemetry | deepseek | #4374, #4379, #4377 |
| #4399 | startup-validators | glm | #4374 |
| #4400 | startup-scoring | deepseek | #4374 |
| #4401 | startup-models | glm | #4374 |
| #4402 | startup-diagram | deepseek | #4374, #4379 |
| #4403 | bedrock-resolve | glm | #4374, #4379 |
| #4404 | bedrock-probes | deepseek | #4379, #4403 |
| #4405 | bedrock-report | glm | #4374, #4403 |
| #4406 | heroku | deepseek | #4374 |
| #4407 | startup-tools | glm | #4374, #4377 |
| #4408 | startup-fixtures | deepseek | #4374, #4399 |
| #4409 | startup-contracts | glm | #4399, #4400, #4401, #4402, #4405, #4406 |
| #4410 | payments-policy | deepseek | #4379 |
| #4411 | payments-bridge | glm | #4379, #4410 |
| #4412 | security-hooks | glm | #4374 |
| #4413 | mcp | deepseek | #4379, #4374 |
| #4414 | cutover | glm | #4378, #4413, #4398, #4386, #4411, #4397, #4412 |
| #4415 | retire | deepseek | #4414, #4409, #4396, #4394, #4395, #4393, #4391, #4390, #4388, #4387, #4382, #4383 |

## Definition of done

Each slice preserves tested public behavior, migrates callers and covered tests, has exact committed verification and independent review, then lands through queue. Overall completion additionally removes local Python requirements from installation, CI, maintenance, hooks and proxy transport; audits every original inventory row; resolves remote payload restrictions; verifies packaging/manifest/parity from clean checkout. Release publication/main changes remain separate operator gates.

No bulk deletion before replacements pass. No Python wrappers counted as Rust ports. No paid/live AWS workloads in ordinary tests. Record compatibility changes and unknowns in Issue evidence.

## Manager continuity

Durable Issues and manager ledger carry assignments/dependencies. [issues.json](issues.json) freezes initial map; daemon state is current. Manager notebook: docs/agents/manager-handoff.md (maintain with Bash cat/tail). Preserve active jobs, topology revisions, exact source/base SHAs, review/queue outcomes and genuine gates during succession. Update active workers explicitly when plan changes.
