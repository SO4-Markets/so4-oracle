# SO4 Oracle Runtime

The oracle now runs as a long-lived native `tokio` binary:

```sh
cargo run --bin oracle
```

Configuration is loaded once at boot via `dotenvy::dotenv()` and process
environment variables. Missing required variables abort startup with the exact
variable name.

## Required Environment

Every variable, its default and whether it is required is listed in
[docs/ENVIRONMENT.md](../docs/ENVIRONMENT.md), the single reference (#1432).
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
| `/oracle/failed-submissions` | GET    | admin  | `operation=<prefix>`, `limit=<n>`         | `{"failures":[FailedSubmission],"matched_count":<n>,"total_count":<n>}`   |
| `/metrics`                   | GET    | admin  | —                                         | Prometheus text exposition                                                |
| `/unmatched`                 | *fallback* | public | —                                     | internal `MatchedPath` fallback label for unmatched paths; never a real endpoint |
| `/unmatched`                 | *      | public | —                                         | internal fallback route used to normalise `404`/`405` into the JSON error envelope |

`/oracle/failed-submissions` returns the most recent failures first,
filtered by operation-name prefix when `?operation=` is given, at most
`?limit=` entries (default 100, capped at 256). `matched_count` is the number
matching the prefix before the limit; `total_count` is the whole ring buffer.
Failures are tagged `"kind": "price"` or `"kind": "keeper"` — keeper records
carry an optional `key` and never emit the price-only `token`/`symbol`/`min`/`max`
fields.

The `readme_endpoint_table_covers_every_route` test in `oracle/src/api/mod.rs`
parses this table and fails if any route registered in `build_router` is
missing from it.

### Timeouts and limits

Request deadlines nest, innermost first (#1452). Platform health-check
timeouts (`fly.toml`, `railway.json`) should sit above the probe's own budget
and at or below the global timeout, so the handler's answer is the one the
platform sees:

| Scope                          | Budget | Where                                                                                                   |
| ------------------------------ | ------ | ------------------------------------------------------------------------------------------------------- |
| `/ready` external checks       | 15s    | `READY_CHECK_TIMEOUT_SECS` (`api/prices.rs`)                                                            |
| Admin Horizon lookups, retries | 20s    | `ADMIN_EXTERNAL_BUDGET` (`api/admin.rs`); `/keeper/balance` answers `504 {"error":"upstream_timeout"}` |
| Every request                  | 30s    | `REQUEST_TIMEOUT` (`api/mod.rs`); `408` when exceeded                                                   |

The global timeout is checked at compile time to stay above both budgets.
`/health` and `/ready` sit outside the concurrency limit (512 in-flight
requests) and are not recorded in the HTTP request metrics, so a load spike or
a probe flood cannot take the liveness probe down or contend on the metrics
lock (#1453, #1454).

Every response carries an `x-request-id`, and the outbound RPC, Horizon and
price-source calls made while serving it send the same id. Background price
and keeper cycles generate their own `cycle_id`, record it on every log line
of the cycle, and send that instead (#1455).

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
