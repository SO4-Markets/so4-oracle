use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use shared_config::TokenConfig;
use tokio::time::{interval, timeout, MissedTickBehavior};

use crate::prices::AggregatedPrice;
use crate::retry::MAX_BACKOFF_DELAY_MS;
use crate::state::{AppState, CachedPrice, FailedSubmission};

const SOURCE_RETRY_ATTEMPTS: u32 = 3;
const SOURCE_RETRY_BASE_DELAY_MS: u64 = 100;
const LEDGER_SEQUENCE_RETRY_ATTEMPTS: u32 = 3;
const LEDGER_SEQUENCE_RETRY_BASE_DELAY_MS: u64 = 100;
/// Hard cap on a single price cycle — mirrors KEEPER_CYCLE_TIMEOUT_SECS (#781).
const PRICE_CYCLE_TIMEOUT_SECS: u64 = 60;

/// Unified error type for all price sources, preserving structure through retries.
#[derive(Debug, Clone)]
pub enum PriceSourceError {
    Binance(crate::binance::BinancePriceError),
    Coinbase(crate::coinbase::CoinbasePriceError),
    Pyth(crate::pyth::PythPriceError),
    Fixed(crate::fixed::FixedPriceError),
    Config(String),
    Unsupported(String),
}

impl CachedPrice {
    pub fn is_stale(&self, stale_after_seconds: u64, now: u64) -> bool {
        // #1041 — when the wall clock moves backward relative to the cached
        // timestamp (NTP step, VM live-migration, container clock correction),
        // `saturating_sub` silently clamps the age to 0, reporting the price
        // as maximally fresh. Treat `timestamp > now` as its own suspicious
        // condition: the price is stale because the age is indeterminate.
        match now.checked_sub(self.timestamp) {
            Some(age) => age >= stale_after_seconds,
            None => true,
        }
    }
}

impl std::fmt::Display for PriceSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binance(e) => write!(f, "{}", e),
            Self::Coinbase(e) => write!(f, "{}", e),
            Self::Pyth(e) => write!(f, "{}", e),
            Self::Fixed(e) => write!(f, "{}", e),
            Self::Config(msg) => write!(f, "config error: {}", msg),
            Self::Unsupported(source) => write!(f, "unsupported source: {}", source),
        }
    }
}

impl std::error::Error for PriceSourceError {}

impl crate::retry::Retryable for PriceSourceError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Binance(e) => e.is_retryable(),
            Self::Coinbase(e) => e.is_retryable(),
            Self::Pyth(e) => e.is_retryable(),
            Self::Fixed(e) => e.is_retryable(),
            // Config and unsupported errors are permanent
            Self::Config(_) | Self::Unsupported(_) => false,
        }
    }
}

pub async fn run_price_loop(state: Arc<AppState>) {
    let mut ticker = interval(state.config.price_loop_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = state.shutdown_token.cancelled() => {
                tracing::info!("price_loop shutting down");
                break;
            }
        }
        tokio::select! {
            _ = state.shutdown_token.cancelled() => {
                tracing::info!("price_loop shutting down");
                break;
            }
            _ = run_price_cycle(Arc::clone(&state)) => {}
        }
    }
}

/// Executes a single price collection and processing cycle (Resolves Issue #391).
/// Fetches the latest Stellar ledger sequence, iterates through configured tokens,
/// builds and aggregates their prices, signs the results, and updates the cache.
/// Never panics on individual errors; they are recorded as failure submissions.
pub async fn run_price_cycle(state: Arc<AppState>) {
    let started = Instant::now();
    {
        let mut status = state.cycle_status.write().await;
        status.price_cycle_running = true;
    }

    let result = timeout(
        Duration::from_secs(PRICE_CYCLE_TIMEOUT_SECS),
        execute_price_cycle(Arc::clone(&state)),
    )
    .await;

    match result {
        Ok((tokens_ok, tokens_failed, tokens_stale)) => {
            finish_cycle(&state, started, tokens_ok, tokens_failed, tokens_stale).await;
        }
        Err(_) => {
            // On timeout the execute_price_cycle future is dropped and we lose its
            // partial counters. Report every configured token as failed so the
            // metrics reflect reality: hardcoding (0, 1, 0) only ever incremented
            // the failure counter by 1 regardless of how many tokens were in-flight
            // (issue #1278).
            let total_tokens = state.config.price_feed.tokens.len();
            tracing::error!(
                timeout_secs = PRICE_CYCLE_TIMEOUT_SECS,
                tokens_failed = total_tokens,
                "price cycle exceeded timeout budget"
            );
            finish_cycle(&state, started, 0, total_tokens, 0).await;
        }
    }
}

