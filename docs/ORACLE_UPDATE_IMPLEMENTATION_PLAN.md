# SO4 Oracle Update Implementation Plan

Date: 2026-06-05

This guide is the implementation handoff for updating `/home/sunny/zero/so4-market-project/so4-oracle`.
It captures the current state, all important contract and binding paths, the live testnet addresses, and the concrete work needed to turn the oracle from an old/stale price-cache worker into a production-ready Soroban keeper.

## Goal

Update the SO4 oracle worker so it can:

- fetch fresh prices for the current deployed testnet tokens,
- aggregate and validate those prices safely,
- expose reliable frontend-readable cached prices,
- submit fresh prices to the deployed Soroban `oracle` contract when required,
- keep deployment config, bindings, docs, and contract addresses aligned with the rest of the SO4 workspace.

## Current Summary

The oracle repo is a Rust Cloudflare Worker workspace:

```text
/home/sunny/zero/so4-market-project/so4-oracle
├── Cargo.toml
├── Cargo.lock
├── Makefile
├── README.md
├── wrangler.toml
├── config/tokens.json
├── docs/RISK_MECHANICS.md
├── oracle/
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── binance.rs
│   │   ├── coinbase.rs
│   │   ├── config.rs
│   │   ├── keeper.rs
│   │   ├── kv_store.rs
│   │   ├── log.rs
│   │   ├── network_config.rs
│   │   ├── prices.rs
│   │   ├── pyth.rs
│   │   ├── retry.rs
│   │   ├── signing.rs
│   │   ├── stellar_rpc.rs
│   │   └── submit.rs
│   └── tests/mock_rpc_integration.rs
└── shared/config/
```

The current tests pass:

```bash
cd /home/sunny/zero/so4-market-project/so4-oracle
CARGO_TARGET_DIR=/tmp/so4-oracle-target cargo test --workspace
```

Result observed on 2026-06-05:

```text
96 passed, 0 failed
```

The big issue is not compilation. The big issue is wiring and freshness:

- `wrangler.toml` and `README.md` still reference old deployed contract IDs.
- `config/tokens.json` uses placeholder BTC/ETH token addresses.
- The scheduled worker currently caches signed price payloads in KV, but does not build and submit an on-chain Soroban transaction.
- `oracle/src/submit.rs` can submit an already-built signed XDR, but no current code path builds the `Oracle.set_prices` transaction XDR.

## Live Testnet Addresses

Source of truth:

```text
/home/sunny/zero/so4-market-project/contracts/.deployed/testnet.env
/home/sunny/zero/so4-market-project/contracts/.deployed/frontend-testnet.env
/home/sunny/zero/so4-market-project/contracts/.deployed/frontend-testnet.ts
/home/sunny/zero/so4-market-project/contracts/.deployed/tokens-testnet.env
```

Current testnet network:

```text
NETWORK=testnet
RPC_URL=https://soroban-testnet.stellar.org
NETWORK_PASSPHRASE=Test SDF Network ; September 2015
ADMIN=GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI
```

Core protocol contracts:

```text
ROLE_STORE=CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q
DATA_STORE=CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J
ORACLE=CBABE5O7QJMXT2I42KHUV7ESNER3Z2BGJCF2QRKWMKVTCBEYFQNHV3J6
MARKET_FACTORY=CBGX3EJFI3JRHSN5B533O2L5P57JFPTCRS55IPWFS5BNDXLJLXDWA5Z2
DEPOSIT_VAULT=CDEB4XGIXSWUUGBBIAZQPOGBALY6OEEOYH2C3UTNDLD6IN4NVOCFZKYY
DEPOSIT_HANDLER=CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C
WITHDRAWAL_VAULT=CDUXAMIG2QCINRO4F36XL3DYLSG2WXZHJGBYNEJJUUXKHCIM2YGDG7MH
WITHDRAWAL_HANDLER=CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE
ORDER_VAULT=CAXZRTYJHTAEEHDJ5WH4MVS2BGRCNNXLJ7BH356RULEZGEV2RKAG3QON
ORDER_HANDLER=CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY
LIQUIDATION_HANDLER=CBXUAR5GCHIRFQL75WTZS3FLA6SMWDPIKG4EKNPWVQVNGVFXBHGTJHTM
ADL_HANDLER=CACFPG3QAKG6DCAJSOP7YGDTM44NV6NPI3SKAG7GUGIV6DMGXPCAMMME
FEE_HANDLER=CC4P3FJ7EAH6F3RYJPQ2T7VIB4I7UJ4EEYGVWTZVXTAUN647QRVSDHS4
REFERRAL_STORAGE=CDHTPQO4RRJ6OUBIW3GDXTIVLVOMIKPJC65PGDJH2G5OLDJRE5KTROWK
READER=CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC
EXCHANGE_ROUTER=CBD6BQSQFROWIIT5QCYN7KL5LJJWUIH7CEWUSZIFMUJO6NPXE6CVGYNW
```

