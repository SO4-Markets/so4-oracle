# Environment variables

**This file is the single reference for every environment variable the
service reads** (#1432). `README.md`, `oracle/README.md` and `CONTRIBUTING.md`
link here instead of keeping their own copies; `.env.example` is a sample
`.env`, not a reference.

Names are read verbatim — `Config::from_lookup` in `oracle/src/config.rs`,
plus the logging and preflight switches in `oracle/src/main.rs`. A near-miss
such as `ORDER_HANDLER_CONTRACT_ID` is silently ignored, and the process then
exits with a "required env var not set" error (#499).

CI enforces this table: `scripts/check-env-docs.sh` fails if a variable the
code reads is missing here, or if a row here names a variable the code no
longer reads. Add or remove a row in the same PR that changes `config.rs` or
`main.rs`.

Required column: **yes** = startup fails without it; **mainnet** = required
only when `STELLAR_NETWORK=mainnet`; blank = optional.

## Network

| Variable          | Required | Default                                  | Description                                                                                  |
| ----------------- | -------- | ---------------------------------------- | -------------------------------------------------------------------------------------------- |
| `STELLAR_NETWORK` |          | `testnet`                                | `testnet` or `mainnet`. Selects the network passphrase; `NETWORK_PASSPHRASE` is not read.    |
| `STELLAR_RPC_URL` | mainnet  | public testnet Soroban RPC               | Soroban RPC endpoint.                                                                        |
| `HORIZON_URL`     |          | `https://horizon-testnet.stellar.org` / `https://horizon.stellar.org` | Horizon endpoint (testnet / mainnet default).                     |
| `BIND_ADDR`       |          | `0.0.0.0:8080`                           | Address and port the HTTP server binds to.                                                   |

## Contract IDs

| Variable             | Required | Default | Description                                                                  |
| -------------------- | -------- | ------- | ---------------------------------------------------------------------------- |
| `ORACLE_CONTRACT_ID` | yes      |         | Oracle contract address.                                                     |
| `ORACLE`             |          |         | Testnet-only alias for `ORACLE_CONTRACT_ID`; mainnet requires the full name. |
| `ROLE_STORE`         | yes      |         | Role-store contract address.                                                 |
| `DATA_STORE`         | yes      |         | Data-store contract address.                                                 |
| `ORDER_HANDLER`      | yes      |         | Order-handler contract address.                                              |
| `DEPOSIT_HANDLER`    | yes      |         | Deposit-handler contract address.                                            |
| `WITHDRAWAL_HANDLER` | yes      |         | Withdrawal-handler contract address.                                         |
| `READER`             | yes      |         | Reader contract address.                                                     |

Startup rejects the same contract ID in two of these variables, naming both.

## Keeper

| Variable                 | Required | Default   | Description                                                                                            |
| ------------------------ | -------- | --------- | ------------------------------------------------------------------------------------------------------ |
| `KEEPER_PRIVATE_KEY`     | yes      |           | Hex-encoded 32-byte ed25519 seed used to sign prices. Secret — never put it in `fly.toml`/`railway.json`. |
| `KEEPER_SECRET_KEY`      | yes      |           | `S…` strkey secret seed used to sign transactions. Secret.                                             |
| `KEEPER_ACCOUNT_ID`      | yes      |           | `G…` public account ID of the keeper.                                                                  |
| `KEEPER_INDEX`           |          | `0`       | `data_store` slot holding this keeper's public key, written into every signed price; 0–255. No work partitioning. |
| `MIN_KEEPER_BALANCE_XLM` |          | `10.0`    | Minimum spendable XLM before the keeper stops submitting. Finite and non-negative.                     |
| `SET_PRICES_TX_FEE`      |          | `1000000` | Inclusion fee (stroops) for `set_prices` transactions.                                                 |
| `KEEPER_TX_FEE`          |          | `2000000` | Inclusion fee (stroops) for keeper handler transactions.                                               |
| `KEEPER_INDEX`           |          | `0`       | Labels which keeper signed a price; no work partitioning (the service is single-instance only).        |
| `MIN_KEEPER_BALANCE_XLM` |          | `10.0`    | Minimum spendable XLM before the keeper stops submitting. Finite and non-negative; `0` disables the balance gate and logs a startup warning (#1462). |
| `SET_PRICES_TX_FEE`      |          | `1000000` | Inclusion fee (stroops) for `set_prices` transactions. 1 to 10000000 (1 XLM); above 2x the default logs a startup warning (#1463). |
| `KEEPER_TX_FEE`          |          | `2000000` | Inclusion fee (stroops) for keeper handler transactions. 1 to 10000000 (1 XLM); above 2x the default logs a startup warning (#1463). |

## API and price feed

| Variable            | Required | Default                       | Description                                                                                                           |
| ------------------- | -------- | ----------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `ADMIN_API_TOKEN`   |          | unset (admin routes `503`)    | Bearer token for admin routes. At least 32 characters; the `.env.example` placeholder is rejected. Secret.           |
| `PYTH_API_KEY`      |          | unset                         | API key for the production Hermes (Pyth) endpoint. Secret. At least 16 chars, no whitespace; a startup warning names pyth tokens when unset. |
| `COINBASE_BASE_URL` |          | Coinbase exchange-rates URL   | Coinbase base URL. Override in tests to point a real `AppState` at a mock server (#1264). |
| `BINANCE_BASE_URL`  |          | Binance ticker-price URL      | Binance base URL. Override in tests to point a real `AppState` at a mock server (#1264). |
| `PYTH_HERMES_URL`   |          | Hermes latest-price-feeds URL | Pyth Hermes base URL. Override in tests to point a real `AppState` at a mock server (#1264). |
| `PRICE_FEED_CONFIG` |          | embedded `config/tokens.json` | Inline JSON array of `TokenConfig` entries — not a file path. Use `PRICE_FEED_CONFIG="$(cat tokens.json)"` for a file. |
| `PRICE_LOOP_MS`     |          | `1000`                        | Interval between price cycles (ms), 250–60000, and shorter than every token's `stale_after_seconds`.                    |
| `KEEPER_LOOP_MS`    |          | `1500`                        | Interval between keeper cycles (ms), 250–60000.                                                                       |

### `TokenConfig` fields (`PRICE_FEED_CONFIG` / `config/tokens.json`)

Each entry is a `TokenConfig` from `shared/config` (#1461). Unknown keys are
rejected (`deny_unknown_fields`), and every range below is enforced at startup
by `shared_config::parse_token_configs` or `oracle::config::parse_price_feed_config`.

| Field                     | Unit             | Default | Valid range                       | Notes                                                                                                                                                                  |
| ------------------------- | ---------------- | ------- | --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `symbol`                  | —                | —       | non-empty, unique (any case)      | Canonical key, e.g. `TWBTC`.                                                                                                                                           |
| `display_symbol`          | —                | `symbol`| optional                          | Market symbol shown by the API, e.g. `BTC`.                                                                                                                            |
| `stellar_address`         | `C…` strkey      | —       | unique (any case); required by the oracle | Token contract the price is signed for.                                                                                                                       |
| `sources`                 | —                | `[]`    | `binance`, `coinbase`, `pyth`, `fixed`; no duplicates; at least `min_sources` entries | Each source needs its symbol/feed field below (or `fixed_price`).                                      |
| `binance_symbol`          | —                | —       | `[A-Za-z0-9_-]+`, unique across tokens | e.g. `BTCUSDT`.                                                                                                                                                   |
| `coinbase_symbol`         | —                | —       | `[A-Za-z0-9_-]+`, unique across tokens | Base currency, e.g. `BTC`.                                                                                                                                        |
| `pyth_feed_id`            | hex feed id      | —       | unique across tokens              | Hermes price feed id.                                                                                                                                                  |
| `fixed_price`             | 1e30 fixed point | —       | positive decimal integer string   | Used by the `fixed` source only. `1000000000000000000000000000000` is 1.0.                                                                                             |
| `min_sources`             | count            | `2`     | 1 to `sources.len()`              | Sources that must survive fetching and outlier filtering. Set it to more than half of `sources`, or a single source can set the price with no cross-check.            |
| `max_deviation_bps`       | basis points     | `100`   | 1 to 10000                        | Outlier cut-off from the median, and the min/max band width. Also the cluster tolerance, floored at 500 bps (`DEFAULT_CLUSTER_TOLERANCE_BPS`), so values below 500 do not tighten clustering. |
| `stale_after_seconds`     | seconds          | `60`    | at least 1                        | A cached price older than this is not submitted. Must comfortably exceed `PRICE_LOOP_MS`, or every price goes stale between cycles.                                    |
| `submit_threshold_bps`    | basis points     | `10`    | 0 to 10000                        | Minimum movement before an on-chain `set_prices`; `0` submits every cycle (highest fee spend).                                                                         |
| `pyth_max_confidence_bps` | basis points     | `50`    | 0 to 10000                        | Widest Pyth confidence interval accepted, relative to the price.                                                                                                       |

## Runtime

| Variable         | Required | Default | Description                                                                                         |
| ---------------- | -------- | ------- | --------------------------------------------------------------------------------------------------- |
| `RUST_LOG`       |          | `info`  | tracing `EnvFilter`, e.g. `oracle=debug,tower_http=debug`. An unparseable value falls back to `info`. |
| `LOG_FORMAT`     |          | `json`  | `json` for log pipelines; `pretty` for human-readable local output.                                 |
| `SKIP_PREFLIGHT` |          | unset   | `true` skips the startup preflight checks. For local debugging only.                                |
