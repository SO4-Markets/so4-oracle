# SO4 Oracle + Keeper — Full Rewrite to a Production Axum Service

Date: 2026-06-18 (rev 2 — full rewrite)
Author handoff for: ibrahimijai
Decision taken: **greenfield rewrite** of the price/keeper service as ONE all-Rust
`axum` + `tokio` binary. No Cloudflare Worker. No TS keeper. No incremental port —
we design it clean and move the proven logic across deliberately.

Reference model: https://github.com/gmx-io/gmx-synthetics
Contracts under port: `/home/sunny/zero/so4-market-project/contracts`

---

## 0. TL;DR — the why, in three sentences

1. Cloudflare Worker cron has a 1-minute floor; on-chain prices live in **temporary
   storage** (expire in seconds), so for ~55s of every minute there is no usable price
   on-chain and nothing executes. "Trade every second" is impossible on that runtime.
2. GMX keepers are **persistent processes** that poll for work and execute within 1–2
   blocks; the fix is a long-running process with a tight loop, i.e. an Axum service.
3. We rewrite — rather than port — because the existing code is shaped around the Worker
   (`worker::Fetch`, `!Send` futures, cron entrypoints, a split TS keeper); a clean
   `tokio` design is faster to get right than unpicking those assumptions one file at a
   time.

What the oracle must do on-chain (the part that felt murky): **nothing continuous.**
Prices are transient and only needed at the instant of execution. The loop is:
read pending work → fetch & sign fresh prices → `oracle.set_prices([...])` →
`handler.execute_*(key)` → repeat. Latency floor = one Soroban ledger (~5s). That is
GMX-grade; you can't beat ledger time.

---

## 1. Current state (what we are replacing)

Two Cloudflare Workers, both on `*/1 * * * *`:

- **`so4-oracle`** (Rust Worker, `oracle/src/lib.rs`): fetch Binance/Coinbase/Pyth →
  aggregate `min`/`max` → ed25519-sign → serve `GET /prices`. Never submits on-chain.
- **`keeper`** (TS Worker, `keeper/src/index.ts`): pull `/prices` → build+sign+submit
  `oracle.set_prices` → read pending order/deposit/withdrawal keys from `reader` →
  `execute_*` each; budget-exceeded → `freeze_order`.

Both die between cron ticks; neither can deliver per-second execution.

### Already shipped at the contract layer (2026-06-18)
- `oracle.set_prices` now `extend_ttl`s the temp price entry (~120 ledgers) so a price
  survives a `set_prices → execute_*` batch (was a `PriceNotFound` hazard).
- `oracle` + `market_factory` `upgrade` migrated to the no-caller ABI; both upgraded
  in place (addresses unchanged). Admin signing identity = `steins-testnet`.

### Deployed testnet addresses (`contracts/.deployed/testnet.env`)
```
ORACLE              = CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY
ORDER_HANDLER       = CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY
DEPOSIT_HANDLER     = CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C
WITHDRAWAL_HANDLER  = CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE
READER              = CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC
DATA_STORE          = CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J
ROLE_STORE          = CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q
NETWORK_PASSPHRASE  = "Test SDF Network ; September 2015"
RPC_URL             = https://soroban-testnet.stellar.org
```

### On-chain `oracle` contract surface we must match exactly
```rust
fn set_prices(env, caller: Address, prices: Vec<SignedPrice>)   // caller needs ORDER_KEEPER
struct SignedPrice { token: Address, min_price: i128, max_price: i128,
                     timestamp: u64, signature: BytesN<64>, keeper_index: u32, ledger_seq: u32 }
```
Signed message (ed25519, SDK hashes internally):
`passphrase ‖ ledger_seq(u32 BE) ‖ token.to_string() strkey bytes ‖ min(i128 BE) ‖ max(i128 BE) ‖ timestamp(u64 BE)`.
Contract checks: `min/max>0`, `min≤max`, `timestamp` within 300s, `ledger_seq` within 60
ledgers, ed25519 verify against keeper pubkey at `keeper_index` in `data_store`. Prices
in `FLOAT_PRECISION = 1e30`, adjusted for token decimals.

---

## 2. Target architecture — one Rust binary

```
so4-oracle  (single statically-deployed binary)
│
├── main.rs            tokio::main → load Config → build AppState → spawn loops → serve axum
│
├── HTTP API (axum + tower-http CORS/trace)
│     GET /health                      public   liveness
│     GET /ready                       public   RPC reachable + keeper funded
│     GET /prices                      public   serves in-memory PriceCache (frontend)
│     GET /oracle/status               admin    last cycle, balance, per-token state
│     GET /keeper/status               admin    pending work + last N executions
│     GET /oracle/failed-submissions   admin    ring buffer of failures
│
├── task: price_loop   tokio::interval(~1s)
│     fetch sources → validate → aggregate min/max → sign → write PriceCache
│
└── task: keeper_loop  tokio::interval(~1–2s)
      poll reader (orders/deposits/withdrawals)
      → if work: set_prices(needed tokens) → execute_*(key) per item → freeze on budget
      → record results; never panics the loop
```

