.PHONY: test check build fmt clippy audit deny docker coverage

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

audit:
	cargo audit

deny:
	cargo deny check advisories bans licenses sources

docker:
	docker build -t so4-oracle .

coverage:
	cargo llvm-cov --all --fail-under-lines 85
