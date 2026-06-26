# SO4 Oracle Runtime

The oracle now runs as a long-lived native `tokio` binary:

```sh
cargo run --bin oracle
```

Configuration is loaded once at boot via `dotenvy::dotenv()` and process
environment variables. Missing required variables abort startup with the exact
variable name.

## Required Environment

- `ORACLE_CONTRACT_ID` or `ORACLE`
- `ROLE_STORE`
- `DATA_STORE`
- `ORDER_HANDLER`
- `DEPOSIT_HANDLER`
- `WITHDRAWAL_HANDLER`
- `READER`
- `KEEPER_PRIVATE_KEY`
- `KEEPER_SECRET_KEY`
- `KEEPER_ACCOUNT_ID`

Optional defaults:

- `ADMIN_API_TOKEN` — when unset, admin-only endpoints (e.g.
  `/oracle/failed-submissions`) respond `503`; when set, they require
  `Authorization: Bearer <token>`

- `BIND_ADDR=0.0.0.0:8080`
- `STELLAR_NETWORK=testnet`
- `STELLAR_RPC_URL=https://soroban-testnet.stellar.org` on testnet
- `HORIZON_URL=https://horizon-testnet.stellar.org` on testnet
- `PRICE_LOOP_MS=1000`
- `KEEPER_LOOP_MS=1500`
- `KEEPER_INDEX=0`
- `MIN_KEEPER_BALANCE_XLM=10`
- `PRICE_FEED_CONFIG`, otherwise `config/tokens.json` is embedded

For mainnet, `STELLAR_RPC_URL` and `ORACLE_CONTRACT_ID` must be explicit.

## Smoke Check

```sh
RUST_LOG=info cargo run --bin oracle
curl http://127.0.0.1:8080/health
```

Expected health response:

```json
{"status":"ok"}
```
