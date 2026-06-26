# so4-oracle

Production Axum service for the SO4.market Soroban oracle and keeper.

This repository contains a single Rust binary that runs:
- Price fetching and aggregation from multiple sources (Binance, Coinbase, Pyth)
- Keeper loop that executes pending orders, deposits, and withdrawals on-chain
- HTTP API for price feeds and operational endpoints

## Architecture

```
so4-oracle  (single statically-deployed binary)
├── main.rs            tokio::main → load Config → build AppState → spawn loops → serve axum
├── HTTP API (axum + tower-http CORS/trace)
│     GET /health                      public   liveness
│     GET /ready                       public   RPC reachable + keeper funded
│     GET /prices                      public   serves in-memory PriceCache (frontend)
│     GET /oracle/status               admin    last cycle, balance, per-token state
│     GET /keeper/status               admin    pending work + last N executions
│     GET /oracle/failed-submissions   admin    ring buffer of failures
│     GET /metrics                     admin    Prometheus metrics
├── task: price_loop   tokio::interval(~1s)
│     fetch sources → validate → aggregate min/max → sign → write PriceCache
└── task: keeper_loop  tokio::interval(~1-2s)
      poll reader (orders/deposits/withdrawals)
      → if work: set_prices(needed tokens) → execute_*(key) per item → freeze on budget
      → record results; never panics the loop
```

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
RPC_URL=https://soroban-testnet.stellar.org
```

## Required Environment Variables

```bash
# Network configuration
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org
HORIZON_URL=https://horizon-testnet.stellar.org
NETWORK_PASSPHRASE="Test SDF Network ; September 2015"

# Contract IDs
ORACLE_CONTRACT_ID=CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY
ORDER_HANDLER_CONTRACT_ID=CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY
DEPOSIT_HANDLER_CONTRACT_ID=CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C
WITHDRAWAL_HANDLER_CONTRACT_ID=CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE
READER_CONTRACT_ID=CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC
DATA_STORE_CONTRACT_ID=CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J
ROLE_STORE_CONTRACT_ID=CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q

# Keeper configuration
KEEPER_PRIVATE_KEY=<hex-encoded-ed25519-private-key>
KEEPER_SECRET_KEY=<S...-strkey-seed>
KEEPER_ACCOUNT_ID=<G...-public-key>
KEEPER_INDEX=0
MIN_KEEPER_BALANCE_XLM=10

# API configuration
BIND_ADDR=0.0.0.0:8080
ADMIN_API_TOKEN=<optional-admin-token>

# Loop intervals (milliseconds)
PRICE_LOOP_MS=1000
KEEPER_LOOP_MS=1500

# Price feed configuration
PRICE_FEED_CONFIG=/path/to/tokens.json
```

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

### Docker

```bash
# Build the image
docker build -t so4-oracle .

# Run with environment variables
docker run -p 8080:8080 \
  -e STELLAR_RPC_URL=https://soroban-testnet.stellar.org \
  -e KEEPER_PRIVATE_KEY=<key> \
  -e KEEPER_SECRET_KEY=<secret> \
  -e KEEPER_ACCOUNT_ID=<account> \
  so4-oracle
```

### Systemd

```bash
# Copy the service file
sudo cp oracle.service /etc/systemd/system/

# Create environment file
sudo cp .env /opt/oracle/.env

# Enable and start
sudo systemctl enable oracle
sudo systemctl start oracle
```

### Fly.io

```bash
# Deploy to Fly.io
fly deploy

# Set secrets
fly secrets set KEEPER_PRIVATE_KEY=<key>
fly secrets set KEEPER_SECRET_KEY=<secret>
fly secrets set KEEPER_ACCOUNT_ID=<account>
```

### Railway

```bash
# Deploy to Railway
railway up

# Set environment variables in Railway dashboard
```

## Endpoints

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/health` | GET | No | Liveness check |
| `/ready` | GET | No | Readiness check (RPC + keeper balance) |
| `/prices` | GET | No | Current price feeds (CORS-enabled) |
| `/oracle/status` | GET | Admin | Oracle status and recent errors |
| `/keeper/status` | GET | Admin | Keeper status and execution history |
| `/oracle/failed-submissions` | GET | Admin | Failed submission history |
| `/metrics` | GET | Admin | Prometheus metrics |
