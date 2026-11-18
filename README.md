# so4-oracle

Production Axum service for the SO4.market Soroban oracle and keeper.

This repository contains a single Rust binary that runs:

- Price fetching and aggregation from multiple sources (Binance, Coinbase, Pyth)
- Keeper loop that executes pending orders, deposits, and withdrawals on-chain
- HTTP API for price feeds and operational endpoints

## Architecture.

```
so4-oracle  (single statically-deployed binary)
├── main.rs            tokio::main → load Config → build AppState → spawn loops → serve axum
├── HTTP API (axum + tower-http CORS/trace)
│     GET /health                      public   liveness
│     GET /ready                       public   RPC reachable + keeper funded
│     GET /prices                      public   serves in-memory PriceCache (frontend)
│     GET /oracle/status               admin    last cycle, balance, per-token state
│     GET /keeper/status               admin    pending work + last N executions
│     GET /keeper/balance              admin    live keeper account XLM balance
│     DELETE /keeper/blacklist/{key}   admin    clears a permanently-blacklisted order/deposit/withdrawal key
│     GET /oracle/failed-submissions   admin    ring buffer of failures
│     GET /metrics                     admin    Prometheus metrics
├── task: price_loop   tokio::interval(~1s)
│     fetch sources → validate → aggregate min/max → sign → write PriceCache
└── task: keeper_loop  tokio::interval(~1-2s)
      poll reader (orders/deposits/withdrawals)
      → if work: set_prices(needed tokens) → execute_*(key) per item → freeze on budget
      → record results; never panics the loop
```

### The two loops and shared state

`main.rs` builds one `AppState` (config, HTTP client, `PriceCache`, cycle and
keeper status, failure/blacklist counters, and the decoded signing keys) and
spawns two tokio tasks that share it.

- **Price loop** (`oracle/src/price_loop.rs`, `PRICE_LOOP_MS`, default 1s) —
  fetches every configured source (Binance, Coinbase, Pyth, fixed), validates
  and aggregates min/max/median per token, signs the payload with the decoded
  `KEEPER_PRIVATE_KEY`, writes the result into `PriceCache`, and submits
  `set_prices` to the oracle contract when medians move.
- **Keeper loop** (`oracle/src/keeper_loop.rs`, `KEEPER_LOOP_MS`, default
  1.5s) — polls the reader contract for pending orders, deposits and
  withdrawals; refreshes prices for the tokens that pending work touches
  (again via `set_prices`); then executes — or freezes — each item on its
  handler contract. Every error is recorded and the loop continues; it never
  panics (`panic = "abort"` in the release profile would take the process
  down).

`PriceCache` is the bridge between the loops and the API: the price loop
writes signed prices into it, `GET /prices` serves it to the frontend, and
the keeper loop reads it to decide which tokens need a fresh submission
before executing work. Cycle/keeper status and the failure ring buffer in
`AppState` back the admin endpoints.

### On-chain contracts each side talks to

| Contract env var       | Used by                    | For                                          |
| ---------------------- | -------------------------- | -------------------------------------------- |
| `ORACLE_CONTRACT_ID`   | price loop, keeper loop    | `set_prices` submissions                     |
| `READER`               | keeper loop                | polling pending orders/deposits/withdrawals  |
| `DATA_STORE`           | keeper loop                | read-side state behind the reader queries    |
| `ORDER_HANDLER`        | keeper loop                | `execute_order` / `freeze_order`             |
| `DEPOSIT_HANDLER`      | keeper loop                | executing / freezing deposits                |
| `WITHDRAWAL_HANDLER`   | keeper loop                | executing / freezing withdrawals             |

`ROLE_STORE` is the on-chain role store the handler contracts authorize
against; the service requires its address at boot and validates it is
distinct from the other contract IDs.

### Failure states an operator will encounter

| Failure state            | What it means                                                                                     | Where to look                                                                                          |
| ------------------------ | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| Frozen order/deposit/withdrawal | repeated freeze/execute failures abandoned the key (`MAX_CONSECUTIVE_FREEZE_FAILURES`, `MAX_CONSECUTIVE_EXECUTION_FAILURES`) | `GET /keeper/status` recent executions, `GET /oracle/failed-submissions`, `oracle_blacklisted_keys` metric |
| Blacklisted key          | the key is permanently skipped until cleared; a replacement must be posted on-chain               | `DELETE /keeper/blacklist/{key}` clears it for retry                                                    |
| Stale prices             | sources failing or the price cycle has stalled                                                    | `GET /prices` timestamps, `GET /oracle/status`, `oracle_last_set_prices_timestamp_seconds` metric       |
| Low keeper balance       | balance fell below `MIN_KEEPER_BALANCE_XLM`; execution halts until the account is funded          | `GET /keeper/balance`, `/ready` reports `503`, `oracle_keeper_balance_below_min` metric                 |

