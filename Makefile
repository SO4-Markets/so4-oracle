.PHONY: test check build

check:
	cargo check --workspace

build:
	cargo build --workspace

test:
	cargo test --workspace
