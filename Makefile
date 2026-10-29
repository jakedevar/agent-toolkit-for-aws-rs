# Rust workspace entry points. These targets never override the daemon-stamped
# build environment (CARGO_TARGET_DIR, RUSTC_WRAPPER/sccache); they rely on it
# being present in the invoking environment.
.PHONY: build fmt clippy test clippy-sdk test-sdk

build:
	cargo build --workspace --locked

fmt:
	cargo fmt --all -- --check

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
