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
| `KEEPER_INDEX`           |          | `0`       | Labels which keeper signed a price; no work partitioning (the service is single-instance only).        |
| `MIN_KEEPER_BALANCE_XLM` |          | `10.0`    | Minimum spendable XLM before the keeper stops submitting. Finite and non-negative.                     |
| `SET_PRICES_TX_FEE`      |          | `1000000` | Inclusion fee (stroops) for `set_prices` transactions.                                                 |
| `KEEPER_TX_FEE`          |          | `2000000` | Inclusion fee (stroops) for keeper handler transactions.                                               |

## API and price feed

| Variable            | Required | Default                       | Description                                                                                                           |
| ------------------- | -------- | ----------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `ADMIN_API_TOKEN`   |          | unset (admin routes `503`)    | Bearer token for admin routes. At least 32 characters; the `.env.example` placeholder is rejected. Secret.           |
| `PYTH_API_KEY`      |          | unset                         | API key for the production Hermes (Pyth) endpoint. Secret.                                                            |
| `PRICE_FEED_CONFIG` |          | embedded `config/tokens.json` | Inline JSON array of `TokenConfig` entries — not a file path. Use `PRICE_FEED_CONFIG="$(cat tokens.json)"` for a file. |
| `PRICE_LOOP_MS`     |          | `1000`                        | Interval between price cycles (ms).                                                                                   |
| `KEEPER_LOOP_MS`    |          | `1500`                        | Interval between keeper cycles (ms).                                                                                  |

## Runtime

| Variable         | Required | Default | Description                                                                                         |
| ---------------- | -------- | ------- | --------------------------------------------------------------------------------------------------- |
| `RUST_LOG`       |          | `info`  | tracing `EnvFilter`, e.g. `oracle=debug,tower_http=debug`. An unparseable value falls back to `info`. |
| `LOG_FORMAT`     |          | `json`  | `json` for log pipelines; `pretty` for human-readable local output.                                 |
| `SKIP_PREFLIGHT` |          | unset   | `true` skips the startup preflight checks. For local debugging only.                                |