/// Inner price cycle logic, bounded by PRICE_CYCLE_TIMEOUT_SECS.
async fn execute_price_cycle(state: Arc<AppState>) -> (usize, usize, usize) {
    let mut tokens_ok = 0usize;
    let mut tokens_failed = 0usize;
    let mut tokens_stale = 0usize;
    let now = crate::current_timestamp_secs();

    // Hermes accepts multiple `ids[]` values. Build the feed-ID list eagerly
    // (it only depends on config, no I/O) so we can kick off the Pyth batch
    // fetch at the same time as the ledger-sequence RPC call (#1033).
    let pyth_feed_ids: Vec<&str> = state
        .config
        .price_feed
        .tokens
        .iter()
        .filter(|token| token.sources.iter().any(|source| source == "pyth"))
        .filter_map(|token| token.pyth_feed_id.as_deref())
        .collect();

    // These two calls are independent — run them concurrently to avoid paying
    // the sum of both round-trip latencies every cycle.
    let (ledger_result, pyth_result) = tokio::join!(
        crate::retry::retry_with_backoff(
            || async {
                crate::stellar_rpc::get_latest_ledger_sequence(&state.config.stellar_rpc_url).await
            },
            LEDGER_SEQUENCE_RETRY_ATTEMPTS,
            LEDGER_SEQUENCE_RETRY_BASE_DELAY_MS,
            MAX_BACKOFF_DELAY_MS,
        ),
        crate::pyth::fetch_pyth_prices(
            &pyth_feed_ids,
            state.config.pyth_api_key.as_ref().map(|key| key.as_str()),
        )
    );

    let ledger_seq = match ledger_result {
        Ok(ledger_seq) => ledger_seq,
        Err(error) => {
            tracing::error!(
                error = %error,
                rpc_url = %state.config.stellar_rpc_url,
                "price cycle aborted: failed to fetch latest ledger sequence from RPC after retries"
            );
            record_error(&state, "get_latest_ledger", error.to_string()).await;
            return (0, 1, 0);
        }
    };

    let (pyth_prices, batch_failed) = match pyth_result {
        Ok(prices) => (prices, false),
        Err(error) => {
            tracing::warn!(error = %error, "batched Pyth request failed");
            (std::collections::HashMap::new(), true)
        }
    };

    // Collect every token's Binance symbol up front and fetch them all in a
    // single batched request, mirroring the Pyth batch-fetch pattern above.
    // This exercises the batched `?symbols=[...]` path in build_spot_price_url_for
    // that was previously unreachable from any production call site (#969).
    let binance_symbols: Vec<String> = state
        .config
        .price_feed
        .tokens
        .iter()
        .filter(|token| token.sources.iter().any(|source| source == "binance"))
        .filter_map(|token| token.binance_symbol.clone())
        .collect();

    let binance_prices = if !binance_symbols.is_empty() {
        match crate::binance::fetch_spot_prices_with_url(
            &state.config.binance_base_url,
            &binance_symbols,
        )
        .await
        {
            Ok(results) => {
                let map: std::collections::HashMap<String, i128> = results.into_iter().collect();
                Some(map)
            }
            Err(error) => {
                tracing::warn!(error = %error, "batched Binance request failed");
                None
            }
        }
    } else {
        None
    };

    let mut new_prices = std::collections::BTreeMap::new();

    // Phase 1: Log and count stale cached prices (fast, cache-only reads).
    {
        let cache = state.price_cache.read().await;
        for token in &state.config.price_feed.tokens {
            let key = token.lookup_key();
            if let Some(cached) = cache.prices.get(&key) {
                if cached.is_stale(token.stale_after_seconds, now) {
                    tracing::warn!(
                        symbol = %token.symbol,
                        token = %token.stellar_address,
                        cached_timestamp = cached.timestamp,
                        stale_after_seconds = token.stale_after_seconds,
                        age_seconds = now.saturating_sub(cached.timestamp),
                        "evicting stale cached price"
                    );
                    tokens_stale += 1;
                    // #1209 — Do NOT skip fetching for stale tokens. Stale tokens
                    // are the ones that MOST need a fresh fetch, so they are only
                    // counted here and still fetched in Phase 2 below.
                }
            }
        }
    }

    // Phase 2: Fetch every token concurrently, stale ones included (#968, #1209).
    // Each token's future owns an Arc clone of state and borrows the
    // shared price maps via Arc, so they can run in parallel.
    let pyth_prices = Arc::new(pyth_prices);
    let binance_prices = binance_prices.map(Arc::new);

    let fetch_futures: Vec<_> = state
        .config
        .price_feed
        .tokens
        .iter()
        .map(|token| {
            let state = Arc::clone(&state);
            let pyth = Arc::clone(&pyth_prices);
            let binance = binance_prices.as_ref().map(Arc::clone);
            let token = Arc::clone(&Arc::new(token.clone()));
            async move {
                let key = token.lookup_key();
                let result = build_cached_price(
                    &state,
                    &token,
                    ledger_seq,
                    &pyth,
                    batch_failed,
                    binance.as_deref(),
                )
                .await;
                (key, token, result)
            }
        })
        .collect();

    let results = futures::future::join_all(fetch_futures).await;

    for (key, token, result) in results {
        match result {
            Ok(price) => {
                new_prices.insert(key, price);
                tokens_ok += 1;
            }
            Err(error) => {
                let ctx = ErrorContext {
                    token: token.stellar_address.clone(),
                    symbol: token.symbol.clone(),
                    ledger_seq,
                };
                record_error_with_context(&state, format!("price:{}", token.symbol), error, ctx)
                    .await;

                let cache = state.price_cache.read().await;
                if let Some(cached) = cache.prices.get(&key) {
                    if !cached.is_stale(token.stale_after_seconds, crate::current_timestamp_secs())
                    {
                        tracing::debug!(
                            symbol = %token.symbol,
                            "keeping existing non-stale cached price due to fetch failure"
                        );
                        new_prices.insert(key, cached.clone());
                        tokens_ok += 1;
                    } else {
                        tracing::warn!(
                            symbol = %token.symbol,
                            "cached price became stale during fetch, removing"
                        );
                        tokens_stale += 1;
                        tokens_failed += 1;
                    }
                } else {
                    tokens_failed += 1;
                }
            }
        }
    }

    // #1210 — Update the cache incrementally after each token, not just at the end.
    // This prevents one slow source from stalling the entire cache write.
    if !new_prices.is_empty() {
        let mut cache = state.price_cache.write().await;
        for (key, price) in new_prices {
            cache.prices.insert(key, price);
        }
        cache.last_updated = Some(SystemTime::now());

        if tokens_stale > 0 {
            tracing::info!(
                tokens_ok,
                tokens_stale,
                "price cycle completed with stale entries evicted"
            );
        }
    } else {
        // If we have no valid prices at all, clear the cache to avoid serving stale data
        let mut cache = state.price_cache.write().await;
        cache.prices.clear();
        cache.last_updated = None;
        tracing::warn!(
            "all price sources failed and no valid cached prices available, cache cleared"
        );
    }

    (tokens_ok, tokens_failed, tokens_stale)
}

