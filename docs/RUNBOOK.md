# Runbook

Operator guidance for the failure modes that recur in the issue tracker.
Each section lists what you see, what it means, which endpoint or metric
confirms it, and the remediation.

Observability surface:

- `GET /oracle/status` — price loop health and last successful submissions
- `GET /keeper/status` — keeper cycle state, `blacklisted_keys`
- `GET /keeper/balance` — keeper account balance vs the configured minimum
- `GET /oracle/failed-submissions?operation=&limit=` — recent failures per operation
- `DELETE /keeper/blacklist/{key}` — un-blacklist an order key
- `GET /metrics` — Prometheus families (`oracle_*`)

## Keeper balance below minimum

- **See**: `oracle_keeper_balance_low_count` rising; balance gate warnings.
- **Means**: the keeper account is below `MIN_KEEPER_BALANCE_XLM`; submissions
  are gated to avoid failing transactions for lack of fees/reserves.
- **Confirm**: `GET /keeper/balance`.
- **Remedy**: fund the keeper account.

## Prices stale

- **See**: `oracle_prices_stale_count` rising; stale timestamps in
  `GET /oracle/status`; `oracle_token_fetch_failures_total` and
  `oracle_token_source_fetch_failures_total` climbing.
- **Means**: the price loop cannot fetch or submit fresh prices (source
  outage, rate limit, or RPC failure).
- **Confirm**: `GET /oracle/status`;
  `GET /oracle/failed-submissions?operation=set_prices&limit=20`.
- **Remedy**: check the configured price sources (and `PYTH_API_KEY` if used),
  check the RPC provider, roll the deployment if the process is wedged.

## A price source is rate-limiting

- **See**: HTTP 429 responses in logs;
  `oracle_token_source_fetch_failures_total` for one source climbing while
  others stay flat.
- **Means**: a provider is throttling this deployment.
- **Confirm**: `GET /oracle/failed-submissions?operation=set_prices&limit=`.
- **Remedy**: check provider quotas and API keys; spread load across sources.

## Orders frozen

- **See**: freeze errors in `GET /oracle/failed-submissions?operation=freeze_order`;
  a loud alert when a key is frozen (#498).
- **Means**: `freeze_order` failed `MAX_CONSECUTIVE_FREEZE_FAILURES` (3)
  times in a row for that order key — see
  [KEEPER_STATE_MACHINE.md](KEEPER_STATE_MACHINE.md).
- **Confirm**: `GET /keeper/status` (`blacklisted_keys`).
- **Remedy**: fix the underlying order, then `DELETE /keeper/blacklist/{key}`.

## Order keys blacklisted

- **See**: `blacklisted_keys` in `GET /keeper/status`; the order silently
  never re-enters the keeper loop.
- **Means**: the key reached `MAX_CONSECUTIVE_FREEZE_FAILURES` (3) or
  `MAX_CONSECUTIVE_EXECUTION_FAILURES` (5) consecutive failures.
- **Confirm**: `GET /keeper/status`.
- **Remedy**: fix the underlying order, then `DELETE /keeper/blacklist/{key}`.
  Note the blacklist is in-memory: a restart clears it and frozen orders may
  be re-submitted.

## `tx_bad_seq` after a timeout

- **See**: `tx_bad_seq` / sequence errors in
  `GET /oracle/failed-submissions?operation=`; `oracle_submit_failures_total`.
- **Means**: a submission timed out client-side but may still have landed
  on-chain, leaving the locally cached account sequence stale.
- **Confirm**: look the transaction up on Horizon for the keeper account;
  check `in_flight_keys` behaviour in the logs (entries are swept after
  `IN_FLIGHT_EXPIRY`, 300 s).
- **Remedy**: let the expiry sweep evict the in-flight entry and retry next
  cycle — the keeper re-fetches the account sequence per submission. If the
  account is wedged on sequence, roll the deployment.

## `/ready` flapping

- **See**: the readiness probe alternating ready/not-ready.
- **Means**: readiness depends on upstream checks (RPC/dependencies) cached in
  `ReadyCache`; flapping usually tracks an unhealthy dependency, not the
  process itself.
- **Confirm**: `GET /health` (process) vs `GET /oracle/status` (upstream).
- **Remedy**: fix or replace the RPC provider; restart only if the process
  itself is wedged.

## The whole cycle exceeding its budget

- **See**: cycle-timeout warnings; `oracle_keeper_cycle_duration_seconds`
  sitting at `KEEPER_CYCLE_TIMEOUT_SECS` (50 s).
- **Means**: one keeper cycle exceeded the hard cap (#490) — almost always
  slow RPC/Horizon responses.
- **Confirm**: `oracle_keeper_cycle_duration_seconds` and
  `oracle_keeper_cycles_total` on `GET /metrics`.
- **Remedy**: check RPC latency, roll the deployment, and review
  `KEEPER_LOOP_MS` and `PRICE_LOOP_MS` in the environment.

## Soroban budget exceeded on execution

- **See**: `Budget, ExceededLimit` (`BUDGET_EXCEEDED_PATTERN`) in
  `GET /oracle/failed-submissions?operation=execute_order`.
- **Means**: the contract call exhausted its Soroban budget. Since #803 this
  counts toward `execution_failure_counts` like any other execute failure.
- **Remedy**: treat as a failed execution; persistent failures push the key
  toward the blacklist threshold — see
  [KEEPER_STATE_MACHINE.md](KEEPER_STATE_MACHINE.md).
