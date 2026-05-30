# so4-oracle.

Oracle keeper and API server for [SO4 Markets](https://github.com/SO4-Markets) — a decentralised perpetuals and spot exchange on Stellar/Soroban.

This workspace feeds signed prices into the on-chain `oracle` contract and exposes REST/WebSocket APIs for frontends and integrators.

---

## Workspace Structure

```
so4-oracle/
├── Cargo.toml          # Workspace manifest
├── wrangler.toml       # Cloudflare Worker deployment config
├── docker-compose.yml  # Local dev stack (Stellar + Redis + APIs)
├── Makefile            # make dev / deploy-local / test
├── scripts/
│   ├── deploy_testnet.sh
│   └── build_contracts.sh
│
├── contracts/          # Soroban smart contracts
├── oracle/             # Cloudflare Worker — keeper price submission
└── apis/               # Native Axum server — REST API
```

---

## Crates

### `oracle` — Cloudflare Worker

Runs on Cloudflare's edge network. Fetches prices from external exchanges, aggregates them, signs with the keeper ed25519 key, and submits to the on-chain `oracle` Soroban contract via Stellar RPC.

Deployed via `wrangler deploy`.

### `apis` — Axum API Server

A standard Tokio/Axum binary that projects can run alongside or independently. Exposes price feeds, market data, and oracle status over HTTP and WebSocket so frontends and integrators don't need to hit Stellar RPC directly.

Runs with `cargo run -p apis`.

---

## Features

### Oracle Worker (`oracle/`)

- [x] Cloudflare Worker scaffolding (Axum + worker-build)
- [x] Fetch prices from Binance
- [x] Fetch prices from Coinbase
- [x] Fetch prices from Pyth Network
- [x] Multi-source median price aggregation
- [x] Outlier rejection (> 3σ from median)
- [x] Confidence interval calculation
- [x] Ed25519 keeper key signing (on-chain oracle message format)
- [x] Stellar RPC client — submit signed prices to on-chain oracle
- [x] Cloudflare Cron Trigger — scheduled price updates (every 60 seconds)
- [x] Multi-token feed configuration (token list + per-token source mapping)
- [x] Retry logic with exponential backoff
- [x] Network selection via env vars (testnet / mainnet)
- [x] Keeper wallet balance monitoring
- [x] Dead-letter queue for failed submissions
- [x] Circuit breaker for large price movements
- [x] KV-backed price caching

### APIs Server (`apis/`)

- [x] `GET /health` — `{"status":"ok"}`
- [x] `GET /prices/:token` — single token price (min/max/timestamp)
- [x] `GET /prices/:token/history` — OHLCV candle history (1m/5m/1h intervals)
- [x] `GET /markets` — all active markets with pool stats
- [x] `GET /markets/:market_id` — single market detail (pool value, OI, funding rate)
- [x] `GET /positions/:account` — account open positions with PnL
- [x] `GET /openapi.json` — auto-generated OpenAPI 3.1 spec
- [x] `GET /docs` — Swagger UI
- [x] In-memory cache layer for oracle prices
- [x] CORS configuration for frontend integration
- [x] Structured logging (`tracing` subscriber)
- [x] Graceful shutdown
- [x] OpenAPI / Swagger spec generation
- [x] Admin endpoint authentication

### Planned (not yet implemented)

- [ ] `GET /orders/:account` — account pending orders
- [ ] `WS /prices/stream` — real-time price push over WebSocket
- [ ] Rate limiting middleware

---

## Getting Started

**Prerequisites:** Rust (stable), `wrangler` CLI, a Cloudflare account for the worker.

```bash
# Check the workspace builds
cargo check

# Run the APIs server locally
cargo run -p apis
# → listening on 0.0.0.0:3000

# Deploy the oracle worker to Cloudflare
wrangler deploy
```

---

## Contract Deployment (Testnet)

Deploy all SO4 contracts to Stellar testnet and write addresses to `.env.testnet`:

```bash
# Prerequisites: stellar CLI, funded testnet identity
stellar keys add deployer --network testnet

# Build WASM (or set CONTRACTS_WASM to a pre-built artifact)
./scripts/build_contracts.sh

# Deploy (idempotent — skips already-deployed contracts)
DEPLOYER=deployer ./scripts/deploy_testnet.sh
```

The script deploys contracts in dependency order, initialises cross-contract references, creates a test market, and writes all addresses to `.env.testnet`.

---

## Local Development (Docker)

Spin up a local Stellar Quickstart node, Redis, and the APIs server:

```bash
make dev          # start all services (Stellar :8000, Redis :6379, APIs :3000)
make deploy-local # deploy contracts to the local node
make test         # run all workspace tests
make down         # stop services
```

Services:
- **Stellar Quickstart** — testnet Soroban RPC at `http://localhost:8000/soroban/rpc`
- **Redis** — cache backend at `redis://localhost:6379`
- **APIs** — REST server at `http://localhost:3000`

---

## Testing

```bash
cargo test --all                    # full workspace
cargo test -p contracts             # Soroban contract tests
cargo test -p contracts --test e2e_full_flow  # end-to-end flow (#112)
```

---

## Environment Variables

| Variable | Crate | Description |
|---|---|---|
| `KEEPER_PRIVATE_KEY` | oracle | Ed25519 private key (hex) for signing prices |
| `STELLAR_RPC_URL` | oracle | Stellar RPC endpoint |
| `ORACLE_CONTRACT_ID` | oracle | On-chain oracle contract address |
| `NETWORK_PASSPHRASE` | oracle | `"Test SDF Network ; September 2015"` or mainnet |
| `PRICE_FEED_CONFIG` | oracle | JSON array of token feed configurations |
| `PRICE_MOVEMENT_THRESHOLD` | oracle | Circuit breaker threshold (default: 10%) |
| `PORT` | apis | Listen port (default `3000`) |
| `API_KEY` | apis | Bearer token for admin endpoints |
| `CORS_ALLOWED_ORIGINS` | apis | Comma-separated allowed origins |

---

## Related Repos

| Repo | Description |
|---|---|
| [SO4-Markets/contracts](https://github.com/SO4-Markets/contracts) | Soroban smart contracts |
| [SO4-Markets/interface](https://github.com/SO4-Markets/interface) | Frontend |

---

## License

MIT.
