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
result exists for bb0df26. The refusal was opaque (no file or key named) and is
not explained by the published version-1 contract, which the manifest's shape
satisfied; diagnosis is in review #4426 and the refusal is recorded in the
Issue #4373 handoffs. This repository now declares the same recipes in the
public `.rsi/jobs.toml`; `.rsi/project.toml` is retained as provenance only.
Until fmt/clippy actually run under daemon jobs, their status is unknown, not
passing.

## Transport failure

The earlier worker's attempt to submit jobs directly through `rsi-rpc` failed
six times with `agent_verb_requires_session_token`; no jobs were created.
Per policy the worker stopped that path immediately — no environment, token or
process inspection — and published clean committed source for manager-native
daemon verification instead (harness #4424). Managers submit jobs against a
registered frozen worker worktree; workers do not troubleshoot transport
credentials.

## AC status after the repair round

- AC1 workspace builds: met (build job above).
- AC2 real CLI + meaningful smoke tests: met at package level (test jobs
  above); the repair round adds regression tests but has not itself been
  re-verified by daemon jobs yet.
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
