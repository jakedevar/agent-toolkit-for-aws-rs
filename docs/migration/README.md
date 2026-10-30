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

CLI path-argument policy (bare `-`, #4780): [cli-bare-dash.md](cli-bare-dash.md) lists the router rule and the audited literal `-` parameters (no parameter reads stdin).

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
| #4382 | launch ([status and divergences](launch-with-aws.md); L5 implemented, awaiting review; L7-L8 open) | deepseek | #4379 |
| #4383 | bedrock-fetch | glm | #4379 |
| #4384 | ai-models | deepseek | #4379 |
| #4385 | ai-data | glm | #4374 |
| #4386 | ai-payloads (S1-S5 done; 0 of 56 inventory rows removed, 56 retained with documented disposition; status: [ai-payloads.md](ai-payloads.md), design: [ai-payloads-design.md](ai-payloads-design.md)) | deepseek | #4379, #4384, #4385 |
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
| #4451 | asm-exec (added by audit #4371; status: [asm-exec.md](asm-exec.md)) | sonnet (assigned) | #4374, #4379; gates #4415 together with #4582 |
| #4487 | plugin-binaries (per-plugin `./bin` build and stage pipeline; design: [plugin-binaries.md](plugin-binaries.md)) | opus (assigned) | serves #4412; reused by #4413 S5 and #4414 |

## Definition of done

Each slice preserves tested public behavior, migrates callers and covered tests, has exact committed verification and independent review, then lands through queue. Overall completion additionally removes local Python requirements from installation, CI, maintenance, hooks and proxy transport; audits every original inventory row; resolves remote payload restrictions; verifies packaging/manifest/parity from clean checkout. Release publication/main changes remain separate operator gates.

No bulk deletion before replacements pass. No Python wrappers counted as Rust ports. No paid/live AWS workloads in ordinary tests. Record compatibility changes and unknowns in Issue evidence.

Launch with AWS L3 (#5190) adds an offline streaming and OIDC foundation;
its API and callback consumers remain in L5 and L4 respectively. The collector
uses `CappedBuf`: steady allocation is at most the effective cap C, while a
moving growth may transiently hold old plus new allocations below 2C. Including
the pinned SDK's at-most 512 KiB admitted frame and one 64 KiB returned chunk,
the conservative peak is below 2C + 576 KiB, not the original design's
C + 576 KiB. Single-use code exchanges do not retry ambiguous connection
failures because the transport cannot prove whether request bytes were sent.
Duplicate Content-Length values now remain detectable at the shared response
header boundary. Live network use still waits for the S1 integration gate.

x402 review triage (#5097, #5098, #5099): the landed amount comparator already uses `E + 6`
and its equivalent-spelling boundary tables record zero spending calls on over-cap entries.
The separate `agents-pay-live` feature excludes live constructors from default and SDK test
artifacts. The platform-only TLS repair extends payment roots to credential HTTP and admin,
and replaces macOS CA-variable presence checks with direct Security framework trust queries.
The r2 trust-plane contract and offline regression lanes are recorded in
[the x402 design](x402-payments-design.md#r-tls-pay-trust-roots-on-the-payment-runtime-q15).
Exact verification and integration status are published on #5097; this note is not a landing claim.

## JSON key order (#5394)

`serde_json`'s `preserve_order` feature is enabled for workspace-wide builds (through `aws-config`'s `rustls` in the two skill crates) but not for per-package builds, so `serde_json::Value` object order, and any output built from it, depends on build scope. Output whose key order is observable must not rely on that: render it from `toolkit_core::Json` (insertion order) or sort explicitly. `toolkit-cli`'s schema engine hands the validator keys in sorted order and re-renders the crate's own sentences from the ordered document (tests in `spec/tests.rs`); `powertools-handler` and `s3-upload-processor` select `preserve_order` themselves and pin their output order in tests. `crates/toolkit-cli/src/launch/` still builds output with `serde_json::json!`; L5 (#5345) moves it to `toolkit_core::Json`.

## Toolkit CLI usage and route-order pins (#5360)

Two reviewed-change guards used to be shared single lines that every parallel command slice edited: a global usage-text length and hash, and the length of the route-order array. They conflicted on every rolling merge, and the merged hash could only be learned with a probe build. They are now per slice:

- **Route order.** `LEGACY_COMMAND_ORDER` (`crates/toolkit-cli/src/registry.rs`, test module) is a slice, so adding a route never edits a length. Insert the new route lines at the slice's alphabetical place in the tail. `the_public_route_order_is_pinned` prints the index of the first route that differs.
- **Usage text.** `crates/toolkit-cli/src/registry/usage_pins.rs` holds `SLICE_PINS`, one sorted line per slice: name, synopsis rank, description rank, and the length and FNV-1a hash of the slice's synopsis, description and exit notes. `SHARED_PIN` pins the shared frame (options and exit codes). A third test rebuilds the whole text from the pinned ranks and compares it with `USAGE`, so a change of the ordering code fails as well. Any unreviewed edit still fails a test; a mutated synopsis fails with the replacement line.

Re-pin procedure, with no probe build and no hand-computed hash:

1. Run (or read the log of) `every_slice_usage_fragment_is_pinned` and `the_shared_usage_frame_is_pinned`. Each failure prints, per slice, the exact line to paste: `replace its line in SLICE_PINS with:`, `add this line ... at its sorted place:` or `delete this line:`. The frame failure prints the whole `SHARED_PIN` line.
2. Check the printed change is the usage change you meant, paste the lines, keep the table sorted by name, and say in the Issue that the usage text changed deliberately.
3. After a merge of rolling, git merges disjoint slices' lines without conflict. If two slices landed in the same alphabetical gap, keep both lines in order. If a red run remains, paste what it prints; never merge hash numbers by hand.

The usage text bytes and the routes of the CLI are unchanged by this tooling.

## Manager continuity

Durable Issues and manager ledger carry assignments/dependencies. [issues.json](issues.json) freezes initial map; daemon state is current. Manager notebook: docs/agents/manager-handoff.md (maintain with Bash cat/tail). Preserve active jobs, topology revisions, exact source/base SHAs, review/queue outcomes and genuine gates during succession. Update active workers explicitly when plan changes.
