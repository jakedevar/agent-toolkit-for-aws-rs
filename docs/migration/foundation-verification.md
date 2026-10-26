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