Test faucet and tokens:

```text
FAUCET=CCWXXBKXHHP5DXC6TYVIL22XUNHD5A75O6WM5D2KM5PY45IOV5VDMARJ
TUSDC=CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES
TWBTC=CCFTOPHUPSUDO2MB4X5D3XYJ2HRJ7NJPAW4UVPAVN7ZLE63EZLSMXDUO
TETH=CAJ6BZKGFT47ALGMVFZZGAOXBV2RWIVYVCU4WJCQIURKRNXU346RWVAU
TXLM=CAHNXBBSXVMGI6G3FUBY3OTNWKQ7434FDDEEE7ZT733WIW6NUZL4ONU6
```

Markets:

```text
MARKET_TOKEN_TWBTC_TUSDC=CDDVSLBGGDV2UOFN5W72R4LW7ABYL7H7ZWVSFHGMXXB3D52ZYANC5G3L
MARKET_TOKEN_TETH_TUSDC=CCBUUSYZJTGVA6PYUNQDFPZFHTBZ2QSHOUO7YAGRQVA46T3ZLSIYULS4
MARKET_TOKEN_TXLM_TUSDC=CDIBR7BDCDWGAG3CC6PBKRSLMISPYKNDGE57DCZO5TMTLZK34TMGKFQQ
```

## Contract Source Paths

Primary oracle integration:

```text
/home/sunny/zero/so4-market-project/contracts/contracts/oracle/src/lib.rs
```

The on-chain oracle exposes:

```rust
pub fn set_prices(env: Env, caller: Address, prices: Vec<SignedPrice>)
pub fn get_primary_price(env: Env, token: Address) -> PriceProps
pub fn try_get_price(env: Env, token: Address) -> Option<PriceProps>
pub fn get_stable_price(env: Env, token: Address) -> Option<i128>
pub fn get_price_with_stable_fallback(env: Env, token: Address) -> PriceProps
pub fn clear_price(env: Env, caller: Address, token: Address)
pub fn clear_prices(env: Env, caller: Address, tokens: Vec<Address>)
```

Important struct:

```rust
pub struct SignedPrice {
    pub token: Address,
    pub min_price: i128,
    pub max_price: i128,
    pub timestamp: u64,
    pub signature: BytesN<64>,
    pub keeper_index: u32,
}
```

The caller must:

- sign/authenticate as a Stellar account,
- have `ORDER_KEEPER` role in `ROLE_STORE`,
- submit keeper signatures that match registered keeper public keys stored in `DATA_STORE`.

Related contracts:

```text
/home/sunny/zero/so4-market-project/contracts/contracts/role_store/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/data_store/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/exchange_router/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/deposit_handler/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/withdrawal_handler/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/order_handler/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/liquidation_handler/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/adl_handler/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/fee_handler/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/test_token/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/contracts/test_faucet/src/lib.rs
```

Shared protocol libraries:

```text
/home/sunny/zero/so4-market-project/contracts/libs/types/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/libs/keys/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/libs/market_utils/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/libs/pricing_utils/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/libs/position_utils/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/libs/increase_position_utils/src/lib.rs
/home/sunny/zero/so4-market-project/contracts/libs/decrease_position_utils/src/lib.rs
```

Deployment and configuration scripts:

```text
/home/sunny/zero/so4-market-project/contracts/scripts/deploy.sh
/home/sunny/zero/so4-market-project/contracts/scripts/bootstrap.sh
/home/sunny/zero/so4-market-project/contracts/scripts/configure_market.sh
/home/sunny/zero/so4-market-project/contracts/scripts/export_frontend_config.sh
/home/sunny/zero/so4-market-project/contracts/scripts/submit_prices.sh
/home/sunny/zero/so4-market-project/contracts/scripts/compute_key.py
/home/sunny/zero/so4-market-project/contracts/mx/deploy.mk
/home/sunny/zero/so4-market-project/contracts/mx/tokens.mk
/home/sunny/zero/so4-market-project/contracts/mx/upgrade.mk
```

## Frontend Binding and SDK Paths

Current external frontend repo:

```text
/home/sunny/zero/so4-market-project/interface
```

