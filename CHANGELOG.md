# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `rust-toolchain.toml` to pin stable toolchain for reproducible builds (#979)
- Package metadata (version, edition, license, repository, description) to workspace `Cargo.toml` (#977)
- Missing env vars (`SET_PRICES_TX_FEE`, `KEEPER_TX_FEE`, `PYTH_API_KEY`) to `oracle/README.md` (#974)
- This `CHANGELOG.md` file (#978)

## [0.1.0] - 2026-09-25

### Added
- Initial release of SO4 Oracle
- Price fetching and aggregation from multiple sources (Binance, Coinbase, Pyth)
- Keeper loop for executing pending orders, deposits, and withdrawals on-chain
- HTTP API for price feeds and operational endpoints
- Structured JSON request logging with request IDs
- Prometheus metrics at `/metrics`
- Circuit breaker for price source failures
- Admin API with token-based authentication
- Docker, systemd, Fly.io, and Railway deployment support

### Fixed
- Circuit breaker threshold and recovery behavior
- Keeper retry and restart logic
- Price aggregation edge cases
- Various security improvements

[Unreleased]: https://github.com/SO4-Markets/so4-oracle/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/SO4-Markets/so4-oracle/releases/tag/v0.1.0