**Concurrency model:** the two loops share `Arc<AppState>`. The keeper loop owns the only
transaction-submitting path and runs submissions **serially** (one keeper account = one
sequence-number stream — do not parallelize). The price loop only writes the cache.

**State:** in-memory (`Arc<RwLock<…>>`). No KV, no DB for v1. Failed-submission history is
a bounded ring buffer. (Postgres optional later — listed in open items.)

**On-chain tx building: all-Rust (committed decision).** Use `stellar-rpc-client` +
`stellar-xdr` (+ `stellar-strkey`, `ed25519-dalek`) to: load account sequence, build the
`InvokeHostFunction` op for `oracle.set_prices` / `handler.execute_*`, `simulateTransaction`,
assemble footprint + resource fee, sign with the keeper seed, `sendTransaction`, poll
`getTransaction`. This is the hard part of the rewrite — budget for it (see §5 Phase 4) and
keep the TS `keeper/src/index.ts` as the behavioral reference for ScVal encoding and the
execute sequence.

---

## 3. Module layout (greenfield)

```
oracle/
├── Cargo.toml                 # axum, tokio, reqwest(rustls), tower-http, tracing,
│                              # tracing-subscriber, dotenvy, serde, serde_json,
│                              # ed25519-dalek, hex, stellar-rpc-client, stellar-xdr,
│                              # stellar-strkey, shared-config
└── src/
    ├── main.rs                # entrypoint: config, state, spawn loops, axum serve, graceful shutdown
    ├── config.rs              # Config::from_env() — all settings + secrets, validated once at boot
    ├── state.rs               # AppState, PriceCache, CycleStatus, FailedSubmission ring buffer
    ├── http.rs                # shared reqwest::Client (OnceLock)
    ├── api/
    │   ├── mod.rs             # Router builder + admin Bearer-auth extractor
    │   ├── prices.rs          # GET /prices, /health, /ready
    │   └── admin.rs           # GET /oracle/status, /keeper/status, /oracle/failed-submissions
    ├── sources/
    │   ├── binance.rs         # KEEP parse fns; HTTP via http::client()
    │   ├── coinbase.rs        #   "
    │   ├── pyth.rs            # KEEP validate/normalize; add full staleness+confidence (GMX #9)
    │   └── fixed.rs           # config-pinned stable (TUSDC)
    ├── prices.rs              # KEEP aggregation; fix 2-source median, config-driven 1-source (GMX #10)
    ├── signing.rs             # KEEP byte layout; add cross-check test vector vs contract
    ├── price_loop.rs          # the ~1s price cycle → PriceCache
    ├── chain/
    │   ├── rpc.rs             # getLatestLedger, getAccount, simulate, send, getTransaction (stellar-rpc-client)
    │   ├── tx_builder.rs      # build/sign InvokeHostFunction for set_prices + execute_*
    │   ├── scval.rs           # SignedPrice ↔ ScVal, Address/i128/BytesN encoding (mirror keeper/index.ts)
    │   └── submit.rs          # send + poll with REAL backoff (tokio::time::sleep)
    ├── keeper_loop.rs         # poll reader → set_prices → execute_* → freeze; serial submit
    └── balance.rs             # keeper XLM balance check; testnet friendbot auto-fund / mainnet alert
```

What carries over **unchanged in logic** (pure, runtime-agnostic — copy + swap HTTP/sleep):
`prices.rs` aggregation, `signing.rs` byte layout, all `parse_*` fns in
`binance/coinbase/pyth/stellar_rpc/submit`, `shared-config`. The 96 existing unit tests
move with them.

What is genuinely **new code**: `chain/tx_builder.rs`, `chain/scval.rs`, `chain/rpc.rs`
(real Soroban tx assembly in Rust), `keeper_loop.rs`, the axum API, `config.rs`,
`state.rs`, `main.rs`.

---

## 4. The price + keeper cycles, precisely

### price_loop (every ~1s)
1. For each configured token, fetch each enabled source (Binance/Coinbase/Pyth/fixed) via
   `http::client()`, with per-source retry+backoff.