Main frontend SDK package:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts
```

Generated TypeScript bindings currently committed:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/exchange-router/src/index.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/synthetics-reader/src/index.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/glv-router/src/index.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/test-faucet/src/index.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/test-token/src/index.ts
```

SDK client wrappers:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/exchange-router.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/synthetics-reader.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/glv-router.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/test-faucet equivalent via generated binding
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/token.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/sac-token.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/order-vault.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/referral-storage.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/index.ts
```

Frontend app contract adapter:

```text
/home/sunny/zero/so4-market-project/interface/apps/web/src/lib/contracts.ts
/home/sunny/zero/so4-market-project/interface/apps/web/src/app/config/contracts.ts
/home/sunny/zero/so4-market-project/interface/apps/web/src/app/config/network.ts
/home/sunny/zero/so4-market-project/interface/apps/web/src/app/config/env.ts
```

Frontend Soroban helpers that stay app-local:

```text
/home/sunny/zero/so4-market-project/interface/apps/web/src/lib/soroban/client.ts
/home/sunny/zero/so4-market-project/interface/apps/web/src/lib/soroban/tx-builder.ts
/home/sunny/zero/so4-market-project/interface/apps/web/src/lib/soroban/simulate.ts
```

Binding generation script:

```text
/home/sunny/zero/so4-market-project/interface/scripts/generate-bindings.ts
```

Frontend docs:

```text
/home/sunny/zero/so4-market-project/interface/docs/contracts.md
/home/sunny/zero/so4-market-project/interface/docs/contracts-sdk-migration.md
/home/sunny/zero/so4-market-project/interface/docs/pool_fauce_implementations_plans_guides.md
```

Note: `so4-oracle/interface` appears to be a nested copy of the frontend repository. Treat it as stale unless the team explicitly wants the oracle repo to vendor a frontend. The canonical frontend path is:

```text
/home/sunny/zero/so4-market-project/interface
```

## Oracle Worker Source Paths

Entry points:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/lib.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/submit.rs
```

Price sources:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/binance.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/coinbase.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/pyth.rs
```

Aggregation and risk:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/prices.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/retry.rs
```

Config and runtime:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/config.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/network_config.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/keeper.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/kv_store.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/stellar_rpc.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/signing.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/log.rs
```

Shared config crate:

```text
/home/sunny/zero/so4-market-project/so4-oracle/shared/config/src/lib.rs
```

Tests:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/tests/mock_rpc_integration.rs
```

## Current Worker HTTP Routes

Defined in `oracle/src/lib.rs`:

```text
GET /                         -> "so4-oracle"
GET /prices                   -> cached price payloads from KV
GET /oracle/status            -> oracle status from KV
GET /oracle/failed-submissions -> failed submission records from KV
GET /keeper/balance           -> keeper XLM balance
```

Recommendation:

- Keep `/prices` public because the frontend can use it.
- Protect `/oracle/status`, `/oracle/failed-submissions`, and `/keeper/balance` with an admin token.
- Add `GET /health` for deployment checks.

## Current Problems To Fix

### 1. Stale contract IDs

Current stale value in `so4-oracle/wrangler.toml`:

```text
ORACLE_CONTRACT_ID = "CAH5Z3RD6UMR6RIDXT4ZGOC5SMDCQRA2T3FO4FJSOYZGQPWS77ZGTXUO"
```

Replace with:

```text
ORACLE_CONTRACT_ID = "CBABE5O7QJMXT2I42KHUV7ESNER3Z2BGJCF2QRKWMKVTCBEYFQNHV3J6"
```

Also update stale references in:

```text
/home/sunny/zero/so4-market-project/so4-oracle/README.md
```

### 2. Placeholder token config

Current `config/tokens.json` uses fake addresses like:

```text
CBTCADDRREPLACE...
CETHADDRREPLACE...
```

Replace with the live testnet token addresses listed above.

### 3. Symbol mismatch

On-chain test tokens are named `TWBTC`, `TETH`, `TXLM`, `TUSDC`, but external price sources use real market symbols:

```text
TWBTC -> BTC
TETH  -> ETH
TXLM  -> XLM
TUSDC -> USDC or fixed stable price
```

Do not derive source symbols directly from `token.symbol`. Add explicit per-source symbols.

### 4. No on-chain price submission

Current `scheduled` flow in `oracle/src/lib.rs`:

1. load config,
2. check keeper balance,
3. fetch latest ledger,
4. fetch and aggregate prices,
5. sign payload,
6. cache payload in KV.

Missing:

1. build `SignedPrice` values for the Soroban oracle contract,
2. build a transaction calling `Oracle.set_prices(caller, prices)`,
3. sign the transaction as the keeper account,
4. submit via `sendTransaction`,
5. poll via `getTransaction`,
6. store success/failure per token and transaction hash.

