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
- `STELLAR_NETWORK=testnet`
- `STELLAR_RPC_URL=https://soroban-testnet.stellar.org` on testnet
- `HORIZON_URL=https://horizon-testnet.stellar.org` on testnet
- `PRICE_LOOP_MS=1000`
- `KEEPER_LOOP_MS=1500`
- `KEEPER_INDEX=0`
- `MIN_KEEPER_BALANCE_XLM=10`
- `SET_PRICES_TX_FEE=1000000` — inclusion fee (stroops) for set_prices transactions
- `KEEPER_TX_FEE=2000000` — inclusion fee (stroops) for keeper execution transactions
- `PRICE_FEED_CONFIG`, otherwise `config/tokens.json` is embedded
- `RUST_LOG=info` — tracing `EnvFilter`; e.g. `oracle=debug,tower_http=debug`
  turns on the balance and health-probe `debug!` lines
- `LOG_FORMAT=json` — set `pretty` for human-readable output when running locally

For mainnet, `STELLAR_RPC_URL` and `ORACLE_CONTRACT_ID` must be explicit.

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

## Log Events

Log lines that alerts or dashboards key on carry a stable `event` field, so
they can be queried by type instead of by matching the human-readable
`message` (#1456). These names are a documented interface: renaming one is a
breaking change for whoever alerts on it.

| `event`                    | Level | Meaning                                                    |
| -------------------------- | ----- | ---------------------------------------------------------- |
| `price_cycle_complete`     | info  | A price cycle finished (`tokens_ok`/`tokens_failed`/...)   |
| `price_cycle_timeout`      | error | A price cycle exceeded its time budget                     |
| `price_cycle_aborted`      | error | Ledger sequence fetch failed; the cycle did not run        |
| `keeper_cycle_complete`    | info  | A keeper cycle finished, with per-kind execution counts    |
| `keeper_cycle_failed`      | error | A keeper cycle returned an error                           |
| `keeper_balance_low`       | error | Keeper spendable balance dropped below the minimum         |
| `keeper_balance_still_low` | debug | Balance is still below the minimum (repeat check)          |
| `keeper_balance_recovered` | info  | Balance went back above the minimum                        |
| `admin_auth_failure`       | warn  | An admin route was called with a missing or wrong token    |
| `retry_attempt_failed`     | warn  | One attempt inside `retry_with_backoff` failed             |