/// Finalizes the cycle timing and updates `CycleStatus` state flags (Resolves Issue #394).
async fn finish_cycle(
    state: &Arc<AppState>,
    started: Instant,
    tokens_ok: usize,
    tokens_failed: usize,
    tokens_stale: usize,
) {
    let latency_ms = started.elapsed().as_millis() as u64;
    {
        let mut status = state.cycle_status.write().await;
        status.price_cycle_running = false;
        status.last_price_cycle_at = Some(SystemTime::now());
    }

    state
        .metrics
        .record_price_cycle(latency_ms, tokens_ok, tokens_failed);

    tracing::info!(
        tokens_ok,
        tokens_failed,
        tokens_stale,
        latency_ms,
        "cycle_complete"
    );
}

/// Fetches prices from all configured sources, filters and aggregates them,
/// and produces a signed cached price (Resolves Issue #392).
async fn build_cached_price(
    state: &Arc<AppState>,
    token: &TokenConfig,
    ledger_seq: u32,
    pyth_prices: &std::collections::HashMap<String, crate::pyth::PythPriceFeed>,
    pyth_batch_failed: bool,
    binance_prices: Option<&std::collections::HashMap<String, i128>>,
) -> Result<CachedPrice, String> {
    let mut prices = Vec::new();
    let mut sources = Vec::new();
    // Observation time per source, used to attest the oldest contributed
    // observation instead of the signing time (#1258).
    let mut observations: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    for source in &token.sources {
        match fetch_source_with_retry(
            source,
            token,
            state.config.pyth_api_key.as_ref().map(|key| key.as_str()),
            pyth_prices,
            pyth_batch_failed,
            binance_prices,
            &state.config.coinbase_base_url,
        )
        .await
        {
            Ok((price, observed_at)) => {
                prices.push(price);
                sources.push(source.clone());
                observations.insert(source.clone(), observed_at);
            }
            Err(error) => {
                let error_str = error.to_string();
                state.metrics.record_token_source_fetch_failure(
                    &token.symbol,
                    &token.stellar_address,
                    source,
                );
                let ctx = ErrorContext {
                    token: token.stellar_address.clone(),
                    symbol: token.symbol.clone(),
                    ledger_seq,
                };
                record_error_with_context(
                    state,
                    format!("source:{}:{}", token.symbol, source),
                    error_str.clone(),
                    ctx,
                )
                .await;
                tracing::warn!(symbol = %token.symbol, source = %source, error = %error_str, "price source failed");
            }
        }
    }

    let aggregate = crate::prices::aggregate_prices(
        &prices,
        &sources,
        token.min_sources,
        token.max_deviation_bps,
    )?;

    // Surface which sources the outlier filter excluded on an otherwise
    // successful cycle - computed by aggregate_prices but previously
    // dropped before reaching any consumer (#728).
    for rejected in &aggregate.rejected_sources {
        state.metrics.record_token_source_outlier_rejection(
            &token.symbol,
            &token.stellar_address,
            &rejected.source,
        );
        tracing::warn!(
            symbol = %token.symbol,
            source = %rejected.source,
            price = rejected.price,
            deviation_bps = rejected.deviation_bps,
            "price source excluded by outlier filter"
        );
    }

    // Attest the oldest observation among the sources that actually contributed
    // to the median, not the signing time (#1258).
    let observed_at = aggregate_observed_at(
        &aggregate.sources_used,
        &observations,
        crate::current_timestamp_secs(),
    );

    signed_cached_price(state, token, ledger_seq, aggregate, observed_at)
}