### 5. Polling bug

`oracle/src/submit.rs` calculates exponential backoff in `poll_until_confirmed`, but does not sleep between polls.

Fix by using the worker delay helper already used in `retry.rs`:

```rust
worker::Delay::from(std::time::Duration::from_millis(backoff_ms)).await;
```

Apply it before incrementing/capping the next backoff.

### 6. Price math edge cases

`oracle/src/prices.rs` currently has a suspicious two-source median behavior. For two prices, median should normally be the average of the two values, not the upper element.

Fix:

- one price: allow only if the token config explicitly permits one source,
- two prices: median = average, min/max = median +/- configured spread,
- three or more prices: use percentile or robust aggregation after outlier filtering.

### 7. Pyth validation is incomplete

`oracle/src/pyth.rs` normalizes price, but should also validate:

- publish time,
- staleness threshold,
- confidence interval,
- price exponent bounds,
- price and confidence overflow.

### 8. KV keys are too broad

Current KV keys use generic keys like:

```text
oracle:status
oracle:last-price:<symbol>
oracle:cached-prices
oracle:failed-submissions
```

Use network and token scoped keys:

```text
oracle:testnet:status
oracle:testnet:last-price:<token-address>
oracle:testnet:cached-prices
oracle:testnet:failed-submissions:<timestamp>:<token-address>
```

### 9. Failed submission records lack token identity

`FailedSubmission` should include:

```rust
network: String
token: String
symbol: String
min: i128
max: i128
tx_hash: Option<String>
error: String
timestamp: u64
sources_used: Vec<String>
ledger_seq: Option<u32>
```

### 10. Operational endpoints need auth

Add an `ADMIN_API_TOKEN` secret. Require:

```text
Authorization: Bearer <ADMIN_API_TOKEN>
```

for:

```text
/oracle/status
/oracle/failed-submissions
/keeper/balance
```

## Recommended Token Feed Config

The shared config should support explicit feed symbols:

```json
[
  {
    "symbol": "TUSDC",
    "display_symbol": "USDC",
    "stellar_address": "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
    "sources": ["fixed"],
    "fixed_price": "1000000000000000000000000000000",
    "min_sources": 1,
    "max_deviation_bps": 50,
    "stale_after_seconds": 60,
    "submit_threshold_bps": 5
  },
  {
    "symbol": "TWBTC",
    "display_symbol": "BTC",
    "stellar_address": "CCFTOPHUPSUDO2MB4X5D3XYJ2HRJ7NJPAW4UVPAVN7ZLE63EZLSMXDUO",
    "sources": ["binance", "coinbase", "pyth"],
    "binance_symbol": "BTCUSDT",
    "coinbase_symbol": "BTC",
    "pyth_feed_id": "<BTC_USD_PRICE_FEED_ID>",
    "min_sources": 2,
    "max_deviation_bps": 100,
    "stale_after_seconds": 60,
    "submit_threshold_bps": 10
  },
  {
    "symbol": "TETH",
    "display_symbol": "ETH",
    "stellar_address": "CAJ6BZKGFT47ALGMVFZZGAOXBV2RWIVYVCU4WJCQIURKRNXU346RWVAU",
    "sources": ["binance", "coinbase", "pyth"],
    "binance_symbol": "ETHUSDT",
    "coinbase_symbol": "ETH",
    "pyth_feed_id": "<ETH_USD_PRICE_FEED_ID>",
    "min_sources": 2,
    "max_deviation_bps": 100,
    "stale_after_seconds": 60,
    "submit_threshold_bps": 10
  },
  {
    "symbol": "TXLM",
    "display_symbol": "XLM",
    "stellar_address": "CAHNXBBSXVMGI6G3FUBY3OTNWKQ7434FDDEEE7ZT733WIW6NUZL4ONU6",
    "sources": ["binance", "coinbase", "pyth"],
    "binance_symbol": "XLMUSDT",
    "coinbase_symbol": "XLM",
    "pyth_feed_id": "<XLM_USD_PRICE_FEED_ID>",
    "min_sources": 2,
    "max_deviation_bps": 150,
    "stale_after_seconds": 60,
    "submit_threshold_bps": 10
  }
]
```

Notes:

- The exact Pyth feed IDs must be verified before implementation.
- `fixed_price` is in SO4 price precision, which is 1e30.
- For testnet `TUSDC`, a fixed 1 USD price is acceptable if the protocol intentionally treats it as a stablecoin.
- If using market feeds for USDC, configure it as `display_symbol = "USDC"` while keeping the token symbol `TUSDC`.

