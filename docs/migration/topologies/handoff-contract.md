# Migration node handoffs

The first line must name the actual node stage. Do not substitute `VERIFICATION`, omit the em dash or colon, or put introductory prose before the block. Keep the full plan or implementation evidence in the same final message; a format correction repeats that evidence.

Planning nodes return this block, followed by their complete inline AC-by-AC plan:

```text
PIPELINE HANDOFF — PLAN:
doc_path: inline
manifest_path: inline
status: complete
summary: Describe the port, contracts, risks and test strategy.
```

Implementation and repair nodes use `IMPLEMENTATION`; review-fix nodes use `FIX`. Replace placeholders with actual evidence:

```text
PIPELINE HANDOFF — IMPLEMENTATION:
doc_path: inline
manifest_path: inline
status: complete
summary: Describe delivered behavior and AC-by-AC evidence.
commit: <actual final verified 40-hex source SHA>
```

Include the actual daemon job id, outcome and candidate evidence after the block. The handoff itself is not verification evidence. A job wait ends the turn without a handoff. Do not declare complete when required verification is unavailable.

For a blocked node, use its actual stage and include every blocker field:

```text
PIPELINE HANDOFF — IMPLEMENTATION:
doc_path: inline
manifest_path: inline
status: blocked
summary: State completed work and the unmet acceptance criteria.
blocker: Describe the condition preventing completion.
blocker_class: technical_impasse
blocker_evidence: Name the actual refusal, job id, log or source evidence.
blocker_question: State the precise engineering decision the manager must resolve.
```

Use the installed Rust `rsi-contract-validate <issue number>` with the final text on stdin to check syntax before publication. This parser check does not run replacement tests, verify a commit or grant acceptance. Never build the harness or alter its policy to fix a project handoff.
