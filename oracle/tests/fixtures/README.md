# Test fixtures

## `rpc/` — Soroban RPC and Horizon response shapes (#1423)

**These files are hand-written, not captured.** They reproduce the response
shapes documented for Soroban RPC (`getLatestLedger`, `sendTransaction`,
`getTransaction`) and Horizon (`GET /accounts/{id}`), including the fields
real servers send that this service ignores (`latestLedgerCloseTime`,
`oldestLedger`, `applicationOrder`, `_links`, liabilities, ...), so the
parsers are exercised against the full payload rather than the minimal subset
a hand-rolled mock tends to contain. Hashes, ledgers and timestamps are
plausible placeholder values.

What is deliberately not here yet:

- `envelopeXdr` / `resultXdr` / `resultMetaXdr` on `getTransaction`, and
  `getLedgerEntries` / `simulateTransaction` bodies: these carry XDR blobs
  that should be captured from public testnet rather than typed by hand.
  `tests/common/mod.rs::account_entry_response` already builds a real
  account-entry XDR for `getLedgerEntries`.
- The one XDR value present, `errorResultXdr` in
  `sendTransaction_ERROR_txBadSeq.json`, is a `TransactionResult` with
  `feeCharged = 100` and result `txBAD_SEQ`; `rpc_fixtures.rs` decodes it.

To replace a file with a real capture, POST the same JSON-RPC method to
`https://soroban-testnet.stellar.org` (never mainnet) and keep the file name.
`rpc_fixtures.rs` has an `#[ignore]`d live check that confirms the current
testnet `getLatestLedger` response still parses.
