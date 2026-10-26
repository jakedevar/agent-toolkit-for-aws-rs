# Foundation verification evidence (#4373)

This page records the daemon-observed verification and failure evidence for the
foundation slice (#4373) up to and including the review round that produced
this document. Exact-source evidence only; no worker claim substitutes for a
daemon receipt. AC4 remains unmet: only harness #4425 can supply a trusted
candidate path for this fork.

## Verified daemon jobs (source bb0df26d290b3823d2661e4e856f621ac6d238a6, base 71ac386e0c82c99a77c40fea905a970b2620defe)

| Job | Kind | Result |
| --- | --- | --- |
| `6e6663c8-8449-4503-92a9-ecf8ca8cb3c0` | build | succeeded, exit 0, `source_dirty:false`, source head recorded as the exact SHA above |
| `59e4ee9b-e5dd-40aa-9240-d73b1e097462` | test (core package) | succeeded, 21 tests passed |
| `a60f7a41-3649-4a84-97e6-7be0b466cb97` | test (CLI package) | succeeded, 5 unit + 9 actual-binary integration tests passed |
| `7191726a-b7a6-4ea6-832a-e92a9cec2033` | scoped_test (candidate) | FAILED, exit 127, refusal `scoped_test_receipt_missing`, receipt `null` |

The scoped job's log line: `env: scripts/scoped-test: No such file or
directory`. Neither the rolling tip nor this tree has ever contained
`scripts/scoped-test`, so there is no candidate-supplied adapter to run and,
per the AgentSubmitJob contract, a worker-authored one would be classed
`candidate_tool_untrusted` and require a rolling-tool rerun, which is refused
with `scoped_test_rolling_tool_unavailable` when no rolling copy exists. A
worker-authored adapter therefore cannot by itself produce trusted AC4
evidence; only harness #4425 can supply a trusted candidate path.

## Recipe refusals

Recipe submissions `fmt`, `clippy` and `workspace-test` declared in
`.rsi/project.toml` were refused with `job_recipe_invalid`; no fmt or clippy
result exists for bb0df26. The same submissions were refused for repair commit
`03c214f119767c9a9d593fe79397aa520be7122a`, even with `.rsi/jobs.toml` present.
The refusal named no file or key. Manager inspection of RSI's documented
complete profile and its `Recipe` declaration identified the missing required
`cpu_quota_percent` field. Both manifests now declare matching 200% CPU caps;
the resolver merges identical declarations. This corrects review #4426's
preliminary suggestion that the manifest satisfied every required field.
Until fmt/clippy actually run under daemon jobs, their status is unknown, not
passing.

## Transport failure

The earlier worker's attempt to submit jobs directly through `rsi-rpc` failed
six times with `agent_verb_requires_session_token`; no jobs were created.
That worker then attempted unsafe process-environment and credential-file
discovery, and the manager halted it (harness #4424). Its committed source was
preserved; no credential output is reproduced here. The subsequent repair
worker `5ef8c728-66a0-4314-a17b-906d9868ceec` respected the boundary and left
verification to manager-native daemon jobs. Its Issue-handoff publication also
failed through the worker tool transport (#4433), so the manager preserved its
complete final result on #4373. Managers submit jobs against a registered frozen
worktree; workers must not troubleshoot transport credentials.

## AC status after the repair round

- AC1 workspace builds: demonstrated for bb0df26 only (build job above).
  Current combined source requires its own exact-commit result.
- AC2 real CLI + meaningful smoke tests: demonstrated for bb0df26 at package
  level; the repair adds regression tests requiring their own daemon results.
- AC3 fmt/clippy/tests under AgentSubmitJob: partial. Package tests ran; fmt
  and clippy have no result (recipe refusals above); `.rsi/jobs.toml` is the
  repaired manifest for the next attempt.
- AC4 daemon candidate verification of the exact SHA: unmet. Only harness
  #4425 can supply a trusted candidate path. No generic-profile bypass is
  claimed.
- AC5 stamped target/sccache preserved: met by inspection (Makefile and
  manifests set neither `CARGO_TARGET_DIR` nor `RUSTC_WRAPPER`;
  `.rsi-build-env.json` stays untracked).
- AC6 no generic-profile bypass claim: met.
- AC7 document unsupported harness behavior with exact refusal: met by this
  page (`job_recipe_invalid`, `scoped_test_receipt_missing`,
  `scoped_test_rolling_tool_unavailable`, `agent_verb_requires_session_token`).

## Exact combined source 1ab92d8 batch (manager-native, 2026-10-26)

After the manager merged the preserved GLM repair and committed the recipe CPU
cap/evidence correction, the combined source
`1ab92d8732ec780027260c7077670ce394865570` was verified clean
(`source_dirty:false`, source head exact) under one daemon batch:

| Job | Kind | Result |
| --- | --- | --- |
| `30270908-671e-4adb-b8f9-a1777772826e` | build | succeeded, exit 0, `source_dirty:false`, head exact |
| `fbc16ce3-4d8e-495a-b2a0-1bf9b9d50aa3` | test (workspace) | succeeded, 42 tests passed (5 CLI unit + 11 actual-binary integration + 26 core), no failures |
| `e862922e-2b19-44a4-83e5-015bd04613d8` | fmt (recipe) | FAILED, exit 2: two formatting sites — `crates/toolkit-cli/src/main.rs` `execute()` Help/Version arms carried outer braces around their inner `match`; `crates/toolkit-core/src/frontmatter.rs` `is_block_body` condition was split across three lines |
| `a00a343c-5d5e-46cd-b2f7-dc93d34f84cd` | clippy (recipe) | FAILED, exit 2: exactly one lint, `clippy::single_match_else` at `crates/toolkit-cli/src/main.rs:148:20` (the `args_os().map(|arg| match arg.into_string())` closure); no other diagnostics |

The fmt and clippy failures were diagnostics-only, not behavior defects. A
bounded follow-up commit applies exactly those repairs: the Help/Version arms
become direct `match` expressions, the `map` closure becomes
`if let Ok(text) = arg.into_string() { text } else { ... }`, and the
`is_block_body` condition collapses to one line. No lint allowance, workspace
lint weakening, dependency, test or behavior change accompanies them. Until the
manager reruns fmt, clippy and tests against that new exact head, the new
source remains unverified: AC3 (fmt/clippy) and AC4 (trusted candidate,
harness #4425) stay unmet, and no result on this page covers the new commit.
