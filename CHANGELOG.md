# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

No version has been released or tagged yet (#1435). `version = "0.1.0"` in
`Cargo.toml` is the pre-release placeholder, not a published release; the
first tag should be cut from a commit whose CI is green. Until then every
entry below is on `main` but unreleased.

Each PR that changes behaviour, configuration or deployment adds a line here
under `[Unreleased]` (see `AGENTS.md` §3).

## [Unreleased]

### Added
- Boot-time bounds for `PRICE_LOOP_MS` / `KEEPER_LOOP_MS` (250–60000 ms, and `PRICE_LOOP_MS` below every token's `stale_after_seconds`), a `KEEPER_INDEX` cap of 255, and `PYTH_API_KEY` shape validation; startup warnings for pyth tokens with no key and for tokens one source outage away from two-source aggregation; the `oracle_token_sources_available` gauge (#1464, #1465, #1466, #1467)
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
- `docs/ENVIRONMENT.md`, the single environment-variable reference, and the `env-docs` CI job (`scripts/check-env-docs.sh`) that checks it against the code (#1432)
- `docs/README.md` index linked from `README.md`; plan documents carry a `Status:` line and `plan.md` moved to `docs/REWRITE_PLAN.md` (#1433, #1434)
- Test-only `COINBASE_BASE_URL` / `BINANCE_BASE_URL` config overrides, threaded through both price fetch paths, and a CI check that no tracked `*.rs` file starts with a UTF-8 BOM (#1264)

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
- `keeper_loop::record_error` now records the transaction hash for post-submission failures (poll timeouts) instead of always storing `None`, so `/oracle/failed-submissions` exposes a hash to look up on a block explorer for ambiguous outcomes (#721)
- Binance, Coinbase, and Pyth HTTP 429 responses are now classified as retryable (matching `RpcError`), so a rate limit triggers backoff and a retry instead of permanently abandoning the price source for the cycle (#708)
- `/oracle/failed-submissions` now reports buffer-wide `total_count` alongside `matched_count`; previously `total_count` held only the filtered count, so paginated callers could not see how many records existed. The `?operation=` prefix semantics are documented in code and the API docs.
- `oracle.service` start limit raised from 3 restarts per 60s to 50 per 300s and moved to the `[Unit]` section, so a crash loop no longer leaves the oracle permanently down pending a manual `systemctl reset-failed` (#544)

### Changed
- `SET_PRICES_TX_FEE` and `KEEPER_TX_FEE` are capped at 10000000 stroops (1 XLM)
  per transaction; a fee above 2x its default is accepted but logged at
  startup (#1463)
- `MIN_KEEPER_BALANCE_XLM=0` now logs a startup warning that it disables the
  keeper balance gate (#1462)
- `shared/config` crate docs explain its split from `oracle::config`, and every
  `TokenConfig` field documents its unit, range and interactions, tabulated in
  `docs/ENVIRONMENT.md` (#1460, #1461)
- `TokenConfig.min` and `TokenConfig.max` removed. They were documented as
  "used by the API server for display" but nothing read them; the API serves
  runtime percentile bounds from `CachedPrice` instead. **Breaking for any
  `PRICE_FEED_CONFIG` that sets `min`/`max`** — such a config now fails
  `deny_unknown_fields` at parse time instead of being silently ignored.
- `Metrics` counters moved behind the single `Counters` mutex, completing the
  migration started in #599 so a `record_*` call is one consistent generation
- `Metrics::record_keeper_cycle` takes a `KeeperCycleTally` instead of seven
  positional arguments
- `FailedSubmission` is now a tagged enum (`"kind": "price"` / `"kind": "keeper"`). Keeper records carry an optional `key` and no longer emit meaningless `token`/`symbol`/`min`/`max` fields; the failure ring buffer and `/oracle/failed-submissions` filter are unchanged in behaviour.

### Tests
- Added coverage for `validate_pyth_price` rejecting a malformed (non-numeric) `conf` field, previously unexercised (#712)
- Added coverage for 429/5xx/4xx retry classification in `BinancePriceError`, `CoinbasePriceError`, and `PythPriceError`, plus poll-timeout hash recovery and `record_error` hash storage in the keeper loop (#708, #721)
