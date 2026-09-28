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
- Boot-time validation rejecting the same contract ID in two of
  `ORACLE_CONTRACT_ID` / `ROLE_STORE` / `DATA_STORE` / `ORDER_HANDLER` /
  `DEPOSIT_HANDLER` / `WITHDRAWAL_HANDLER` / `READER`, naming both variables
- `/metrics` now exports `oracle_http_requests_total` and the
  `oracle_http_request_duration_seconds` histogram, which were recorded but
  never emitted
- `SubmitError::may_still_confirm` and `SubmitError::diagnostic_events`

### Fixed
- The keeper's in-flight-key decision now branches on the `SubmitError`
  variant instead of `error.contains("not confirmed after")` (#719)
- `TESTNET_PASSPHRASE` corrected to `Test SDF Network ; September 2015`; the
  previous value computed the wrong network ID and every testnet transaction
  signed with it would have been rejected
- The keeper loop reacts to shutdown while a cycle is in flight, instead of
  waiting for that cycle's retry budget to elapse
- An unreadable keeper balance no longer halts the whole keeper; only a
  balance known to be below the minimum gates submissions
- `rustls` 0.23.41 -> 0.23.45 (RUSTSEC-2026-0285) and `h2` 0.4.14 -> 0.4.16
  (RUSTSEC-2026-0258); `cargo deny check advisories` is now clean
- `oracle` crate declares its license, so `cargo deny check licenses` passes
- `ConfigError` derives `Clone`/`Eq` so it can be embedded in `EnvError`

### Changed
- `TokenConfig.min` and `TokenConfig.max` removed. They were documented as
  "used by the API server for display" but nothing read them; the API serves
  runtime percentile bounds from `CachedPrice` instead. **Breaking for any
  `PRICE_FEED_CONFIG` that sets `min`/`max`** — such a config now fails
  `deny_unknown_fields` at parse time instead of being silently ignored.
- `Metrics` counters moved behind the single `Counters` mutex, completing the
  migration started in #599 so a `record_*` call is one consistent generation
- `Metrics::record_keeper_cycle` takes a `KeeperCycleTally` instead of seven
  positional arguments

### Fixed
- `keeper_loop::record_error` now records the transaction hash for post-submission failures (poll timeouts) instead of always storing `None`, so `/oracle/failed-submissions` exposes a hash to look up on a block explorer for ambiguous outcomes (#721)
- Binance, Coinbase, and Pyth HTTP 429 responses are now classified as retryable (matching `RpcError`), so a rate limit triggers backoff and a retry instead of permanently abandoning the price source for the cycle (#708)
- `oracle.service` start limit raised from 3 restarts per 60s to 50 per 300s and moved to the `[Unit]` section, so a crash loop no longer leaves the oracle permanently down pending a manual `systemctl reset-failed` (#544)

### Tests
- Added coverage for `validate_pyth_price` rejecting a malformed (non-numeric) `conf` field, previously unexercised (#712)
- Added coverage for 429/5xx/4xx retry classification in `BinancePriceError`, `CoinbasePriceError`, and `PythPriceError`, plus poll-timeout hash recovery and `record_error` hash storage in the keeper loop (#708, #721)

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
