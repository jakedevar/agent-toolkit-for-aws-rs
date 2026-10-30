# Rust workspace entry points. These targets never override the daemon-stamped
# build environment (CARGO_TARGET_DIR, RUSTC_WRAPPER/sccache); they rely on it
# being present in the invoking environment.
.PHONY: build fmt fmt-patch clippy test test-scripts clippy-sdk test-sdk clippy-live plugin-binaries plugin-binaries-check openclaw-node-test openclaw-npm-cache
.PHONY: ai-models-sdk ai-models-test clippy-ai-models

build:
	cargo build --workspace --locked

ai-models-sdk:
	cargo test -p toolkit-cli --features ai-models-live --locked --lib ai_models::
	cargo test -p toolkit-cli --features ai-models-live --locked --test ai_models
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::service_bridge::

ai-models-test:
	cargo test -p toolkit-cli --locked --lib ai_models::
	cargo test -p toolkit-cli --locked --test ai_models

clippy-ai-models:
	cargo clippy -p toolkit-cli --features ai-models-live --all-targets --locked -- -D warnings

fmt:
	cargo fmt --all -- --check

# Diagnostic (#5509): runs `cargo fmt --all` in a scratch copy of the worktree
# and prints the result as a unified patch between BEGIN/END marker lines.
# Never writes the worktree; apply the patch with `git apply`.
fmt-patch:
	sh scripts/fmt-patch.sh

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace --locked

# The toolkit-aws `sdk` feature (#4463) is off by default, so the targets above
# never compile it. These two build, lint and test it on its own, and
# toolkit-cli with its `aws-sdk` feature (the asm exec SigV4/HTTPS wiring).
clippy-sdk:
	cargo clippy -p toolkit-aws --features sdk --all-targets --locked -- -D warnings
	cargo clippy -p toolkit-cli --features aws-sdk --all-targets --locked -- -D warnings

test-sdk:
	cargo test -p toolkit-aws --features sdk --locked
	cargo test -p toolkit-cli --features aws-sdk --locked --lib asm_exec
	cargo test -p toolkit-cli --features aws-sdk --locked --lib mcp_proxy
	cargo test -p toolkit-cli --features aws-sdk --locked --test mcp_proxy_cli
	cargo test -p toolkit-cli --features aws-sdk --locked --test launch_callback net5_probe_reports_named_missing_capability
	cargo test -p toolkit-cli --features aws-sdk --locked --test launch_callback shipped_binary_exchange_publishes_session
	cargo test -p toolkit-cli --features aws-sdk --locked --test agents_pay_ac3 --test agents_pay_wiring --test agents_pay_gate

# Lint-only coverage of the live x402 payment module (design Q24): the
# library and binaries only, never a test target (the G1a guards refuse
# every test build with that feature; T-AC3-MAKE pins this recipe).
clippy-live:
	cargo clippy -p toolkit-cli --features agents-pay-live --lib --bins --locked -- -D warnings

docgraph-spike:
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_bridge_spike service_orchestrators_use_only_the_injected_connector -- --exact

clippy-docgraph-live:
	cargo clippy -p toolkit-cli --all-targets --features docgraph-live --locked -- -D warnings

docgraph-live-wipe:
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_live live_wipe_ignores_ambient_confirmation -- --exact

docgraph-live-consent:
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_live live_consent_precedes_input_files_and_only_argv_counts -- --exact

docgraph-live-all-consent:
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_live live_memory_requires_consent_for_every_mutating_operation -- --exact

docgraph-required-credentials:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::source::tests::required_credentials_rebuild_expired_and_never_use_unsigned_fallback -- --exact

docgraph-guarded-canary:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::source::tests::required_guarded_source_

docgraph-database:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::database::

docgraph-database-peer:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::database::tests::a_forbidden_connected_peer_receives_zero_bytes -- --exact

docgraph-database-admission:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::database::tests::forbidden_or_mixed_answers_are_never_dialed -- --exact

docgraph-review-client:
	cargo test -p toolkit-cli --features docgraph-live --locked --lib docgraph::tests::live