async fn fetch_source_with_retry(
    source: &str,
    token: &TokenConfig,
    pyth_api_key: Option<&str>,
    pyth_prices: &std::collections::HashMap<String, crate::pyth::PythPriceFeed>,
    pyth_batch_failed: bool,
    binance_prices: Option<&std::collections::HashMap<String, i128>>,
    coinbase_base_url: &str,
) -> Result<(i128, u64), PriceSourceError> {
    crate::retry::retry_with_backoff(
        || async {
            fetch_source_price(
                source,
                token,
                pyth_api_key,
                pyth_prices,
                pyth_batch_failed,
                binance_prices,
                coinbase_base_url,
            )
            .await
        },
        SOURCE_RETRY_ATTEMPTS,
        SOURCE_RETRY_BASE_DELAY_MS,
        MAX_BACKOFF_DELAY_MS,
    )
    .await
}

async fn fetch_source_price(
    source: &str,
    token: &TokenConfig,
    pyth_api_key: Option<&str>,
    pyth_prices: &std::collections::HashMap<String, crate::pyth::PythPriceFeed>,
    pyth_batch_failed: bool,
    binance_prices: Option<&std::collections::HashMap<String, i128>>,
    coinbase_base_url: &str,
) -> Result<(i128, u64), PriceSourceError> {
    match source {
        "binance" => {
            let symbol = token
                .binance_symbol
                .as_ref()
                .ok_or_else(|| PriceSourceError::Config("missing binance_symbol".to_string()))?;
            let price = if let Some(prices) = binance_prices {
                prices.get(symbol).copied().ok_or_else(|| {
                    PriceSourceError::Config(format!(
                        "binance symbol not returned in batch: {symbol}"
                    ))
                })?
            } else {
                // Batch request failed or no batch was made — fall back to a
                // single-symbol request so this token still gets a price.
                let results = crate::binance::fetch_spot_prices(std::slice::from_ref(symbol))
                    .await
                    .map_err(PriceSourceError::Binance)?;
                results
                    .into_iter()
                    .find(|(got_symbol, _)| got_symbol == symbol)
                    .map(|(_, price)| price)
                    .ok_or_else(|| {
                        PriceSourceError::Config(format!("binance symbol not returned: {symbol}"))
                    })?
            };
            // Binance has no server-side timestamp: the fetch returning is the
            // observation (#1258).
            Ok((price, crate::current_timestamp_secs()))
        }
        "coinbase" => {
            let symbol = token
                .coinbase_symbol
                .as_ref()
                .ok_or_else(|| PriceSourceError::Config("missing coinbase_symbol".to_string()))?;
            let price = crate::coinbase::fetch_spot_price_with_url(coinbase_base_url, symbol)
                .await
                .map_err(PriceSourceError::Coinbase)?;
            Ok((price, crate::current_timestamp_secs()))
        }
        "pyth" => {
            let feed_id = token
                .pyth_feed_id
                .as_ref()
                .ok_or_else(|| PriceSourceError::Config("missing pyth_feed_id".to_string()))?;
            let feed = if let Some(feed) = pyth_prices.get(feed_id) {
                feed.clone()
            } else {
                // If batch failed, we should not fall back to individual requests
                // as this would create N requests when Hermes is unstable
                if pyth_batch_failed {
                    tracing::warn!(
                        symbol = %token.symbol,
                        feed_id = %feed_id,
                        "skipping Pyth price fetch due to batch failure"
                    );
                    return Err(PriceSourceError::Pyth(
                        crate::pyth::PythPriceError::NetworkError(
                            "batch request failed, skipping individual fallback".to_string(),
                        ),
                    ));
                }

                tracing::warn!(
                    symbol = %token.symbol,
                    feed_id = %feed_id,
                    "Pyth feed not found in batch, falling back to individual request"
                );
                crate::pyth::fetch_pyth_prices(&[feed_id], pyth_api_key)
                    .await
                    .and_then(|mut feeds| {
                        feeds.remove(feed_id).ok_or_else(|| {
                            crate::pyth::PythPriceError::MissingFeedId(feed_id.clone())
                        })
                    })
                    .map_err(PriceSourceError::Pyth)?
            };
            let price = crate::pyth::validate_pyth_price(
                &feed.price,
                crate::current_timestamp_secs(),
                token.stale_after_seconds,
                token.pyth_max_confidence_bps,
            )
            .map_err(PriceSourceError::Pyth)?;
            // Pyth attests its own `publish_time`, which `validate_pyth_price`
            // already required to be present and non-negative (#1258). Keep a
            // fallback so a future validation change degrades to fetch time
            // instead of panicking.
            let observed_at = feed
                .price
                .publish_time
                .and_then(|t| u64::try_from(t).ok())
                .unwrap_or_else(crate::current_timestamp_secs);
            Ok((price, observed_at))
        }
        "fixed" => {
            let price = crate::fixed::fixed_price(token).map_err(PriceSourceError::Fixed)?;
            Ok((price, crate::current_timestamp_secs()))
        }
        other => Err(PriceSourceError::Unsupported(other.to_string())),
    }
}

