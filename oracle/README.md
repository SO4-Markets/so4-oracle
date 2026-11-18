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

For mainnet, `STELLAR_RPC_URL` and `ORACLE_CONTRACT_ID` must be explicit.

## Endpoints

The router (`build_router` in `oracle/src/api/mod.rs`) serves exactly these
routes. "Admin" rows require `Authorization: Bearer <ADMIN_API_TOKEN>`; when
the token is unset they respond `503`, and repeated bad tokens from one client
are throttled with `429`. Query parameters and response shapes:

| Endpoint                     | Method | Auth   | Query parameters                          | Response                                                                 |
| ---------------------------- | ------ | ------ | ----------------------------------------- | ------------------------------------------------------------------------ |
| `/health`                    | GET    | public | —                                         | `{"status":"ok","version":"<semver>"}`                                    |
| `/ready`                     | GET    | public | —                                         | same body as `/health`; `503` until the first price fetch succeeds        |
| `/prices`                    | GET    | public | —                                         | array of `CachedPrice` objects from the in-memory cache; `503 {"error":"no_prices"}` before the first cycle |
| `/oracle/status`             | GET    | admin  | —                                         | `OracleStatusResponse`: last cycle timings and per-token price state      |
| `/keeper/status`             | GET    | admin  | —                                         | `KeeperStatusResponse`: pending orders/deposits/withdrawals, recent executions, cycle timing |
| `/keeper/balance`            | GET    | admin  | —                                         | `{"account_id":"G…","balance_stroops":<i64>,"balance_xlm":<f64>,"min_balance_xlm":<f64>,"is_funded":<bool>}` |
| `/keeper/blacklist/{key}`    | DELETE | admin  | —                                         | `{"key":"<key>","cleared":true}`                                          |
| `/oracle/failed-submissions` | GET    | admin  | `operation=<prefix>`, `limit=<n>`         | `{"failures":[FailedSubmission],"total_count":<n>}`                       |
| `/metrics`                   | GET    | admin  | —                                         | Prometheus text exposition                                                |

`/oracle/failed-submissions` returns the most recent failures first,
filtered by operation-name prefix when `?operation=` is given, at most
`?limit=` entries (default 100, capped at 256).

The `readme_endpoint_table_covers_every_route` test in `oracle/src/api/mod.rs`
parses this table and fails if any route registered in `build_router` is
missing from it.

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
