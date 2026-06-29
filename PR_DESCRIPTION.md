# Oracle Utility Functions and Test Enhancements

This PR implements utility functions and enhances test coverage for the so4-oracle project.

## Changes

### #399 - Implement current_timestamp_secs()
- Added a public `current_timestamp_secs()` function to `src/lib.rs`
- Returns `u64` representing the current Unix timestamp in seconds
- Consolidated duplicate implementations from `price_loop.rs` and `pyth.rs` into a single public utility
- Updated `price_loop.rs` and `pyth.rs` to use the centralized function

### #400 - Test fixed_source_builds_signed_cached_price()
- Enhanced the existing test in `src/price_loop.rs` to verify all fields of `CachedPrice`
- Added assertions for `token_address`, `min`, `max`, `median`, and `timestamp`
- Ensures the full pipeline with fixed source produces correct output

### #401 - Implement get_latest_ledger_sequence()
- Verified existing implementation in `src/stellar_rpc.rs`
- Function already properly handles errors via `RpcError` enum
- Returns `u32` ledger sequence number from Stellar RPC endpoint

### #402 - Implement rpc_post() helper
- Verified existing implementation in `src/stellar_rpc.rs`
- Helper function already includes correct headers (`Content-Type: application/json`)
- Properly checks for HTTP 200 status and returns response body
- Comprehensive error handling for network and HTTP errors

## Testing
- All existing tests pass
- Enhanced test coverage for fixed source price building pipeline

Closes #399, #400, #401, #402
