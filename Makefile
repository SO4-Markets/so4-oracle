.PHONY: verify fmt-check clippy test release check build fmt audit deny docker coverage

# The verification gate (AGENTS.md §2). CI runs these same targets, so the
# flags live in exactly one place (#1437). `make verify` runs all four.
verify: fmt-check clippy test release

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-targets --all-features --locked -- -D warnings

test:
	RUSTFLAGS=-Dwarnings cargo test --all --locked

release:
	cargo build --release --all --locked

check:
	cargo check --workspace --locked

build:
	cargo build --workspace

fmt:
	cargo fmt --all

audit:
	cargo audit

deny:
	cargo deny check advisories bans licenses sources

docker:
	docker build -t so4-oracle .

coverage:
	cargo llvm-cov --all --fail-under-lines 85