## Deployed Contract Reference

Testnet oracle:

```text
ORACLE=CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY
ORDER_HANDLER=CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY
DEPOSIT_HANDLER=CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C
WITHDRAWAL_HANDLER=CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE
READER=CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC
DATA_STORE=CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J
ROLE_STORE=CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q
NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
STELLAR_RPC_URL=https://soroban-testnet.stellar.org
```

## Required Environment Variables.

The names below are the exact names the binary reads at startup via
`Config::from_env()`. Using any other name (e.g. `ORDER_HANDLER_CONTRACT_ID`)
will silently be ignored and the process will exit with a "required env var
not set" error (#499).

```bash
# Network configuration
STELLAR_NETWORK=testnet           # "testnet" (default) or "mainnet"
STELLAR_RPC_URL=https://soroban-testnet.stellar.org  # required on mainnet; optional on testnet
HORIZON_URL=https://horizon-testnet.stellar.org      # optional; defaults to network default

# Contract IDs — use the short names exactly as shown
ORACLE_CONTRACT_ID=CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY
ORDER_HANDLER=CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY
DEPOSIT_HANDLER=CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C
WITHDRAWAL_HANDLER=CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE
READER=CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC
DATA_STORE=CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J
ROLE_STORE=CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q

# Keeper configuration
KEEPER_PRIVATE_KEY=<64-hex-char ed25519 private key>
KEEPER_SECRET_KEY=<S...-strkey seed>
KEEPER_ACCOUNT_ID=<G...-public key>
KEEPER_INDEX=0
MIN_KEEPER_BALANCE_XLM=10
# Optional inclusion fees (stroops); defaults match historical hardcoded values
SET_PRICES_TX_FEE=1000000
KEEPER_TX_FEE=2000000

# API configuration
BIND_ADDR=0.0.0.0:8080
ADMIN_API_TOKEN=<optional admin token>

# Loop intervals (milliseconds)
PRICE_LOOP_MS=1000
KEEPER_LOOP_MS=1500

# Logging (optional)
RUST_LOG=info                     # tracing EnvFilter, e.g. oracle=debug,tower_http=debug
LOG_FORMAT=json                   # "json" (default) or "pretty" for local development

# Price feed configuration (optional; falls back to embedded config/tokens.json).
# Must be an inline JSON string — NOT a file path. Pass the JSON directly or
# read a file at startup:
#   PRICE_FEED_CONFIG="$(cat /path/to/tokens.json)"
# Example with a single token entry:
PRICE_FEED_CONFIG='[{"symbol":"TUSDC","display_symbol":"USDC","stellar_address":"CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES"}]'

# Pyth authentication (optional now, mandatory once Pyth enforces auth)
# PYTH_API_KEY=<your-pyth-api-key>
```

> **Note:** `NETWORK_PASSPHRASE` is not read by the binary — the correct
> passphrase is selected automatically based on `STELLAR_NETWORK`. You do
> not need to set it.

## Development

```bash
# Check the code
cargo check --workspace

# Run tests
cargo test --workspace

# Run locally (with .env file)
cargo run --bin oracle

# Build for production
cargo build --release --bin oracle
```

## Deployment

**This service must run as exactly one instance — do not scale it horizontally.**
Double-submission prevention and the freeze-failure/blacklist counters
(`AppState::in_flight_keys`, `freeze_failure_counts`, `frozen_order_blacklist`,
`execution_failure_counts` in `oracle/src/state.rs`) live entirely in
in-process memory, not in a shared store. A second replica would start with
an empty `in_flight_keys` map, independently poll the same pending
orders/deposits/withdrawals, and race the first replica to submit competing
transactions for the same keys — the exact scenario the in-flight tracking
exists to prevent, reopened at the process level. `fly.toml`'s
`min_machines_running = 1` only guarantees at least one machine stays up
(so Fly's autostop doesn't suspend it to zero); it does not cap the count,
so running `fly scale count 2` (or setting a replica count on Railway)
would silently introduce this race with no error or warning. See
`AGENTS.md`'s "Repository-Specific Traps" for the same note in the
contributor-facing doc.

### Docker

```bash
# Build the image
docker build -t so4-oracle .

# Run with environment variables
docker run -p 8080:8080 \
  -e STELLAR_RPC_URL=https://soroban-testnet.stellar.org \
  -e ORACLE_CONTRACT_ID=CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY \
  -e ROLE_STORE=CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q \
  -e DATA_STORE=CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J \
  -e ORDER_HANDLER=CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY \
  -e DEPOSIT_HANDLER=CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C \
  -e WITHDRAWAL_HANDLER=CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE \
  -e READER=CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC \
  -e KEEPER_PRIVATE_KEY=<key> \
  -e KEEPER_SECRET_KEY=<secret> \
  -e KEEPER_ACCOUNT_ID=<account> \
  so4-oracle
```