2. Validate (Pyth: publish-time/staleness/confidence/expo/overflow — GMX #9).
3. Aggregate → `min`/`max` in 1e30, decimals-adjusted; drop outliers beyond
   `max_deviation_bps`; require `min_sources`.
4. ed25519-sign the canonical message with `ledger_seq` from a cached `getLatestLedger`.
5. Write `CachedPrice` into `PriceCache`; `/prices` serves this (i128 as strings).

### keeper_loop (every ~1–2s)
1. Read pending `order/deposit/withdrawal` keys from `reader` (count + keys, like the TS
   keeper). If none → idle tick.
2. Determine the token set needed (each market's `index_token` + collateral tokens). Pull
   their freshest `SignedPrice`s from `PriceCache`.
3. Build + submit `oracle.set_prices(caller, prices)` (serial).
4. Per pending item: build + submit `handler.execute_*(keeper, key)`. On
   `Budget, ExceededLimit` for an order → `freeze_order`. Re-`set_prices` per item (or per
   small batch) so prices never age out mid-drain (TTL safety; mirrors contract §0 fix).
5. Record success/failure per item (tx hash, ledger, error) into status + failure ring.
6. Never `panic`/exit on error — log, record, continue to next tick. Loop is supervised.

### Submission discipline
- One keeper account → submit transactions **serially**; fetch sequence fresh per tx or
  track locally. Channel accounts are a later scaling step, not v1.
- `submit.rs` polls `getTransaction` with **real** `tokio::time::sleep` backoff (the old
  Worker code computed backoff but slept zero on native — fix it here).

---

## 5. Build phases (checklist)

### Phase 0 — Scaffold
- [ ] New `Cargo.toml` (drop `worker*`, `js-sys`, `tower-service`; add the §3 deps).
- [ ] `main.rs` skeleton: `tokio::main`, `tracing_subscriber` JSON to stdout, `dotenvy`,
      bind axum, `/health` only. Confirm it runs and serves `/health`.

### Phase 1 — Config + state
- [ ] `Config::from_env()` reads & validates everything once: network (passphrase, rpc,
      horizon), all contract IDs, `KEEPER_PRIVATE_KEY` (ed25519 hex), `KEEPER_SECRET_KEY`
      (S… seed for tx signing), `KEEPER_ACCOUNT_ID`, `ADMIN_API_TOKEN`, `PRICE_FEED_CONFIG`
      (or `config/tokens.json` fallback), loop intervals, thresholds, `keeper_index`.
- [ ] `AppState { config, http, price_cache, cycle_status, failures }` behind `Arc`.

### Phase 2 — Price pipeline + API
- [ ] Move `sources/*`, `prices.rs`, `signing.rs`; swap HTTP→reqwest, sleep→tokio, time→
      `SystemTime`, logs→`tracing`.
- [ ] Fix `prices.rs` 2-source median (average) + config-driven 1-source (GMX #10).
- [ ] Finish Pyth validation (GMX #9).
- [ ] `price_loop` task + `GET /prices`, `/oracle/status`. Verify against live sources.

### Phase 3 — Chain reads
- [ ] `chain/rpc.rs` on `stellar-rpc-client`: `getLatestLedger`, `getAccount`,
      `simulateTransaction`, `sendTransaction`, `getTransaction`.
- [ ] `balance.rs` keeper balance; testnet friendbot auto-fund, mainnet hard alert.
- [ ] Reader polling: `get_{order,deposit,withdrawal}_count` + `_keys` (mirror TS keeper).

### Phase 4 — Chain writes (the hard part)
- [ ] `chain/scval.rs`: `SignedPrice` → ScVal map (field order:
      keeper_index, ledger_seq, max_price, min_price, signature, timestamp, token — Soroban
      sorts map keys), Address/i128/u64/BytesN encoding. Unit-test against a vector captured
      from the working TS keeper.
- [ ] `chain/tx_builder.rs`: build `InvokeHostFunction` for `set_prices` and `execute_*`;
      simulate → assemble footprint+fee → sign with keeper seed → base64 XDR.
- [ ] `chain/submit.rs`: send + poll with real backoff.
- [ ] **Gate:** end-to-end on testnet — sign a price, `set_prices`, confirm
      `oracle.try_get_price(token)` returns it.

### Phase 5 — Keeper loop
- [ ] `keeper_loop`: poll → set_prices → execute_* → freeze-on-budget; serial submit;
      supervised (never exits); per-item result recording.
- [ ] `/keeper/status`, `/oracle/failed-submissions`.

### Phase 6 — Hardening + deploy
- [ ] Graceful shutdown (`tokio::signal`), `/ready` gating, structured metrics
      (cycle latency, executions, failures, balance) — optional Prometheus.
- [ ] Dockerfile (multi-stage → debian-slim/distroless) + systemd unit (`Restart=always`)
      or Fly.io/Railway. Secrets via env/secret-store, never logged.
- [ ] Delete `wrangler.toml`, `keeper/`, `oracle/build/`, `.wrangler/`.

### Phase 7 — Tests
- [ ] Carry the 96 unit tests; `wiremock` for source + RPC; ScVal/signing vector tests;
      a mocked end-to-end keeper cycle; live testnet smoke (create order → executed within
      ~1 ledger).

---

## 6. GMX alignment — where SO4 still diverges
Severity: 🔴 security/correctness · 🟠 robustness · 🟡 fidelity. These are **contract**
changes (separate workstream from this service rewrite) unless noted.

1. 🔴 **Single oracle signer, no on-chain median.** `set_prices` trusts one ed25519 sig
   per price. GMX requires N independent signers + on-chain median + min-signers. The
   service rewrite should be built so the price loop can carry N signatures per token
   (forward-compatible struct) even before the contract enforces it.
2. 🔴 **Off-chain aggregation is trusted blindly** — the keeper key can mint any price
   (coupled to #1).
3. 🔴 **set_prices + execute are separate txs** — GMX passes oracle params into
   `executeOrder` atomically. Mitigated operationally here (re-set per item + temp TTL);
   the real fix is inline prices in `execute_*`.
4. 🔴 **No request-block enforcement** — `execute_order` doesn't require the price's
   `ledger_seq ≥ order.created_ledger_seq`; limit/stop fills are front-runnable. Needs an
   order field + check.
5. 🟠 **Loose staleness windows** (300s / 60 ledgers) — tighten once the loop is sub-2s.
6. 🟠 **One key signs prices AND submits AND has ORDER_KEEPER** — GMX separates oracle
   signers from order keepers. The rewrite already separates `KEEPER_PRIVATE_KEY`
   (signing) from `KEEPER_SECRET_KEY` (tx) at the config layer — keep them distinct keys.
7. 🟡 **Pyth signature discarded** — verifying Pyth on-chain via Stellar's Pyth receiver
   is the closest analog to GMX's Chainlink Data Streams move; future direction.
8. 🟡 **Pyth validation incomplete** (#9 above) — fixed in the service rewrite (Phase 2).
9. 🟡 **2-source median / 1-source policy** — fixed in the service rewrite (Phase 2).

✅ Aligned: 1e30 precision; transient temp-storage price model (+TTL fix); feed-then-execute
flow; min/max pair; stable-price fallback; create-order → keeper-executes two-step.

---

## 7. Config & secrets (env)
Vars: `STELLAR_NETWORK`, `STELLAR_RPC_URL`, `HORIZON_URL`, `NETWORK_PASSPHRASE`,
`ORACLE_CONTRACT_ID`, `ORDER_HANDLER`, `DEPOSIT_HANDLER`, `WITHDRAWAL_HANDLER`,
`READER`, `DATA_STORE`, `PRICE_LOOP_MS`, `KEEPER_LOOP_MS`, `MIN_KEEPER_BALANCE_XLM`,
`KEEPER_INDEX`, `BIND_ADDR`.
Secrets: `KEEPER_PRIVATE_KEY` (ed25519 hex, price signing), `KEEPER_SECRET_KEY` (S… seed,
tx signing — keep distinct per GMX #6), `KEEPER_ACCOUNT_ID`, `ADMIN_API_TOKEN`,
`PRICE_FEED_CONFIG` (optional; else `config/tokens.json`). Single address source of truth:
`contracts/.deployed/<network>.env`. Verify `ORACLE=CBEMTV23…` (README still cites the old
`CBABE5O7…`). Fill real Pyth feed IDs for BTC/ETH/XLM; pin TUSDC as fixed stable.

## 8. Risks
- **Rust Soroban tx assembly** (Phase 4) is the schedule risk — use the TS keeper as the
  oracle for correct ScVal encoding + footprint/fee assembly; gate on a live testnet
  `set_prices` round-trip before building the keeper loop.
- **Sequence-number contention** — submit serially.
- **RPC outages/limits** — loops degrade (skip tick, alert), never crash-loop.
- **Key custody** — load from env/secret-store, never log; distinct signing vs tx keys.

## 9. Open items (decide as we go, none block Phase 0–2)
- [ ] Hosting: Fly.io / Railway / VPS+systemd?
- [ ] When to take on contract GMX items §6 #1–#4 (multi-signer, atomic, request-block)?
- [ ] TUSDC: fixed-pinned stable vs live feed.
- [ ] Durability: in-memory only vs Postgres for failed-submission history.
- [ ] `stellar-rpc-client`/`stellar-xdr` exact versions matching the deployed network
      protocol (cli is 26.x; pin XDR `curr`).

---
## Appendix — files studied this session
`oracle/src/{lib,signing,prices,binance,coinbase,pyth,keeper,network_config,config,retry,submit,stellar_rpc,log}.rs`,
`keeper/src/index.ts`, `shared/config/src/lib.rs`,
`contracts/contracts/{oracle,order_handler,market_factory}/src/lib.rs`,
`contracts/mx/*.mk`, `contracts/.deployed/testnet.env`.
