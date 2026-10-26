# Contributing to this Rust fork

This fork accepts agent-managed Rust conversion work. RSI programme Issue #4369 is the delivery tracker. [AGENTS.md](AGENTS.md) governs repository development; [migration plan](docs/migration/README.md) lists assigned slices and dependencies.

1. Manager assigns one bounded RSI Issue and an allowed model.
2. Worker reads allocated source, acceptance criteria and [worker contract](docs/agents/worker-contract.md).
3. Worker ports covered behavior to Rust, migrates tests/callers and records compatibility decisions.
4. Worker commits on allocated branch and verifies exact committed source through daemon jobs.
5. Independent model reviews verified SHA. RSI merge queue integrates accepted work and records Issue closure.

No pull request or upstream AWS team approval is required for this fork. Do not switch branches or publish main/releases. Primary implementation uses DeepSeek and GLM through OpenRouter; optional planning/review assistance follows live RSI grants.

Legacy Python remains migration evidence until each replacement passes. New executable implementation and tests are Rust. Existing product docs/examples do not impose Python development dependencies.

Report fork bugs and follow-ups in RSI project Issue tracker. Keep security-sensitive details private; never paste credentials. AWS service or upstream AWS product vulnerabilities retain their appropriate AWS private reporting process. Preserve upstream LICENSE, NOTICE and attribution.
