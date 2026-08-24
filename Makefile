.PHONY: test check build fmt clippy verify

check:
	cargo check --workspace

build:
	cargo build --workspace

test:
	cargo test --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all-targets -- -D warnings

verify: fmt clippy test