/// The timestamp to attest for an aggregated price: the oldest observation
/// among the sources that contributed to the median (#1258).
///
/// Only `sources_used` counts: a source rejected by the cluster or outlier
/// filters must not make the attested price look older than the data behind it.
fn aggregate_observed_at(
    sources_used: &[String],
    observations: &std::collections::HashMap<String, u64>,
    fallback: u64,
) -> u64 {
    sources_used
        .iter()
        .filter_map(|source| observations.get(source).copied())
        .min()
        .unwrap_or(fallback)
}

/// Signs the aggregated price payload and returns a CachedPrice (Resolves Issue #393).
/// `timestamp` is the oldest source observation in the aggregate (#1258), so the
/// signature attests when the price was observed, not when it was signed.
/// The signature matches the required format and is encoded to a 128 hex character string.
fn signed_cached_price(
    state: &Arc<AppState>,
    token: &TokenConfig,
    ledger_seq: u32,
    aggregate: AggregatedPrice,
    timestamp: u64,
) -> Result<CachedPrice, String> {
    // Sign with the key decoded once at startup (#1226); fall back to the
    // string key only so a missing/invalid key still reports its error.
    let signature = match state.price_signing_key.as_deref() {
        Some(key) => crate::signing::sign_price_with_key(
            key,
            &state.config.network_passphrase,
            ledger_seq,
            &token.stellar_address,
            aggregate.min,
            aggregate.max,
            timestamp,
        ),
        None => crate::signing::sign_price(
            state.config.keeper_private_key.as_str(),
            &state.config.network_passphrase,
            ledger_seq,
            &token.stellar_address,
            aggregate.min,
            aggregate.max,
            timestamp,
        )
        .map_err(|err| err.to_string())?,
    };

    Ok(CachedPrice {
        token_address: token.stellar_address.clone(),
        symbol: token.symbol.clone(),
        display_symbol: token.display_symbol().to_string(),
        keeper_index: state.config.keeper_index,
        min: aggregate.min,
        max: aggregate.max,
        median: aggregate.median,
        timestamp,
        ledger_seq,
        sources_used: aggregate.sources_used,
        signature: hex::encode(signature.to_bytes()),
    })
}

