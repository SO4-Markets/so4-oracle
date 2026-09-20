# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- Pre-cycle batched Binance spot price fetching in `price_loop.rs` (`#969`, PR #991).
- Character-class validation for `pyth_feed_id` in `shared-config` and runtime guard in `oracle::pyth` (`#971`, PR #992).
- Full 20-label repository taxonomy in `CONTRIBUTING.md` documenting Component, Type/Area, and Workflow labels (`#966`, PR #990).
- Comprehensive documentation for `PYTH_API_KEY` environment variable across deployment manifests and guides (`#967`, PR #989).

### Changed
- Standardized MSRV pinning across CI workflows and Docker base configuration.

---

## [0.1.0] - 2026-09-02

### Added
- **Docker Healthcheck**: Added `HEALTHCHECK` directive in `Dockerfile` targeting `/health` endpoint (`#865`).
- **Configuration Documentation**: Added comprehensive field documentation and doc-comments for `Config` (`#864`, `#861`).
- **Display Symbol Fallback**: Added fallback logic for empty `display_symbol` to canonical `symbol` (`#861`).
- **Shutdown Timeout**: Bound shutdown drain and graceful loop termination under cancellation (`#861`, `#807`).

### Fixed
- Fixed invalid `RUST_LOG` environment configuration parsing (`#866`).
- Cleaned up Binance and Pyth price fetch paths, removing unreachable normalization branches and dead error mappings (`#859`, `#862`, `#703`, `#704`).
- Replaced non-existent GitHub team references in workflow files with active maintainers (`#863`).

---

## [0.0.9] - 2026-08-28

### Added
- **Admin Frozen Order API**: Exposed `frozen_order_blacklist` via admin endpoint for operations visibility (`#847`).
- **Keeper Balance Endpoint**: Added `GET /keeper/balance` endpoint and documentation (`#834`).
- **Predicate Integration Testing**: Added integration test suite utilizing `predicates` dev-dependency (`#836`).

### Performance
- **Keeper Order Failure Capping**: Capped repeated order execution failures and bounded retry exhaustion (`#858`, `#803`).
- **Keeper Queue Optimization**: Replaced $O(n)$ `remove(0)` on `last_executions` with $O(1)$ `VecDeque` (`#841`).
- **Account Sequence Caching**: Cached Stellar account sequence per keeper cycle rather than refetching per item (`#846`).
- **In-flight Key Sweeping**: Added sweeps for `in_flight_keys` stranded by cycle-level timeout (`#845`).

### Fixed
- **RPC Retry with Backoff**: Added exponential backoff retry to `get_latest_ledger_sequence` (`#843`).
- **Secret Key Sanitization**: Prevented secret key characters from leaking into `sign_transaction` error strings (`#831`).
- **Ready Timeout Alignment**: Raised `/ready` healthcheck timeout to match HTTP client timeout (`#832`).
- **Deployment Manifests**: Added missing required environment variables as placeholders in `fly.toml` and `railway.json` (`#840`).
- **Surface Outlier Rejections**: Surfaced rejected-source details and metrics for prices filtered by deviation check (`#849`, `#728`).
- **Trace Context**: Fixed empty `request_id` in API trace spans (`#857`, `#790`).
- **Dependency Cleanups**: Dropped unused `stellar-xdr` base64 feature and unused `tower-http` features (`#858`, `#815`, `#844`).

---

## [0.0.8] - 2026-08-25

### Added
- **Hermetic Integration Tests**: Comprehensive integration test coverage for price cycle token failures, multi-source aggregation, and keeper loops (`#827`, `#826`, `#825`, `#663`).
- **Machine-Readable Agent Guidelines**: Added `AGENTS.md` specifying repository invariants and review gates (`#660`).
- **Metrics Recording**: Added Prometheus metrics for cycle latency, price fetch errors, and outlier rejections (`#826`).

### Fixed
- Resolved multiple race conditions in keeper status snapshots and ledger sequence tracking (`#827`, `#825`).
- Deduplicated keeper loop RPC calls and fixed test flakiness under high concurrency (`#825`, `#837`).

---

## [0.0.1] - 2026-08-15

### Added
- Initial modular architecture for the `so4-oracle` service:
  - `oracle`: Axum-based API server, price feed aggregation loop, and Soroban keeper worker.
  - `shared/config`: Common token configuration, parsing, and validation crate.
- Core price sources: Binance spot API, Coinbase API, Pyth Hermes network, and local fixed fallback.
- Stellar Soroban contract interaction: on-chain price submission and signature encoding.
- Deployment support for Fly.io, Railway, and Docker containerization.
