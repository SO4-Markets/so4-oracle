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

Complete environment variables read by `Config::from_lookup` (see `oracle/src/config.rs` and `oracle/README.md`):

#### Required Contract IDs
All 7 Soroban contract IDs are strictly required for the service to start:

| Variable | Description |
|---|---|
| `ORACLE_CONTRACT_ID` | Deployed oracle contract address (or `ORACLE` alias on testnet) |
| `ROLE_STORE` | Deployed role store contract address |
| `DATA_STORE` | Deployed data store contract address |
| `ORDER_HANDLER` | Deployed order handler contract address |
| `DEPOSIT_HANDLER` | Deployed deposit handler contract address |
| `WITHDRAWAL_HANDLER` | Deployed withdrawal handler contract address |
| `READER` | Deployed reader contract address |

#### Required Keeper Credentials
Both signing credentials and the public account ID are required:

| Variable | Format / Description |
|---|---|
| `KEEPER_PRIVATE_KEY` | 64-character hex-encoded Ed25519 signing key (32 bytes) for transaction signatures |
| `KEEPER_SECRET_KEY` | Stellar `S...`-prefixed secret key Strkey for the keeper account |
| `KEEPER_ACCOUNT_ID` | Stellar `G...`-prefixed public account Strkey for the keeper account |

#### Network & Endpoints

| Variable | Default | Description |
|---|---|---|
| `STELLAR_NETWORK` | `testnet` | Target network: `testnet` or `mainnet` (network passphrase is automatically derived) |
| `STELLAR_RPC_URL` | Testnet public RPC | Soroban RPC endpoint (optional on testnet; required on mainnet) |
| `HORIZON_URL` | Public Horizon | Stellar Horizon endpoint for keeper balance queries |
| `BIND_ADDR` | `0.0.0.0:8080` | Local HTTP API bind address and listening port |
| `PRICE_FEED_CONFIG` | Embedded `config/tokens.json` | JSON array of `TokenConfig` entries (see `config/tokens.json` for schema) |

#### Worker Loop Tuning & Defaults

| Variable | Default | Description |
|---|---|---|
| `PRICE_LOOP_MS` | `1000` | Price fetch and submission cycle interval in milliseconds |
| `KEEPER_LOOP_MS` | `1500` | Keeper execution loop cycle interval in milliseconds |
| `MIN_KEEPER_BALANCE_XLM` | `10` | Minimum keeper account balance in XLM before triggering alerts and 503 readiness degradation |
| `SET_PRICES_TX_FEE` | `100` | Transaction fee in stroops for `set_prices` contract calls |
| `KEEPER_TX_FEE` | `100` | Transaction fee in stroops for keeper contract calls |
| `KEEPER_INDEX` | `0` | Instance index offset for this keeper process |

#### Optional Authentication & API Keys

| Variable | Default | Description |
|---|---|---|
| `ADMIN_API_TOKEN` | None | Bearer token for admin routes (`/oracle/status`, `/keeper/*`, etc.). When unset, admin endpoints return 503. |
| `PYTH_API_KEY` | None | Optional API key for authenticating with Pyth price feed services |

**Run locally**

```bash
cargo run -p oracle
# → listening on 0.0.0.0:8080 (or whatever BIND_ADDR is set to)
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
└── tests/                   Integration tests
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
- Integration tests go in `tests/` at the workspace root.
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
