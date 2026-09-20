# Changelog

All notable, user-facing or behavior-relevant changes to this project are
documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How to contribute an entry

- Every merged PR that changes **behavior** (price aggregation, keeper logic,
  circuit-breaker thresholds, retry/restart behavior, security, configuration,
  or the HTTP API surface) **must** add an entry under `## [Unreleased]`.
- Group the entry under the most specific heading that applies:
  `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, or `Security`.
- Write entries in the imperative mood ("Add", "Fix", "Change") and reference
  the PR number, e.g. `(#123)`.
- Internal refactors, test-only changes, and dependency bumps that do not
  affect runtime behavior do **not** require an entry.
- When a release is cut, move the `## [Unreleased]` entries into a new
  `## [x.y.z] - YYYY-MM-DD` section and start a fresh `## [Unreleased]`.

## [Unreleased]

### Added

- Initial changelog and contribution convention for tracking
  behavior-relevant changes.

## [0.1.0] - 2025-01-01

### Added

- Single statically-deployed Rust binary (`so4-oracle`) that runs the price
  fetcher/aggregator, the on-chain keeper loop, and the HTTP API.
- Price fetching and aggregation from multiple sources: Binance, Coinbase,
  and Pyth.
- Keeper loop that executes pending orders, deposits, and withdrawals on-chain.
- HTTP API (Axum + tower-http CORS/trace):
  - `GET /health` — public liveness probe.
  - `GET /ready` — public readiness probe (RPC reachable + keeper funded).
  - `GET /prices` — public in-memory `PriceCache` feed for the frontend.
  - `GET /oracle` — oracle price endpoint.
  - Admin endpoints for operational control.
- Configuration via `config/tokens.json` and environment variables
  (see `.env.example`).
- CI workflow (`.github/workflows/ci.yml`) and repository metadata workflow.
- Docker image (`Dockerfile`) and deployment manifests (`fly.toml`,
  `railway.json`, `oracle.service`).