docgraph-live-suite:
	cargo test -p toolkit-cli --locked --test docgraph_live
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_live
	cargo test -p toolkit-cli --features docgraph-live --locked --lib docgraph::
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_bridge_spike

docgraph-credential-deadline:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::source::tests::a_stalled_provider_is_abandoned_at_the_deadline -- --exact

docgraph-credential-process:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::source::tests::bounded_resolution_refuses_a_credential_process_before_it_runs -- --exact

docgraph-dns-cap:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::lookup::
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::database::tests::an_abandoned_lookup_makes_the_next_request_fail_closed_without_a_dial -- --exact

docgraph-service-dns:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::service::

docgraph-bounded:
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::source::tests::bounded_resolution_
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::source::tests::a_stalled_provider
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::lookup::
	cargo test -p toolkit-aws --features sdk --locked --lib sdk::database::

docgraph-live-modes:
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_live live_rejects_recorded_and_dry_run_before_files -- --exact

docgraph-env-selection:
	cargo test -p toolkit-cli --locked --test docgraph_live environment_cannot_select_live_mode -- --exact
	cargo test -p toolkit-cli --features docgraph-live --locked --test docgraph_live environment_cannot_select_live_mode -- --exact

# Per-plugin ./bin binaries (#4487, docs/migration/plugin-binaries.md). Stage
# every declared binary for every declared platform with the locally
# installed toolchains (zig + cargo-zigbuild cross-link the non-host targets),
# then check launchers and staged files. Never commit the staged files.
plugin-binaries:
	cargo run --locked --package plugin-stage -- stage --platform all

plugin-binaries-check:
	cargo run --locked --package plugin-stage -- check

# Shell-level self-test of scripts/at-commit-test.sh (#4865): the runner the
# daemon invokes for AgentSubmitJob test jobs with params.at_commit.
test-scripts:
	sh scripts/at-commit-test.selftest.sh
	sh scripts/run-rsid-test-shards.selftest.sh
	sh scripts/fmt-patch.selftest.sh
	sh scripts/openclaw-node-test.selftest.sh

# OpenClaw package node tests (#5158, design OQ8), the daemon recipe
# openclaw-node-test: no network (fresh network namespace), no Python, npm
# dependencies only from the offline cache checked against package-lock.json.
# It fails closed (exit 3) while the cache is missing. Fill the cache once per
# host with `make openclaw-npm-cache`, an operator action that uses the
# network and is never a daemon recipe.
openclaw-node-test:
	sh scripts/openclaw-node-test.selftest.sh
	sh scripts/openclaw-node-test.sh run

openclaw-npm-cache:
	sh scripts/openclaw-node-test.sh populate

# docgraph live S3 (#5515): the Gremlin WebSocket plane (toolkit-aws feature
# `gremlin`, enabled by toolkit-cli `docgraph-live`). gremlin-sync formats the
# workspace and records the new optional dependency in Cargo.lock from the
# local registry cache (--offline: no network).
gremlin-sync:
	cargo fmt --all
	cargo metadata --format-version 1 --offline > /dev/null

docgraph-gremlin:
	cargo test -p toolkit-aws --features gremlin --locked --lib sdk::gremlin::
	cargo test -p toolkit-aws --features gremlin --locked --lib sdk::database::

clippy-gremlin:
	cargo clippy -p toolkit-aws --features gremlin --all-targets --locked -- -D warnings

# S4 ordered memory dispatch and wire contracts; every backend is injected.
docgraph-memory:
	cargo test -p toolkit-cli --features docgraph-live --locked --lib docgraph::live_memory::

# Resolve the optional S4 model crate; no service endpoint is contacted.
docgraph-memory-sync:
	cargo metadata --format-version 1 --features docgraph-live --offline > /dev/null
	cargo tree -p toolkit-cli --features docgraph-live --locked --offline -e features

# #5455: discard only this package's stale compiled Cargo test paths before
# scoped verification. Rebuild and run the unchanged security hook tests.
docgraph-scoped-prime:
	cargo clean -p secret-safety-hook
	cargo test -p secret-safety-hook --locked
