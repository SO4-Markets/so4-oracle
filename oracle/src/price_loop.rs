use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use shared_config::TokenConfig;
use tokio::time::{interval, timeout, MissedTickBehavior};
use tracing::Instrument;

use crate::prices::AggregatedPrice;
use crate::retry::MAX_BACKOFF_DELAY_MS;
use crate::state::{AppState, CachedPrice, FailedSubmission};

/// Price with its observation timestamp for tracking staleness correctly.
#[derive(Debug, Clone, Copy)]
struct PriceWithObservationTime {
    price: i128,
    observation_time: u64,
}

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

/// A price-source failure tagged with the source and token it came from, set
/// where the error is raised so the failure metric and the failure ring read
/// one identity instead of a parallel argument list (#1450).
#[derive(Debug, Clone)]
pub struct SourceFailure {
    pub source: String,
    pub symbol: String,
    pub token: String,
    pub error: PriceSourceError,
}

impl std::fmt::Display for SourceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for SourceFailure {}

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
        if state
            .keeper_balance_below_min
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!("keeper balance below minimum; skipping price cycle");
            continue;
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
/// Runs one price cycle under a fresh cycle id (#1455): every log line in the
/// cycle carries it as `cycle_id`, and outbound source/RPC calls send it as
/// `x-request-id`, so one cycle's fetches can be grouped and traced.
pub async fn run_price_cycle(state: Arc<AppState>) {
    let cycle_id = crate::http::new_cycle_id();
    let span = tracing::info_span!("price_cycle", cycle_id = %cycle_id);
    crate::http::with_correlation_id(cycle_id, run_price_cycle_inner(state))
        .instrument(span)
        .await
}