## Environment Variables and Secrets

Static Worker vars in `wrangler.toml`:

```text
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org
ORACLE_CONTRACT_ID=CBABE5O7QJMXT2I42KHUV7ESNER3Z2BGJCF2QRKWMKVTCBEYFQNHV3J6
PRICE_MOVEMENT_THRESHOLD=10
```

Secrets:

```text
KEEPER_PRIVATE_KEY=<hex-encoded 32-byte ed25519 signing key used for price attestations>
KEEPER_ACCOUNT_ID=<G... Stellar account ID that submits txs and has ORDER_KEEPER>
KEEPER_SECRET_KEY=<S... Stellar secret seed if tx builder signs Stellar transactions>
ADMIN_API_TOKEN=<random high-entropy token for protected routes>
PRICE_FEED_CONFIG=<JSON token config if not loaded from config/tokens.json>
```

Important separation:

- `KEEPER_PRIVATE_KEY` is currently used by `oracle/src/signing.rs` to create ed25519 price attestations.
- `KEEPER_ACCOUNT_ID` is the Stellar account address checked for balance.
- On-chain `set_prices` also requires an authenticated `caller` with `ORDER_KEEPER`, so transaction signing needs the keeper Stellar secret seed or an equivalent secure signing path.
- Do not commit real secrets. `.dev.vars` must stay local-only.

## Keeper Roles and Public Key Setup

The keeper account must be granted `ORDER_KEEPER` in `ROLE_STORE`.

Relevant contract path:

```text
/home/sunny/zero/so4-market-project/contracts/contracts/role_store/src/lib.rs
```

The on-chain oracle also verifies the ed25519 signature against a keeper public key stored in `DATA_STORE`.

Relevant oracle code:

```text
/home/sunny/zero/so4-market-project/contracts/contracts/oracle/src/lib.rs
```

Relevant key helper:

```text
/home/sunny/zero/so4-market-project/contracts/libs/keys/src/lib.rs
```

The implementation agent must verify or add a script for:

- computing the keeper public key storage key,
- writing the keeper public key to `DATA_STORE`,
- confirming `Oracle.set_prices` accepts a signed price for `keeper_index = 0`.

The script may live at:

```text
/home/sunny/zero/so4-market-project/contracts/scripts/register_oracle_keeper.sh
```

or, if this is oracle-repo-specific:

```text
/home/sunny/zero/so4-market-project/so4-oracle/scripts/register_oracle_keeper.sh
```

## Transaction Builder Plan

There are two valid implementation routes. Pick one and document the choice.

### Option A: Rust Worker builds Soroban transaction

Keep the worker Rust-native and add a transaction builder module:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/tx_builder.rs
```

Responsibilities:

- load keeper Stellar secret,
- load account sequence from Horizon or RPC,
- encode `Oracle.set_prices(caller, prices)` invoke contract operation,
- simulate transaction if needed,
- assemble transaction resources and fee,
- sign transaction,
- return base64 transaction XDR,
- pass XDR to `submit::submit_and_poll`.

Potential dependency concerns:

- Confirm every crate compiles to `wasm32-unknown-unknown`.
- Keep bundle size below Cloudflare Worker limits.
- Avoid blocking or native TLS-only dependencies in the Worker runtime.

### Option B: TypeScript Worker or helper builds XDR

Use `@stellar/stellar-sdk` or generated TS contract bindings to build and sign the transaction, while the Rust worker keeps price aggregation.

Possible approaches:

- Add a small TypeScript build step that exports a callable XDR builder.
- Split the oracle into a TS Worker and move current Rust modules only if needed.
- Keep Rust for aggregation but call a JS helper through the Worker bundle.

This is usually easier for Soroban transaction construction because the frontend already uses TypeScript bindings, but it is a larger architectural change.

### Recommended path

Start with Option A only if the implementation agent confirms the chosen Rust dependencies work in Cloudflare Worker WASM. If that becomes heavy or brittle, choose Option B and make transaction building TypeScript-first.

The current code already has:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/submit.rs
```

So the missing layer is not RPC submission. It is XDR construction and keeper account signing.

## Scheduled Flow Target

The final scheduled handler should do this:

