# Rust workspace entry points. These targets never override the daemon-stamped
# build environment (CARGO_TARGET_DIR, RUSTC_WRAPPER/sccache); they rely on it
# being present in the invoking environment.
.PHONY: build fmt clippy test clippy-sdk test-sdk plugin-binaries plugin-binaries-check

build:
	cargo build --workspace --locked

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace --locked

# The toolkit-aws `sdk` feature (#4463) is off by default, so the targets above
# never compile it. These two build, lint and test it on its own.
clippy-sdk:
	cargo clippy -p toolkit-aws --features sdk --all-targets --locked -- -D warnings

test-sdk:
	cargo test -p toolkit-aws --features sdk --locked

# Per-plugin ./bin binaries (#4487, docs/migration/plugin-binaries.md). Stage
# every declared binary for every declared platform with the locally
# installed toolchains (zig + cargo-zigbuild cross-link the non-host targets),
# then check launchers and staged files. Never commit the staged files.
plugin-binaries:
	cargo run --locked --package plugin-stage -- stage --platform all

plugin-binaries-check:
	cargo run --locked --package plugin-stage -- check
