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
  `Authorization: Bearer <token>`. Must be at least 32 characters (the
  `.env.example` placeholder is rejected at startup); repeated failed attempts
  from one client are throttled with `429`
- `PYTH_API_KEY` — optional API key for Pyth price feeds

- `BIND_ADDR=0.0.0.0:8080`
- `STELLAR_NETWORK=testnet`: `testnet`, `mainnet`, or `custom` (local/standalone)
- `STELLAR_RPC_URL=https://soroban-testnet.stellar.org` on testnet
- `HORIZON_URL=https://horizon-testnet.stellar.org` on testnet
- `NETWORK_PASSPHRASE=Standalone Network ; February 2017`: required for
  `custom` networks (e.g. `stellar/quickstart --local`); not read on
  testnet/mainnet, where the passphrase is fixed
- `PRICE_LOOP_MS=1000`
- `KEEPER_LOOP_MS=1500`
- `KEEPER_INDEX=0`
- `MIN_KEEPER_BALANCE_XLM=10`
- `SET_PRICES_TX_FEE=1000000` — inclusion fee (stroops) for set_prices transactions
- `KEEPER_TX_FEE=2000000` — inclusion fee (stroops) for keeper execution transactions
- `PRICE_FEED_CONFIG`, otherwise `config/tokens.json` is embedded

For mainnet, `STELLAR_RPC_URL` and `ORACLE_CONTRACT_ID` must be explicit.

For a local/standalone or private network, set `STELLAR_NETWORK=custom` plus
`NETWORK_PASSPHRASE` (your network's passphrase) and `STELLAR_RPC_URL` (e.g.
`http://127.0.0.1:8000/soroban/rpc` for `stellar/quickstart --local`);
`HORIZON_URL` is optional.

## Smoke Check

```sh
RUST_LOG=info cargo run --bin oracle
curl http://127.0.0.1:8080/health
```

Expected health response (fields added in #497; optional fields are omitted
until the first price/keeper cycle completes):

```json
{
  "status": "ok",
  "last_price_cycle_secs_ago": 3,
  "last_keeper_cycle_secs_ago": 5,
  "price_cycle_count": 12,
  "keeper_cycle_count": 8,
  "token_fetch_failures": 0,
  "submit_failures": 0
}
```

`last_price_cycle_secs_ago` and `last_keeper_cycle_secs_ago` are absent
until the first cycle of each type completes. `status` is always `"ok"` on
the `/health` path; use `/ready` for a liveness check that returns `503`
while the initial price fetch is still pending.