- [ ] Load network config.
- [ ] Load token feed config.
- [ ] Check keeper account balance.
- [ ] Fetch latest ledger sequence.
- [ ] Fetch all configured token prices.
- [ ] Validate source freshness and confidence.
- [ ] Filter outliers.
- [ ] Aggregate min/max price in 1e30 precision.
- [ ] Compare against last submitted price.
- [ ] Skip tokens below submit threshold if desired.
- [ ] Build `SignedPrice` values.
- [ ] Build one `Oracle.set_prices` transaction for all accepted tokens.
- [ ] Sign the Stellar transaction as keeper account.
- [ ] Submit XDR through `sendTransaction`.
- [ ] Poll through `getTransaction` with real backoff sleeps.
- [ ] Store success status in KV.
- [ ] Store per-token failure status in KV.
- [ ] Keep `/prices` cache updated even if on-chain submission fails, but mark status clearly.

## Concrete Implementation Checklist

### Phase 0: Safety and repo hygiene

- [ ] Confirm whether `/home/sunny/zero/so4-market-project/so4-oracle/interface` is intentionally vendored.
- [ ] If not intended, remove or ignore it in future work so agents do not update the wrong frontend.
- [ ] Do not revert existing dirty worktree changes without explicit user approval.
- [ ] Keep all build artifacts out of commits.
- [ ] Keep real secrets out of git.

### Phase 1: Config refresh

- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/wrangler.toml`.
- [ ] Replace old `ORACLE_CONTRACT_ID` with `CBABE5O7QJMXT2I42KHUV7ESNER3Z2BGJCF2QRKWMKVTCBEYFQNHV3J6`.
- [ ] Replace placeholder KV IDs with real Cloudflare KV namespace IDs before deploy.
- [ ] Add `ADMIN_API_TOKEN` as a secret, not a plain var.
- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/README.md` deployed contract references.
- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/config/tokens.json` to use TUSDC/TWBTC/TETH/TXLM.

### Phase 2: Config schema upgrade

- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/shared/config/src/lib.rs`.
- [ ] Add `display_symbol`.
- [ ] Add `coinbase_symbol`.
- [ ] Add `fixed_price`.
- [ ] Add `min_sources`.
- [ ] Add `max_deviation_bps`.
- [ ] Add `stale_after_seconds`.
- [ ] Add `submit_threshold_bps`.
- [ ] Preserve backward compatibility where easy, but prefer explicit validation.
- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/config.rs`.
- [ ] Add tests for valid TUSDC/TWBTC/TETH/TXLM config.
- [ ] Add tests rejecting missing source symbols when a source requires them.

### Phase 3: Source adapters

- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/binance.rs`.
- [ ] Query targeted Binance symbols instead of fetching the full ticker list.
- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/coinbase.rs`.
- [ ] Use `coinbase_symbol` rather than `token.symbol`.
- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/pyth.rs`.
- [ ] Validate staleness and confidence.
- [ ] Add `fixed` price source support for TUSDC.
- [ ] Add source-level error reporting that includes token address and symbol.

### Phase 4: Price aggregation

- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/prices.rs`.
- [ ] Fix two-source median.
- [ ] Make one-source behavior config-driven.
- [ ] Use `max_deviation_bps` per token.
- [ ] Return structured aggregation output:

```rust
pub struct AggregatedPrice {
    pub min: i128,
    pub max: i128,
    pub median: i128,
    pub sources_used: Vec<String>,
    pub rejected_sources: Vec<RejectedSource>,
}
```

- [ ] Add tests for BTC/ETH/XLM/stable price cases.
- [ ] Add tests for outliers and minimum source requirements.

### Phase 5: KV and status improvements

- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/kv_store.rs`.
- [ ] Scope keys by network.
- [ ] Scope last price by token address.
- [ ] Include token identity in failed submissions.
- [ ] Store transaction hash and confirmed ledger when available.
- [ ] Add `last_onchain_submission_time`.
- [ ] Add `last_cache_update_time`.
- [ ] Add `onchain_status` field per token.

### Phase 6: Protected admin routes

- [ ] Add auth helper in `oracle/src/lib.rs` or new `oracle/src/auth.rs`.
- [ ] Protect `/keeper/balance`.
- [ ] Protect `/oracle/status`.
- [ ] Protect `/oracle/failed-submissions`.
- [ ] Keep `/` and `/prices` public.
- [ ] Add `GET /health` public.
- [ ] Add tests for missing/invalid auth if practical.

### Phase 7: Transaction building

- [ ] Decide Option A or Option B from the transaction builder plan.
- [ ] Implement transaction builder.
- [ ] Build `Vec<SignedPrice>` exactly matching the contract struct.
- [ ] Ensure `caller` is the keeper account with `ORDER_KEEPER`.
- [ ] Ensure `keeper_index` matches the public key registered in `DATA_STORE`.
- [ ] Ensure the signed message matches the contract's `build_price_message` exactly:

