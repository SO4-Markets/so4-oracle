# Contributing to so4-oracle

Thanks for helping build SO4 Markets. This guide covers how to set up the project, where things live, and how to get your changes merged.

---

## Setup

**Prerequisites**

- Rust stable (install via [rustup](https://rustup.rs))
- Optional: `cargo-watch` for hot-reloading during development (`cargo install cargo-watch`)

**Clone and build**

```bash
git clone git@github.com:SO4-Markets/so4-oracle.git
cd so4-oracle

# Build all workspace crates
cargo build --workspace

# Type-check without building artifacts
cargo check --workspace

# Run all tests
cargo test --workspace
```

**Environment variables**

The full list is defined in `oracle/src/config.rs` (`Config::from_lookup`). The table below shows all variables; required ones are marked with a star.

| Variable | Required | Default | Description |
|---|---|---|---|
| `STELLAR_NETWORK` | | `testnet` | `testnet` or `mainnet` — the network passphrase is derived automatically |
| `STELLAR_RPC_URL` | * (mainnet) | public testnet RPC | Soroban RPC endpoint |
| `HORIZON_URL` | | testnet/mainnet Horizon URL | Stellar Horizon endpoint |
| `BIND_ADDR` | | `0.0.0.0:8080` | Address and port the HTTP server binds to |
| `PRICE_FEED_CONFIG` | | embedded `config/tokens.json` | JSON array of `TokenConfig` entries (see `config/tokens.json` for schema) |
| `ORACLE_CONTRACT_ID` (or `ORACLE`) | * | | Deployed oracle contract address |
| `ROLE_STORE` | * | | On-chain role-store contract address |
| `DATA_STORE` | * | | On-chain data-store contract address |
| `ORDER_HANDLER` | * | | On-chain order-handler contract address |
| `DEPOSIT_HANDLER` | * | | On-chain deposit-handler contract address |
| `WITHDRAWAL_HANDLER` | * | | On-chain withdrawal-handler contract address |
| `READER` | * | | On-chain reader contract address |
| `KEEPER_PRIVATE_KEY` | * | | Keeper ed25519 private key (hex-encoded 32 bytes) |
| `KEEPER_SECRET_KEY` | * | | Keeper Stellar strkey secret seed (S-prefixed) |
| `KEEPER_ACCOUNT_ID` | * | | Keeper Stellar account ID (G-prefixed strkey) |
| `KEEPER_INDEX` | | `0` | Labels which keeper signed a given price (carries no work-partitioning behavior — see README.md for single-instance constraint) |
| `ADMIN_API_TOKEN` | | | Bearer token for admin routes (`/oracle/status`, `/metrics`, etc.) |
| `PYTH_API_KEY` | | | API key for the production Hermes (Pyth) endpoint |
| `MIN_KEEPER_BALANCE_XLM` | | `1.0` | Minimum keeper XLM balance before halting |
| `SET_PRICES_TX_FEE` | | `1000000` | Inclusion fee (stroops) for `set_prices` transactions |
| `KEEPER_TX_FEE` | | `2000000` | Inclusion fee (stroops) for keeper handler transactions |
| `PRICE_LOOP_MS` | | `1000` | Interval between price-feed refresh cycles (ms) |
| `KEEPER_LOOP_MS` | | `1500` | Interval between keeper execution cycles (ms) |

**Run locally**

```bash
cargo run -p oracle
# → listening on 0.0.0.0:3000 (or whatever BIND_ADDR is set to)
```

Watch mode (rebuilds on save):

```bash
cargo watch -x "run -p oracle"
```

---

## Project Layout

```
so4-oracle/
├── oracle/              Long-running Axum/Tokio binary — price loop, keeper loop, HTTP API
│   └── src/
│       ├── main.rs          Entry point: starts server, price loop, keeper loop
│       ├── config.rs        Config loading from env vars
│       ├── state.rs         AppState shared across all tasks
│       ├── price_loop.rs    Periodic price fetching and on-chain submission
│       ├── keeper_loop.rs   Periodic keeper task execution (orders, deposits, withdrawals)
│       ├── metrics.rs       In-memory counters exposed at GET /metrics
│       ├── api/
│       │   ├── mod.rs       Router: /health, /ready, /prices, /metrics, /oracle/status, etc.
│       │   ├── prices.rs    Public price feed and health/readiness handlers
│       │   └── admin.rs     Admin-only status and metrics handlers
│       ├── binance.rs       Binance price source
│       ├── coinbase.rs      Coinbase price source
│       ├── pyth.rs          Pyth price source
│       └── fixed.rs         Fixed-price source (for stablecoins)
├── shared/
│   └── config/src/lib.rs    TokenConfig struct + parse_token_configs() — shared by oracle
├── config/
│   └── tokens.json          Example token config for local development
└── oracle/tests/            Integration tests
```

There is **no** Cloudflare Worker, `wrangler.toml`, or `apis/` crate in this repository. The oracle is a plain Axum binary deployed via Docker (see `Dockerfile`) on Fly.io / Railway (see `fly.toml`, `railway.json`).

---

## HTTP API

Once running, the oracle exposes:

| Route | Auth | Description |
|---|---|---|
| `GET /health` | None | Always returns `{"status":"ok"}` — liveness probe |
| `GET /ready` | None | Returns 200 only when price cache is warm and loops are not stale |
| `GET /prices` | None | Current cached prices for all configured tokens |
| `GET /metrics` | Bearer | Cycle counts and latency gauges |
| `GET /oracle/status` | Bearer | Price cache + cycle status |
| `GET /keeper/status` | Bearer | Pending keeper operations + recent executions |
| `GET /keeper/balance` | Bearer | Current keeper account XLM balance |
| `GET /oracle/failed-submissions` | Bearer | Ring buffer of failed on-chain submissions |

---

## Finding Work

All open issues are tracked on [GitHub Issues](https://github.com/SO4-Markets/so4-oracle/issues). Issues are labelled:

| Label | Meaning |
|---|---|
| `good first issue` | Self-contained, well-defined, good starting point |
| `bug` | Something is broken |
| `documentation` | Docs, comments, diagrams |
| `enhancement` | New feature or improvement |
| `infrastructure` | CI, Docker, deploy scripts, tooling |
| `apis` | HTTP API, REST endpoints, request/response handling |
| `contracts` | Soroban smart contract interactions, on-chain calls |
| `oracle-worker` | Cloudflare Worker oracle crate |
| `performance` | Performance improvement |
| `testing` | Test infrastructure, coverage, flaky tests |
| `security` | Security vulnerability or hardening |
| `priority` | High-priority issue requiring immediate attention |
| `dependencies` | Dependency updates, version bumps |
| `github_actions` | CI/CD workflows, GitHub Actions configuration |
| `rust` | Rust language–specific issues or improvements |

Before starting, leave a comment on the issue so no one duplicates effort.

---

## Workflow

1. **Fork** the repo (external contributors) or create a branch (team members).
2. Branch naming: `feat/short-description`, `fix/short-description`, `test/short-description`.
3. Make your changes. Keep commits focused — one logical change per commit.
4. **Run checks locally** before opening a PR:
   ```bash
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test --workspace
   ```
5. Open a PR against `main`. Fill in the PR template.
6. Request a review from a maintainer.

---

## Working with Coding Agents

If you are using an autonomous coding agent (or if you are an agent), you must read and strictly follow the [AGENTS.md](./AGENTS.md) contract. It contains the mandatory verification gate commands and specific repository traps you must be aware of to avoid breaking the build.

---

## Pull Request Guidelines

- **Title:** Start with a type prefix: `feat:`, `fix:`, `test:`, `docs:`, `chore:`.
- **Description:** What does this do, and why? Link the relevant issue (`Closes #N`).
- **Tests:** New functionality must include tests. Bug fixes should include a regression test.
- **No partial implementations:** If a function is not yet complete, leave it as a stub with `todo!()` rather than committing broken logic.
- **No unnecessary refactors:** Keep PRs focused on the stated issue.

---

## Code Style

- `cargo fmt` is enforced in CI. Run it before pushing.
- `cargo clippy -- -D warnings` must pass. Address all warnings.
- No comments explaining *what* code does — names should do that. Add a comment only when the *why* is non-obvious.
- No emojis in code or commit messages.

---

## Testing

- Unit tests go in the same file: `#[cfg(test)] mod tests { ... }`.
- Integration tests go in `oracle/tests/`.
- For HTTP endpoint tests, use `axum::test` or `reqwest` against a spawned server.

---

## Commit Messages

```
type(scope): short summary (≤72 chars)

Optional body — explain the why, not the what.
```

Types: `feat`, `fix`, `test`, `docs`, `chore`, `refactor`
Scopes: `oracle`, `shared`, `config`, `workspace`

---

## Questions

Open a discussion on GitHub or drop a message in the team channel. Don't open an issue just to ask a question.