### Systemd

```bash
# Build the release binary
cargo build --release --bin oracle

# Create the deployment directory
sudo mkdir -p /opt/oracle

# Copy the binary
sudo cp target/release/oracle /opt/oracle/oracle
sudo chown oracle:oracle /opt/oracle/oracle

# Copy the service file
sudo cp oracle.service /etc/systemd/system/

# Create environment file
sudo cp .env /opt/oracle/.env

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable oracle
sudo systemctl start oracle
```

The unit restarts on any exit with a 5-second delay, and systemd's start limit
allows 50 restarts per 5 minutes — enough for a bad deploy or a transient boot
dependency to self-heal. If the limit is ever exhausted (check with
`systemctl status oracle`), clear it with:

```bash
sudo systemctl reset-failed oracle
sudo systemctl start oracle
```

### Fly.io

```bash
# Set secrets BEFORE deploying (never in fly.toml: its [env] block is committed and shown by `fly config show`)
fly secrets set KEEPER_PRIVATE_KEY=<hex> KEEPER_SECRET_KEY=<S...> ADMIN_API_TOKEN=<token>
fly secrets set PYTH_API_KEY=<key>   # optional

# Deploy to Fly.io (secrets are already in place; the app starts with them)
fly deploy
```

Non-secret settings (network, contract IDs, `KEEPER_ACCOUNT_ID`, loop
intervals, fees) live in `fly.toml`'s `[env]`; edit the `YOUR_…` placeholders there.

### Railway

```bash
# Deploy to Railway
railway up
```

`railway.json` only carries non-secret variables. Add `KEEPER_PRIVATE_KEY`,
`KEEPER_SECRET_KEY`, `ADMIN_API_TOKEN` (and optionally `PYTH_API_KEY`) in the
Railway dashboard under the service's **Variables** tab, and mark each one
**Sealed** so its value can't be viewed or exported afterwards. Never add them
to `railway.json`, which is committed to the repo.

> **Secrets never go in a tracked manifest (#1232).** CI's `manifest-secrets`
> job fails if `KEEPER_PRIVATE_KEY`, `KEEPER_SECRET_KEY`, `ADMIN_API_TOKEN` or
> `PYTH_API_KEY` is assigned in `fly.toml` or `railway.json`.

## Documentation

- [docs/RUNBOOK.md](docs/RUNBOOK.md) — operator runbook: symptoms, diagnosis, remediation
- [docs/KEEPER_STATE_MACHINE.md](docs/KEEPER_STATE_MACHINE.md) — the keeper failure state machine
- [docs/RISK_MECHANICS.md](docs/RISK_MECHANICS.md) — design-only reference for the external liquidation/ADL/insurance-fund contracts (not implemented in this service)

## Endpoints

| Endpoint                     | Method | Auth  | Description                                                   |
| ---------------------------- | ------ | ----- | ------------------------------------------------------------- |
| `/health`                    | GET    | No    | Liveness check                                                |
| `/ready`                     | GET    | No    | Readiness check (RPC + keeper balance)                        |
| `/prices`                    | GET    | No    | Current price feeds (CORS-enabled)                            |
| `/oracle/status`             | GET    | Admin | Oracle status and recent errors                               |
| `/keeper/status`             | GET    | Admin | Keeper status and execution history                           |
| `/keeper/balance`            | GET    | Admin | Live keeper account XLM balance                               |
| `/keeper/blacklist/{key}`    | DELETE | Admin | Clears a permanently-blacklisted order/deposit/withdrawal key |
| `/oracle/failed-submissions` | GET    | Admin | Failed submission history                                     |
| `/metrics`                   | GET    | Admin | Prometheus metrics                                            |

## Observability

Every request emits a structured JSON log carrying: `timestamp`, `level`, `method`, `route` (matched path, not raw URI), `status`, `latency_ms`, and `request_id`. An `x-request-id` header is accepted on inbound requests; if absent, a UUIDv4 is generated. The request ID is echoed in the response headers and included in all handler-internal log events.

The following HTTP metrics are exposed at `/metrics`:

- `oracle_http_requests_total{route,method,status_class}` (counter)
- `oracle_http_request_duration_seconds_bucket{route,le}` (histogram)
- `oracle_http_requests_in_flight` (gauge)
- `oracle_http_auth_failures_total{route}` (counter for 401s on admin routes)

> **Note:** Health check traffic (`/health` and `/ready`) is logged at the `debug` level. Because these endpoints are polled frequently (e.g., every 30s by the Docker HEALTHCHECK), logging them at `info` would drown out real traffic.