```text
network_passphrase
ledger_sequence as u32 big-endian
token contract strkey bytes
min_price as i128 big-endian
max_price as i128 big-endian
timestamp as u64 big-endian
```

- [ ] Build XDR for `Oracle.set_prices(caller, prices)`.
- [ ] Submit XDR with `/oracle/src/submit.rs`.
- [ ] Store result in KV.

### Phase 8: Fix submit polling

- [ ] Update `/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/submit.rs`.
- [ ] Add actual `worker::Delay` sleep in `poll_until_confirmed`.
- [ ] Add a testable abstraction if needed so native tests do not wait.
- [ ] Store failure details when status is `FAILED`.
- [ ] Treat non-`PENDING` send status as a structured rejected submission.

### Phase 9: Contract setup script

- [ ] Add script to register keeper public key in `DATA_STORE`.
- [ ] Add script or Makefile target to grant keeper `ORDER_KEEPER`.
- [ ] Add verification command to call `has_role`.
- [ ] Add verification command to submit one TUSDC fixed price to oracle.
- [ ] Add verification command to call `try_get_price`.

Candidate files:

```text
/home/sunny/zero/so4-market-project/contracts/scripts/register_oracle_keeper.sh
/home/sunny/zero/so4-market-project/contracts/scripts/verify_oracle_keeper.sh
/home/sunny/zero/so4-market-project/so4-oracle/Makefile
```

### Phase 10: Frontend alignment

- [ ] Confirm frontend reads `/prices` from the oracle Worker if needed.
- [ ] Confirm frontend contract IDs match `/home/sunny/zero/so4-market-project/contracts/.deployed/frontend-testnet.ts`.
- [ ] If the frontend needs an oracle binding, generate and commit one under:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/oracle/src/index.ts
```

- [ ] Add an SDK wrapper if frontend needs direct oracle reads:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/oracle.ts
```

- [ ] Export it from:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/index.ts
```

- [ ] Instantiate it in:

```text
/home/sunny/zero/so4-market-project/interface/apps/web/src/lib/contracts.ts
```

### Phase 11: Tests

- [ ] Run:

```bash
cd /home/sunny/zero/so4-market-project/so4-oracle
CARGO_TARGET_DIR=/tmp/so4-oracle-target cargo test --workspace
```

- [ ] Add unit tests for config schema.
- [ ] Add unit tests for fixed-price TUSDC.
- [ ] Add unit tests for BTC/ETH/XLM source symbol mapping.
- [ ] Add unit tests for two-source median.
- [ ] Add unit tests for Pyth staleness/confidence checks.
- [ ] Add tests for KV key naming.
- [ ] Add tests for submit response parsing and polling delay behavior.
- [ ] Add a mocked end-to-end scheduled cycle that produces a `set_prices` XDR or a mocked submit call.

### Phase 12: Deployment

- [ ] Create real Cloudflare KV namespace.
- [ ] Update KV namespace IDs in `wrangler.toml`.
- [ ] Set Worker secrets:

```bash
cd /home/sunny/zero/so4-market-project/so4-oracle
wrangler secret put KEEPER_PRIVATE_KEY
wrangler secret put KEEPER_ACCOUNT_ID
wrangler secret put KEEPER_SECRET_KEY
wrangler secret put ADMIN_API_TOKEN
wrangler secret put PRICE_FEED_CONFIG
```

- [ ] Deploy:

```bash
cd /home/sunny/zero/so4-market-project/so4-oracle
wrangler deploy
```

- [ ] Trigger scheduled locally before production:

```bash
wrangler dev --test-scheduled
```

- [ ] Check `/health`.
- [ ] Check `/prices`.
- [ ] Check protected admin routes with bearer token.
- [ ] Confirm `Oracle.try_get_price(TUSDC)` returns fresh data after a scheduled run.

## Suggested File Additions

Add:

```text
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/auth.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/tx_builder.rs
/home/sunny/zero/so4-market-project/so4-oracle/oracle/src/fixed.rs
```

Maybe add:

```text
/home/sunny/zero/so4-market-project/so4-oracle/scripts/register_oracle_keeper.sh
/home/sunny/zero/so4-market-project/so4-oracle/scripts/verify_oracle_submission.sh
```

If frontend direct oracle reads are needed, add:

```text
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/generated/oracle/src/index.ts
/home/sunny/zero/so4-market-project/interface/packages/contracts/src/clients/oracle.ts
```

## Suggested Wrangler Config Shape

The current `wrangler.toml` can stay TOML, but update it carefully:

```toml
name = "so4-oracle"
main = "oracle/build/index.js"
compatibility_date = "2026-06-05"

