# Repeatable Python-to-Rust topology

Programme #4369; workflow Issue #4372. Definitions are manager-owned, durable daemon records plus versioned JSON in this directory.

| Profile | Implement / fix / repair | Plan and independent review | Topology id |
| --- | --- | --- | --- |
| DeepSeek | deepseek/deepseek-v4.1-flash-1017 | z-ai/glm-5.3-flash | a50c475b-884f-4d1e-8a9f-1ea13405d9b4 |
| GLM | z-ai/glm-5.3-flash | deepseek/deepseek-v4.1-flash-1017 | 466d3b4a-0d17-473a-8a3b-a9c0a7c7c1cf |

Both use OpenRouter high effort and passed AgentTopologyUpsert validate_only with zero diagnostics. Revision 3 stored. This records definition validation, not a successful migration execution.

Revision 2 addresses #4418: planning passes its inline plan to implementation and keeps transient schema/RPC inspection files outside source. Cargo output is ignored at repository root. The initial foundation execution `6e9378b2-0c80-4b69-b8c8-dd564e8f41ab` was cancelled after planning left untracked scratch files; preserved commit `60be731a8ce0bbd31cfc0e0560dbf1acffe0b410` contains those files only and is not an accepted source. Restart from the clean manager revision. Implementation must resolve the real Rust verification adapter; a generic project profile does not satisfy candidate verification.

Revision 3 adds literal [handoff templates](handoff-contract.md) checked with the installed Rust parser. The revision 2 foundation run `538d00c0-0bea-486e-9f5b-3a301b348119` kept the tree clean, then failed `handoff_invalid`: the planner omitted its stage and the format continuation chose unsupported `VERIFICATION`. Harness issue #4419 records the continuation losing the original plan. No implementation or verification ran in either execution.

Bootstrap #4373 now uses a hand-driven DeepSeek Issue worker with the [concrete foundation plan](../foundation-plan.md). This is the bootstrap exception: two plan protocol failures occurred before the verification adapter existed. Exact-source review and queue landing remain required. Do not repeat the failed executions or treat a corrected prompt as a successful topology run. Subsequent ports use revision 3; first successful end-to-end execution remains an open acceptance criterion for #4372.

## One run per Issue

1. Read live manager authority, Issue, dependency evidence, topology record/digest and OpenRouter provider health/spend cap. No worker launch without allowed_launches and headroom.
2. Choose profile matching Issue owner. Resolve exact base commit; bootstrap uses pushed manager source, normal runs use integrated tip. Use an Epic in current scope.
3. Execute AgentTopologyExecute with topology_id, expected_digest, epic_id, base_commit, inputs {issue: NUMBER}, unique idempotency_key. Default owner wake arms one settlement watch; do not add another or poll.
4. Plan node maps Python contracts and risks. Implementation commits Rust changes and owns daemon verification/wake. Review checks exact verified SHA with opposite model family.
5. Changes-requested routes to bounded fix and re-review. Accepted review routes to daemon merge queue and automatic closure. Queue refusal routes to bounded repair/review/land; persistent failures settle blocked for manager.
6. On settlement inspect typed outcome, final SHA, job/review/landing evidence and Issue. Do not equate execution acceptance, worker completion or queued publication with landed work.

## Versioning and feedback

Retrieve definition with AgentTopologyGet. Change per-node prompts/models in versioned JSON and validate through AgentTopologyUpsert validate_only. Update stored definition using observed expected_revision and new idempotency key; retain returned digest/revision in source. Running executions retain snapshots; message affected workers when policy/plan changes.

Track first-pass acceptance, repair rounds, verified behavior coverage, elapsed time, token/API cost and infrastructure refusals. Improve prompts/process from observed failures. Do not increase loops, spend or concurrency to conceal a broken verification/integration adapter.

Keep model roles explicit; optional Opus/Sol planning or Sonnet/Terra review requires a revision selecting an operator-granted model. No provider-native delegation. No auto-upgrading model ids.