struct ErrorContext {
    token: String,
    symbol: String,
    /// Ledger sequence in scope when the failure was recorded, if any (#726).
    /// `0` for failures that occur before any ledger sequence is known for
    /// this cycle (e.g. the `get_latest_ledger` fetch itself failing).
    ledger_seq: u32,
}

async fn record_error(
    state: &Arc<AppState>,
    operation: impl Into<String>,
    error: impl Into<String>,
) {
    record_error_with_context(
        state,
        operation,
        error,
        ErrorContext {
            token: String::new(),
            symbol: String::new(),
            ledger_seq: 0,
        },
    )
    .await;
}

async fn record_error_with_context(
    state: &Arc<AppState>,
    operation: impl Into<String>,
    error: impl Into<String>,
    ctx: ErrorContext,
) {
    state.failures.lock().await.push(FailedSubmission {
        at: SystemTime::now(),
        operation: operation.into(),
        network: state.config.network.as_str().to_string(),
        token: ctx.token,
        symbol: ctx.symbol,
        min: 0,
        max: 0,
        tx_hash: None,
        error: error.into(),
        timestamp: crate::current_timestamp_secs(),
        ledger_seq: ctx.ledger_seq,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Network, PriceFeedConfig, SecretString};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    fn test_state(token: TokenConfig) -> Arc<AppState> {
        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url: "http://localhost:0".to_string(),
            horizon_url: "http://localhost:0".to_string(),
            oracle_contract_id: "CORACLE".to_string(),
            role_store_contract_id: "CROLE".to_string(),
            data_store_contract_id: "CDATA".to_string(),
            order_handler_contract_id: "CORDER".to_string(),
            deposit_handler_contract_id: "CDEPOSIT".to_string(),
            withdrawal_handler_contract_id: "CWITHDRAW".to_string(),
            reader_contract_id: "CREADER".to_string(),
            keeper_private_key: SecretString::new(
                "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            ),
            keeper_secret_key: SecretString::new("SSECRET".to_string()),
            keeper_account_id: "GACCOUNT".to_string(),
            keeper_index: 0,
            admin_api_token: None,
            pyth_api_key: None,
            min_keeper_balance_xlm: 0.0,
            set_prices_tx_fee: crate::config::DEFAULT_SET_PRICES_TX_FEE,
            keeper_tx_fee: crate::config::DEFAULT_KEEPER_TX_FEE,
            price_loop_interval: Duration::from_millis(1),
            keeper_loop_interval: Duration::from_millis(1),
            price_feed: PriceFeedConfig {
                tokens: vec![token],
            },
            coinbase_base_url: crate::coinbase::COINBASE_EXCHANGE_RATES_URL.to_string(),
            binance_base_url: crate::binance::BINANCE_TICKER_PRICE_URL.to_string(),
        };
        Arc::new(AppState::new(Arc::new(config)))
    }

    // ── #512: run_price_loop shutdown coverage ───────────────────────────────

    fn shutdown_test_state() -> Arc<AppState> {
        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url: "not-a-valid-url".to_string(), // immediate error — no network
            horizon_url: "not-a-valid-url".to_string(),
            oracle_contract_id: "CORACLE".to_string(),
            role_store_contract_id: "CROLE".to_string(),
            data_store_contract_id: "CDATA".to_string(),
            order_handler_contract_id: "CORDER".to_string(),
            deposit_handler_contract_id: "CDEPOSIT".to_string(),
            withdrawal_handler_contract_id: "CWITHDRAW".to_string(),
            reader_contract_id: "CREADER".to_string(),
            keeper_private_key: SecretString::new(
                "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            ),
            keeper_secret_key: SecretString::new("SSECRET".to_string()),
            keeper_account_id: "GACCOUNT".to_string(),
            keeper_index: 0,
            admin_api_token: None,
            pyth_api_key: None,
            min_keeper_balance_xlm: 0.0,
            set_prices_tx_fee: crate::config::DEFAULT_SET_PRICES_TX_FEE,
            keeper_tx_fee: crate::config::DEFAULT_KEEPER_TX_FEE,
            price_loop_interval: Duration::from_millis(50),
            keeper_loop_interval: Duration::from_millis(50),
            price_feed: PriceFeedConfig { tokens: vec![] },
            coinbase_base_url: crate::coinbase::COINBASE_EXCHANGE_RATES_URL.to_string(),
            binance_base_url: crate::binance::BINANCE_TICKER_PRICE_URL.to_string(),
        };
        Arc::new(AppState::new(Arc::new(config)))
    }

    #[tokio::test]
    async fn run_price_loop_exits_promptly_on_shutdown() {
        let state = shutdown_test_state();
        let state2 = Arc::clone(&state);

        let handle = tokio::spawn(run_price_loop(state2));

        // Let the loop tick at least once.
        tokio::time::sleep(Duration::from_millis(120)).await;

        state.shutdown_token.cancel();

        let completed = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(
            completed.is_ok(),
            "run_price_loop must exit within 500 ms of shutdown_token cancellation"
        );
    }

    #[tokio::test]
    async fn fixed_source_builds_signed_cached_price() {
        let token = TokenConfig {
            symbol: "TUSDC".to_string(),
            display_symbol: Some("USDC".to_string()),
            stellar_address: "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES".to_string(),
            sources: vec!["fixed".to_string()],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: None,
            fixed_price: Some("1000000000000000000000000000000".to_string()),
            min_sources: 1,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            pyth_max_confidence_bps: 100,
        };

        let state = test_state(token.clone());
        let cached = build_cached_price(
            &state,
            &token,
            123,
            &std::collections::HashMap::new(),
            false,
            None,
        )
        .await
        .unwrap();
        let configured_price = token.fixed_price.as_ref().unwrap().parse::<i128>().unwrap();
        let spread = configured_price * token.max_deviation_bps as i128 / 10_000;

        // Verify all fields are correct (closes #400)
        assert_eq!(
            cached.token_address,
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES"
        );
        assert_eq!(cached.symbol, "TUSDC");
        assert_eq!(cached.display_symbol, "USDC");
        // With 1 source, compute_confidence_interval_with_spread adds spread_bps (100 bps = 1%)
        // mid = 1_000_000_000_000_000_000_000_000_000_000
        // spread = mid * 100 / 10_000 = 10_000_000_000_000_000_000_000_000_000
        // min = mid - spread = 990_000_000_000_000_000_000_000_000_000
        // max = mid + spread = 1_010_000_000_000_000_000_000_000_000_000
        assert_eq!(cached.min, configured_price - spread);
        assert_eq!(cached.max, configured_price + spread);
        assert_eq!(cached.median, configured_price);
        assert_eq!(cached.ledger_seq, 123);
        assert_eq!(cached.sources_used, vec!["fixed"]);
        assert_eq!(cached.signature.len(), 128);
        assert!(cached.timestamp > 0, "timestamp should be positive");
    }

    #[tokio::test]
    async fn signed_timestamp_is_the_oldest_contributing_observation() {
        // A Pyth feed published 30 s ago combined with a live `fixed` source
        // must be attested at the Pyth observation time, not the signing time
        // (#1258).
        let now = crate::current_timestamp_secs();
        let published_at = now - 30;
        let feed_id = "feed-1258";
        let token = TokenConfig {
            symbol: "TBTC".to_string(),
            display_symbol: Some("BTC".to_string()),
            stellar_address: "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES".to_string(),
            sources: vec!["pyth".to_string(), "fixed".to_string()],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: Some(feed_id.to_string()),
            fixed_price: Some("1000000000000000000000000000000".to_string()),
            min_sources: 2,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            pyth_max_confidence_bps: 100,
        };

        let mut pyth_prices = std::collections::HashMap::new();
        pyth_prices.insert(
            feed_id.to_string(),
            crate::pyth::PythPriceFeed {
                id: feed_id.to_string(),
                price: crate::pyth::PythPriceData {
                    price: "100000000".to_string(),
                    conf: None,
                    expo: -8,
                    publish_time: Some(published_at as i64),
                },
            },
        );

        let state = test_state(token.clone());
        let cached = build_cached_price(&state, &token, 123, &pyth_prices, false, None)
            .await
            .unwrap();

        assert_eq!(cached.timestamp, published_at);
        assert!(cached.sources_used.contains(&"pyth".to_string()));
        assert!(cached.sources_used.contains(&"fixed".to_string()));
    }

    #[test]
    fn aggregate_observed_at_picks_the_oldest_contributing_source() {
        let observations = std::collections::HashMap::from([
            ("pyth".to_string(), 990),
            ("binance".to_string(), 1_000),
        ]);
        let used = vec!["binance".to_string(), "pyth".to_string()];
        assert_eq!(aggregate_observed_at(&used, &observations, 1_005), 990);
    }

    #[test]
    fn aggregate_observed_at_ignores_rejected_sources() {
        let observations = std::collections::HashMap::from([
            ("pyth".to_string(), 990),
            ("binance".to_string(), 1_000),
            // Fetched, then rejected by the outlier filter: must not count.
            ("coinbase".to_string(), 100),
        ]);
        let used = vec!["pyth".to_string(), "binance".to_string()];
        assert_eq!(aggregate_observed_at(&used, &observations, 1_005), 990);
    }

    #[test]
    fn aggregate_observed_at_falls_back_without_observations() {
        let used = vec!["pyth".to_string()];
        let observations = std::collections::HashMap::new();
        assert_eq!(aggregate_observed_at(&used, &observations, 42), 42);
    }

    #[test]
    fn test_cached_price_is_stale() {
        let price = CachedPrice {
            token_address: "test".to_string(),
            symbol: "TEST".to_string(),
            display_symbol: "TEST".to_string(),
            keeper_index: 0,
            min: 100,
            max: 100,
            median: 100,
            timestamp: 1000,
            ledger_seq: 12345,
            sources_used: vec!["test".to_string()],
            signature: "sig".to_string(),
        };

        assert!(!price.is_stale(60, 1050));
        assert!(!price.is_stale(60, 1059));

        // Stale (exceeded or equal to stale_after)
        assert!(price.is_stale(60, 1060));
        assert!(price.is_stale(60, 1061));
        assert!(price.is_stale(60, 1100));

        // Edge cases
        assert!(!price.is_stale(60, 1000));
        // #1041 — clock moved backward: timestamp > now means indeterminate age,
        // treated as stale rather than silently reported as fresh.
        assert!(price.is_stale(60, 0));
        assert!(price.is_stale(0, 1001));
        assert!(price.is_stale(0, 1000));
    }

    #[test]
    fn test_cached_price_is_stale_clock_backward() {
        let price = CachedPrice {
            token_address: "test".to_string(),
            symbol: "TEST".to_string(),
            display_symbol: "TEST".to_string(),
            keeper_index: 0,
            min: 100,
            max: 100,
            median: 100,
            timestamp: u64::MAX,
            ledger_seq: 12345,
            sources_used: vec!["test".to_string()],
            signature: "sig".to_string(),
        };

        // #1041 — timestamp far ahead of now: treated as stale, not fresh.
        // Previously, saturating_sub silently clamped age to 0.
        assert!(price.is_stale(60, 1000));
    }
}