[build]
command = "cd oracle && worker-build --release"

[[kv_namespaces]]
binding = "ORACLE_KV"
id = "<production-kv-id>"
preview_id = "<preview-kv-id>"

[vars]
STELLAR_NETWORK = "testnet"
STELLAR_RPC_URL = "https://soroban-testnet.stellar.org"
ORACLE_CONTRACT_ID = "CBABE5O7QJMXT2I42KHUV7ESNER3Z2BGJCF2QRKWMKVTCBEYFQNHV3J6"
PRICE_MOVEMENT_THRESHOLD = "10"

[observability]
enabled = true

[triggers]
crons = ["*/1 * * * *"]
```

Do not store secrets in `[vars]`.

## Suggested `/prices` Response Shape

Keep the frontend response simple and explicit:

```json
[
  {
    "network": "testnet",
    "token": "CCFTOPHUPSUDO2MB4X5D3XYJ2HRJ7NJPAW4UVPAVN7ZLE63EZLSMXDUO",
    "symbol": "TWBTC",
    "displaySymbol": "BTC",
    "min": "68000000000000000000000000000000000",
    "max": "68010000000000000000000000000000000",
    "median": "68005000000000000000000000000000000",
    "timestamp": 1780660800,
    "sourcesUsed": ["binance", "coinbase", "pyth"],
    "onchainStatus": "submitted",
    "txHash": "<stellar-tx-hash-or-null>",
    "ledger": 123456
  }
]
```

Use strings for large `i128` values in JSON to avoid JavaScript precision loss.

## Verification Commands

Oracle repo:

```bash
cd /home/sunny/zero/so4-market-project/so4-oracle
make test
make check
```

Contracts repo:

```bash
cd /home/sunny/zero/so4-market-project/contracts
cargo test -p oracle
cargo test -p role_store
cargo test -p data_store
```

Frontend SDK:

```bash
cd /home/sunny/zero/so4-market-project/interface
bun run --cwd packages/contracts typecheck
bun run --cwd apps/web typecheck
```

Expected known note:

- The frontend may still have unrelated strict type errors from app features.
- Oracle SDK or binding changes should not introduce new errors under `packages/contracts`.

## Open Questions For Implementation Agent

- Which runtime should own Soroban transaction construction: Rust Worker or TypeScript helper?
- What are the verified Pyth feed IDs for BTC/USD, ETH/USD, XLM/USD, and USDC/USD?
- Should TUSDC be fixed at 1 USD or use a live USDC/USD feed?
- Is the nested `so4-oracle/interface` directory intentionally part of this repo?
- Should `/prices` be a frontend-only cache endpoint, or should frontend read on-chain oracle prices directly?
- Should the worker submit prices every minute, or only when movement exceeds `submit_threshold_bps`?
- Should stable prices be configured in `DATA_STORE` so on-chain `get_price_with_stable_fallback` handles TUSDC without a keeper submission?

## Priority Order

Do these first:

- [ ] Update stale contract IDs and token config.
- [ ] Fix source symbol mapping.
- [ ] Fix two-source median and one-source behavior.
- [ ] Add protected admin routes.
- [ ] Implement transaction builder and on-chain `set_prices` submission.
- [ ] Register keeper public key and role.
- [ ] Verify on-chain `try_get_price`.

Then do hardening:

- [ ] Pyth confidence and staleness.
- [ ] KV key scoping.
- [ ] Better failed submission diagnostics.
- [ ] Frontend oracle binding if direct reads are needed.
- [ ] Cleanup stale nested interface folder.

## Final Acceptance Criteria

The oracle update is complete when:

- [ ] `wrangler.toml` points to the current testnet oracle contract.
- [ ] `config/tokens.json` contains TUSDC/TWBTC/TETH/TXLM live addresses.
- [ ] The Worker can fetch and aggregate all configured token prices.
- [ ] `/prices` returns large numeric prices as strings and includes token addresses.
- [ ] Protected routes reject unauthenticated requests.
- [ ] The keeper account has enough XLM.
- [ ] The keeper account has `ORDER_KEEPER`.
- [ ] The keeper public key is registered in `DATA_STORE`.
- [ ] Scheduled worker builds and submits `Oracle.set_prices`.
- [ ] `submit_and_poll` sleeps between polls and records the final ledger.
- [ ] `Oracle.try_get_price(token)` confirms prices are on-chain after a scheduled run.
- [ ] `cargo test --workspace` passes in `so4-oracle`.
- [ ] Any frontend SDK changes typecheck in `packages/contracts`.

