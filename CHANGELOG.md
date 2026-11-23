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
- `CycleSummary` now returns the pending order/deposit/withdrawal counts to callers. `KeeperCycleTally` carries them into `/metrics` as `oracle_keeper_cycle_pending_keys{kind=…}` and the new `oracle_keeper_cycle_all_failed_total` counter fires when work was pending but nothing succeeded.
- `/oracle/status` now exposes `last_full_submission` and a per-token `{last_submitted_median, current_median, delta_bps, threshold_bps}` block; `/metrics` adds `oracle_last_full_submission_timestamp_seconds` and `oracle_price_delta_bps{symbol=…}` so a stalled submission heartbeat can be diagnosed.
- `oracle_build_info{version,commit,rustc,network}` gauge on `/metrics`. The
  commit and compiler are injected at build time by `oracle/build.rs`, and
  `network` comes from `Config`, so a scrape identifies the exact deploy and
  distinguishes two accidentally co-located instances
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
- `COINBASE_BASE_URL`, `BINANCE_BASE_URL` and `PYTH_HERMES_URL` overrides for
  the three price sources, so integration tests can use a mock server
- Shared `IDLE_BACKOFF_MULTIPLIER` / `KEEPER_STALE_THRESHOLD_MULTIPLIER`
  constants used by both the keeper loop and `/ready`
- `docs/ENVIRONMENT.md`, the single environment-variable reference, and the `env-docs` CI job (`scripts/check-env-docs.sh`) that checks it against the code (#1432)
- `docs/README.md` index linked from `README.md`; plan documents carry a `Status:` line and `plan.md` moved to `docs/REWRITE_PLAN.md` (#1433, #1434)
- Test-only `COINBASE_BASE_URL` / `BINANCE_BASE_URL` config overrides, threaded through both price fetch paths, and a CI check that no tracked `*.rs` file starts with a UTF-8 BOM (#1264)

### Fixed
- Coinbase's request shape is no longer inferred from substrings in the base URL: the currency is always sent as `?currency=…`, so a plain `http://host:port/path` override produces `/path?currency=BTC` instead of `/pathBTC` (#1264).
- `retry_with_backoff`'s non-retryable branch now has direct coverage: an error whose `is_retryable()` is false invokes the closure exactly once and is returned unchanged (#1227).
- Repaired the broken build/test baseline: missing delimiters in `config.rs`, `http.rs`, `keeper_loop.rs` and `api/mod.rs`, a stray fragment in `/oracle/status`, stale signing-key/aggregator APIs in tests, and an invalid test keeper strkey.
- Jitter can no longer scale a positive delay to zero (which turned retry
  loops into busy loops) or above its input (which defeated the max-delay cap);
  the factor is now injectable for property tests
- `poll_until_confirmed` no longer sleeps after its final attempt and uses one
  16 s cap for every backoff path, keeping its total budget inside the 50 s
  keeper-cycle timeout. Tests now use the shipped constants and assert the
  actual sleep sequence instead of `cfg(test)`-only 1 ms values
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
- `/ready` no longer returns `keeper_loop_stale` during the idle backoff: its
  threshold is now derived from the same constants as the keeper loop
- Idle backoff is only applied when a cycle found no pending work, had no
  errors, and neither the stale-price nor low-balance gate fired; failed work
  and gated cycles retry on the next tick
- Coinbase, Binance and Pyth fetches take their base URL from `Config` instead
  of a compile-time production constant
- Pyth prices with a `publish_time` more than `MAX_FUTURE_SKEW_SECONDS` ahead
  of the local clock are rejected; smaller future skew is warned about and
  treated as age zero rather than silently passing the staleness check
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