async fn run_price_cycle_inner(state: Arc<AppState>) {
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
                event = "price_cycle_timeout",
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
            &state.config.pyth_hermes_url,
            &pyth_feed_ids,
            state.config.pyth_api_key.as_ref().map(|key| key.as_str()),
        )
    );

    let ledger_seq = match ledger_result {
        Ok(ledger_seq) => ledger_seq,
        Err(error) => {
            tracing::error!(
                event = "price_cycle_aborted",
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
        match crate::binance::fetch_spot_prices(&state.config.binance_base_url, &binance_symbols)
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
                    source: None,
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
        event = "price_cycle_complete",
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
    let mut observation_times = Vec::new();

    for source in &token.sources {
        match fetch_source_with_retry(
            source,
            token,
            &state.config,
            state.config.pyth_api_key.as_ref().map(|key| key.as_str()),
            pyth_prices,
            pyth_batch_failed,
            binance_prices,
        )
        .await
        {
            Ok(price_with_time) => {
                prices.push(price_with_time.price);
                observation_times.push(price_with_time.observation_time);
                sources.push(source.clone());
            }
            Err(failure) => {
                let error_str = failure.to_string();
                state.metrics.record_token_source_fetch_failure(
                    &failure.symbol,
                    &failure.token,
                    &failure.source,
                );
                let operation = format!("source:{}:{}", failure.symbol, failure.source);
                tracing::warn!(symbol = %failure.symbol, source = %failure.source, error = %error_str, "price source failed");
                let ctx = ErrorContext {
                    token: failure.token,
                    symbol: failure.symbol,
                    source: Some(failure.source),
                    ledger_seq,
                };
                record_error_with_context(state, operation, error_str, ctx).await;
            }
        }
    }

    state
        .metrics
        .set_token_sources_available(&token.symbol, prices.len());

    let aggregate = crate::prices::aggregate_prices(
        &prices,
        &sources,
        &observation_times,
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

    signed_cached_price(state, token, ledger_seq, aggregate)
}

async fn fetch_source_with_retry(
    source: &str,
    token: &TokenConfig,
    config: &crate::config::Config,
    pyth_api_key: Option<&str>,
    pyth_prices: &std::collections::HashMap<String, crate::pyth::PythPriceFeed>,
    pyth_batch_failed: bool,
    binance_prices: Option<&std::collections::HashMap<String, i128>>,
) -> Result<PriceWithObservationTime, SourceFailure> {
    crate::retry::retry_with_backoff(
        || async {
            fetch_source_price(
                source,
                token,
                config,
                pyth_api_key,
                pyth_prices,
                pyth_batch_failed,
                binance_prices,
            )
            .await
        },
        SOURCE_RETRY_ATTEMPTS,
        SOURCE_RETRY_BASE_DELAY_MS,
        MAX_BACKOFF_DELAY_MS,
    )
    .await
    .map_err(|error| SourceFailure {
        source: source.to_string(),
        symbol: token.symbol.clone(),
        token: token.stellar_address.clone(),
        error,
    })
}

async fn fetch_source_price(
    source: &str,
    token: &TokenConfig,
    config: &crate::config::Config,
    pyth_api_key: Option<&str>,
    pyth_prices: &std::collections::HashMap<String, crate::pyth::PythPriceFeed>,
    pyth_batch_failed: bool,
    binance_prices: Option<&std::collections::HashMap<String, i128>>,
) -> Result<PriceWithObservationTime, PriceSourceError> {
    let now = crate::current_timestamp_secs();
    match source {
        "binance" => {
            let symbol = token
                .binance_symbol
                .as_ref()
                .ok_or_else(|| PriceSourceError::Config("missing binance_symbol".to_string()))?;
            if let Some(prices) = binance_prices {
                prices
                    .get(symbol)
                    .copied()
                    .map(|price| PriceWithObservationTime {
                        price,
                        observation_time: now,
                    })
                    .ok_or_else(|| {
                        PriceSourceError::Config(format!(
                            "binance symbol not returned in batch: {symbol}"
                        ))
                    })
            } else {
                // Batch request failed or no batch was made — fall back to a
                // single-symbol request so this token still gets a price.
                let results = crate::binance::fetch_spot_prices(
                    &config.binance_base_url,
                    std::slice::from_ref(symbol),
                )
                .await
                .map_err(PriceSourceError::Binance)?;
                results
                    .into_iter()
                    .find(|(got_symbol, _)| got_symbol == symbol)
                    .map(|(_, price)| PriceWithObservationTime {
                        price,
                        observation_time: now,
                    })
                    .ok_or_else(|| {
                        PriceSourceError::Config(format!("binance symbol not returned: {symbol}"))
                    })
            }
        }
        "coinbase" => {
            let symbol = token
                .coinbase_symbol
                .as_ref()
                .ok_or_else(|| PriceSourceError::Config("missing coinbase_symbol".to_string()))?;
            crate::coinbase::fetch_spot_price(&config.coinbase_base_url, symbol)
                .await
                .map(|price| PriceWithObservationTime {
                    price,
                    observation_time: now,
                })
                .map_err(PriceSourceError::Coinbase)
        }
        "pyth" => {
            let feed_id = token
                .pyth_feed_id
                .as_ref()
                .ok_or_else(|| PriceSourceError::Config("missing pyth_feed_id".to_string()))?;
            if let Some(feed) = pyth_prices.get(feed_id) {
                crate::pyth::validate_pyth_price(
                    &feed.price,
                    now,
                    token.stale_after_seconds,
                    token.pyth_max_confidence_bps,
                )
                .map(|result| PriceWithObservationTime {
                    price: result.price,
                    observation_time: result.observation_time,
                })
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
                crate::pyth::fetch_pyth_prices(&config.pyth_hermes_url, &[feed_id], pyth_api_key)
                    .await
                    .and_then(|mut feeds| {
                        feeds.remove(feed_id).ok_or_else(|| {
                            crate::pyth::PythPriceError::MissingFeedId(feed_id.clone())
                        })
                    })
                    .and_then(|feed| {
                        crate::pyth::validate_pyth_price(
                            &feed.price,
                            now,
                            token.stale_after_seconds,
                            token.pyth_max_confidence_bps,
                        )
                        .map(|result| PriceWithObservationTime {
                            price: result.price,
                            observation_time: result.observation_time,
                        })
                    })
            }
            .map_err(PriceSourceError::Pyth)
        }
        "fixed" => crate::fixed::fixed_price(token)
            .map(|price| PriceWithObservationTime {
                price,
                observation_time: now,
            })
            .map_err(PriceSourceError::Fixed),
        other => Err(PriceSourceError::Unsupported(other.to_string())),
    }
}

/// Signs the aggregated price payload and returns a CachedPrice (Resolves Issue #393).
/// The signature matches the required format and is encoded to a 128 hex character string.
fn signed_cached_price(
    state: &Arc<AppState>,
    token: &TokenConfig,
    ledger_seq: u32,
    aggregate: AggregatedPrice,
) -> Result<CachedPrice, String> {
    // Use the minimum observation time from all sources used, not the signing time.
    // This ensures staleness is measured from the oldest contributing price.
    let timestamp = aggregate.min_observation_time;
    // Sign with the key decoded once at config load (#1226). Decoding failures
    // are boot failures (#1473), so `None` means the key was never configured.
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
        None => return Err("KEEPER_PRIVATE_KEY is not configured".to_string()),
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
    source: Option<String>,
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
            source: None,
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
        source: ctx.source,
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
    use crate::config::{Config, Network, PriceFeedConfig};
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
            keeper_private_key: Some(std::sync::Arc::new(
                crate::signing::signing_key_from_hex(
                    "1111111111111111111111111111111111111111111111111111111111111111",
                )
                .unwrap(),
            )),
            keeper_secret_key: None,
            keeper_account_id: "GACCOUNT".to_string(),
            keeper_index: 0,
            admin_api_token: None,
            pyth_api_key: None,
            coinbase_base_url: crate::coinbase::COINBASE_EXCHANGE_RATES_URL.to_string(),
            binance_base_url: crate::binance::BINANCE_TICKER_PRICE_URL.to_string(),
            pyth_hermes_url: crate::pyth::PYTH_HERMES_URL.to_string(),
            min_keeper_balance_xlm: 0.0,
            set_prices_tx_fee: crate::config::DEFAULT_SET_PRICES_TX_FEE,
            keeper_tx_fee: crate::config::DEFAULT_KEEPER_TX_FEE,
            price_loop_interval: Duration::from_millis(1),
            keeper_loop_interval: Duration::from_millis(1),
            price_feed: PriceFeedConfig {
                tokens: vec![token],
            },
        };
        Arc::new(AppState::new(Arc::new(config)))
    }

    // #1449 — every variant is classified on purpose. `variant` has no
    // wildcard arm, so a new variant fails to compile until it is listed, and
    // the coverage check fails until it has a case.
    #[test]
    fn every_price_source_error_variant_is_classified() {
        use crate::retry::Retryable;

        fn variant(err: &PriceSourceError) -> &'static str {
            match err {
                PriceSourceError::Binance(_) => "Binance",
                PriceSourceError::Coinbase(_) => "Coinbase",
                PriceSourceError::Pyth(_) => "Pyth",
                PriceSourceError::Fixed(_) => "Fixed",
                PriceSourceError::Config(_) => "Config",
                PriceSourceError::Unsupported(_) => "Unsupported",
            }
        }
        let binance = crate::binance::BinancePriceError::NetworkError("x".into());
        let coinbase = crate::coinbase::CoinbasePriceError::MissingUsdRate;
        let pyth = crate::pyth::PythPriceError::JsonError("x".into());
        let fixed = crate::fixed::FixedPriceError::MissingFixedPrice;
        let cases = [
            (PriceSourceError::Binance(binance), true),
            (PriceSourceError::Coinbase(coinbase), false),
            (PriceSourceError::Pyth(pyth), false),
            (PriceSourceError::Fixed(fixed), false),
            (PriceSourceError::Config("x".into()), false),
            (PriceSourceError::Unsupported("x".into()), false),
        ];
        let seen: std::collections::HashSet<_> = cases.iter().map(|(e, _)| variant(e)).collect();
        assert_eq!(seen.len(), 6);
        for (err, expected) in &cases {
            assert_eq!(err.is_retryable(), *expected, "{err:?}");
        }
    }

    // #1450 — a source failure carries its own identity, and the failure ring
    // entry is built from it rather than from a parallel argument list.
    #[tokio::test]
    async fn source_failure_identity_reaches_failure_ring() {
        let token = TokenConfig {
            symbol: "TUSDC".to_string(),
            display_symbol: Some("USDC".to_string()),
            stellar_address: "CTOKEN".to_string(),
            sources: vec!["fixed".to_string()],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: None,
            fixed_price: None,
            min_sources: 1,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            pyth_max_confidence_bps: 100,
        };
        let state = test_state(token.clone());
        let no_pyth = std::collections::HashMap::new();

        let config = crate::config::Config::default_for_tests();
        let failure =
            fetch_source_with_retry("fixed", &token, &config, None, &no_pyth, false, None)
                .await
                .unwrap_err();
        assert_eq!(failure.source, "fixed");
        assert_eq!(failure.symbol, "TUSDC");
        assert_eq!(failure.token, "CTOKEN");
        assert!(matches!(
            failure.error,
            PriceSourceError::Fixed(crate::fixed::FixedPriceError::MissingFixedPrice)
        ));

        let built = build_cached_price(&state, &token, 7, &no_pyth, false, None).await;
        assert!(built.is_err());
        let failures = state.failures.lock().await;
        let entry = failures
            .iter()
            .next_back()
            .expect("source failure recorded");
        assert_eq!(entry.operation, "source:TUSDC:fixed");
        assert_eq!(entry.source.as_deref(), Some("fixed"));
        assert_eq!(entry.symbol, "TUSDC");
        assert_eq!(entry.token, "CTOKEN");
        assert_eq!(entry.ledger_seq, 7);
    }

    // ── #1416: unit coverage for the pure parts of the price path ───────────

    fn unit_token(sources: &[&str]) -> TokenConfig {
        TokenConfig {
            symbol: "TUSDC".to_string(),
            display_symbol: Some("USDC".to_string()),
            stellar_address: "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES".to_string(),
            sources: sources.iter().map(|s| s.to_string()).collect(),
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: Some("feed-1".to_string()),
            fixed_price: None,
            min_sources: 1,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            pyth_max_confidence_bps: 100,
        }
    }

    fn unit_aggregate() -> AggregatedPrice {
        AggregatedPrice {
            min: 990,
            max: 1_010,
            median: 1_000,
            sources_used: vec!["fixed".to_string()],
            rejected_sources: Vec::new(),
            min_observation_time: 1_700_000_000,
        }
    }

    // The signature in a CachedPrice must verify against build_price_message
    // for exactly the values placed in that CachedPrice.
    #[test]
    fn signed_cached_price_signature_matches_its_own_fields() {
        use ed25519_dalek::Verifier;

        let token = unit_token(&["fixed"]);
        let state = test_state(token.clone());
        let result = signed_cached_price(&state, &token, 42, unit_aggregate());
        let cached = result.unwrap();

        // The timestamp is the oldest contributing observation, not signing time.
        assert_eq!(cached.timestamp, 1_700_000_000);
        assert_eq!(cached.min, 990);
        assert_eq!(cached.max, 1_010);
        assert_eq!(cached.median, 1_000);
        assert_eq!(cached.ledger_seq, 42);

        let message = crate::signing::build_price_message(
            &state.config.network_passphrase,
            cached.ledger_seq,
            &cached.token_address,
            cached.min,
            cached.max,
            cached.timestamp,
        );
        let raw = hex::decode(&cached.signature).unwrap();
        let bytes: [u8; 64] = raw.try_into().unwrap();
        let signature = ed25519_dalek::Signature::from_bytes(&bytes);
        let key = state.price_signing_key.as_ref().unwrap().verifying_key();
        assert!(key.verify(&message, &signature).is_ok());
    }

    #[test]
    fn signed_cached_price_errors_without_a_signing_key() {
        let token = unit_token(&["fixed"]);
        let state = Arc::new(AppState::new(Arc::new(Config::default_for_tests())));
        let result = signed_cached_price(&state, &token, 42, unit_aggregate());
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn finish_cycle_records_counters_and_clears_running_flag() {
        let state = test_state(unit_token(&["fixed"]));
        state.cycle_status.write().await.price_cycle_running = true;

        finish_cycle(&state, Instant::now(), 3, 2, 1).await;
        finish_cycle(&state, Instant::now(), 1, 0, 0).await;

        let metrics = state.metrics.to_response();
        assert_eq!(metrics.price_cycle_count, 2);
        assert_eq!(metrics.token_fetch_ok, 4);
        assert_eq!(metrics.token_fetch_failures, 2);
        let status = state.cycle_status.read().await;
        assert!(!status.price_cycle_running);
        assert!(status.last_price_cycle_at.is_some());
    }

    // A permanent error returns on the first attempt: no backoff sleep runs,
    // so the paused clock never advances.
    #[tokio::test(start_paused = true)]
    async fn fetch_source_with_retry_does_not_retry_a_permanent_error() {
        let token = unit_token(&["nope"]);
        let no_pyth = std::collections::HashMap::new();
        let started = tokio::time::Instant::now();

        let config = crate::config::Config::default_for_tests();
        let failure = fetch_source_with_retry("nope", &token, &config, None, &no_pyth, false, None)
            .await
            .unwrap_err();

        assert!(matches!(failure.error, PriceSourceError::Unsupported(_)));
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    // A transient error is retried with backoff before giving up.
    #[tokio::test(start_paused = true)]
    async fn fetch_source_with_retry_backs_off_on_a_transient_error() {
        let token = unit_token(&["pyth"]);
        let no_pyth = std::collections::HashMap::new();
        let started = tokio::time::Instant::now();

        // Batch failed and the feed is absent: a retryable network error.
        let config = crate::config::Config::default_for_tests();
        let failure = fetch_source_with_retry("pyth", &token, &config, None, &no_pyth, true, None)
            .await
            .unwrap_err();

        assert!(matches!(
            failure.error,
            PriceSourceError::Pyth(crate::pyth::PythPriceError::NetworkError(_))
        ));
        assert!(started.elapsed() > Duration::ZERO, "backoff expected");
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
            keeper_private_key: Some(std::sync::Arc::new(
                crate::signing::signing_key_from_hex(
                    "1111111111111111111111111111111111111111111111111111111111111111",
                )
                .unwrap(),
            )),
            keeper_secret_key: None,
            keeper_account_id: "GACCOUNT".to_string(),
            keeper_index: 0,
            admin_api_token: None,
            pyth_api_key: None,
            coinbase_base_url: crate::coinbase::COINBASE_EXCHANGE_RATES_URL.to_string(),
            binance_base_url: crate::binance::BINANCE_TICKER_PRICE_URL.to_string(),
            pyth_hermes_url: crate::pyth::PYTH_HERMES_URL.to_string(),
            min_keeper_balance_xlm: 0.0,
            set_prices_tx_fee: crate::config::DEFAULT_SET_PRICES_TX_FEE,
            keeper_tx_fee: crate::config::DEFAULT_KEEPER_TX_FEE,
            price_loop_interval: Duration::from_millis(50),
            keeper_loop_interval: Duration::from_millis(50),
            price_feed: PriceFeedConfig { tokens: vec![] },
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
